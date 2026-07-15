// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use evdev::{Device, EventType, Key as EvdevKey};
use parking_lot::RwLock;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{mapping_cache::NativeKey, state::Lookup};

// ---------------------------------------------------------------------------
// Platform-specific Key enum — discriminants ARE the evdev KEY_* codes
// ---------------------------------------------------------------------------

/// Linux evdev keycode for a physical key on a US ANSI keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u16)]
pub enum Key {
    LeftControl = 29,   // KEY_LEFTCTRL
    RightControl = 97,  // KEY_RIGHTCTRL
    LeftShift = 42,     // KEY_LEFTSHIFT
    RightShift = 54,    // KEY_RIGHTSHIFT
    LeftAlt = 56,       // KEY_LEFTALT
    RightAlt = 100,     // KEY_RIGHTALT
    LeftCommand = 125,  // KEY_LEFTMETA
    RightCommand = 126, // KEY_RIGHTMETA
    CapsLock = 58,      // KEY_CAPSLOCK
    Tab = 15,           // KEY_TAB
    Space = 57,         // KEY_SPACE
    Return = 28,        // KEY_ENTER
    Backspace = 14,     // KEY_BACKSPACE
    Delete = 111,       // KEY_DELETE
    Escape = 1,         // KEY_ESC
    UpArrow = 103,      // KEY_UP
    DownArrow = 108,    // KEY_DOWN
    LeftArrow = 105,    // KEY_LEFT
    RightArrow = 106,   // KEY_RIGHT
    PageUp = 104,       // KEY_PAGEUP
    PageDown = 109,     // KEY_PAGEDOWN
    Home = 102,         // KEY_HOME
    End = 107,          // KEY_END
    F1 = 59,            // KEY_F1
    F2 = 60,            // KEY_F2
    F3 = 61,            // KEY_F3
    F4 = 62,            // KEY_F4
    F5 = 63,            // KEY_F5
    F6 = 64,            // KEY_F6
    F7 = 65,            // KEY_F7
    F8 = 66,            // KEY_F8
    F9 = 67,            // KEY_F9
    F10 = 68,           // KEY_F10
    F11 = 87,           // KEY_F11
    F12 = 88,           // KEY_F12
    A = 30,
    B = 48,
    C = 46,
    D = 32,
    E = 18,
    F = 33,
    G = 34,
    H = 35,
    I = 23,
    J = 36,
    K = 37,
    L = 38,
    M = 50,
    N = 49,
    O = 24,
    P = 25,
    Q = 16,
    R = 19,
    S = 31,
    T = 20,
    U = 22,
    V = 47,
    W = 17,
    X = 45,
    Y = 21,
    Z = 44,
    Number1 = 2,
    Number2 = 3,
    Number3 = 4,
    Number4 = 5,
    Number5 = 6,
    Number6 = 7,
    Number7 = 8,
    Number8 = 9,
    Number9 = 10,
    Number0 = 11,
}

impl Key {
    pub const fn as_native(self) -> u16 {
        self as u16
    }

    pub const fn as_modifier_bit(self) -> Option<u8> {
        match self {
            Self::LeftControl => Some(0),
            Self::RightControl => Some(1),
            Self::LeftShift => Some(2),
            Self::RightShift => Some(3),
            Self::LeftAlt => Some(4),
            Self::RightAlt => Some(5),
            Self::LeftCommand => Some(6),
            Self::RightCommand => Some(7),
            _ => None,
        }
    }

