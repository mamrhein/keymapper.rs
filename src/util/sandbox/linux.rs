// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Linux sandbox using uinput for virtual keyboards and evdev for monitoring.
//!
//! TODO: Implement two-uinput-device sandbox.

use super::{Sandbox, SandboxError};

/// Linux sandbox for end-to-end keyboard mapping tests.
pub struct LinuxSandbox;

impl Sandbox for LinuxSandbox {
    fn new() -> Result<Option<Self>, SandboxError> {
        Ok(Some(Self))
    }

    fn setup(&mut self) -> Result<(), SandboxError> {
        Err(SandboxError::NotSupported(
            "Linux sandbox not yet implemented".to_string(),
        ))
    }

    fn inject_key_down(&self, _code: u16) -> Result<(), SandboxError> {
        Err(SandboxError::NotSupported(
            "Linux sandbox not yet implemented".to_string(),
        ))
    }

    fn inject_key_up(&self, _code: u16) -> Result<(), SandboxError> {
        Err(SandboxError::NotSupported(
            "Linux sandbox not yet implemented".to_string(),
        ))
    }

    fn drain_output_events(&self) -> Vec<super::CapturedEvent> {
        Vec::new()
    }

    fn input_device_id(&self) -> Option<&str> {
        None
    }

    fn teardown(&mut self) {}
}
