// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Wayland active window query via compositor-specific mechanisms.
//!
//! Probes compositors in order: KDE (KWin), GNOME, COSMIC, then
//! wlroots-based compositors and Hyprland.  Each probe is a synchronous,
//! blocking operation that connects, queries, and disconnects immediately.
//!
//! Wherever a compositor reports the active window's owning process (KDE,
//! GNOME), the PID is resolved to the `.desktop` application id via
//! [`super::apps::resolve_process_app_id`] — the same namespace
//! [`super::apps::list_app_names`] produces.  The remaining backends return
//! the app id / class the compositor itself reports for the active window.
//! COSMIC has no D-Bus interface for this; it reports the active window
//! through the `cosmic-toplevel-info` Wayland protocol extension instead.

use std::time::Duration;

use cosmic_protocols::toplevel_info::v1::client::{
    zcosmic_toplevel_handle_v1, zcosmic_toplevel_info_v1,
};
use wayland_client::{
    Connection, Dispatch, QueueHandle, protocol::wl_registry,
};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::{
    ext_foreign_toplevel_handle_v1, ext_foreign_toplevel_list_v1,
};

/// Synchronously query the current foreground application name on Wayland.
///
/// Tries each compositor backend in order and returns the first successful
/// result.  Returns `"unknown"` if no backend succeeds.
pub fn get_active_app_name() -> String {
    // Probe each compositor in priority order.
    let candidates: [fn() -> String; 5] = [
        query_kde,
        query_gnome,
        query_cosmic,
        query_wlroots,
        query_hyprland,
    ];

    for query in candidates {
        let result = query();
        if !result.is_empty() && result != "unknown" {
            return result;
        }
    }

    "unknown".to_string()
}

// ---------------------------------------------------------------------------
// KDE (KWin) — D-Bus synchronous query
// ---------------------------------------------------------------------------

fn query_kde() -> String {
    use zbus::blocking::Connection;

    let conn = match Connection::session() {
        Ok(c) => c,
        Err(_) => return String::new(),
    };

    // KWin's Workspace3 interface exposes activeWindow() which returns
    // the PID of the active window.
    let pid: i32 = match conn.call_method(
        Some("org.kde.KWin"),
        "/KWin",
        Some("org.kde.kwin.Workspace3"),
        "activeWindow",
        &(),
    ) {
        Ok(reply) => match reply.body().deserialize() {
            Ok(v) => v,
            Err(_) => return String::new(),
        },
        Err(_) => return String::new(),
    };

    if pid > 0 {
        return super::apps::resolve_process_app_id(pid as u32)
            .unwrap_or_default();
    }

    String::new()
}

// ---------------------------------------------------------------------------
// GNOME — D-Bus Shell.Eval introspection
// ---------------------------------------------------------------------------

fn query_gnome() -> String {
    use zbus::blocking::Connection;

    let conn = match Connection::session() {
        Ok(c) => c,
        Err(_) => return String::new(),
    };

    // GNOME Shell exposes a read-only Eval interface that can run
    // JavaScript in the shell context.  We use it to query the focused
    // window's owning PID and resolve it against the .desktop files.  The
    // wm_class alone is not used because it is not reliably a .desktop
    // application id.
    let js = "global.display.focus_window && \
              global.display.focus_window.get_pid() || 0";

    let pid: i32 = match conn.call_method(
        Some("org.gnome.Shell"),
        "/org/gnome/Shell",
        Some("org.gnome.Shell.Eval"),
        "Eval",
        &(js,),
    ) {
        Ok(reply) => match reply.body().deserialize() {
            Ok(v) => v,
            Err(_) => return String::new(),
        },
        Err(_) => return String::new(),
    };

    if pid > 0 {
        return super::apps::resolve_process_app_id(pid as u32)
            .unwrap_or_default();
    }

    String::new()
}

// ---------------------------------------------------------------------------
// COSMIC — cosmic-toplevel-info Wayland protocol
// ---------------------------------------------------------------------------

