// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

use std::{
    ffi::c_void,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use objc2_core_foundation::{
    CFMachPort, CFRetained, CFRunLoop, CFRunLoopSource, kCFRunLoopCommonModes,
};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventFlags, CGEventSource, CGEventSourceStateID,
    CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement, CGEventType,
    CGKeyCode,
};
use parking_lot::RwLock;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{key_names, mapping_cache::NativeAction, state::Lookup};

// ---------------------------------------------------------------------------
// Platform-specific Key enum — discriminants ARE the CGKeyCode values
// ---------------------------------------------------------------------------

/// macOS virtual keycode for a physical key on a US ANSI keyboard.
///
/// Discriminant values come from `<HIToolbox/Events.h>` (`kVK_*` constants).
/// `key as u16` yields the native CGKeyCode — no translation needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum Key {
    // --- Modifiers ---
    LeftControl = 59,  // kVK_Control
    RightControl = 62, // kVK_RightControl
    LeftShift = 56,    // kVK_Shift
    RightShift = 60,   // kVK_RightShift
    LeftAlt = 58,      // kVK_Option
    RightAlt = 61,     // kVK_RightOption
    LeftCommand = 55,  // kVK_Command
    RightCommand = 54, // kVK_RightCommand
    CapsLock = 57,     // kVK_CapsLock
    // --- Editor / misc ---
    Tab = 48,       // kVK_Tab
    Space = 49,     // kVK_Space
    Return = 36,    // kVK_Return
    Backspace = 51, // kVK_Delete
    Delete = 117,   // kVK_ForwardDelete
    Escape = 53,    // kVK_Escape
    // --- Navigation ---
    UpArrow = 126,    // kVK_UpArrow
    DownArrow = 125,  // kVK_DownArrow
    LeftArrow = 123,  // kVK_LeftArrow
    RightArrow = 124, // kVK_RightArrow
    PageUp = 116,     // kVK_PageUp
    PageDown = 121,   // kVK_PageDown
    Home = 115,       // kVK_Home
    End = 119,        // kVK_End
    // --- Function keys ---
    F1 = 122,  // kVK_F1
    F2 = 120,  // kVK_F2
    F3 = 99,   // kVK_F3
    F4 = 118,  // kVK_F4
    F5 = 96,   // kVK_F5
    F6 = 97,   // kVK_F6
    F7 = 98,   // kVK_F7
    F8 = 100,  // kVK_F8
    F9 = 101,  // kVK_F9
    F10 = 109, // kVK_F10
    F11 = 103, // kVK_F11
    F12 = 111, // kVK_F12
    // --- Letters (US QWERTY) ---
    A = 0,  // kVK_ANSI_A
    B = 11, // kVK_ANSI_B
    C = 8,  // kVK_ANSI_C
    D = 2,  // kVK_ANSI_D
    E = 14, // kVK_ANSI_E
    F = 3,  // kVK_ANSI_F
    G = 5,  // kVK_ANSI_G
    H = 4,  // kVK_ANSI_H
    I = 34, // kVK_ANSI_I
    J = 38, // kVK_ANSI_J
    K = 40, // kVK_ANSI_K
    L = 37, // kVK_ANSI_L
    M = 46, // kVK_ANSI_M
    N = 45, // kVK_ANSI_N
    O = 31, // kVK_ANSI_O
    P = 35, // kVK_ANSI_P
    Q = 12, // kVK_ANSI_Q
    R = 15, // kVK_ANSI_R
    S = 1,  // kVK_ANSI_S
    T = 17, // kVK_ANSI_T
    U = 32, // kVK_ANSI_U
    V = 9,  // kVK_ANSI_V
    W = 13, // kVK_ANSI_W
    X = 7,  // kVK_ANSI_X
    Y = 16, // kVK_ANSI_Y
    Z = 6,  // kVK_ANSI_Z
    // --- Numbers ---
    Number1 = 18, // kVK_ANSI_1
    Number2 = 19, // kVK_ANSI_2
    Number3 = 20, // kVK_ANSI_3
    Number4 = 21, // kVK_ANSI_4
    Number5 = 23, // kVK_ANSI_5
    Number6 = 22, // kVK_ANSI_6
    Number7 = 26, // kVK_ANSI_7
    Number8 = 28, // kVK_ANSI_8
    Number9 = 25, // kVK_ANSI_9
    Number0 = 29, // kVK_ANSI_0
}

