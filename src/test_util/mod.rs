// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Test and monitoring helpers for the e2e test harness: the key event
//! injector, the `keymapper_monitor` capture backend, and the
//! `keymapper_testwindow` deterministic-active-app helper.

pub mod key_injector;
pub mod monitor;
pub mod reader;
pub mod test_window;