    pub fn as_modifier_positions(self) -> Option<Vec<u8>> {
        match self {
            Self::LeftControl | Self::RightControl => Some(vec![0, 1]),
            Self::LeftShift | Self::RightShift => Some(vec![2, 3]),
            Self::LeftAlt | Self::RightAlt => Some(vec![4, 5]),
            Self::LeftCommand | Self::RightCommand => Some(vec![6, 7]),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::LeftControl => "LeftControl",
            Self::RightControl => "RightControl",
            Self::LeftShift => "LeftShift",
            Self::RightShift => "RightShift",
            Self::LeftAlt => "LeftAlt",
            Self::RightAlt => "RightAlt",
            Self::LeftCommand => "LeftCommand",
            Self::RightCommand => "RightCommand",
            Self::CapsLock => "CapsLock",
            Self::Tab => "Tab",
            Self::Space => "Space",
            Self::Return => "Return",
            Self::Backspace => "Backspace",
            Self::Delete => "Delete",
            Self::Escape => "Escape",
            Self::UpArrow => "UpArrow",
            Self::DownArrow => "DownArrow",
            Self::LeftArrow => "LeftArrow",
            Self::RightArrow => "RightArrow",
            Self::PageUp => "PageUp",
            Self::PageDown => "PageDown",
            Self::Home => "Home",
            Self::End => "End",
            Self::F1 => "F1",
            Self::F2 => "F2",
            Self::F3 => "F3",
            Self::F4 => "F4",
            Self::F5 => "F5",
            Self::F6 => "F6",
            Self::F7 => "F7",
            Self::F8 => "F8",
            Self::F9 => "F9",
            Self::F10 => "F10",
            Self::F11 => "F11",
            Self::F12 => "F12",
            Self::A => "A",
            Self::B => "B",
            Self::C => "C",
            Self::D => "D",
            Self::E => "E",
            Self::F => "F",
            Self::G => "G",
            Self::H => "H",
            Self::I => "I",
            Self::J => "J",
            Self::K => "K",
            Self::L => "L",
            Self::M => "M",
            Self::N => "N",
            Self::O => "O",
            Self::P => "P",
            Self::Q => "Q",
            Self::R => "R",
            Self::S => "S",
            Self::T => "T",
            Self::U => "U",
            Self::V => "V",
            Self::W => "W",
            Self::X => "X",
            Self::Y => "Y",
            Self::Z => "Z",
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

    pub fn try_from_str(name: &str) -> Option<Self> {
        match name {
            "Ctrl" => Some(Self::LeftControl),
            "Shift" => Some(Self::LeftShift),
            "Alt" | "Option" => Some(Self::LeftAlt),
            "Command" | "Cmd" | "Super" => Some(Self::LeftCommand),
            "LeftControl" | "LeftCtrl" => Some(Self::LeftControl),
            "RightControl" | "RightCtrl" => Some(Self::RightControl),
            "LeftShift" => Some(Self::LeftShift),
            "RightShift" => Some(Self::RightShift),
            "LeftAlt" | "LeftOption" => Some(Self::LeftAlt),
            "RightAlt" | "RightOption" => Some(Self::RightAlt),
            "LeftCommand" | "LeftCmd" => Some(Self::LeftCommand),
            "RightCommand" | "RightCmd" => Some(Self::RightCommand),
            "CapsLock" | "Caps" => Some(Self::CapsLock),
            "Tab" => Some(Self::Tab),
            "Space" => Some(Self::Space),
            "Return" | "Enter" => Some(Self::Return),
            "Backspace" => Some(Self::Backspace),
            "Delete" => Some(Self::Delete),
            "Escape" | "Esc" => Some(Self::Escape),
            "UpArrow" | "Up" => Some(Self::UpArrow),
            "DownArrow" | "Down" => Some(Self::DownArrow),
            "LeftArrow" | "Left" => Some(Self::LeftArrow),
            "RightArrow" | "Right" => Some(Self::RightArrow),
            "PageUp" | "PgUp" => Some(Self::PageUp),
            "PageDown" | "PgDn" => Some(Self::PageDown),
            "Home" => Some(Self::Home),
            "End" => Some(Self::End),
            "F1" => Some(Self::F1),
            "F2" => Some(Self::F2),
            "F3" => Some(Self::F3),
            "F4" => Some(Self::F4),
            "F5" => Some(Self::F5),
            "F6" => Some(Self::F6),
            "F7" => Some(Self::F7),
            "F8" => Some(Self::F8),
            "F9" => Some(Self::F9),
            "F10" => Some(Self::F10),
            "F11" => Some(Self::F11),
            "F12" => Some(Self::F12),
            "A" => Some(Self::A),
            "B" => Some(Self::B),
            "C" => Some(Self::C),
            "D" => Some(Self::D),
            "E" => Some(Self::E),
            "F" => Some(Self::F),
            "G" => Some(Self::G),
            "H" => Some(Self::H),
            "I" => Some(Self::I),
            "J" => Some(Self::J),
            "K" => Some(Self::K),
            "L" => Some(Self::L),
            "M" => Some(Self::M),
            "N" => Some(Self::N),
            "O" => Some(Self::O),
            "P" => Some(Self::P),
            "Q" => Some(Self::Q),
            "R" => Some(Self::R),
            "S" => Some(Self::S),
            "T" => Some(Self::T),
            "U" => Some(Self::U),
            "V" => Some(Self::V),
            "W" => Some(Self::W),
            "X" => Some(Self::X),
            "Y" => Some(Self::Y),
            "Z" => Some(Self::Z),
            "1" | "Number1" => Some(Self::Number1),
            "2" | "Number2" => Some(Self::Number2),
            "3" | "Number3" => Some(Self::Number3),
            "4" | "Number4" => Some(Self::Number4),
            "5" | "Number5" => Some(Self::Number5),
            "6" | "Number6" => Some(Self::Number6),
            "7" | "Number7" => Some(Self::Number7),
            "8" | "Number8" => Some(Self::Number8),
            "9" | "Number9" => Some(Self::Number9),
            "0" | "Number0" => Some(Self::Number0),
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
        Self::from_str(&s).ok_or_else(|| {
            serde::de::Error::custom(crate::key_names::unknown_key_error(&s))
        })
    }
}

// ---------------------------------------------------------------------------
// Modifier handling
// ---------------------------------------------------------------------------

fn keycode_to_modifier_bit(code: u16) -> Option<u8> {
    match code {
        29 => Some(0),  // KEY_LEFTCTRL
        97 => Some(1),  // KEY_RIGHTCTRL
        42 => Some(2),  // KEY_LEFTSHIFT
        54 => Some(3),  // KEY_RIGHTSHIFT
        56 => Some(4),  // KEY_LEFTALT
        100 => Some(5), // KEY_RIGHTALT
        125 => Some(6), // KEY_LEFTMETA
        126 => Some(7), // KEY_RIGHTMETA
        _ => None,
    }
}

fn modifier_bit_to_code(bit: u8) -> Option<u16> {
    match bit {
        0 => Some(29),  // KEY_LEFTCTRL
        1 => Some(97),  // KEY_RIGHTCTRL
        2 => Some(42),  // KEY_LEFTSHIFT
        3 => Some(54),  // KEY_RIGHTSHIFT
        4 => Some(56),  // KEY_LEFTALT
        5 => Some(100), // KEY_RIGHTALT
        6 => Some(125), // KEY_LEFTMETA
        7 => Some(126), // KEY_RIGHTMETA
        _ => None,
    }
}

fn emit_key_event(
    device: &mut uinput::VirtualDevice,
    native_key: &NativeKey,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut pressed_modifiers: Vec<u16> = Vec::new();

    for bit in 0..8 {
        if (native_key.modifiers >> bit) & 1 == 1 {
            if let Some(code) = modifier_bit_to_code(bit) {
                device.press(&EvdevKey::new(code))?;
                pressed_modifiers.push(code);
                device.synchronize()?;
                thread::sleep(Duration::from_millis(1));
            }
        }
    }

    device.press(&EvdevKey::new(native_key.base))?;
    device.synchronize()?;
    thread::sleep(Duration::from_millis(1));

    device.release(&EvdevKey::new(native_key.base))?;
    device.synchronize()?;
    thread::sleep(Duration::from_millis(1));

    for code in pressed_modifiers.into_iter().rev() {
        device.release(&EvdevKey::new(code))?;
        device.synchronize()?;
        thread::sleep(Duration::from_millis(1));
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// evdev event loop
// ---------------------------------------------------------------------------

extern "C" fn signal_handler(_sig: libc::c_int) {
    SHUTDOWN_REQUESTED.store(true, Ordering::Release);
}

static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);

fn find_keyboard_device() -> Result<Device, Box<dyn std::error::Error>> {
    use std::{fs, path::Path};

    let input_path = Path::new("/dev/input");
    if !input_path.exists() {
        return Err("No /dev/input directory found.".into());
    }

    let mut devices: Vec<Device> = Vec::new();
    for entry in fs::read_dir(input_path)? {
        let entry = entry?;
        let path = entry.path();
        if !path.to_string_lossy().starts_with("/dev/input/event") {
            continue;
        }
        if let Ok(device) = Device::open(&path) {
            devices.push(device);
        }
    }

    if devices.is_empty() {
        return Err("No keyboard device found. Try: sudo usermod -aG input \
                    $USER"
            .into());
    }

    for device in devices {
        if device.supported_events().contains(EventType::KEY) {
            return Ok(device);
        }
    }

    Err("No keyboard device found that supports EV_KEY".into())
}

pub(crate) fn start_mapping(
    lookup: Arc<RwLock<dyn Lookup>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut raw_device = find_keyboard_device()?;
    raw_device.grab()?;

    let mut virtual_device = uinput::default()?
        .name("CrossPlatform_Virtual_Keyboard")?
        .event(uinput::event::Keyboard::All)?
        .create()?;

    thread::sleep(Duration::from_millis(200));
    println!("Linux uinput virtual keyboard ready.");

    let handler_ptr = signal_handler as *const () as usize;
    unsafe {
        libc::signal(libc::SIGINT, handler_ptr);
        libc::signal(libc::SIGTERM, handler_ptr);
    }

    let mut active_modifiers: u8 = 0;

    while !SHUTDOWN_REQUESTED.load(Ordering::Acquire) {
        match raw_device.fetch_events() {
            Ok(events) => {
                for event in events {
                    if event.event_type() == EventType::KEY {
                        let code = event.code();
                        let value = event.value();

                        if let Some(bit) = keycode_to_modifier_bit(code) {
                            if value == 1 {
                                active_modifiers |= 1 << bit;
                            } else if value == 0 {
                                active_modifiers &= !(1 << bit);
                            }
                        }

                        let guard = lookup.read();
                        let current_app = guard.active_app().to_string();
                        let active_outputs = guard
                            .for_app(&current_app, code, active_modifiers)
                            .or_else(|| guard.global(code, active_modifiers))
                            .map(|v| v.to_vec());
                        drop(guard);

                        if let Some(outputs) = active_outputs {
                            if value == 1 {
                                for native_key in &outputs {
                                    if let Err(e) = emit_key_event(
                                        &mut virtual_device,
                                        native_key,
                                    ) {
                                        eprintln!("emit error: {}", e);
                                    }
                                }
                            }
                            continue;
                        }

                        let key = EvdevKey::new(code);
                        if value == 1 {
                            virtual_device.press(&key)?;
                        } else if value == 0 {
                            virtual_device.release(&key)?;
                        } else {
                            virtual_device.press(&key)?;
                            virtual_device.release(&key)?;
                        }
                        virtual_device.synchronize()?;
                    }
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(e) => {
                eprintln!("Linux: error reading events: {}", e);
                thread::sleep(Duration::from_millis(100));
            }
        }
    }

    println!("Shutdown signal received. Cleaning up...");
    Ok(())
}