impl Key {
    /// Convert to the native CGKeyCode.  Zero-cost — the discriminant IS the
    /// code.
    pub const fn as_native(self) -> u16 {
        self as u16
    }

    /// Return the modifier-bitmask contribution of this key.
    ///
    /// Modifier keys return a bitmask with the corresponding bit set.
    /// Non-modifier keys return `0`.  When a rule specifies a group
    /// modifier (e.g. "ctrl" meaning either side), the bit is set for
    /// whichever physical key matches — handled by the alias layer so
    /// that "leftctrl" and "rightctrl" each resolve to their own bit.
    pub const fn as_modifier_bits(self) -> u8 {
        match self {
            // bit 0: left control
            Self::LeftControl => 1 << 0,
            // bit 1: right control
            Self::RightControl => 1 << 1,
            // bit 2: left shift
            Self::LeftShift => 1 << 2,
            // bit 3: right shift
            Self::RightShift => 1 << 3,
            // bit 4: left alt (option)
            Self::LeftAlt => 1 << 4,
            // bit 5: right alt (option)
            Self::RightAlt => 1 << 5,
            // bit 6: left command
            Self::LeftCommand => 1 << 6,
            // bit 7: right command
            Self::RightCommand => 1 << 7,
            _ => 0, // non-modifier keys contribute nothing
        }
    }

