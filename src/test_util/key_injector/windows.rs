// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Windows keyboard injector using `SendInput` for event injection.
//!
//! Injected events are posted to the current desktop session and are
//! visible to both the daemon's hook and external observers.

use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, SendInput, VIRTUAL_KEY,
};

use super::{InjectorError, KeyInjector};

/// Windows keyboard injector for end-to-end tests.
pub struct WindowsInjector {
    /// Flag indicating whether `setup()` has been called successfully.
    is_setup: bool,
}

impl WindowsInjector {
    /// Inject a synthetic keyboard event using `SendInput`.
    fn inject_key(
        &self,
        code: u16,
        is_down: bool,
    ) -> Result<(), InjectorError> {
        let flags: u32 = if is_down {
            0
        } else {
            0x0002 // KEYEVENTF_KEYUP
        };

        let input = INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(code),
                    wScan: 0,
                    dwFlags: windows::Win32::UI::Input::KeyboardAndMouse::KEYBD_EVENT_FLAGS(flags),
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };

        let result = unsafe {
            SendInput(&[input], std::mem::size_of::<INPUT>() as i32)
        };

        if result != 1 {
            Err(InjectorError::InjectionFailed(format!(
                "SendInput returned {result}, expected 1"
            )))
        } else {
            Ok(())
        }
    }
}

impl KeyInjector for WindowsInjector {
    fn new() -> Result<Option<Self>, InjectorError> {
        // `SendInput` does not require elevation; it operates on the current
        // desktop session.  No prerequisite check is needed.
        Ok(Some(Self { is_setup: false }))
    }

    fn setup(&mut self) -> Result<(), InjectorError> {
        self.is_setup = true;
        Ok(())
    }

    fn inject_key_down(&self, code: u16) -> Result<(), InjectorError> {
        self.inject_key(code, true)
    }

    fn inject_key_up(&self, code: u16) -> Result<(), InjectorError> {
        self.inject_key(code, false)
    }

    fn teardown(&mut self) {
        self.is_setup = false;
    }
}

impl Drop for WindowsInjector {
    fn drop(&mut self) {
        self.teardown();
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injector_new_succeeds() {
        let result = WindowsInjector::new();
        assert!(result.is_ok(), "new() should succeed on Windows");
        assert!(
            result.as_ref().unwrap().is_some(),
            "new() should return Some on Windows"
        );
    }

    #[test]
    fn injector_error_display() {
        let e = InjectorError::PermissionDenied("test".to_string());
        assert!(format!("{e}").contains("permission denied"));

        let e = InjectorError::DeviceCreationFailed("test".to_string());
        assert!(format!("{e}").contains("device creation failed"));

        let e = InjectorError::InjectionFailed("test".to_string());
        assert!(format!("{e}").contains("injection failed"));

        let e = InjectorError::NotSupported("test".to_string());
        assert!(format!("{e}").contains("not supported"));
    }

    #[test]
    fn injector_error_is_standard_error() {
        let e: Box<dyn std::error::Error> =
            Box::new(InjectorError::NotSupported("test".into()));
        assert!(!e.to_string().is_empty());
    }

    #[test]
    fn injector_teardown_without_setup() {
        let mut injector = WindowsInjector::new().unwrap().unwrap();
        // Teardown without setup should not panic.
        injector.teardown();
    }

    #[test]
    fn injector_drop_without_teardown() {
        let injector = WindowsInjector::new().unwrap().unwrap();
        // Drop without explicit teardown should not panic.
        drop(injector);
    }
}