/// Query the active window on COSMIC.
///
/// COSMIC exposes the active window through the `zcosmic_toplevel_info_v1`
/// Wayland global (crate `cosmic-protocols`) rather than D-Bus.  The
/// protocol extends `ext-foreign-toplevel-list-v1`: the foreign toplevel
/// handle carries the `app_id`, the COSMIC toplevel handle carries the
/// state including `activated`.  The probe connects, collects the initial
/// toplevel batch, and disconnects again.
fn query_cosmic() -> String {
    let conn = match Connection::connect_to_env() {
        Ok(conn) => conn,
        Err(_) => return String::new(),
    };

    let mut event_queue = conn.new_event_queue();
    let qh = event_queue.handle();
    let display = conn.display();
    let _registry = display.get_registry(&qh, ());

    let mut state = CosmicQueryState::default();

    // Receive the list of compositor globals.
    if event_queue.roundtrip(&mut state).is_err() {
        return String::new();
    }

    // `zcosmic_toplevel_info_v1` is only advertised by COSMIC, so on any
    // other Wayland compositor this probe is a no-op.
    let Some(toplevel_info) = state.toplevel_info.clone() else {
        return String::new();
    };

    if state.toplevel_info_version < 2 {
        // Protocol version 1 sends all toplevels eagerly; the `stop`
        // request ends the batch and triggers the `finished` event.
        toplevel_info.stop();
    }

    // Collect the initial toplevel batch.  The protocol explicitly allows
    // a client that only cares about the current state to perform
    // roundtrips until the batch is complete.  A bounded number of
    // roundtrips keeps this probe from hanging on a misbehaving
    // compositor.
    for _ in 0..5 {
        if state.done {
            break;
        }
        if event_queue.roundtrip(&mut state).is_err() {
            return String::new();
        }
    }

    // The `activated` state is only reported by the COSMIC protocol, so
    // requiring it keeps this probe COSMIC-specific even on wlroots
    // compositors that also implement the foreign-toplevel-list protocol.
    state
        .toplevels
        .iter()
        .find(|toplevel| toplevel.activated)
        .and_then(|toplevel| toplevel.app_id.clone())
        .filter(|app_id| !app_id.is_empty())
        .unwrap_or_default()
}

/// A single toplevel as reported by the COSMIC toplevel-info protocol.
struct CosmicToplevel {
    /// Handle from `ext-foreign-toplevel-list-v1`; carries the `app_id`
    /// (protocol version >= 2).
    foreign:
        Option<ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1>,
    /// Handle from `zcosmic_toplevel_info_v1`; carries the `activated`
    /// state.
    cosmic: Option<zcosmic_toplevel_handle_v1::ZcosmicToplevelHandleV1>,
    app_id: Option<String>,
    activated: bool,
}

/// Event-loop state for the one-shot COSMIC active-window query.
#[derive(Default)]
struct CosmicQueryState {
    foreign_list:
        Option<ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1>,
    toplevel_info: Option<zcosmic_toplevel_info_v1::ZcosmicToplevelInfoV1>,
    toplevel_info_version: u32,
    toplevels: Vec<CosmicToplevel>,
    /// Set once the compositor finished the initial toplevel batch.
    done: bool,
}