    /// Return the canonical config-name for this key.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LeftControl => "leftcontrol",
            Self::RightControl => "rightcontrol",
            Self::LeftShift => "leftshift",
            Self::RightShift => "rightshift",
            Self::LeftAlt => "leftalt",
            Self::RightAlt => "rightalt",
            Self::LeftCommand => "leftcommand",
            Self::RightCommand => "rightcommand",
            Self::CapsLock => "capslock",
            Self::Tab => "tab",
            Self::Space => "space",
            Self::Return => "return",
            Self::Backspace => "backspace",
            Self::Delete => "delete",
            Self::Escape => "escape",
            Self::UpArrow => "uparrow",
            Self::DownArrow => "downarrow",
            Self::LeftArrow => "leftarrow",
            Self::RightArrow => "rightarrow",
            Self::PageUp => "pageup",
            Self::PageDown => "pagedown",
            Self::Home => "home",
            Self::End => "end",
            Self::F1 => "f1",
            Self::F2 => "f2",
            Self::F3 => "f3",
            Self::F4 => "f4",
            Self::F5 => "f5",
            Self::F6 => "f6",
            Self::F7 => "f7",
            Self::F8 => "f8",
            Self::F9 => "f9",
            Self::F10 => "f10",
            Self::F11 => "f11",
            Self::F12 => "f12",
            Self::A => "a",
            Self::B => "b",
            Self::C => "c",
            Self::D => "d",
            Self::E => "e",
            Self::F => "f",
            Self::G => "g",
            Self::H => "h",
            Self::I => "i",
            Self::J => "j",
            Self::K => "k",
            Self::L => "l",
            Self::M => "m",
            Self::N => "n",
            Self::O => "o",
            Self::P => "p",
            Self::Q => "q",
            Self::R => "r",
            Self::S => "s",
            Self::T => "t",
            Self::U => "u",
            Self::V => "v",
            Self::W => "w",
            Self::X => "x",
            Self::Y => "y",
            Self::Z => "z",
            Self::Number1 => "1",
            Self::Number2 => "2",
            Self::Number3 => "3",
            Self::Number4 => "4",
            Self::Number5 => "5",
            Self::Number6 => "6",
            Self::Number7 => "7",
            Self::Number8 => "8",
            Self::Number9 => "9",
            Self::Number0 => "0",
        }
    }

    /// Parse a canonical name into a Key variant.
    ///
    /// Returns `None` for unrecognised names (caller should error).
    pub fn from_canonical(name: &str) -> Option<Self> {
        match name {
            "leftcontrol" => Some(Self::LeftControl),
            "rightcontrol" => Some(Self::RightControl),
            "leftshift" => Some(Self::LeftShift),
            "rightshift" => Some(Self::RightShift),
            "leftalt" => Some(Self::LeftAlt),
            "rightalt" => Some(Self::RightAlt),
            "leftcommand" => Some(Self::LeftCommand),
            "rightcommand" => Some(Self::RightCommand),
            "capslock" => Some(Self::CapsLock),
            "tab" => Some(Self::Tab),
            "space" => Some(Self::Space),
            "return" => Some(Self::Return),
            "backspace" => Some(Self::Backspace),
            "delete" => Some(Self::Delete),
            "escape" => Some(Self::Escape),
            "uparrow" => Some(Self::UpArrow),
            "downarrow" => Some(Self::DownArrow),
            "leftarrow" => Some(Self::LeftArrow),
            "rightarrow" => Some(Self::RightArrow),
            "pageup" => Some(Self::PageUp),
            "pagedown" => Some(Self::PageDown),
            "home" => Some(Self::Home),
            "end" => Some(Self::End),
            "f1" => Some(Self::F1),
            "f2" => Some(Self::F2),
            "f3" => Some(Self::F3),
            "f4" => Some(Self::F4),
            "f5" => Some(Self::F5),
            "f6" => Some(Self::F6),
            "f7" => Some(Self::F7),
            "f8" => Some(Self::F8),
            "f9" => Some(Self::F9),
            "f10" => Some(Self::F10),
            "f11" => Some(Self::F11),
            "f12" => Some(Self::F12),
            "a" => Some(Self::A),
            "b" => Some(Self::B),
            "c" => Some(Self::C),
            "d" => Some(Self::D),
            "e" => Some(Self::E),
            "f" => Some(Self::F),
            "g" => Some(Self::G),
            "h" => Some(Self::H),
            "i" => Some(Self::I),
            "j" => Some(Self::J),
            "k" => Some(Self::K),
            "l" => Some(Self::L),
            "m" => Some(Self::M),
            "n" => Some(Self::N),
            "o" => Some(Self::O),
            "p" => Some(Self::P),
            "q" => Some(Self::Q),
            "r" => Some(Self::R),
            "s" => Some(Self::S),
            "t" => Some(Self::T),
            "u" => Some(Self::U),
            "v" => Some(Self::V),
            "w" => Some(Self::W),
            "x" => Some(Self::X),
            "y" => Some(Self::Y),
            "z" => Some(Self::Z),
            "1" | "number1" => Some(Self::Number1),
            "2" | "number2" => Some(Self::Number2),
            "3" | "number3" => Some(Self::Number3),
            "4" | "number4" => Some(Self::Number4),
            "5" | "number5" => Some(Self::Number5),
            "6" | "number6" => Some(Self::Number6),
            "7" | "number7" => Some(Self::Number7),
            "8" | "number8" => Some(Self::Number8),
            "9" | "number9" => Some(Self::Number9),
            "0" | "number0" => Some(Self::Number0),
            _ => None,
        }
    }
}

impl Serialize for Key {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Key {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        let l = s.to_lowercase();
        // Try alias first, then canonical name.
        let canonical = key_names::resolve_alias(&l).unwrap_or(&l);
        Self::from_canonical(canonical).ok_or_else(|| {
            serde::de::Error::custom(key_names::unknown_key_error(&s))
        })
    }
}

// ---------------------------------------------------------------------------
// Modifier extraction
// ---------------------------------------------------------------------------

