// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! egui/eframe application for the keyboard monitor.
//!
//! On Linux the monitor runs headless and captures the daemon's output
//! directly (see [`super::linux`]); on other platforms it creates a small
//! focused window that captures all keyboard events.  Both write events
//! to the output file and exit cleanly on SIGTERM/SIGINT.

use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use eframe::egui;

use super::{ModifierState, OutputEvent, map_egui_key, writer::EventWriter};
use crate::common::hid_usage::HidUsage;

/// The egui application state.
pub struct MonitorApp {
    /// File writer for captured events.
    writer: EventWriter,
    /// Signal flag set by the signal handler on SIGTERM/SIGINT.
    shutdown: Arc<AtomicBool>,
    /// Non-modifier keys that were pressed in the previous frame.
    prev_keys_down: std::collections::HashSet<HidUsage>,
    /// Modifier state from the previous frame.
    prev_modifiers: ModifierState,
}

impl MonitorApp {
    pub fn new(output_path: PathBuf, shutdown: Arc<AtomicBool>) -> Self {
        let writer = EventWriter::new(&output_path)
            .expect("failed to open output file for event logging");
        Self {
            writer,
            shutdown,
            prev_keys_down: std::collections::HashSet::new(),
            prev_modifiers: ModifierState::default(),
        }
    }
}

impl eframe::App for MonitorApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx();

        // Check for shutdown signal.
        if self.shutdown.load(Ordering::Relaxed) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

        // Process keyboard events by comparing current state with previous
        // frame.  We use `keys_down` as the source of truth because raw
        // events may be coalesced or filtered by the backend.

        // --- Non-modifier keys ---
        let current_keys: std::collections::HashSet<HidUsage> =
            ctx.input(|i| {
                i.keys_down
                    .iter()
                    .filter_map(|egui_key| map_egui_key(*egui_key))
                    .collect()
            });

        // Emit events for keys that changed state.
        let added: Vec<HidUsage> = current_keys
            .difference(&self.prev_keys_down)
            .cloned()
            .collect();
        let removed: Vec<HidUsage> = self
            .prev_keys_down
            .difference(&current_keys)
            .cloned()
            .collect();

        for key in added {
            let _ = self.writer.write(OutputEvent { down: true, key });
        }
        for key in removed {
            let _ = self.writer.write(OutputEvent { down: false, key });
        }

        // --- Modifier keys ---
        // Use the `Modifiers` struct which provides individual modifier state.
        //
        // Only `mac_cmd` may drive the super slot: on non-macOS platforms
        // the egui backend maps `command` to the ctrl state (and drops the
        // super key entirely), so including it here would log every ctrl
        // press a second time as `LeftCommand`.
        let current_mods = ctx.input(|i| {
            let mods = i.modifiers;
            ModifierState {
                left_control: mods.ctrl,
                left_shift: mods.shift,
                left_alt: mods.alt,
                left_super: mods.mac_cmd,
            }
        });

        // Emit modifier events for state changes.
        if current_mods.left_control != self.prev_modifiers.left_control {
            let _ = self.writer.write(OutputEvent {
                down: current_mods.left_control,
                key: HidUsage::LeftControl,
            });
        }
        if current_mods.left_shift != self.prev_modifiers.left_shift {
            let _ = self.writer.write(OutputEvent {
                down: current_mods.left_shift,
                key: HidUsage::LeftShift,
            });
        }
        if current_mods.left_alt != self.prev_modifiers.left_alt {
            let _ = self.writer.write(OutputEvent {
                down: current_mods.left_alt,
                key: HidUsage::LeftAlt,
            });
        }
        if current_mods.left_super != self.prev_modifiers.left_super {
            let _ = self.writer.write(OutputEvent {
                down: current_mods.left_super,
                key: HidUsage::LeftCommand,
            });
        }

        self.prev_keys_down = current_keys;
        self.prev_modifiers = current_mods;

        // Show a minimal UI so the window doesn't appear empty.
        ui.horizontal(|ui| {
            ui.strong("keymapper_monitor");
            ui.label("active");
        });
    }
}

/// Build the eframe native options for a minimal monitor window.
pub fn build_native_options() -> eframe::NativeOptions {
    let mut options = eframe::NativeOptions::default();
    options.viewport.inner_size = Some(egui::vec2(300.0, 50.0));
    options.viewport.min_inner_size = Some(egui::vec2(200.0, 40.0));
    options.viewport.title = Some("keymapper_monitor".to_string());
    options
}

/// Entry point for the monitor application.
///
/// On Linux, grabs the daemon's uinput output device directly (no window,
/// no window-manager focus dependency).  On other platforms, creates the
/// egui window and starts the per-frame event capture.  Exits cleanly on
/// SIGTERM/SIGINT.
pub fn run(output_path: PathBuf) {
    #[cfg(target_os = "linux")]
    {
        super::linux::run(&output_path);
    }

    #[cfg(not(target_os = "linux"))]
    {
        let shutdown = register_signal_handlers();

        let native_options = build_native_options();

        eframe::run_native(
            "keymapper_monitor",
            native_options,
            Box::new(move |_cc| {
                Ok(Box::new(MonitorApp::new(output_path, shutdown)))
            }),
        )
        .expect("eframe failed to run");
    }
}