impl Dispatch<wl_registry::WlRegistry, ()> for CosmicQueryState {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match &*interface {
                // Only needed on protocol version >= 2, where the
                // toplevels arrive via the foreign-toplevel-list
                // protocol.
                "ext_foreign_toplevel_list_v1" => {
                    state.foreign_list =
                        Some(registry.bind(name, version.min(1), qh, ()));
                }
                "zcosmic_toplevel_info_v1" => {
                    let version = version.min(3);
                    state.toplevel_info_version = version;
                    state.toplevel_info =
                        Some(registry.bind(name, version, qh, ()));
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1, ()>
    for CosmicQueryState
{
    fn event(
        state: &mut Self,
        _proxy: &ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1,
        event: ext_foreign_toplevel_list_v1::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let ext_foreign_toplevel_list_v1::Event::Toplevel { toplevel } =
            event
        {
            // The `activated` state is only reported by the COSMIC
            // protocol, so every foreign toplevel is wrapped in a
            // `zcosmic_toplevel_handle_v1`.
            let cosmic = state
                .toplevel_info
                .as_ref()
                .filter(|_| state.toplevel_info_version >= 2)
                .map(|info| info.get_cosmic_toplevel(&toplevel, qh, ()));
            state.toplevels.push(CosmicToplevel {
                foreign: Some(toplevel),
                cosmic,
                app_id: None,
                activated: false,
            });
        }
    }

    wayland_client::event_created_child!(
        CosmicQueryState,
        ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1,
        [ext_foreign_toplevel_list_v1::EVT_TOPLEVEL_OPCODE => (
            ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
            ()
        )]
    );
}

impl Dispatch<ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1, ()>
    for CosmicQueryState
{
    fn event(
        state: &mut Self,
        handle: &ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
        event: ext_foreign_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(toplevel) = state
            .toplevels
            .iter_mut()
            .find(|toplevel| toplevel.foreign.as_ref() == Some(handle))
        else {
            return;
        };

        if let ext_foreign_toplevel_handle_v1::Event::AppId { app_id } = event
        {
            toplevel.app_id = Some(app_id);
        }
    }
}

impl Dispatch<zcosmic_toplevel_handle_v1::ZcosmicToplevelHandleV1, ()>
    for CosmicQueryState
{
    fn event(
        state: &mut Self,
        handle: &zcosmic_toplevel_handle_v1::ZcosmicToplevelHandleV1,
        event: zcosmic_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(toplevel) = state
            .toplevels
            .iter_mut()
            .find(|toplevel| toplevel.cosmic.as_ref() == Some(handle))
        else {
            return;
        };

        match event {
            // Legacy `app_id` (protocol version 1 only; version >= 2
            // reports it via the foreign toplevel handle).
            zcosmic_toplevel_handle_v1::Event::AppId { app_id } => {
                toplevel.app_id.get_or_insert(app_id);
            }
            // The state is a list of 32-bit values; only the `activated`
            // entry is of interest.
            zcosmic_toplevel_handle_v1::Event::State { state } => {
                toplevel.activated =
                    state.as_chunks::<4>().0.iter().any(|chunk| {
                        zcosmic_toplevel_handle_v1::State::try_from(
                            u32::from_ne_bytes(*chunk),
                        )
                        .is_ok_and(|state| {
                            state
                                == zcosmic_toplevel_handle_v1::State::Activated
                        })
                    });
            }
            _ => {}
        }
    }
}

impl Dispatch<zcosmic_toplevel_info_v1::ZcosmicToplevelInfoV1, ()>
    for CosmicQueryState
{
    fn event(
        state: &mut Self,
        _proxy: &zcosmic_toplevel_info_v1::ZcosmicToplevelInfoV1,
        event: zcosmic_toplevel_info_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            // Protocol version >= 2: initial toplevel batch is complete.
            zcosmic_toplevel_info_v1::Event::Done => state.done = true,
            // Protocol version 1: the `stop` request was honored.
            zcosmic_toplevel_info_v1::Event::Finished => state.done = true,
            // Protocol version 1: a toplevel was created.
            zcosmic_toplevel_info_v1::Event::Toplevel { toplevel } => {
                state.toplevels.push(CosmicToplevel {
                    foreign: None,
                    cosmic: Some(toplevel),
                    app_id: None,
                    activated: false,
                });
            }
            _ => {}
        }
    }

    wayland_client::event_created_child!(
        CosmicQueryState,
        zcosmic_toplevel_info_v1::ZcosmicToplevelInfoV1,
        [zcosmic_toplevel_info_v1::EVT_TOPLEVEL_OPCODE => (
            zcosmic_toplevel_handle_v1::ZcosmicToplevelHandleV1,
            ()
        )]
    );
}

// ---------------------------------------------------------------------------
// wlroots-based compositors (sway, niri) — IPC socket query
// ---------------------------------------------------------------------------

fn query_wlroots() -> String {
    // Sway and other wlroots-based compositors expose a JSON IPC protocol
    // over a Unix domain socket.
    let socket = std::env::var("SWAYSOCK").unwrap_or_else(|_| {
        std::env::var("XDG_RUNTIME_DIR")
            .map(|rd| format!("{rd}/sway-ipc.sock"))
            .unwrap_or_else(|_| "/run/user/1000/sway-ipc.sock".to_string())
    });

    if !std::path::Path::new(&socket).exists() {
        return String::new();
    }

    // Build the IPC get_tree message: length-prefixed binary protocol.
    let payload = r#"{"payload":"", "cmd":"get_tree"}"#;

    let mut msg = Vec::with_capacity(12 + payload.len());
    msg.extend_from_slice(b"sway"); // magic
    msg.extend_from_slice(&4u32.to_le_bytes()); // version
    msg.extend_from_slice(&(payload.len() as u32).to_le_bytes()); // length
    msg.extend_from_slice(payload.as_bytes()); // payload

    query_wlroots_socket(&socket, &msg)
}

fn query_wlroots_socket(socket: &str, request: &[u8]) -> String {
    use std::io::{Read, Write};

    let mut stream = match std::os::unix::net::UnixStream::connect(socket) {
        Ok(s) => {
            s.set_read_timeout(Some(Duration::from_millis(50))).ok();
            s
        }
        Err(_) => return String::new(),
    };

    if stream.write_all(request).is_err() {
        return String::new();
    }

    let mut response = Vec::new();
    if stream.read_to_end(&mut response).is_err() {
        return String::new();
    }

    // The IPC response uses the same length-prefixed binary protocol as the
    // request: 4-byte "sway" magic, 4-byte version, 4-byte payload length,
    // then the JSON payload.
    if response.len() < 12 {
        return String::new();
    }

    // Verify the magic bytes.
    if &response[0..4] != b"sway" {
        return String::new();
    }

    let payload_len = u32::from_le_bytes([
        response[8],
        response[9],
        response[10],
        response[11],
    ]) as usize;

    if response.len() < 12 + payload_len {
        return String::new();
    }

    let json = &response[12..12 + payload_len];
    let Ok(json_str) = std::str::from_utf8(json) else {
        return String::new();
    };
    extract_json_string(json_str, "app_id")
}

// ---------------------------------------------------------------------------
// Hyprland — control socket query
// ---------------------------------------------------------------------------

fn query_hyprland() -> String {
    // Hyprland exposes its IPC via a socket at:
    // /tmp/hypr/$HYPRLAND_INSTANCE_SIGNATURE/.socket2.sock
    let signature = match std::env::var("HYPRLAND_INSTANCE_SIGNATURE") {
        Ok(s) => s,
        Err(_) => return String::new(),
    };

    let socket = format!("/tmp/hypr/{signature}/.socket2.sock");
    if !std::path::Path::new(&socket).exists() {
        return String::new();
    }

    // Send the "j/activewindow" command to get JSON output about the
    // active window.
    use std::io::{Read, Write};

    let mut stream = match std::os::unix::net::UnixStream::connect(&socket) {
        Ok(s) => {
            s.set_read_timeout(Some(Duration::from_millis(50))).ok();
            s
        }
        Err(_) => return String::new(),
    };

    if stream.write_all(b"j/activewindow\n").is_err() {
        return String::new();
    }

    let mut response = Vec::new();
    if stream.read_to_end(&mut response).is_err() {
        return String::new();
    }

    // Parse the JSON response for the "class" field (X11 WM_CLASS equivalent).
    let Ok(json) = std::str::from_utf8(&response) else {
        return String::new();
    };

    extract_json_string(json, "class")
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Simple helper to extract a string value for a given key from a JSON-like
/// string.  Not a full parser — sufficient for the flat JSON responses from
/// compositor IPC sockets.
fn extract_json_string(json: &str, key: &str) -> String {
    let pattern = format!("\"{key}\"");
    let start = match json.find(&pattern) {
        Some(pos) => pos,
        None => return String::new(),
    };

    let after_key = &json[start + pattern.len()..];

    // Skip whitespace and the colon.
    let after_colon = match after_key.find(':') {
        Some(pos) => &after_key[pos + 1..],
        None => return String::new(),
    };

    // Find the opening quote of the value.
    let trimmed = after_colon.trim_start();
    let value_start = match trimmed.find('"') {
        Some(pos) => &trimmed[pos + 1..],
        None => return String::new(),
    };

    // Find the closing quote, handling escaped quotes.
    let mut result = String::new();
    let mut bytes = value_start.bytes();

    while let Some(b) = bytes.next() {
        if b == b'\\' {
            // Escaped character — take the next byte literally.
            if let Some(next) = bytes.next() {
                result.push(next as char);
            }
        } else if b == b'"' {
            // End of string.
            return result;
        } else {
            result.push(b as char);
        }
    }

    result
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::query_cosmic;

    /// The probe must fail gracefully instead of panicking, whether or
    /// not a Wayland compositor is reachable.
    #[test]
    fn does_not_panic() {
        // The probe returns either an empty string or an application id,
        // never the `"unknown"` sentinel (that is produced by
        // `get_active_app_name`).
        let result = query_cosmic();
        assert_ne!(result, "unknown");
    }
}