/// Map CGEventFlags to our 8-bit modifier layout.
fn extract_modifier_bits(flags: CGEventFlags) -> u8 {
    let mut bits: u8 = 0;
    if flags.contains(CGEventFlags::MaskControl) {
        bits |= (1 << 0) | (1 << 1); // control group
    }
    if flags.contains(CGEventFlags::MaskShift) {
        bits |= (1 << 2) | (1 << 3); // shift group
    }
    if flags.contains(CGEventFlags::MaskAlternate) {
        bits |= (1 << 4) | (1 << 5); // alt/option group
    }
    if flags.contains(CGEventFlags::MaskCommand) {
        bits |= (1 << 6) | (1 << 7); // command group
    }
    bits
}

// ---------------------------------------------------------------------------
// Event tap implementation
// ---------------------------------------------------------------------------

/// Shared mutable state bridged into the C callback via `user_info`.
struct TapContext {
    /// Trait-object lookup: decouples this module from RuntimeState's shape.
    lookup: Arc<RwLock<dyn Lookup>>,
    /// Pre-created event source reused for every synthetic keyboard event.
    /// Avoids a per-keystroke allocation inside the hot callback path.
    source: CFRetained<CGEventSource>,
}

static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Holds the tap, run-loop-source, and callback context so they stay alive
/// for the lifetime of the event-loop, and are cleanly reclaimed on drop.
struct EventTapHandle {
    tap: CFRetained<CFMachPort>,
    #[allow(dead_code)]
    run_loop_source: CFRetained<CFRunLoopSource>,
    /// Raw pointer to the heap-allocated `TapContext` passed as `user_info`.
    /// Reclaimed in `Drop` to avoid a memory leak.
    context_ptr: *mut TapContext,
}

impl Drop for EventTapHandle {
    fn drop(&mut self) {
        // Disable the tap so no further callbacks fire.
        CGEvent::tap_enable(&self.tap, false);

        // Reclaim and free the leaked `Box<TapContext>`. This also drops
        // the `CFRetained<CGEventSource>`, releasing the CoreFoundation
        // object.
        unsafe {
            drop(Box::from_raw(self.context_ptr));
        }
    }
}

/// Async-signal-safe handler that flips the shutdown flag.
extern "C" fn signal_handler(_sig: libc::c_int) {
    SHUTDOWN_REQUESTED.store(true, Ordering::Release);
}

pub(crate) fn start_mapping(
    lookup: Arc<RwLock<dyn Lookup>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mask: u64 =
        (1u64 << CGEventType::KeyDown.0) | (1u64 << CGEventType::KeyUp.0);

    let source =
        CGEventSource::new(CGEventSourceStateID::CombinedSessionState)
            .ok_or("Failed to create CGEventSource")?;

    // Allocate the context on the heap and leak the `Box` to get a stable
    // pointer for the FFI callback.  The `EventTapHandle` owns this pointer
    // and reclaims it in `Drop`.
    let context_ptr =
        Box::into_raw(Box::new(TapContext { lookup, source })) as *mut _;

    let tap = unsafe {
        CGEvent::tap_create(
            CGEventTapLocation::HIDEventTap,
            CGEventTapPlacement::HeadInsertEventTap,
            CGEventTapOptions::Default,
            mask,
            Some(macos_keyboard_callback_ffi),
            context_ptr as *mut c_void,
        )
    };

    let Some(tap) = tap else {
        // Tap creation failed; reclaim the context to avoid the leak.
        unsafe {
            drop(Box::from_raw(context_ptr));
        }
        return Err("Failed to create macOS CGEventTap. Verify \
                    Accessibility privileges!"
            .into());
    };

    let Some(run_loop_source) =
        CFMachPort::new_run_loop_source(None, Some(&tap), 0)
    else {
        unsafe {
            drop(Box::from_raw(context_ptr));
        }
        return Err("Failed to create CFRunLoopSource from Mach Port.".into());
    };

    let run_loop = CFRunLoop::current().ok_or("No current run loop")?;
    run_loop
        .add_source(Some(&run_loop_source), unsafe { kCFRunLoopCommonModes });

    CGEvent::tap_enable(&tap, true);
    println!("Modern compile-safe macOS Event Tap actively running...");

    // Register signal handlers for graceful shutdown.
    let handler_ptr = signal_handler as *const () as usize;
    unsafe {
        libc::signal(libc::SIGINT, handler_ptr);
        libc::signal(libc::SIGTERM, handler_ptr);
    }

    // Own the tap, run-loop-source, and context pointer. Dropped together
    // when the run-loop exits.
    let handle = EventTapHandle {
        tap,
        run_loop_source,
        context_ptr,
    };

    // Poll the run-loop with a short timeout so we can check the shutdown
    // flag each iteration. This avoids an infinite `CFRunLoop::run()` block
    // and lets us exit cleanly on SIGINT / SIGTERM.
    while !SHUTDOWN_REQUESTED.load(Ordering::Acquire) {
        CFRunLoop::run_in_mode(
            unsafe { kCFRunLoopCommonModes },
            0.5, // 500 ms timeout
            true,
        );
    }

    println!("Shutdown signal received. Cleaning up...");

    // `handle` is dropped here, which:
    // 1. Disables the tap
    // 2. Reclaims and frees the `TapContext` (and its `CGEventSource`)
    drop(handle);

    Ok(())
}

