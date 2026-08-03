// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Parses `.desktop` files and provides a cached lookup from executable name
//! to application id (the file stem without `.desktop`).
//!
//! Only the `[Desktop Entry]` main group is parsed, and only the keys we need:
//! `Type`, `Exec`, `Hidden`, and `NoDisplay`.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

/// Directories that contain system-wide and user `.desktop` files.
const DESKTOP_DIRS: &[&str] =
    &["~/.local/share/applications", "/usr/share/applications"];

/// Cached map from executable name (e.g., `"firefox"`) to app id
/// (e.g., `"org.mozilla.firefox"`).  Populated on first access.
static DESKTOP_CACHE: LazyLock<HashMap<String, String>> =
    LazyLock::new(build_cache);

/// Resolve a binary name to its `.desktop` app id.
pub fn resolve_app_id(exe: &str) -> Option<String> {
    DESKTOP_CACHE.get(exe).cloned()
}

/// Build the full lookup map by scanning all known desktop directories.
fn build_cache() -> HashMap<String, String> {
    let mut cache = HashMap::new();

    for dir in expanded_dirs() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
                continue;
            }
            if let Some((exe, app_id)) = parse_desktop_file(&path) {
                // Later entries (user dir scanned first via XDG convention)
                // overwrite earlier ones, so user overrides work.
                cache.insert(exe, app_id);
            }
        }
    }
    cache
}

/// Expand tilde in directory paths and filter to existing directories.
fn expanded_dirs() -> Vec<PathBuf> {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));

    DESKTOP_DIRS
        .iter()
        .map(|dir| {
            let path = PathBuf::from(*dir);
            if path.starts_with("~") {
                let relative = path.strip_prefix("~").unwrap_or(&path);
                home.join(relative)
            } else {
                path
            }
        })
        .filter(|p| p.is_dir())
        .collect()
}

/// Parse a `.desktop` file and return the (executable, app_id) pair if it is
/// a visible application entry.
fn parse_desktop_file(path: &Path) -> Option<(String, String)> {
    // app_id is the file stem (filename without .desktop).
    let app_id = path.file_stem().and_then(|s| s.to_str())?.to_string();
    if app_id.is_empty() {
        return None;
    }

    let content = fs::read_to_string(path).ok()?;

    // Parse only the main [Desktop Entry] group.
    let mut in_main_group = false;
    let mut exe: Option<String> = None;
    let mut is_application = true; // Type defaults to Application per spec
    let mut hidden = false;
    let mut no_display = false;

    for line in content.lines() {
        let line = line.trim();

        // Skip empty lines and comments.
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // Detect group headers.
        if line.starts_with('[') {
            in_main_group = line == "[Desktop Entry]";
            continue;
        }

        if !in_main_group {
            break; // We only care about the main group.
        }

        let Some((key, value)) = split_key_value(line) else {
            continue;
        };

        match key {
            "Type" => is_application = value.trim() == "Application",
            "Hidden" => hidden = value.trim().to_lowercase() == "true",
            "NoDisplay" => no_display = value.trim().to_lowercase() == "true",
            "Exec" => exe = parse_exec_value(value),
            _ => {}
        }
    }

    // Skip non-applications, hidden entries, or entries without an Exec key.
    if !is_application || hidden || no_display {
        return None;
    }

    let exe = exe?;
    Some((exe, app_id))
}

/// Split a "Key=Value" line into (key, value), trimming whitespace.
fn split_key_value(line: &str) -> Option<(&str, &str)> {
    let pos = line.find('=')?;
    Some((&line[..pos], &line[pos + 1..]))
}

/// Parse the `Exec=` value and extract the executable name.
///
/// The Exec value may contain:
/// - An absolute or relative path (possibly quoted).
/// - `--flag` arguments before the executable (some desktop files put flags first).
/// - `%` specifiers (`%f`, `%u`, `%F`, `%U`, etc.).
fn parse_exec_value(value: &str) -> Option<String> {
    let value = value.trim();

    // Tokenize by whitespace, handling quoted strings.
    let tokens: Vec<String> = tokenize_exec(value);

    // Find the first token that is NOT a flag or %specifier.
    let exe_token = tokens
        .iter()
        .find(|t| !t.starts_with('-') && !t.starts_with('%'))?;

    // Strip surrounding quotes from the token.
    let exe_token = exe_token.trim_matches('"');

    // Extract the file stem (basename without extension).
    Path::new(exe_token)
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
}

/// Split an Exec value into tokens, respecting quoted strings.
fn tokenize_exec(value: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;

    for ch in value.chars() {
        match ch {
            '"' => in_quotes = !in_quotes,
            c if (c == ' ' || c == '\t' || c == '\\') && !in_quotes => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }

    if !current.is_empty() {
        tokens.push(current);
    }

    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_exec_simple() {
        assert_eq!(parse_exec_value("firefox %u"), Some("firefox".into()));
    }

    #[test]
    fn parse_exec_absolute_path() {
        assert_eq!(
            parse_exec_value("/usr/bin/firefox %u"),
            Some("firefox".into())
        );
    }

    #[test]
    fn parse_exec_with_flags() {
        assert_eq!(
            parse_exec_value("--no-sandbox /opt/slack/slack %U"),
            Some("slack".into())
        );
    }

    #[test]
    fn parse_exec_quoted_path() {
        assert_eq!(
            parse_exec_value("\"/usr/bin/my app\" %f"),
            Some("my app".into())
        );
    }

    #[test]
    fn parse_exec_no_args() {
        assert_eq!(parse_exec_value("code-oss"), Some("code-oss".into()));
    }

    #[test]
    fn parse_exec_percent_only() {
        assert_eq!(parse_exec_value("firefox %F %U"), Some("firefox".into()));
    }

    #[test]
    fn parse_exec_empty() {
        assert_eq!(parse_exec_value(""), None);
    }

    #[test]
    fn parse_exec_only_specifiers() {
        assert_eq!(parse_exec_value("%f %u"), None);
    }
}
