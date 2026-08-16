// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! File-based event logging for the keyboard monitor.
//!
//! Writes `down <Key>` / `up <Key>` lines to a file, flushing after every
//! write so the e2e test harness can read events in real time.

use std::io::Write;

use super::OutputEvent;

/// Wraps a file handle and writes `OutputEvent` lines.
pub struct EventWriter {
    file: fs_err::File,
}

impl EventWriter {
    /// Open (or create) the output file and truncate any existing content.
    pub fn new(path: &std::path::Path) -> std::io::Result<Self> {
        let file = fs_err::File::create(path)?;
        Ok(Self { file })
    }

    /// Write a single event line and flush.
    pub fn write(&mut self, event: OutputEvent) -> std::io::Result<()> {
        let direction = if event.down { "down" } else { "up" };
        writeln!(self.file, "{} {}", direction, event.key.as_str())?;
        // Flush on every write so the test harness can read events
        // synchronously without waiting for buffer flush.
        self.file.flush()?;
        Ok(())
    }
}
