// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Linux log destination: stderr, with no embedded timestamp.

use std::io;

use time::format_description::OwnedFormatItem;

use super::LogSink;

/// Build the Linux sink: stderr with an empty ftlog timestamp format.
///
/// systemd forwards the daemon's stderr to the journal, which timestamps each
/// line itself, so the timestamp format is empty to avoid a duplicate.
pub(crate) fn log_sink() -> Result<LogSink, String> {
    Ok(LogSink {
        root: Box::new(io::stderr()),
        time_format: Some(empty_time_format()),
    })
}

/// An empty `time` format description, so ftlog writes no timestamp (the
/// journal adds one).
fn empty_time_format() -> OwnedFormatItem {
    // An empty description always parses (to an empty compound item that
    // formats to the empty string).
    time::format_description::parse_owned::<1>("")
        .expect("an empty format description always parses")
}
