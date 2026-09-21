// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Log capture and level control for the e2e harness.
//!
//! [`LogSource`] abstracts where the daemon's log lines come from and gives
//! the harness mark/read-new semantics: [`LogSource::mark`] records the
//! current end of the stream, and repeated [`LogSource::read_new`] calls with
//! the returned token yield only the complete lines appended since the last
//! read.  Two sources exist:
//!
//! - [`FileLogSource`] tails a file the daemon appends to.  On macOS and
//!   Windows the ftlog appender rotates by switching to a dated sibling
//!   (`keymapperd-YYYYMMDD.log`), so the source resolves the current file as
//!   the most recently modified `{stem}*.log` on every call.  On CI Linux the
//!   harness redirects the spawned daemon's stderr to a temp file and uses the
//!   same source in fixed mode.  Both modes re-open the file on every read and
//!   detect truncation or rotation defensively.
//! - [`JournalLogSource`] (local Linux only) reads the systemd user journal of
//!   the `keymapperd.service` unit.  The mark is a line count rather than a
//!   timestamp because `journalctl --since "@<epoch>"` is inclusive of the
//!   whole second, which would leak the previous phase's lines into the
//!   window.
//!
//! The level-control helpers wrap
//! [`keymapper::daemon::control::set_log_level`]: the harness sets `debug`
//! before injecting a phase and resets to `info` (the standard default)
//! afterwards.

#[cfg(target_os = "linux")]
use std::process::Command;
use std::{
    collections::HashMap,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::SystemTime,
};

use keymapper::daemon::{control, logging::LevelFilter};
use thiserror::Error;

/// A mark token: an opaque per-source position (a byte offset in file
/// sources, a line count in the journal source).
pub type Mark = u64;

/// Errors from reading a log source.
#[derive(Debug, Error)]
pub enum CaptureError {
    /// An I/O error while reading the log file.
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// A mark token that this source never created.
    #[error("unknown mark {0}")]
    UnknownMark(Mark),

    /// `journalctl` could not be run or failed (local Linux).
    #[cfg(target_os = "linux")]
    #[error("journalctl failed: {0}")]
    Journal(String),
}

/// A source of daemon log lines with mark/read-new semantics.
pub trait LogSource {
    /// Record the current end of the stream.  Later [`LogSource::read_new`]
    /// calls with the returned token yield only lines appended after this
    /// mark.
    fn mark(&mut self) -> Result<Mark, CaptureError>;

    /// Read all complete lines appended since the last read for *mark* (or
    /// since the mark itself on the first call).  Incomplete trailing lines
    /// are held back until they terminate.
    fn read_new(&mut self, mark: Mark) -> Result<Vec<String>, CaptureError>;
}

// ---------------------------------------------------------------------------
// File-based source
// ---------------------------------------------------------------------------

/// Where the current log file lives.
enum Target {
    /// A fixed file (the temp file a CI harness redirects the daemon's
    /// stderr to).
    Fixed(PathBuf),
    /// The most recently modified `{1}*.log` in the directory (the daemon's
    /// rotating log, which switches to dated siblings).
    Rotated(PathBuf, String),
}

/// A log source that tails a file the daemon appends to.
///
/// The file is re-opened on every read, and truncation or rotation is
/// detected defensively: when the resolved file changed, or shrank below a
/// mark's offset, all marks reset to the start of the (new) file.  A missing
/// file yields no lines rather than an error, because the daemon may not
/// have created it yet.
pub struct FileLogSource {
    /// Where the current log file lives.
    target: Target,
    /// The file the stored offsets refer to.
    current: Option<PathBuf>,
    /// Per-mark read positions (byte offsets into [`Self::current`]).
    positions: HashMap<Mark, u64>,
    /// The next mark token to hand out.
    next_mark: Mark,
}

impl FileLogSource {
    /// Tail a fixed file (e.g. the temp file a CI harness redirects the
    /// daemon's stderr to).  The file may not exist yet.
    pub fn fixed(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        Self {
            target: Target::Fixed(path),
            current: None,
            positions: HashMap::new(),
            next_mark: 1,
        }
    }

