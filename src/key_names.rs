// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

/// Returns a user-friendly error message for an unrecognised key name.
pub fn unknown_key_error(s: &str) -> String {
    format!(
        "unknown key name '{}'. Use names like CapsLock, LeftCtrl, A, F1, 1, \
         etc.",
        s
    )
}
