// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! XFCE (labwc) — wlr-foreign-toplevel-management Wayland protocol
//! active-window query.
//!
//! The XFCE Wayland session runs labwc as its compositor.  labwc offers
//! neither a D-Bus interface nor a sway-compatible IPC socket, so the
//! active window is read from the `zwlr_foreign_toplevel_manager_v1`
//! Wayland global.  The probe connects, collects the initial
//! toplevel batch (toplevels, their app ids, and their states), and
//! disconnects again.
//!
//! The protocol is bound at version 1: all state and property events the
//! probe needs (`app_id`, `state`, `done`) are version 1, and the version
//! 1 flow — every open toplevel and its properties are sent eagerly at
//! bind time — is what the probe relies on.  Requiring the `activated`
//! state keeps the reported window correct even though many compositors
//! advertise the same global, which is why this backend is probed last.

use wayland_client::{
    Connection, Dispatch, QueueHandle,
    protocol::{wl_output, wl_registry},
};
use wayland_protocols_wlr::foreign_toplevel::v1::client::{
    zwlr_foreign_toplevel_handle_v1::{
        self, State as ToplevelState, ZwlrForeignToplevelHandleV1,
    },
    zwlr_foreign_toplevel_manager_v1::{self, ZwlrForeignToplevelManagerV1},
};

use super::connect_wayland;

/// Query the active window on the XFCE (labwc) compositor.
///
/// Connects, collects the initial toplevel batch, and disconnects.  On
/// compositors that do not advertise `zwlr_foreign_toplevel_manager_v1`
/// this probe is a no-op.
pub(super) fn query_xfce() -> String {
    let Some(conn) = connect_wayland() else {
        return String::new();
    };

    let mut event_queue = conn.new_event_queue();
    let qh = event_queue.handle();
    let display = conn.display();
    let _registry = display.get_registry(&qh, ());

    let mut state = XfceQueryState::default();

    // Receive the list of compositor globals.
    if event_queue.roundtrip(&mut state).is_err() {
        return String::new();
    }

    // `zwlr_foreign_toplevel_manager_v1` is a wlroots-family global; if it
    // is absent, there is nothing to query and this probe is a no-op.
    let Some(toplevel_manager) = state.toplevel_manager.clone() else {
        return String::new();
    };

    // We only care about the current state; `stop` ends the event stream,
    // and the `finished` event confirms the batch is complete.
    toplevel_manager.stop();

    // Collect the initial toplevel batch.  The compositor flushes every
    // pending toplevel event before answering the roundtrip's `sync`, and
    // a well-behaved one honors `stop` with `finished` in the same flush.
    // A bounded number of roundtrips keeps this probe from hanging on a
    // misbehaving compositor.
    for _ in 0..5 {
        if state.done {
            break;
        }
        if event_queue.roundtrip(&mut state).is_err() {
            return String::new();
        }
    }

    state
        .toplevels
        .iter()
        .find(|toplevel| toplevel.activated && !toplevel.closed)
        .and_then(|toplevel| toplevel.app_id.clone())
        .filter(|app_id| !app_id.is_empty())
        .unwrap_or_default()
}

/// A single toplevel as reported by the foreign-toplevel-management
/// protocol.
struct XfceToplevel {
    /// Handle from `zwlr_foreign_toplevel_manager_v1`; carries the
    /// `app_id` and the `activated` state.
    handle: ZwlrForeignToplevelHandleV1,
    app_id: Option<String>,
    activated: bool,
    /// Set once the compositor sent `closed`; the toplevel must no longer
    /// be reported as active.
    closed: bool,
}

/// Event-loop state for the one-shot XFCE active-window query.
#[derive(Default)]
struct XfceQueryState {
    toplevel_manager: Option<ZwlrForeignToplevelManagerV1>,
    toplevels: Vec<XfceToplevel>,
    /// Set once the compositor finished the toplevel event stream.
    done: bool,
}

impl Dispatch<wl_registry::WlRegistry, ()> for XfceQueryState {
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
            // Bound at version 1 (see the module doc).
            && &*interface == "zwlr_foreign_toplevel_manager_v1"
        {
            state.toplevel_manager =
                Some(registry.bind(name, version.min(1), qh, ()));
        }
    }
}

impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for XfceQueryState {
    fn event(
        state: &mut Self,
        _proxy: &ZwlrForeignToplevelManagerV1,
        event: zwlr_foreign_toplevel_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            // A toplevel was created; all of its properties (app id,
            // state) follow on the handle.
            zwlr_foreign_toplevel_manager_v1::Event::Toplevel { toplevel } => {
                state.toplevels.push(XfceToplevel {
                    handle: toplevel,
                    app_id: None,
                    activated: false,
                    closed: false,
                });
            }
            // The `stop` request was honored; the event stream is
            // complete.
            zwlr_foreign_toplevel_manager_v1::Event::Finished => {
                state.done = true;
            }
            _ => {}
        }
    }

    wayland_client::event_created_child!(
        XfceQueryState,
        ZwlrForeignToplevelManagerV1,
        [zwlr_foreign_toplevel_manager_v1::EVT_TOPLEVEL_OPCODE => (
            ZwlrForeignToplevelHandleV1,
            ()
        )]
    );
}

impl Dispatch<ZwlrForeignToplevelHandleV1, ()> for XfceQueryState {
    fn event(
        state: &mut Self,
        handle: &ZwlrForeignToplevelHandleV1,
        event: zwlr_foreign_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(toplevel) = state
            .toplevels
            .iter_mut()
            .find(|toplevel| toplevel.handle == *handle)
        else {
            return;
        };

        match event {
            zwlr_foreign_toplevel_handle_v1::Event::AppId { app_id } => {
                toplevel.app_id.get_or_insert(app_id);
            }
            // The state is a list of 32-bit values; only the `activated`
            // entry is of interest.
            zwlr_foreign_toplevel_handle_v1::Event::State { state } => {
                toplevel.activated =
                    state.as_chunks::<4>().0.iter().any(|chunk| {
                        ToplevelState::try_from(u32::from_ne_bytes(*chunk))
                            .is_ok_and(|state| {
                                state == ToplevelState::Activated
                            })
                    });
            }
            // A closed toplevel must not be reported as active, even if
            // its last reported state was `activated`.
            zwlr_foreign_toplevel_handle_v1::Event::Closed => {
                toplevel.closed = true;
            }
            _ => {}
        }
    }

    wayland_client::event_created_child!(
        XfceQueryState,
        ZwlrForeignToplevelHandleV1,
        [zwlr_foreign_toplevel_handle_v1::EVT_OUTPUT_ENTER_OPCODE => (
            wl_output::WlOutput,
            ()
        )]
    );
}

/// `output_enter` creates a `wl_output` child; the probe never inspects
/// outputs, but the child registration above requires a `Dispatch` impl.
impl Dispatch<wl_output::WlOutput, ()> for XfceQueryState {
    fn event(
        _state: &mut Self,
        _proxy: &wl_output::WlOutput,
        _event: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

#[cfg(test)]
mod tests {
    use super::query_xfce;

    /// The probe must fail gracefully instead of panicking, whether or
    /// not a Wayland compositor is reachable.
    #[test]
    fn does_not_panic() {
        // The probe returns either an empty string or an application id,
        // never the `"unknown"` sentinel (that is produced by
        // `get_active_app_name`).
        let result = query_xfce();
        assert_ne!(result, "unknown");
    }
}
