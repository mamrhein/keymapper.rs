// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Raw Input device identification and message-only window.
//!
//! Provides per-keyboard device identification on Windows.  The low-level
//! keyboard hook (`WH_KEYBOARD_LL`) intercepts keys but cannot identify the
//! source device.  Raw Input solves this by exposing the `hDevice` handle
//! for each keyboard event.
//!
//! Architecture:
//! - A message-only window receives `WM_INPUT` via `RegisterRawInputDevices`
//!   with `RIDEV_INPUTSINK`.
//! - A dedicated thread runs the `GetMessageW` pump for this window.
//! - Extracted `RawInputEvent`s are sent through a `crossbeam-channel`.

use std::ptr;

use crossbeam_channel::Sender;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::{
    GetRawInputData, RegisterRawInputDevices, HRAWINPUT, RAWINPUT,
    RAWINPUTDEVICE, RAWINPUTHEADER, RIDEV_INPUTSINK, RID_INPUT,
};
use windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW,
    PostQuitMessage, PostMessageW, RegisterClassExW, TranslateMessage,
    WNDCLASSEXW, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, HCURSOR, HICON,
    HWND_MESSAGE, MSG, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CREATE, WM_DESTROY,
    WM_INPUT, WM_KEYUP, WM_SYSKEYUP, WM_USER,
};

/// HID usage page for Generic Desktop.
const HID_USAGE_PAGE_GENERIC: u16 = 0x01;

/// HID usage for Keyboard within the Generic Desktop page.
const HID_USAGE_KEYBOARD: u16 = 0x06;

/// Message type value for keyboard events in Raw Input.
const RIM_TYPEKEYBOARD: u32 = 0x01;

/// Extra message ID used to signal the message-only window to terminate.
const WM_STOP: u32 = WM_USER + 1;

/// A keyboard event extracted from a Raw Input `WM_INPUT` message.
///
/// The `device_handle_ptr` can be used to correlate the event with a specific
/// physical keyboard.  Convert it to a string (e.g. via `GetRawInputDeviceInfo`)
/// to match against the `KeyboardInfo::device` path populated at startup.
#[derive(Debug)]
pub struct RawInputEvent {
    /// Virtual-key code of the event.
    pub vk_code: VIRTUAL_KEY,

    /// `true` for key-up, `false` for key-down.
    pub is_key_up: bool,

    /// Raw device handle identifying the source keyboard, stored as a raw
    /// pointer value for `Send` compatibility.  Use this to look up the
    /// corresponding device path string.
    pub device_handle_ptr: usize,
}

/// Handle to the background message loop thread.  Dropping this does NOT
/// stop the thread; call [`stop_raw_input_loop`] to terminate it gracefully.
pub struct RawInputLoop {
    hwnd: HWND,
}

// ---------------------------------------------------------------------------
// Static state for the window procedure
// ---------------------------------------------------------------------------

/// Shared sender for raw input events.  Accessed from the window procedure
/// so that events can be pushed from the message pump thread.
static RAW_INPUT_TX: parking_lot::Mutex<Option<Sender<RawInputEvent>>> =
    parking_lot::Mutex::new(None);

/// Stores the sender that the window procedure uses to push events.
fn set_raw_input_tx(tx: Sender<RawInputEvent>) {
    *RAW_INPUT_TX.lock() = Some(tx);
}

/// Retrieves the sender for pushing raw input events.
fn get_raw_input_tx() -> Option<Sender<RawInputEvent>> {
    RAW_INPUT_TX.lock().clone()
}

// ---------------------------------------------------------------------------
// Window procedure
// ---------------------------------------------------------------------------

/// Custom window procedure that intercepts `WM_INPUT` and forwards events
/// through the channel.
unsafe extern "system" fn raw_input_window_proc(
    hwnd: HWND,
    msg: u32,
    w_param: WPARAM,
    l_param: LPARAM,
) -> LRESULT {
    match msg {
        WM_CREATE => LRESULT(0),
        WM_INPUT => {
            unsafe { handle_wm_input(hwnd, l_param) };
            LRESULT(0)
        }
        WM_STOP => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, w_param, l_param) },
    }
}

