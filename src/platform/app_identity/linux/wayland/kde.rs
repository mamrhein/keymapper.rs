// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! KDE (KWin) — D-Bus synchronous active-window query.
//!
//! KWin exposes the active window through the `org.kde.KWin` D-Bus
//! interface, which [`kdotool`] wraps in a synchronous Rust API.

/// Query the active window on KDE via [`kdotool`].
///
/// Returns the window's `class_name`, or an empty string when KWin is not
/// running or `kdotool` cannot reach it.
pub(super) fn query_kde() -> String {
    use kdotool::get_active_window_info;

    get_active_window_info().map_or(String::new(), |info| info.class_name)
}
