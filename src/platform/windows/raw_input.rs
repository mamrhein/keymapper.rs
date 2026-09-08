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
//! Raw Input is also the source of the HID key identity: keyboard events
//! (`RIM_TYPEKEYBOARD`) are converted to `HidUsage` via the `Key` table, and
//! Consumer Page events (`RIM_TYPEHID`, e.g. media keys from standalone
//! Consumer Control devices) are decoded from the raw report via `hid.dll`.
//!
//! Architecture:
//! - A message-only window receives `WM_INPUT` via `RegisterRawInputDevices`
//!   with `RIDEV_INPUTSINK` for both keyboard and Consumer Control devices.
//! - A dedicated thread runs the `GetMessageW` pump for this window.
//! - Extracted `RawInputEvent`s are sent through a `crossbeam-channel`.

use std::ptr;

use crossbeam_channel::Sender;
use windows::{
    Win32::{
        Devices::HumanInterfaceDevice::{
            HidD_FreePreparsedData, HidP_GetData, HidP_Input,
            PHIDP_PREPARSED_DATA,
        },
        Foundation::{HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM},
        UI::{
            Input::{
                GetRawInputData, GetRawInputDeviceInfoW, HRAWINPUT,
                KeyboardAndMouse::VIRTUAL_KEY, RAWHID, RAWINPUT,
                RAWINPUTDEVICE, RAWINPUTHEADER, RID_INPUT, RIDEV_INPUTSINK,
                RIDI_PREPARSEDDATA, RegisterRawInputDevices,
            },
            WindowsAndMessaging::{
                CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, CreateWindowExW,
                DefWindowProcW, DispatchMessageW, GetMessageW, HCURSOR, HICON,
                HWND_MESSAGE, MSG, PostMessageW, PostQuitMessage,
                RegisterClassExW, TranslateMessage, WINDOW_EX_STYLE,
                WINDOW_STYLE, WM_CREATE, WM_DESTROY, WM_INPUT, WM_KEYUP,
                WM_SYSKEYUP, WM_USER, WNDCLASSEXW,
            },
        },
    },
    core::PCWSTR,
};

use super::key::Key;
use crate::common::hid_usage::{HidUsage, PAGE_CONSUMER};

/// HID usage page for Generic Desktop.
const HID_USAGE_PAGE_GENERIC: u16 = 0x01;

/// HID usage for Keyboard within the Generic Desktop page.
const HID_USAGE_KEYBOARD: u16 = 0x06;

/// HID usage page for Consumer.
const HID_USAGE_PAGE_CONSUMER: u16 = 0x0C;

/// HID usage for Consumer Control within the Consumer page.
const HID_USAGE_CONSUMER_CONTROL: u16 = 0x01;

/// Message type value for keyboard events in Raw Input.
const RIM_TYPEKEYBOARD: u32 = 0x01;

/// Message type value for generic HID events in Raw Input.
///
/// The Windows SDK defines this as 4.  The `windows` crate's `RIM_TYPEHID`
/// constant is incorrect (2), so the SDK value is defined locally, matching
/// the pattern used for `RIM_TYPEKEYBOARD` above.
const RIM_TYPEHID: u32 = 0x04;

/// Extra message ID used to signal the message-only window to terminate.
const WM_STOP: u32 = WM_USER + 1;

/// A keyboard event extracted from a Raw Input `WM_INPUT` message.
///
/// The `usage` field carries the decoded HID identity of the key — the
/// preferred identity for mapping lookups.  Keyboard events
/// (`RIM_TYPEKEYBOARD`) expose the key as a virtual-key code, which is
/// converted to a `HidUsage` via the static `Key` table.  Raw HID events
/// (`RIM_TYPEHID`, e.g. Consumer Page media keys) expose the raw report,
/// which is decoded to a `HidUsage` via the device's report descriptor.
///
/// The `device_handle_ptr` can be used to correlate the event with a specific
/// physical keyboard.  Convert it to a string (e.g. via
/// `GetRawInputDeviceInfo`) to match against the `KeyboardInfo::device` path
/// populated at startup.
#[derive(Debug, Clone)]
pub struct RawInputEvent {
    /// Decoded HID identity of the event, if it could be resolved.
    pub usage: Option<HidUsage>,

