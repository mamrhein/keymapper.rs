// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! COSMIC — cosmic-toplevel-info Wayland protocol active-window query.
//!
//! COSMIC exposes the active window through the `zcosmic_toplevel_info_v1`
//! Wayland global (crate `cosmic-protocols`) rather than D-Bus.  The probe
//! connects, collects the initial toplevel batch (toplevels, their app ids,
//! and their states), and disconnects again.
//!
//! The protocol is bound at version 1: current COSMIC builds advertise
//! version 3, but do not implement the version >= 2 client flow — the
//! `get_cosmic_toplevel` request never receives its `state` and `done`
//! events, so the active toplevel can never be determined.  Version 1,
//! which sends all toplevels and their states eagerly, is fully supported.

use cosmic_protocols::toplevel_info::v1::client::{
    zcosmic_toplevel_handle_v1, zcosmic_toplevel_info_v1,
};
use wayland_client::{
    Connection, Dispatch, QueueHandle, protocol::wl_registry,
};

use super::connect_wayland;

/// Query the active window on COSMIC.
///
/// Connects, collects the initial toplevel batch, and disconnects.  On
/// non-COSMIC compositors the `zcosmic_toplevel_info_v1` global is not
/// advertised, so this probe is a no-op.
pub(super) fn query_cosmic() -> String {
    let Some(conn) = connect_wayland() else {
        return String::new();
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

    // Version 1 sends all toplevels eagerly; the `stop` request ends the
    // batch and triggers the `finished` event.
    toplevel_info.stop();

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
    /// Handle from `zcosmic_toplevel_info_v1`; carries the `app_id` and
    /// the `activated` state.
    cosmic: zcosmic_toplevel_handle_v1::ZcosmicToplevelHandleV1,
    app_id: Option<String>,
    activated: bool,
}

/// Event-loop state for the one-shot COSMIC active-window query.
#[derive(Default)]
struct CosmicQueryState {
    toplevel_info: Option<zcosmic_toplevel_info_v1::ZcosmicToplevelInfoV1>,
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
            // Bound at version 1 (see the module doc): current COSMIC
            // builds advertise version 3 but do not implement the
            // version >= 2 client flow.
            && &*interface == "zcosmic_toplevel_info_v1"
        {
            state.toplevel_info =
                Some(registry.bind(name, version.min(1), qh, ()));
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
            // A toplevel was created; all of its properties (app id,
            // state) follow on the handle.
            zcosmic_toplevel_info_v1::Event::Toplevel { toplevel } => {
                state.toplevels.push(CosmicToplevel {
                    cosmic: toplevel,
                    app_id: None,
                    activated: false,
                });
            }
            // The `stop` request was honored; the initial batch is
            // complete.
            zcosmic_toplevel_info_v1::Event::Finished => state.done = true,
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
            .find(|toplevel| toplevel.cosmic == *handle)
        else {
            return;
        };

        match event {
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
