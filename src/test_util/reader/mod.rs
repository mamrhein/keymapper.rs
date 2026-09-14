// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! A "normal" stdin reader for the e2e test harness.
//!
//! Unlike the old monitor (which grabbed the daemon's output device or hook
//! chain), this binary is an ordinary terminal application: it puts its stdin
//! in raw mode and records whatever characters the operating system delivers
//! to the focused terminal.  That is the truest "what a real app receives"
//! layer — mapped outputs and forwarded passthroughs alike, at character
//! fidelity.
//!
//! The platform-specific `setup()` acquires keyboard focus and disables line
//! buffering / echo so each keystroke is available immediately.  After setup,
//! the shared loop appends every received byte to the output file (flushing
//! per read) so the harness can poll it in real time.  The file is created
//! only after setup succeeds, which the harness uses as its "ready" signal.

use std::{
    io::{Read, Write},
    path::Path,
};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

/// Put the terminal on *fd* into raw mode (the `cfmakeraw` equivalent): no
/// canonical (line) buffering, no echo, no signal generation, no input
/// processing.  This is what every interactive terminal program (vim, htop)
/// does; it is not a capture mechanism.  Shared by the unix platforms.
#[cfg(unix)]
fn make_raw(fd: libc::c_int) {
    // Safety: tcgetattr(2)/tcsetattr(2) on a fd we own; the termios struct is
    // fully initialized by tcgetattr before we modify it.
    unsafe {
        let mut termios: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(fd, &mut termios) == -1 {
            eprintln!(
                "error: tcgetattr failed: {}",
                std::io::Error::last_os_error()
            );
            std::process::exit(1);
        }

        termios.c_iflag &= !(libc::IGNBRK
            | libc::BRKINT
            | libc::PARMRK
            | libc::ISTRIP
            | libc::INLCR
            | libc::IGNCR
            | libc::ICRNL
            | libc::IXON);
        termios.c_oflag &= !libc::OPOST;
        termios.c_lflag &= !(libc::ECHO
            | libc::ECHONL
            | libc::ICANON
            | libc::ISIG
            | libc::IEXTEN);
        termios.c_cflag &= !(libc::CSIZE | libc::PARENB);
        termios.c_cflag |= libc::CS8;

        if libc::tcsetattr(fd, libc::TCSANOW, &termios) == -1 {
            eprintln!(
                "error: tcsetattr failed: {}",
                std::io::Error::last_os_error()
            );
            std::process::exit(1);
        }
    }
}

/// Entry point for the reader application.
///
/// *output_path* is the file that receives the recorded bytes.  It is created
/// (truncating any previous content) once keyboard focus and raw mode are
/// established, then appended to for the process's lifetime.
pub fn run(output_path: &Path) {
    // Acquire keyboard focus and put stdin in raw mode.  On success, stdin
    // (fd 0) is the focused terminal's input stream.
    #[cfg(target_os = "linux")]
    linux::setup();
    #[cfg(target_os = "macos")]
    macos::setup();
    #[cfg(target_os = "windows")]
    windows::setup();

    // Ready signal: create/truncate the output file only now that focus and
    // raw mode are in place, so the harness never observes a "ready" reader
    // that is not actually receiving keys.
    let mut out = fs_err::File::create(output_path).unwrap_or_else(|e| {
        eprintln!("error: failed to create output file {output_path:?}: {e}");
        std::process::exit(1);
    });

    // Record every received byte, flushing per read so the harness can poll
    // the file synchronously.  The loop exits on EOF or read error (the
    // harness kills the process to stop it).
    let stdin = std::io::stdin();
    let mut buf = [0u8; 256];
    loop {
        let n = match stdin.lock().read(&mut buf) {
            Ok(0) => break, // EOF: the terminal closed.
            Ok(n) => n,
            Err(e) => {
                eprintln!("error: failed to read stdin: {e}");
                break;
            }
        };
        if let Err(e) = out.write_all(&buf[..n]) {
            eprintln!("error: failed to write output file: {e}");
            break;
        }
        // Flush on every read so the harness observes bytes in real time.
        if let Err(e) = out.flush() {
            eprintln!("error: failed to flush output file: {e}");
            break;
        }
    }
}
