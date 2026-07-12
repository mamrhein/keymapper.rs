// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

/// Parses the common aliases that all platforms share.
///
/// Returns the canonical lower-case key name (e.g. `"leftcontrol"`),
/// which the platform-specific
/// [`from_canonical`](crate::os::Key::from_canonical) function then maps to an
/// enum variant.
///
/// Returns `None` when the string is not a recognised alias, signalling
/// the caller to try a canonical-name lookup instead.
pub fn resolve_alias(s: &str) -> Option<&'static str> {
    match s {
        // Modifiers
        "ctrl" | "leftctrl" | "leftcontrol" => Some("leftcontrol"),
        "rightctrl" | "rightcontrol" => Some("rightcontrol"),
        "shift" | "leftshift" => Some("leftshift"),
        "rightshift" => Some("rightshift"),
        "alt" | "leftalt" => Some("leftalt"),
        "rightalt" | "rightoption" => Some("rightalt"),
        "cmd" | "command" | "leftcmd" | "leftcommand" | "super"
        | "leftsuper" => Some("leftcommand"),
        "rightcmd" | "rightcommand" | "rightsuper" => Some("rightcommand"),
        "caps" | "capslock" => Some("capslock"),
        // Editor / misc
        "enter" => Some("return"),
        "esc" => Some("escape"),
        // Navigation
        "up" | "uparrow" | "up_arrow" => Some("uparrow"),
        "down" | "downarrow" | "down_arrow" => Some("downarrow"),
        "left" | "leftarrow" | "left_arrow" => Some("leftarrow"),
        "right" | "rightarrow" | "right_arrow" => Some("rightarrow"),
        "pageup" | "page_up" | "pgup" => Some("pageup"),
        "pagedown" | "page_down" | "pgdn" => Some("pagedown"),
        // Windows / Command disambiguation
        "leftwin" | "win" => Some("leftcommand"),
        _ => None,
    }
}

/// Returns a user-friendly error message for an unrecognised key name.
pub fn unknown_key_error(s: &str) -> String {
    format!(
        "unknown key name '{}'. Use names like capslock, leftcontrol, a, f1, \
         1, etc.",
        s
    )
}
