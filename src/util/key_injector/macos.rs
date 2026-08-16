// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! macOS keyboard injector using CGEvent for event injection.
//!
//! Injected events are posted at HIDEventTap so they reach the session
//! level and are visible to both the daemon's capture and external
//! observers.

use std::{thread, time::Duration};

use objc2_core_graphics::{
    CGEvent, CGEventSource, CGEventSourceStateID, CGEventTapLocation,
    CGEventTapPlacement, CGEventType,
};

use super::{InjectorError, KeyInjector};

/// macOS keyboard injector for end-to-end tests.
pub struct MacOSInjector {
    /// Flag indicating whether `setup()` has been called successfully.
    is_setup: bool,
}

impl MacOSInjector {
    /// Check if the current process has the permissions needed to create
    /// an event tap.
    fn check_permissions() -> Result<(), InjectorError> {
        let mask =
            (1u64 << CGEventType::KeyDown.0) | (1u64 << CGEventType::KeyUp.0);

        // IMPORTANT: `tap_create` with a nil transformer returns a *listing*
        // of existing taps, not a new tap. On a fresh system where no apps
        // have Accessibility permissions, that list is empty and the call
        // returns `None` regardless of whether *we* could create a tap.
        // To actually probe our own permission, we must pass a real callback
        // so the function creates a temporary probe tap.
        let probe = unsafe {
            CGEvent::tap_create(
                CGEventTapLocation::HIDEventTap,
                CGEventTapPlacement::HeadInsertEventTap,
                objc2_core_graphics::CGEventTapOptions::Default,
                mask,
                Some(probe_callback),
                std::ptr::null_mut(),
            )
        };

        if probe.is_some() {
            Ok(())
        } else {
            Err(InjectorError::PermissionDenied(
                "Accessibility permission required. Grant it in System \
                 Settings > Privacy & Security > Accessibility."
                    .to_string(),
            ))
        }
    }

    /// Inject a keyboard event using a freshly created event source.
    fn inject_key(
        &self,
        code: u16,
        is_down: bool,
    ) -> Result<(), InjectorError> {
        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .ok_or_else(|| {
                InjectorError::InjectionFailed(
                    "failed to create CGEventSource for injection".to_string(),
                )
            })?;

        let Some(event) =
            CGEvent::new_keyboard_event(Some(&source), code, is_down)
        else {
            return Err(InjectorError::InjectionFailed(format!(
                "failed to create keyboard event for code {code}"
            )));
        };

        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
        thread::sleep(Duration::from_millis(5));
        Ok(())
    }
}

impl KeyInjector for MacOSInjector {
    fn new() -> Result<Option<Self>, InjectorError> {
        Self::check_permissions()?;

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

impl Drop for MacOSInjector {
    fn drop(&mut self) {
        self.teardown();
    }
}

// ---------------------------------------------------------------------------
// FFI callback used only for the permission probe tap
// ---------------------------------------------------------------------------

/// Minimal callback that just passes events through.  Only used during
/// permission probing — the probe tap is dropped immediately after creation.
unsafe extern "C-unwind" fn probe_callback(
    _proxy: objc2_core_graphics::CGEventTapProxy,
    _event_type: CGEventType,
    event: core::ptr::NonNull<objc2_core_graphics::CGEvent>,
    _user_info: *mut std::ffi::c_void,
) -> *mut objc2_core_graphics::CGEvent {
    event.as_ptr()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injector_new_returns_some_or_permission_error() {
        let result = MacOSInjector::new();
        assert!(
            result.is_ok()
                || matches!(
                    result.as_ref().err().unwrap(),
                    InjectorError::PermissionDenied(..)
                ),
            "Injector::new() returned {:?}",
            result.err()
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
    fn injector_error_is_std_error() {
        let e: Box<dyn std::error::Error> =
            Box::new(InjectorError::NotSupported("test".into()));
        assert!(!e.to_string().is_empty());
    }
}