    /// Tail the daemon's rotating log in *dir*: the most recently modified
    /// `{stem}.log` or `{stem}-<period>.log` (ftlog switches to a dated
    /// sibling at each rotation).
    pub fn rotated(dir: impl Into<PathBuf>, stem: &str) -> Self {
        Self {
            target: Target::Rotated(dir.into(), stem.to_string()),
            current: None,
            positions: HashMap::new(),
            next_mark: 1,
        }
    }

    /// Resolve the file the daemon is writing to right now.
    fn resolve_current(&self) -> Result<Option<PathBuf>, CaptureError> {
        match &self.target {
            Target::Fixed(path) => Ok(Some(path.clone())),
            Target::Rotated(dir, stem) => {
                let base = format!("{stem}.log");
                let prefix = format!("{stem}-");
                let mut newest: Option<(PathBuf, SystemTime)> = None;
                for entry in fs_err::read_dir(dir)? {
                    let entry = entry?;
                    let name =
                        entry.file_name().to_string_lossy().into_owned();
                    let is_log_sibling = name == base
                        || (name.starts_with(&prefix)
                            && name.ends_with(".log"));
                    if !is_log_sibling {
                        continue;
                    }
                    // A missing mtime sorts oldest, so the entry loses any
                    // tie rather than failing the whole scan.
                    let mtime = entry
                        .metadata()?
                        .modified()
                        .unwrap_or(SystemTime::UNIX_EPOCH);
                    if newest.as_ref().is_none_or(|(_, t)| mtime > *t) {
                        newest = Some((entry.path(), mtime));
                    }
                }
                Ok(newest.map(|(path, _)| path))
            }
        }
    }

    /// The size of *path*, or `None` when the file does not exist yet.
    fn size_of(path: &Path) -> Result<Option<u64>, CaptureError> {
        match fs_err::metadata(path) {
            Ok(meta) => Ok(Some(meta.len())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Adopt *path* as the current file, invalidating stored offsets.
    fn adopt(&mut self, path: Option<PathBuf>) {
        if path != self.current {
            self.positions.clear();
            self.current = path;
        }
    }
}

impl LogSource for FileLogSource {
    fn mark(&mut self) -> Result<Mark, CaptureError> {
        let path = self.resolve_current()?;
        self.adopt(path.clone());
        let size = path
            .as_ref()
            .and_then(|p| Self::size_of(p).ok().flatten())
            .unwrap_or(0);
        let mark = self.next_mark;
        self.next_mark += 1;
        self.positions.insert(mark, size);
        Ok(mark)
    }

    fn read_new(&mut self, mark: Mark) -> Result<Vec<String>, CaptureError> {
        let mut pos = *self
            .positions
            .get(&mark)
            .ok_or(CaptureError::UnknownMark(mark))?;

        // A rotation (or the file appearing for the first time) changes the
        // resolved path and invalidates the stored offsets.
        let path = self.resolve_current()?;
        if path != self.current {
            self.adopt(path.clone());
            pos = 0;
        }
        let Some(path) = path else {
            // The daemon has not created a log file yet.
            return Ok(Vec::new());
        };

        let Some(size) = Self::size_of(&path)? else {
            return Ok(Vec::new());
        };
        if size < pos {
            // Truncation or in-place rotation: every mark is stale.
            self.positions.clear();
            self.positions.insert(mark, 0);
            pos = 0;
        }
        if size == pos {
            return Ok(Vec::new());
        }

        let mut file = std::fs::File::open(&path)?;
        file.seek(SeekFrom::Start(pos))?;
        let mut buf = Vec::new();
        file.read_to_end(&mut buf)?;

        // Hold back an incomplete trailing line until it terminates.
        let Some(end) = buf.iter().rposition(|&b| b == b'\n') else {
            return Ok(Vec::new());
        };
        self.positions.insert(mark, pos + end as u64 + 1);

        let text = String::from_utf8_lossy(&buf[..=end]);
        Ok(text
            .split_inclusive('\n')
            .map(|line| line.trim_end_matches(['\n', '\r']).to_string())
            .collect())
    }
}

// ---------------------------------------------------------------------------
// Journal-based source (local Linux only)
// ---------------------------------------------------------------------------

/// A log source that reads the systemd user journal of a unit.
///
/// The mark is a line count, not a timestamp: `journalctl --since "@<epoch>"`
/// is inclusive of the whole second, so a mark taken mid-second would leak
/// the previous phase's lines into the window.  Each read re-fetches the
/// unit's log and returns the slice after the stored count; a vacuumed
/// journal (fewer lines than the mark) falls back to the start.  The `-o cat`
/// output carries only the daemon's raw lines, without the journal's
/// timestamp/host/pid prefix, so they match the unified log grammar.
#[cfg(target_os = "linux")]
pub struct JournalLogSource {
    /// The unit to read (e.g. `keymapperd.service`).
    unit: String,
    /// Per-mark read positions (line counts).
    positions: HashMap<Mark, usize>,
    /// The next mark token to hand out.
    next_mark: Mark,
}

#[cfg(target_os = "linux")]
impl JournalLogSource {
    /// Read the user journal of *unit*.
    pub fn new(unit: &str) -> Self {
        Self {
            unit: unit.to_string(),
            positions: HashMap::new(),
            next_mark: 1,
        }
    }

    /// Fetch the unit's current log lines.
    fn fetch_lines(&self) -> Result<Vec<String>, CaptureError> {
        let out = Command::new("journalctl")
            .args(["--user", "-u", &self.unit, "-q", "-o", "cat"])
            .output()
            .map_err(|e| {
                CaptureError::Journal(format!("failed to run journalctl: {e}"))
            })?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            return Err(CaptureError::Journal(stderr.trim().to_string()));
        }
        Ok(String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_string)
            .collect())
    }
}

#[cfg(target_os = "linux")]
impl LogSource for JournalLogSource {
    fn mark(&mut self) -> Result<Mark, CaptureError> {
        let count = self.fetch_lines()?.len();
        let mark = self.next_mark;
        self.next_mark += 1;
        self.positions.insert(mark, count);
        Ok(mark)
    }

