// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Event logging for the keyboard monitor.
//!
//! Writes `down <Key>` / `up <Key>` lines to a file or to the process's
//! stdout, flushing after every write so the e2e test harness can read
//! events in real time.

use std::io::{self, Write};

use super::OutputEvent;

/// Destination for captured events: a file or the process's stdout.
pub enum EventWriter {
    /// Events are written to a file (truncated on open).
    File(fs_err::File),
    /// Events are written to the process's stdout.
    Stdout,
}

impl EventWriter {
    /// Open (or create) the output file and truncate any existing content.
    pub fn file(path: &std::path::Path) -> io::Result<Self> {
        Ok(Self::File(fs_err::File::create(path)?))
    }

    /// Write events to the process's stdout.
    pub fn stdout() -> Self {
        Self::Stdout
    }

    /// Write a single event line and flush.
    pub fn write(&mut self, event: OutputEvent) -> io::Result<()> {
        let direction = if event.down { "down" } else { "up" };

        match self {
            Self::File(file) => {
                writeln!(file, "{} {}", direction, event.key.as_str())?;
                // Flush on every write so the test harness can read events
                // synchronously without waiting for buffer flush.
                file.flush()
            }
            Self::Stdout => {
                // `std::io::Stdout` is internally synchronized, so the sink
                // can be written from any thread.
                let stdout = io::stdout();
                let mut lock = stdout.lock();
                writeln!(lock, "{} {}", direction, event.key.as_str())?;
                lock.flush()
            }
        }
    }
}
