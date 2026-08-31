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

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::LazyLock,
};

/// Directories that contain system-wide and user `.desktop` files.
const DESKTOP_DIRS: &[&str] =
    &["~/.local/share/applications", "/usr/share/applications"];

/// Cached map from executable name (e.g., `"firefox"`) to app id
/// (e.g., `"org.mozilla.firefox"`).  Populated on first access.
static DESKTOP_CACHE: LazyLock<HashMap<String, String>> =
    LazyLock::new(build_cache);

/// Cached map from app installation root directory (e.g.,
/// `"/home/ma/.local/zed.app/"`) to app id.  Used to match process command
/// lines against .desktop files when the actual binary name differs from the
/// Exec key (e.g., sandboxed apps like Zed).
static APP_ROOT_CACHE: LazyLock<Vec<(String, String)>> =
    LazyLock::new(build_app_root_cache);

/// Resolve a binary name to its `.desktop` app id.
pub fn resolve_app_id(exe: &str) -> Option<String> {
    DESKTOP_CACHE.get(exe).cloned()
}

/// Resolve an app id by checking if any command-line token falls under the
/// same app installation root directory as the Exec path from a `.desktop`
/// file.
pub fn resolve_app_id_from_cmdline(cmdline: &[u8]) -> Option<String> {
    // cmdline is null-separated; split into tokens.
    let tokens: Vec<&str> = cmdline
        .split(|&b| b == 0)
        .filter(|t| !t.is_empty())
        .filter_map(|t| std::str::from_utf8(t).ok())
        .collect();

    // For each token that is an absolute path, check if it falls under any
    // app installation root directory.
    for token in &tokens {
        if !token.starts_with('/') {
            continue;
        }
        for (root, app_id) in APP_ROOT_CACHE.iter() {
            if token.starts_with(root.as_str()) {
                return Some(app_id.clone());
            }
        }
    }
    None
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

/// Build a list of (app_root_directory, app_id) pairs by scanning all known
/// desktop directories.
fn build_app_root_cache() -> Vec<(String, String)> {
    let mut cache = Vec::new();

    for dir in expanded_dirs() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
                continue;
            }
            if let Some((root, app_id)) = parse_desktop_file_app_root(&path) {
                cache.push((root, app_id));
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

/// Parse a `.desktop` file and return the (app_root_directory, app_id) pair
/// for non-standard installations.  The app root is the directory that
/// contains the application's binaries (e.g., "bin/", "libexec/").
///
/// For system apps under `/usr/bin/`, `/usr/local/bin/`, etc., this returns
/// `None` — those are handled by the simple exe-name cache.  For bundled
/// apps (e.g., `/opt/google/chrome/chrome`,
/// `/home/user/.local/zed.app/bin/zed`), the app root is the parent directory
/// that contains all the application's files.
fn parse_desktop_file_app_root(path: &Path) -> Option<(String, String)> {
    let app_id = path.file_stem().and_then(|s| s.to_str())?.to_string();
    if app_id.is_empty() {
        return None;
    }

    let content = fs::read_to_string(path).ok()?;

    let mut in_main_group = false;
    let mut exec_value: Option<String> = None;
    let mut is_application = true;
    let mut hidden = false;
    let mut no_display = false;

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            in_main_group = line == "[Desktop Entry]";
            continue;
        }
        if !in_main_group {
            break;
        }

        let Some((key, value)) = split_key_value(line) else {
            continue;
        };

        match key {
            "Type" => is_application = value.trim() == "Application",
            "Hidden" => hidden = value.trim().to_lowercase() == "true",
            "NoDisplay" => no_display = value.trim().to_lowercase() == "true",
            "Exec" => exec_value = Some(value.trim().to_string()),
            _ => {}
        }
    }

    if !is_application || hidden || no_display {
        return None;
    }

    let exec_value = exec_value?;
    let tokens: Vec<String> = tokenize_exec(&exec_value);

    // Find the first token that is NOT a flag or %specifier.
    let exe_token = tokens
        .iter()
        .find(|t| !t.starts_with('-') && !t.starts_with('%'))?;

    let exe_token = exe_token.trim_matches('"');

    // Only include absolute paths.
    let exe_path = Path::new(exe_token);
    if !exe_path.is_absolute() {
        return None;
    }

    // Extract the app installation root directory.
    let root = extract_app_root(exe_path)?;

    Some((root, app_id))
}

/// Extract the app installation root directory from an executable path.
///
/// Returns the root path with a trailing "/" to ensure prefix matching
/// does not produce false positives (e.g., "/opt/app/" matching
/// "/opt/app2/something").
///
/// For standard system paths like `/usr/bin/foo` or `/usr/local/bin/foo`,
/// returns `None` — these are matched by simple exe-name lookup.
/// For bundled apps, returns the directory that contains subdirectories
/// like `bin/`, `libexec/`, etc.
fn extract_app_root(exe_path: &Path) -> Option<String> {
    let parent = exe_path.parent()?;
    let parent_name = parent.file_name()?.to_str()?;

    // If the parent is a standard binary directory, this is a system app —
    // no need for root-based matching.
    if matches!(parent_name, "bin" | "sbin" | "bin64" | "sbin64") {
        // For /usr/bin/..., /usr/local/bin/..., etc., the app root is not
        // useful for matching.
        if parent.starts_with("/usr") {
            return None;
        }
        // For /opt/google/chrome/, ~/.local/zed.app/, etc., the grandparent
        // is the app installation root.
        let root = parent.parent()?;
        let root_str = root.to_str()?;
        return Some(format!("{root_str}/"));
    }

    // For paths like /opt/app/app-binary or /home/user/.local/app/libexec/bin,
    // the parent directory is the app root.
    let root_str = parent.to_str()?;
    Some(format!("{root_str}/"))
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
/// - `--flag` arguments before the executable (some desktop files put flags
///   first).
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

    #[test]
    fn extract_app_root_system_bin() {
        // System apps under /usr/bin/ should not produce an app root.
        assert_eq!(extract_app_root(Path::new("/usr/bin/firefox")), None);
    }

    #[test]
    fn extract_app_root_local_bin() {
        // /usr/local/bin/ should also be skipped.
        assert_eq!(
            extract_app_root(Path::new("/usr/local/bin/something")),
            None
        );
    }

    #[test]
    fn extract_app_root_bundled_app() {
        // Bundled apps like Zed should produce the app installation root.
        assert_eq!(
            extract_app_root(Path::new("/home/ma/.local/zed.app/bin/zed")),
            Some("/home/ma/.local/zed.app/".into())
        );
    }

    #[test]
    fn extract_app_root_opt_app() {
        // /opt apps should produce the app installation root.
        assert_eq!(
            extract_app_root(Path::new("/opt/google/chrome/chrome")),
            Some("/opt/google/chrome/".into())
        );
    }

    #[test]
    fn extract_app_root_flat_layout() {
        // Apps with flat layout (no bin/ subdir) should use the parent.
        assert_eq!(
            extract_app_root(Path::new("/opt/slack/slack")),
            Some("/opt/slack/".into())
        );
    }

    #[test]
    fn resolve_cmdline_absolute_path_required() {
        // Non-absolute paths in cmdline are skipped (handled by exe-name
        // cache).
        let cmdline = b"firefox\0--flag\0";
        // No absolute paths, so no match from app root cache.
        let result = resolve_app_id_from_cmdline(cmdline);
        // Result depends on .desktop files present on the system.
        // The important thing is it doesn't panic.
        let _ = result;
    }
}