    fn read_new(&mut self, mark: Mark) -> Result<Vec<String>, CaptureError> {
        let lines = self.fetch_lines()?;
        let pos = *self
            .positions
            .get(&mark)
            .ok_or(CaptureError::UnknownMark(mark))?;
        // The journal may have been vacuumed since the mark.
        let pos = pos.min(lines.len());
        let new_lines = lines[pos..].to_vec();
        self.positions.insert(mark, lines.len());
        Ok(new_lines)
    }
}

// ---------------------------------------------------------------------------
// Level control
// ---------------------------------------------------------------------------

/// Errors from changing the running daemon's log level.
#[derive(Debug, Error)]
pub enum LevelControlError {
    /// No daemon control endpoint could be reached: the daemon is not
    /// running, or it predates the control socket.
    #[error(
        "{0}; is keymapperd running, and does it support the control socket?"
    )]
    Unreachable(String),

    /// The daemon rejected the command.
    #[error("the daemon rejected the level change: {0}")]
    Rejected(String),

    /// The daemon replied with an unrecognised frame.
    #[error("unexpected reply from the daemon: {0}")]
    Unexpected(String),
}

/// Set the running daemon's log level to `debug`, so a phase's key events
/// and emits are logged.
pub fn set_debug() -> Result<(), LevelControlError> {
    set_level(LevelFilter::Debug)
}

/// Reset the running daemon's log level to `info`, the standard default.
pub fn reset_to_default() -> Result<(), LevelControlError> {
    set_level(LevelFilter::Info)
}

/// Send a `SET-LOG-LEVEL` command and classify the daemon's reply.
fn set_level(level: LevelFilter) -> Result<(), LevelControlError> {
    let reply = control::set_log_level(level).map_err(|e| match e {
        control::ControlError::Connect(reason) => {
            LevelControlError::Unreachable(reason)
        }
        other => LevelControlError::Unexpected(other.to_string()),
    })?;
    classify_reply(&reply)
}