    /// Virtual-key code of the event (keyboard events only; `None` for raw
    /// HID events).
    pub vk_code: Option<VIRTUAL_KEY>,

    /// `true` for key-up, `false` for key-down.
    pub is_key_up: bool,

    /// Raw device handle identifying the source keyboard, stored as a raw
    /// pointer value for `Send` compatibility.  Use this to look up the
    /// corresponding device path string.
    pub device_handle_ptr: usize,
}

/// Handle to the background message loop thread.  Dropping this does NOT
/// stop the thread; call [`stop_raw_input_loop`] to terminate it gracefully.
#[allow(dead_code)]
pub struct RawInputLoop {
    #[allow(dead_code)]
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

    // Interpret the buffer as a RAWINPUT struct and dispatch on the data
    // type: keyboard events carry a virtual-key code, while generic HID
    // events (e.g. Consumer Page media keys) carry the raw report.
    unsafe {
        let raw_input = &*(buffer.as_ptr() as *const RAWINPUT);
        match raw_input.header.dwType {
            RIM_TYPEKEYBOARD => {
                let keyboard = &raw_input.data.keyboard;
                let vk = keyboard.VKey;
                let message = keyboard.Message;

                // Determine key-up vs. key-down from the Message field.
                let is_key_up = message == WM_KEYUP || message == WM_SYSKEYUP;

                // The raw keyboard data only exposes the virtual-key code;
                // the HID identity is derived via the static `Key` table.
                let usage = Key::from_native(vk).map(Key::to_hid_usage);

                let event = RawInputEvent {
                    usage,
                    vk_code: Some(VIRTUAL_KEY(vk)),
                    is_key_up,
                    device_handle_ptr: raw_input.header.hDevice.0 as usize,
                };

                if let Some(tx) = get_raw_input_tx() {
                    let _ = tx.send(event);
                }
            }
            RIM_TYPEHID => {
                let hid = &raw_input.data.hid;
                let data_len = hid.dwSizeHid as usize;

                // `bRawData` is a flexible array member; bound it to the
                // size reported by the driver and verify it fits the buffer.
                let offset =
                    hid as *const RAWHID as usize - buffer.as_ptr() as usize;
                if data_len == 0 || data_len > buffer.len() - offset {
                    return;
                }

                let report = std::slice::from_raw_parts(
                    hid.bRawData.as_ptr(),
                    data_len,
                );
                let Some(usage) =
                    decode_hid_usage(raw_input.header.hDevice, report)
                else {
                    // Undecodable report (e.g. a key-up with no usages) —
                    // nothing to dispatch.
                    return;
                };

                // Raw HID events carry no key state; a decodable report
                // identifies a key press.
                let event = RawInputEvent {
                    usage: Some(usage),
                    vk_code: None,
                    is_key_up: false,
                    device_handle_ptr: raw_input.header.hDevice.0 as usize,
                };

                if let Some(tx) = get_raw_input_tx() {
                    let _ = tx.send(event);
                }
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Raw HID report decoding
// ---------------------------------------------------------------------------

/// Number of usage fields to decode from a raw HID report.
const MAX_HID_DATA_ENTRIES: usize = 64;

/// Local mirror of the `HIDP_DATA` structure from `hidpi.h`.
///
/// The `windows` crate's `HIDP_DATA` is truncated (it is missing the
/// `Value` union and the trailing reserved fields), so this mirror defines
/// the full 64-bit layout (identical offsets on x86-64 and aarch64) and is
/// used to pass storage to `HidP_GetData`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct HidpData {
    data_in: u16,
    reserved: u16,
    usage_page: u16,
    link_collection: u16,
    usage: u16,
    padding: u32,
    /// `Value` union: `NumericValue` and `StringValue` share the first four
    /// bytes.  Eight bytes mirror the pointer width on x86-64 so the struct
    /// size and field offsets match the C layout exactly.
    value: [u8; 8],
    reserved1: u16,
    reserved2: u16,
}

impl HidpData {
    /// Return the numeric value of the usage field.
    fn numeric_value(self) -> i32 {
        i32::from_le_bytes(self.value[..4].try_into().unwrap())
    }
}

/// Decode a raw HID report into the first pressed Consumer Page usage.
///
/// Obtains the device's preparsed data (report descriptor) via
/// `GetRawInputDeviceInfoW` and asks `hid.dll` to decode the report.  The
/// first entry on the Consumer page with a non-zero value identifies the
/// pressed key (e.g. Play/Pause).  Reports without a pressed key (key-ups)
/// yield `None`.
fn decode_hid_usage(device: HANDLE, report: &[u8]) -> Option<HidUsage> {
    // Query the required buffer size for the preparsed data handle.
    let mut size: u32 = 0;
    if unsafe {
        GetRawInputDeviceInfoW(
            Some(device),
            RIDI_PREPARSEDDATA,
            None,
            &mut size,
        )
    } == u32::MAX
    {
        return None;
    }

    let mut preparsed = PHIDP_PREPARSED_DATA::default();
    if unsafe {
        GetRawInputDeviceInfoW(
            Some(device),
            RIDI_PREPARSEDDATA,
            Some(
                &mut preparsed as *mut PHIDP_PREPARSED_DATA
                    as *mut std::ffi::c_void,
            ),
            &mut size,
        )
    } == u32::MAX
    {
        return None;
    }

    if preparsed.0 == 0 {
        return None;
    }

    let usage = (|| {
        // `HidP_GetData` decodes the report into one entry per usage field.
        let mut entries: Vec<HidpData> =
            vec![HidpData::default(); MAX_HID_DATA_ENTRIES];
        let mut count: u32 = entries.len() as u32;
        // The safe wrapper demands a mutable slice, so pass an owned copy.
        let mut owned_report = report.to_vec();
        let status = unsafe {
            HidP_GetData(
                HidP_Input,
                entries.as_mut_ptr().cast(),
                &mut count,
                preparsed,
                &mut owned_report,
            )
        };
        if status.0 != 0 {
            return None;
        }

        entries[..count as usize].iter().find_map(|entry| {
            if entry.usage_page == PAGE_CONSUMER && entry.numeric_value() != 0
            {
                HidUsage::from_code(
                    ((PAGE_CONSUMER as u32) << 16) | entry.usage as u32,
                )
            } else {
                None
            }
        })
    })();

    unsafe {
        let _ = HidD_FreePreparsedData(preparsed);
    }

    usage
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
            Some(HWND_MESSAGE), // Message-only window — no visible presence.
            None,
            Some(HINSTANCE(ptr::null_mut())),
            None,
        )
    }?;

    if hwnd.is_invalid() {
        return Err(
            "Failed to create message-only window for Raw Input".into()
        );
    }

    Ok(hwnd)
}

// ---------------------------------------------------------------------------
// Raw Input device registration
// ---------------------------------------------------------------------------

/// Registers keyboard and Consumer Control devices for Raw Input, targeting
/// the given window.
///
/// Keyboard events (`RIM_TYPEKEYBOARD`) carry virtual-key codes; Consumer
/// Control events (`RIM_TYPEHID`) carry raw HID reports for media keys.
/// Uses `RIDEV_INPUTSINK` so that events are delivered even when the
/// application is not in the foreground.
fn register_keyboards(hwnd: HWND) -> Result<(), Box<dyn std::error::Error>> {
    let rids = [
        RAWINPUTDEVICE {
            usUsagePage: HID_USAGE_PAGE_GENERIC,
            usUsage: HID_USAGE_KEYBOARD,
            dwFlags: RIDEV_INPUTSINK,
            hwndTarget: hwnd,
        },
        RAWINPUTDEVICE {
            usUsagePage: HID_USAGE_PAGE_CONSUMER,
            usUsage: HID_USAGE_CONSUMER_CONTROL,
            dwFlags: RIDEV_INPUTSINK,
            hwndTarget: hwnd,
        },
    ];

    unsafe {
        RegisterRawInputDevices(
            &rids,
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
/// - `RawInputLoop`: holds the `HWND` and must be kept alive while receiving
///   events.
/// - `crossbeam_channel::Receiver<RawInputEvent>`: receives raw keyboard
///   events.
pub fn start_raw_input_loop() -> Result<
    (RawInputLoop, crossbeam_channel::Receiver<RawInputEvent>),
    Box<dyn std::error::Error>,
> {
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
/// a `WM_QUIT` or `WM_STOP` message is received.
///
/// Uses `GetMessageW` with the specific `HWND` so that only messages destined
/// for this window are processed.  This avoids competing with the main
/// thread's message loop, which handles the `WH_KEYBOARD_LL` hook callbacks.
fn run_message_loop(hwnd: HWND) {
    let mut msg = MSG::default();

    loop {
        let got_message = unsafe { GetMessageW(&mut msg, Some(hwnd), 0, 0) };

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
#[allow(dead_code)]
pub fn stop_raw_input_loop(hwnd: HWND) {
    unsafe {
        let _ = PostMessageW(Some(hwnd), WM_STOP, WPARAM(0), LPARAM(0));
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
            usage: Some(HidUsage::A),
            vk_code: Some(VIRTUAL_KEY(0x41)), // 'A'
            is_key_up: false,
            device_handle_ptr: 0,
        };

        assert_eq!(event.usage, Some(HidUsage::A));
        assert_eq!(event.vk_code.unwrap().0, 0x41);
        assert!(!event.is_key_up);
        assert_eq!(event.device_handle_ptr, 0);
    }

    #[test]
    fn raw_input_event_key_up() {
        let event = RawInputEvent {
            usage: Some(HidUsage::LeftShift),
            vk_code: Some(VIRTUAL_KEY(0x10)), // LShift
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
        assert_eq!(HID_USAGE_PAGE_CONSUMER, 0x0C);
        assert_eq!(HID_USAGE_CONSUMER_CONTROL, 0x01);
    }

    #[test]
    fn rim_type_hid_constant_matches_sdk() {
        // The Windows SDK defines RIM_TYPEHID as 4.
        assert_eq!(RIM_TYPEHID, 0x04);
    }

    #[test]
    fn hidp_data_layout_matches_sdk() {
        // The 64-bit layout must match hidpi.h: UsagePage at offset 4,
        // Usage at offset 8, NumericValue at offset 16, size 28.
        assert_eq!(std::mem::offset_of!(HidpData, usage_page), 4);
        assert_eq!(std::mem::offset_of!(HidpData, usage), 8);
        assert_eq!(std::mem::offset_of!(HidpData, value), 16);
        assert_eq!(std::mem::size_of::<HidpData>(), 28);
    }

    #[test]
    fn channel_sends_and_receives_events() {
        let (tx, rx) = crossbeam_channel::unbounded::<RawInputEvent>();

        let event = RawInputEvent {
            usage: Some(HidUsage::W),
            vk_code: Some(VIRTUAL_KEY(0x57)), // 'W'
            is_key_up: false,
            device_handle_ptr: 0,
        };

        tx.send(event).unwrap();

        let received = rx.recv().unwrap();
        assert_eq!(received.usage, Some(HidUsage::W));
        assert_eq!(received.vk_code.unwrap().0, 0x57);
        assert!(!received.is_key_up);
    }

    #[test]
    fn channel_is_unbounded() {
        let (tx, rx) = crossbeam_channel::unbounded::<RawInputEvent>();

        // Send a burst of events without blocking.
        for i in 0..1000 {
            tx.send(RawInputEvent {
                usage: None,
                vk_code: Some(VIRTUAL_KEY(i as u16)),
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

    #[test]
    fn multi_device_event_distinction() {
        // Verify that events from different devices can be distinguished.
        let event1 = RawInputEvent {
            usage: Some(HidUsage::A),
            vk_code: Some(VIRTUAL_KEY(0x41)),
            is_key_up: false,
            device_handle_ptr: 0x1000, // Device 1
        };

        let event2 = RawInputEvent {
            usage: Some(HidUsage::A),
            vk_code: Some(VIRTUAL_KEY(0x41)),
            is_key_up: false,
            device_handle_ptr: 0x2000, // Device 2
        };

        // Same key, different devices.
        assert_eq!(event1.vk_code, event2.vk_code);
        assert_ne!(event1.device_handle_ptr, event2.device_handle_ptr);
    }

    #[test]
    fn concurrent_senders_stress_test() {
        use std::thread;

        let (tx, rx) = crossbeam_channel::unbounded::<RawInputEvent>();

        let num_senders = 10;
        let events_per_sender = 100;

        let handles: Vec<_> = (0..num_senders)
            .map(|sender_id| {
                let tx = tx.clone();
                thread::spawn(move || {
                    for i in 0..events_per_sender {
                        tx.send(RawInputEvent {
                            usage: None,
                            vk_code: Some(VIRTUAL_KEY(i as u16)),
                            is_key_up: i % 2 == 0,
                            device_handle_ptr: sender_id as usize,
                        })
                        .unwrap();
                    }
                })
            })
            .collect();

        for handle in handles {
            handle.join().unwrap();
        }

        // Drop the original sender so we can count exactly.
        drop(tx);

        let mut received = 0;
        while let Ok(_event) = rx.try_recv() {
            received += 1;
        }
        assert_eq!(received, num_senders * events_per_sender);
    }

    #[test]
    fn event_ordering_preserved_for_single_sender() {
        let (tx, rx) = crossbeam_channel::unbounded::<RawInputEvent>();

        // Send events in a specific order.
        for i in 0..100 {
            tx.send(RawInputEvent {
                usage: None,
                vk_code: Some(VIRTUAL_KEY(i)),
                is_key_up: false,
                device_handle_ptr: 0,
            })
            .unwrap();
        }

        // Receive and verify ordering.
        for expected in 0..100 {
            let event = rx.recv().unwrap();
            assert_eq!(
                event.vk_code.unwrap().0,
                expected,
                "event ordering violated at index {expected}"
            );
        }
    }

    #[test]
    fn static_sender_set_and_get() {
        // Verify the static sender storage works correctly.
        // Note: this test may interfere with other tests that use the
        // same static, so we restore the state afterwards.

        let (tx, _rx) = crossbeam_channel::unbounded::<RawInputEvent>();

        // Save the current state.
        let previous = get_raw_input_tx();

        set_raw_input_tx(tx);

        // Verify we can retrieve it.
        assert!(get_raw_input_tx().is_some());

        // Restore the previous state.
        if let Some(prev_tx) = previous {
            set_raw_input_tx(prev_tx);
        } else {
            // Create a new bounded channel and drop the receiver immediately,
            // so the sender is in a "disconnected" state similar to None.
            let (dummy_tx, dummy_rx) =
                crossbeam_channel::bounded::<RawInputEvent>(1);
            drop(dummy_rx);
            set_raw_input_tx(dummy_tx);
        }
    }

    #[test]
    fn raw_input_event_debug_format() {
        let event = RawInputEvent {
            usage: Some(HidUsage::A),
            vk_code: Some(VIRTUAL_KEY(0x41)),
            is_key_up: true,
            device_handle_ptr: 0x1234,
        };

        let debug_str = format!("{event:?}");
        assert!(debug_str.contains("RawInputEvent"));
        assert!(debug_str.contains("device_handle_ptr"));
    }

    #[test]
    fn wm_stop_message_constant() {
        // Verify that WM_STOP is defined as a custom message.
        assert_eq!(WM_STOP, WM_USER + 1);
    }

    #[test]
    fn rim_type_keyboard_constant() {
        // Verify the keyboard input type constant.
        assert_eq!(RIM_TYPEKEYBOARD, 0x01);
    }
}
