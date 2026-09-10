// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Windows test-window backend for the e2e harness.
//!
//! Creates a visible top-level window and brings it to the foreground, so
//! `get_active_app_name()` resolves to this helper's executable file name
//! (e.g. `keymapper_testwindow.exe`).  The process runs a message loop until
//! it is killed, keeping the window alive for the duration of a test run.

use std::sync::atomic::Ordering;

use windows::{
    Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM},
        UI::WindowsAndMessaging::{
            CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, CreateWindowExW,
            DefWindowProcW, GetMessageW, MSG, RegisterClassExW, SW_SHOW,
            SetForegroundWindow, ShowWindow, WINDOW_EX_STYLE, WNDCLASSEXW,
            WS_OVERLAPPEDWINDOW,
        },
    },
    core::{PCWSTR, w},
};

use crate::test_util::monitor::register_signal_handlers;

/// Window procedure — no custom handling, defer everything to the default.
unsafe extern "system" fn test_window_proc(
    hwnd: HWND,
    msg: u32,
    w_param: WPARAM,
    l_param: LPARAM,
) -> LRESULT {
    unsafe { DefWindowProcW(hwnd, msg, w_param, l_param) }
}

/// Entry point for the Windows test-window helper.
pub fn run() {
    let class_name = w!("KeyMapperTestWindow");

    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(test_window_proc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: HINSTANCE(std::ptr::null_mut()),
        hIcon: Default::default(),
        hCursor: Default::default(),
        hbrBackground: Default::default(),
        lpszMenuName: PCWSTR::null(),
        lpszClassName: class_name,
        hIconSm: Default::default(),
    };

    unsafe {
        let _ = RegisterClassExW(&wc);
    }

    // A visible, non-zero-area top-level window so it can take the
    // foreground and is not skipped by zero-area window filters.
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class_name,
            w!("keymapper test window"),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            320,
            200,
            None,
            None,
            Some(HINSTANCE(std::ptr::null_mut())),
            None,
        )
    }
    .expect("failed to create the test window");

    if hwnd.is_invalid() {
        panic!("test window handle is invalid");
    }

    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
    }

    eprintln!("testwindow: focused the test window");

    let shutdown = register_signal_handlers();

    // Run the message loop that keeps the window alive.  It exits when the
    // process is killed (hard terminate in e2e) or the shutdown flag is set.
    let mut msg = MSG::default();
    unsafe {
        while !shutdown.load(Ordering::Relaxed)
            && GetMessageW(&mut msg, None, 0, 0).as_bool()
        {
            // Pump one message.
        }
    }
}