/// Process a `WM_INPUT` message and send the extracted event through the
/// channel.
unsafe fn handle_wm_input(_hwnd: HWND, l_param: LPARAM) {
    let h_raw_input = HRAWINPUT(l_param.0 as *mut std::ffi::c_void);

    // First call: query the required buffer size (pass None for pData).
    let mut size: u32 = 0;
    let _ = unsafe {
        GetRawInputData(
            h_raw_input,
            RID_INPUT,
            None,
            &mut size,
            std::mem::size_of::<RAWINPUTHEADER>() as u32,
        )
    };

    if size == 0 {
        return;
    }

    // Allocate buffer and retrieve the actual data.
    let mut buffer = vec![0u8; size as usize];
    let bytes_read = unsafe {
        GetRawInputData(
            h_raw_input,
            RID_INPUT,
            Some(buffer.as_mut_ptr() as *mut std::ffi::c_void),
            &mut size,
            std::mem::size_of::<RAWINPUTHEADER>() as u32,
        )
    };

    // `UINT_MAX` (0xFFFFFFFF) indicates an error.
    if bytes_read == 0xFFFFFFFF {
        return;
    }

    // Interpret the buffer as a RAWINPUT struct and extract keyboard data.
    let (dw_type, vk, message, _device_ptr) = unsafe {
        let raw_input = &*(buffer.as_ptr() as *const RAWINPUT);
        let keyboard = &raw_input.data.keyboard;
        (
            raw_input.header.dwType,
            keyboard.VKey,
            keyboard.Message,
            raw_input.header.hDevice.0 as usize,
        )
    };

    // We only process keyboard input.
    if dw_type != RIM_TYPEKEYBOARD {
        return;
    }

    // Determine key-up vs. key-down from the Message field.
    let is_key_up = message == WM_KEYUP || message == WM_SYSKEYUP;

    let event = RawInputEvent {
        vk_code: VIRTUAL_KEY(vk),
        is_key_up,
        device_handle_ptr: _device_ptr,
    };

    if let Some(tx) = get_raw_input_tx() {
        let _ = tx.send(event);
    }
}

// ---------------------------------------------------------------------------
// Message-only window creation
// ---------------------------------------------------------------------------

/// Creates an invisible message-only window that receives Raw Input events.
///
/// Returns the `HWND` of the created window.  The window has no visible
/// presence and does not appear in the taskbar or Alt+Tab list.
fn create_message_only_window() -> Result<HWND, Box<dyn std::error::Error>> {
    let class_name = windows::core::w!("KeyMapperRawInputWindow");

    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(raw_input_window_proc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: HINSTANCE(ptr::null_mut()),
        hIcon: HICON::default(),
        hCursor: HCURSOR::default(),
        hbrBackground: Default::default(),
        lpszMenuName: PCWSTR::null(),
        lpszClassName: class_name,
        hIconSm: HICON::default(),
    };

    unsafe {
        let _ = RegisterClassExW(&wc);
    }

    // Create a message-only window by passing HWND_MESSAGE as the parent.
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class_name,
            PCWSTR::null(), // No title.
            WINDOW_STYLE(0),
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            HWND_MESSAGE, // Message-only window — no visible presence.
            None,
            HINSTANCE(ptr::null_mut()),
            None,
        )
    }?;

    if hwnd.is_invalid() {
        return Err(
            "Failed to create message-only window for Raw Input".into(),
        );
    }

    Ok(hwnd)
}

// ---------------------------------------------------------------------------
// Raw Input device registration
// ---------------------------------------------------------------------------