/// Classify the daemon's reply to a `SET-LOG-LEVEL` command: `OK <level>`
/// succeeds, `ERROR <reason>` is a rejection, anything else is unexpected.
fn classify_reply(reply: &str) -> Result<(), LevelControlError> {
    if reply.starts_with("OK ") {
        Ok(())
    } else if let Some(reason) = reply.strip_prefix("ERROR ") {
        Err(LevelControlError::Rejected(reason.to_string()))
    } else {
        Err(LevelControlError::Unexpected(reply.to_string()))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;

    /// Append raw bytes to a file, creating it if absent.
    fn append(path: &Path, data: &[u8]) {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        file.write_all(data).unwrap();
        file.flush().unwrap();
    }

    /// A fixed-mode source on a fresh temp file.
    fn temp_source() -> (tempfile::TempDir, FileLogSource, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.log");
        (dir, FileLogSource::fixed(path.clone()), path)
    }

    #[test]
    fn mark_and_read_new_returns_only_appended_lines() {
        let (_dir, mut source, path) = temp_source();
        append(&path, b"old1\nold2\n");

        let mark = source.mark().unwrap();
        append(&path, b"new1\nnew2\n");

        assert_eq!(source.read_new(mark).unwrap(), vec!["new1", "new2"]);
    }

    #[test]
    fn read_new_polls_growth() {
        let (_dir, mut source, path) = temp_source();
        let mark = source.mark().unwrap();

        assert_eq!(source.read_new(mark).unwrap(), Vec::<String>::new());
        append(&path, b"a\n");
        assert_eq!(source.read_new(mark).unwrap(), vec!["a"]);
        append(&path, b"b\nc\n");
        assert_eq!(source.read_new(mark).unwrap(), vec!["b", "c"]);
        // A second poll without growth yields nothing.
        assert_eq!(source.read_new(mark).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn incomplete_line_is_held_back() {
        let (_dir, mut source, path) = temp_source();
        let mark = source.mark().unwrap();

        append(&path, b"part");
        assert_eq!(source.read_new(mark).unwrap(), Vec::<String>::new());
        append(&path, b"ial\n");
        assert_eq!(source.read_new(mark).unwrap(), vec!["partial"]);
    }

    #[test]
    fn truncation_resets_to_start() {
        let (_dir, mut source, path) = temp_source();
        append(&path, b"old1\nold2\n");
        let mark = source.mark().unwrap();

        // Simulate a truncating rotation: the file shrank below the mark.
        std::fs::write(&path, b"new\n").unwrap();

        assert_eq!(source.read_new(mark).unwrap(), vec!["new"]);
    }

    #[test]
    fn rotation_to_newer_sibling_switches_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut source = FileLogSource::rotated(dir.path(), "keymapperd");

        let base = dir.path().join("keymapperd.log");
        append(&base, b"old\n");
        let mark = source.mark().unwrap();

        // The daemon rotated to a dated sibling (created later, so its mtime
        // is newer).
        std::thread::sleep(std::time::Duration::from_millis(5));
        let rotated = dir.path().join("keymapperd-20260921.log");
        append(&rotated, b"new\n");

        assert_eq!(source.read_new(mark).unwrap(), vec!["new"]);
    }

    #[test]
    fn mark_before_file_exists() {
        let (_dir, mut source, path) = temp_source();

        let mark = source.mark().unwrap();
        assert_eq!(source.read_new(mark).unwrap(), Vec::<String>::new());

        append(&path, b"first\n");
        assert_eq!(source.read_new(mark).unwrap(), vec!["first"]);
    }

    #[test]
    fn multiple_marks_read_independently() {
        let (_dir, mut source, path) = temp_source();
        append(&path, b"a\n");
        let mark1 = source.mark().unwrap();
        append(&path, b"b\n");
        let mark2 = source.mark().unwrap();
        append(&path, b"c\n");

        assert_eq!(source.read_new(mark1).unwrap(), vec!["b", "c"]);
        assert_eq!(source.read_new(mark2).unwrap(), vec!["c"]);
        // mark1's position advanced with its first read.
        assert_eq!(source.read_new(mark1).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn unknown_mark_is_an_error() {
        let (_dir, mut source, _path) = temp_source();
        assert!(matches!(
            source.read_new(42),
            Err(CaptureError::UnknownMark(42))
        ));
    }

    #[test]
    fn classify_reply_accepts_ok() {
        assert!(classify_reply("OK debug").is_ok());
        assert!(classify_reply("OK info").is_ok());
    }

    #[test]
    fn classify_reply_rejects_error() {
        let err = classify_reply("ERROR invalid level").unwrap_err();
        assert!(matches!(err, LevelControlError::Rejected(_)));
    }

    #[test]
    fn classify_reply_flags_garbage() {
        let err = classify_reply("nonsense").unwrap_err();
        assert!(matches!(err, LevelControlError::Unexpected(_)));
    }
}
