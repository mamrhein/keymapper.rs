// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

use std::{collections::HashMap, fs, path::Path};

use crate::config::{AppConfig, ChordTrigger, Key, KeyAction};

/// Modifier bitmask layout (u8):
///
/// bit 0: left control      bit 1: right control
/// bit 2: left shift        bit 3: right shift
/// bit 4: left alt          bit 5: right alt
/// bit 6: left command/win  bit 7: right command/win
///
/// When a rule specifies "ctrl" (unspecified side), both bits 0 and 1 are
/// set so the rule matches either physical control key.

/// A compiled chord: specific modifier requirement + base key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NativeChord {
    /// Bitmask of required modifier groups.  A rule matches when
    /// `(pressed_modifiers & self.modifiers) == self.modifiers`.
    pub modifiers: u8,
    /// The non-modifier base key's native code.
    pub base: u16,
}

/// Platform-native structural action layout.
#[derive(Debug, Clone)]
pub enum NativeAction {
    RemapTo(u16),
    Shortcut(Vec<u16>),
}

/// Compiled key-mapping cache optimised for fast runtime lookups.
///
/// Single-key rules are stored in HashMaps for O(1) lookup.  Chord rules
/// are stored as small Vecs and scanned linearly — the rule count is small
/// enough that O(n) is negligible compared to FFI overhead.
pub struct RuntimeLookupCache {
    // Single-key rules (bare key, no modifiers required).
    process_map: HashMap<String, HashMap<u16, NativeAction>>,
    global_map: HashMap<u16, NativeAction>,
    // Chord rules (modifiers + base key).
    process_chords: HashMap<String, Vec<(NativeChord, NativeAction)>>,
    global_chords: Vec<(NativeChord, NativeAction)>,
}

impl RuntimeLookupCache {
    pub(crate) fn process_map(
        &self,
    ) -> &HashMap<String, HashMap<u16, NativeAction>> {
        &self.process_map
    }

    pub(crate) fn global_map(&self) -> &HashMap<u16, NativeAction> {
        &self.global_map
    }

    pub(crate) fn process_chords(
        &self,
    ) -> &HashMap<String, Vec<(NativeChord, NativeAction)>> {
        &self.process_chords
    }

    pub(crate) fn global_chords(&self) -> &Vec<(NativeChord, NativeAction)> {
        &self.global_chords
    }
}

impl RuntimeLookupCache {
    /// Load a TOML config file, parse it, and compile the lookup cache
    /// in one step.  Used by both initialisation and hot-reload.
    pub fn compile_from_path<P: AsRef<Path>>(
        path: P,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let content = fs::read_to_string(path)?;
        let parsed = AppConfig::load_from_str(&content)?;
        Ok(Self::compile_from_config(&parsed))
    }

    pub fn compile_from_config(app_config: &AppConfig) -> Self {
        let mut process_map: HashMap<String, HashMap<u16, NativeAction>> =
            HashMap::new();
        let mut global_map: HashMap<u16, NativeAction> = HashMap::new();
        let mut process_chords: HashMap<
            String,
            Vec<(NativeChord, NativeAction)>,
        > = HashMap::new();
        let mut global_chords: Vec<(NativeChord, NativeAction)> = Vec::new();

        for rule in &app_config.rules {
            let native_action = match &rule.action {
                KeyAction::RemapTo(target_key) => {
                    NativeAction::RemapTo(target_key.as_native())
                }
                KeyAction::Shortcut(target_keys) => {
                    let compiled: Vec<u16> =
                        target_keys.iter().map(|k| k.as_native()).collect();
                    NativeAction::Shortcut(compiled)
                }
            };

            match &rule.trigger {
                ChordTrigger::Key(key) => {
                    // Single-key rule — goes into the HashMap.
                    let native = key.as_native();
                    if rule.applications.is_empty() {
                        global_map.insert(native, native_action);
                    } else {
                        for app in &rule.applications {
                            process_map
                                .entry(app.to_lowercase())
                                .or_default()
                                .insert(native, native_action.clone());
                        }
                    }
                }
                ChordTrigger::Chord { base, modifiers } => {
                    // Chord rule — goes into the linear scan list.
                    let chord_mods =
                        compile_modifier_bits(modifiers.iter().copied());
                    let chord = NativeChord {
                        modifiers: chord_mods,
                        base: base.as_native(),
                    };
                    let entry = (chord, native_action);

                    if rule.applications.is_empty() {
                        global_chords.push(entry);
                    } else {
                        for app in &rule.applications {
                            process_chords
                                .entry(app.to_lowercase())
                                .or_default()
                                .push(entry.clone());
                        }
                    }
                }
            }
        }

        RuntimeLookupCache {
            process_map,
            global_map,
            process_chords,
            global_chords,
        }
    }
}

/// Compile a list of modifier keys into a combined group bitmask.
///
/// Each key contributes its _group_ bits so that specifying "ctrl" (which
/// resolves to LeftControl via the alias layer) still matches right-control.
fn compile_modifier_bits<'a>(keys: impl Iterator<Item = Key> + 'a) -> u8 {
    let mut bits: u8 = 0;
    for key in keys {
        bits |= modifier_group_bits(key);
    }
    bits
}

/// Return the group mask for a modifier key.
///
/// Left/right pairs share the same mask so that specifying either side
/// matches both.  Non-modifier keys contribute nothing.
fn modifier_group_bits(key: Key) -> u8 {
    match key {
        // Control group: bits 0 | 1
        Key::LeftControl | Key::RightControl => (1 << 0) | (1 << 1),
        // Shift group: bits 2 | 3
        Key::LeftShift | Key::RightShift => (1 << 2) | (1 << 3),
        // Alt/Option group: bits 4 | 5
        Key::LeftAlt | Key::RightAlt => (1 << 4) | (1 << 5),
        // Command/Meta/Win group: bits 6 | 7
        Key::LeftCommand | Key::RightCommand => (1 << 6) | (1 << 7),
        // Non-modifier keys contribute nothing.
        _ => 0,
    }
}