/// Registers all keyboard devices for Raw Input, targeting the given window.
///
/// Uses `RIDEV_INPUTSINK` so that events are delivered even when the
/// application is not in the foreground.
fn register_keyboards(hwnd: HWND) -> Result<(), Box<dyn std::error::Error>> {
    let rid = RAWINPUTDEVICE {
        usUsagePage: HID_USAGE_PAGE_GENERIC,
        usUsage: HID_USAGE_KEYBOARD,
        dwFlags: RIDEV_INPUTSINK,
        hwndTarget: hwnd,
    };

    unsafe {
        RegisterRawInputDevices(
            &[rid],
            std::mem::size_of::<RAWINPUTDEVICE>() as u32,
        )?;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Public API: start the raw input message loop
// ---------------------------------------------------------------------------

/// Starts the Raw Input message loop in a background thread.
///
/// Creates a message-only window, registers all keyboards for Raw Input,
/// and spawns a dedicated thread that pumps `WM_INPUT` messages.  Extracted
/// events are sent through the returned channel receiver.
///
/// Returns a tuple of:
/// - `RawInputLoop`: holds the `HWND` and must be kept alive while receiving events.
/// - `crossbeam_channel::Receiver<RawInputEvent>`: receives raw keyboard events.
pub fn start_raw_input_loop(
) -> Result<(RawInputLoop, crossbeam_channel::Receiver<RawInputEvent>), Box<dyn std::error::Error>>
{
    let (tx, rx) = crossbeam_channel::unbounded();

    let hwnd = create_message_only_window()?;
    register_keyboards(hwnd)?;

    // Store the sender so the window procedure can access it.
    set_raw_input_tx(tx);

    // `HWND` is not `Send` in the `windows` crate, but it is an opaque OS
    // handle with no Rust-level thread affinity.  We pass it as a raw usize
    // and reconstruct it inside the spawned thread.
    let hwnd_ptr = hwnd.0 as usize;
    let handle = std::thread::spawn(move || {
        let hwnd = HWND(hwnd_ptr as *mut std::ffi::c_void);
        run_message_loop(hwnd);
    });

    // We don't join the handle — the thread lives for the lifetime of the
    // application.  Drop the JoinHandle to detach.
    std::mem::forget(handle);

    Ok((RawInputLoop { hwnd }, rx))
}

/// Runs the Windows message pump for the message-only window.  Blocks until
/// a `WM_QUIT` message is received.
fn run_message_loop(_hwnd: HWND) {
    let mut msg = MSG::default();

    // Standard Windows message loop.
    loop {
        let got_message = unsafe { GetMessageW(&mut msg, None, 0, 0) };

        // `GetMessageW` returns FALSE (BOOL(0)) on WM_QUIT, or FALSE on error.
        if !got_message.as_bool() {
            break;
        }

        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// Stops the raw input message loop by posting the `WM_STOP` message.
///
/// The background thread will exit its `GetMessageW` loop after processing
/// this message.
pub fn stop_raw_input_loop(hwnd: HWND) {
    unsafe {
        let _ = PostMessageW(hwnd, WM_STOP, WPARAM(0), LPARAM(0));
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_input_event_fields_are_accessible() {
        let event = RawInputEvent {
            vk_code: VIRTUAL_KEY(0x41), // 'A'
            is_key_up: false,
            device_handle_ptr: 0,
        };

        assert_eq!(event.vk_code.0, 0x41);
        assert!(!event.is_key_up);
        assert_eq!(event.device_handle_ptr, 0);
    }

    #[test]
    fn raw_input_event_key_up() {
        let event = RawInputEvent {
            vk_code: VIRTUAL_KEY(0x10), // LShift
            is_key_up: true,
            device_handle_ptr: 0,
        };

        assert!(event.is_key_up);
    }

    #[test]
    fn hid_usage_constants_are_correct() {
        // Verify the HID usage page and usage constants match expected values.
        assert_eq!(HID_USAGE_PAGE_GENERIC, 0x01);
        assert_eq!(HID_USAGE_KEYBOARD, 0x06);
    }

    #[test]
    fn channel_sends_and_receives_events() {
        let (tx, rx) = crossbeam_channel::unbounded::<RawInputEvent>();

        let event = RawInputEvent {
            vk_code: VIRTUAL_KEY(0x57), // 'W'
            is_key_up: false,
            device_handle_ptr: 0,
        };

        tx.send(event).unwrap();

        let received = rx.recv().unwrap();
        assert_eq!(received.vk_code.0, 0x57);
        assert!(!received.is_key_up);
    }

    #[test]
    fn channel_is_unbounded() {
        let (tx, rx) = crossbeam_channel::unbounded::<RawInputEvent>();

        // Send a burst of events without blocking.
        for i in 0..1000 {
            tx.send(RawInputEvent {
                vk_code: VIRTUAL_KEY(i as u16),
                is_key_up: i % 2 == 0,
                device_handle_ptr: 0,
            })
            .unwrap();
        }

        // Receive them all back.
        let mut count = 0;
        while let Ok(_event) = rx.try_recv() {
            count += 1;
        }
        assert_eq!(count, 1000);
    }
}
