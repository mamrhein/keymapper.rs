// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! macOS reader setup: the harness runs this binary inside Terminal.app (via
//! `osascript`) and activates it, so the reader's inherited stdin is the
//! terminal's pty and Terminal.app is the focused application.  The daemon's
//! Karabiner virtual keyboard — and forwarded passthroughs — deliver their
//! keys to the focused app, i.e. here.  Setup only needs to put that pty in
//! raw mode so each keystroke is available immediately.

/// Put the inherited stdin (the terminal pty) in raw mode.
pub(super) fn setup() {
    super::make_raw(0);
}