/// FFI callback invoked by the event tap for every matching keyboard event.
///
/// # Safety
/// Called from CoreGraphics on the run-loop thread.  `proxy` and `user_info`
/// are managed by the system / our `TapContext`.
unsafe extern "C-unwind" fn macos_keyboard_callback_ffi(
    _proxy: objc2_core_graphics::CGEventTapProxy,
    _type: CGEventType,
    event: core::ptr::NonNull<objc2_core_graphics::CGEvent>,
    user_info: *mut std::ffi::c_void,
) -> *mut objc2_core_graphics::CGEvent {
    if user_info.is_null() {
        return event.as_ptr();
    }

    let context = unsafe { &*(user_info as *const TapContext) };

    // CGKeyCode is u16 — matches the native key code directly.
    let native_key: CGKeyCode = unsafe {
        CGEvent::integer_value_field(
            Some(event.as_ref()),
            CGEventField::KeyboardEventKeycode,
        )
    } as CGKeyCode;

    let is_down = _type == CGEventType::KeyDown;

    // Extract currently pressed modifier groups.
    let pressed_modifiers = extract_modifier_bits(unsafe {
        CGEvent::flags(Some(event.as_ref()))
    });

    // Resolve the remapping through the trait interface.  Clone the action
    // out so we can drop the read lock before expensive CGEvent operations.
    let guard = context.lookup.read();
    let current_app = guard.active_app().to_lowercase();
    let active_action = guard
        .for_app(&current_app, native_key, pressed_modifiers)
        .or_else(|| guard.global(native_key, pressed_modifiers))
        .cloned();
    drop(guard);

    if let Some(action) = active_action {
        match action {
            NativeAction::RemapTo(target_code) => {
                // Modify the existing event's keycode in place.
                unsafe {
                    CGEvent::set_integer_value_field(
                        Some(event.as_ref()),
                        CGEventField::KeyboardEventKeycode,
                        target_code as i64,
                    );
                }
                return event.as_ptr();
            }
            NativeAction::Shortcut(target_codes) => {
                if is_down {
                    for code in &target_codes {
                        if let Some(e) = CGEvent::new_keyboard_event(
                            Some(&context.source),
                            *code as CGKeyCode,
                            true,
                        ) {
                            CGEvent::post(
                                CGEventTapLocation::HIDEventTap,
                                Some(&e),
                            );
                        }
                    }
                } else {
                    for code in target_codes.iter().rev() {
                        if let Some(e) = CGEvent::new_keyboard_event(
                            Some(&context.source),
                            *code as CGKeyCode,
                            false,
                        ) {
                            CGEvent::post(
                                CGEventTapLocation::HIDEventTap,
                                Some(&e),
                            );
                        }
                    }
                }
                // Suppress the original event for shortcuts.
                return std::ptr::null_mut();
            }
        }
    }

    event.as_ptr()
}
