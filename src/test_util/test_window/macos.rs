// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! macOS test-window backend for the e2e harness.
//!
//! Creates an `NSWindow` and activates the application, so
//! `get_active_app_name()` resolves to this helper's process name (the
//! CoreGraphics window owner name).  The process runs the application until
//! it is killed, keeping the window focused for the duration of a test run.
//!
//! This backend is for local runs only: e2e is skipped in CI on macOS because
//! the Karabiner DriverKit extension needs a one-time user approval.

use objc2::MainThreadMarker;
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType,
    NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

/// Entry point for the macOS test-window helper.
pub fn run() {
    // AppKit requires the main thread; this binary's `main` runs there.
    let mtm = MainThreadMarker::new().expect("must run on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);

    let rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(320.0, 200.0));
    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Miniaturizable;

    // `app.run()` below blocks until the process is killed, so the window
    // stays alive for the whole test run.
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer_screen(
            mtm.alloc(),
            rect,
            style,
            NSBackingStoreType::Buffered,
            false,
            None,
        )
    };
    let title = NSString::from_str("keymapper test window");
    window.setTitle(&title);
    window.makeKeyAndOrderFront(None);

    app.activate();

    eprintln!("testwindow: focused the test window");

    // Block until the process is terminated.
    app.run();
}
