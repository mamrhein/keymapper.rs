// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Rotating-file log destination for the file-logging platforms (macOS and
//! Windows).
//!
//! This module also owns the per-OS resolution of the log directory: the
//! two file platforms agree on the sink but not on the destination path,
//! so the `#[cfg(windows)]` split lives here rather than in `common`.

use std::path::PathBuf;

use super::LogSink;

/// Build the file sink: a daily-rotated, 7-day-retention appender under the
/// shared log directory, with ftlog's default timestamp.
///
/// These platforms have no journal to timestamp lines, so ftlog's default
/// `YYYY-MM-DD HH:MM:SS.mmm±HH` is kept (`time_format: None`). The parent
/// directory is created here because ftlog's appender does not create it.
pub(crate) fn log_sink() -> Result<LogSink, String> {
    let path = log_file_path()?;
    if let Some(parent) = path.parent() {
        fs_err::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    let appender = ftlog::appender::FileAppender::builder()
        .path(&path)
        .rotate(ftlog::appender::Period::Day)
        .expire(ftlog::appender::Duration::days(7))
        .build();
    Ok(LogSink {
        root: Box::new(appender),
        time_format: None,
    })
}

/// The log subdirectory under [`local_app_data_dir`] (Windows).
#[cfg(windows)]
const LOG_DIR_NAME: &str = "logs";

/// The log directory on the file-logging platforms (macOS, Windows).
///
/// Linux logs to stderr (the journal forwards it), so this only exists on
/// the file platforms.  Windows uses `%LOCALAPPDATA%\<APP_DIR_NAME>\logs`,
/// sharing its `<APP_DIR_NAME>` root with the control-pipe publish file
/// via [`local_app_data_dir`]; macOS uses `~/Library/Logs/<APP_DIR_NAME>`.
/// Returns a descriptive error when the OS base directory cannot be
/// resolved.
///
/// [`local_app_data_dir`]: crate::platform::windows::local_app_data_dir
fn log_dir() -> Result<PathBuf, String> {
    #[cfg(windows)]
    {
        crate::platform::windows::local_app_data_dir()
            .map(|dir| dir.join(LOG_DIR_NAME))
            .ok_or_else(|| {
                "no local data directory (LOCALAPPDATA) available".to_string()
            })
    }
    #[cfg(not(windows))]
    {
        // macOS: ~/Library/Logs/<APP_DIR_NAME>
        use crate::common::paths::APP_DIR_NAME;

        dirs::home_dir()
            .map(|home| home.join("Library").join("Logs").join(APP_DIR_NAME))
            .ok_or_else(|| "no home directory available".to_string())
    }
}

/// The log file path on the file-logging platforms (macOS, Windows).
fn log_file_path() -> Result<PathBuf, String> {
    // The directory layout (including the standardized `keymapperd` name)
    // is owned by `log_dir` above; only the per-process file name is
    // decided here.
    let dir = log_dir()?;
    // Name the file after the running process so keymapperd and virtkbdd log
    // to separate files.  Fall back to the historical name when the executable
    // path cannot be resolved.
    let file_name = std::env::current_exe()
        .ok()
        .and_then(|exe| {
            exe.file_stem()
                .map(|stem| format!("{}.log", stem.to_string_lossy()))
        })
        .unwrap_or_else(|| "keymapperd.log".to_string());
    Ok(dir.join(file_name))
}
