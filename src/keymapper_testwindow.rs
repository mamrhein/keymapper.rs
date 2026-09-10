// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! The `keymapper_testwindow` binary.
//!
//! A thin entry point around [`keymapper::test_util::test_window::run`].  It
//! opens and focuses a window whose owning process resolves to a known app
//! name, so the e2e harness can make `get_active_app_name()` deterministic.

fn main() {
    keymapper::test_util::test_window::run();
}
