// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

use std::{env, path::PathBuf, process::Command};

/// Path to the compiled keymapper binary.
fn bin_path() -> PathBuf {
    env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("keymapper")
}

/// Write *content* as a `config.yaml` in a unique temp directory, return the
/// directory path.  The caller must delete it (or let the process exit).
fn write_config_dir(label: &str, content: &str) -> PathBuf {
    let dir = env::temp_dir().join(format!("keymapper_test_{}", label));
    std::fs::create_dir_all(&dir).expect("failed to create temp dir");
    let config_path = dir.join("config.yaml");
    std::fs::write(&config_path, content).expect("failed to write config");
    dir
}

/// Run `keymapper config <subcommand>` in the given directory and return
/// stdout as a string.  The process is expected to succeed (exit code 0).
fn run_check_in_dir(dir: &PathBuf) -> String {
    let output = Command::new(bin_path())
        .args(["config", "check"])
        .current_dir(dir)
        .output()
        .expect("failed to run keymapper");

    assert!(
        output.status.success(),
        "keymapper exited with {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Run `keymapper config <subcommand>` in the given directory and return
/// stderr.  The process is expected to fail (non-zero exit code).
fn run_check_fails_in_dir(dir: &PathBuf) -> String {
    let output = Command::new(bin_path())
        .args(["config", "check"])
        .current_dir(dir)
        .output()
        .expect("failed to run keymapper");

    assert!(
        !output.status.success(),
        "keymapper should have failed but exited successfully"
    );

    String::from_utf8_lossy(&output.stderr).into_owned()
}

// ---------------------------------------------------------------------------
// Clean configs
// ---------------------------------------------------------------------------

#[test]
fn check_clean_config() {
    let dir = write_config_dir(
        "clean",
        r#"
- mappings:
    CapsLock: LeftControl
"#,
    );
    let out = run_check_in_dir(&dir);
    assert!(out.contains("no issues found"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn check_chord_mapping_clean() {
    let dir = write_config_dir(
        "chord",
        r#"
- mappings:
    Ctrl+H: LeftArrow
    Ctrl+Shift+Left: Cmd+Left
"#,
    );
    let out = run_check_in_dir(&dir);
    assert!(out.contains("no issues found"));
    std::fs::remove_dir_all(&dir).ok();
}

// ---------------------------------------------------------------------------
// Empty / missing configs
// ---------------------------------------------------------------------------

#[test]
fn check_empty_config() {
    let dir = write_config_dir("empty", "groups: []");
    let out = run_check_in_dir(&dir);
    assert!(out.contains("no rule groups defined"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn check_no_config_file() {
    let dir = env::temp_dir().join("keymapper_test_no_config");
    std::fs::create_dir_all(&dir).ok();

    let stderr = run_check_fails_in_dir(&dir);
    assert!(stderr.contains("No configuration file found"));

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn check_invalid_yaml() {
    let dir = write_config_dir("invalid", "::: bad yaml content [");
    let stderr = run_check_fails_in_dir(&dir);
    assert!(stderr.contains("Failed to parse"));
    std::fs::remove_dir_all(&dir).ok();
}

// ---------------------------------------------------------------------------
// No-op detection
// ---------------------------------------------------------------------------

#[test]
fn check_no_op_mapping() {
    let dir = write_config_dir(
        "noop",
        r#"
- mappings:
    A: A
"#,
    );
    let out = run_check_in_dir(&dir);
    assert!(out.contains("A remaps to itself (no-op)"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn check_no_op_chord() {
    // A chord that maps to itself.
    let dir = write_config_dir(
        "noop_chord",
        r#"
- mappings:
    Ctrl+H: Ctrl+H
"#,
    );
    let out = run_check_in_dir(&dir);
    assert!(out.contains("remaps to itself (no-op)"));
    std::fs::remove_dir_all(&dir).ok();
}

// ---------------------------------------------------------------------------
// Duplicate trigger detection
// ---------------------------------------------------------------------------

#[test]
fn check_duplicate_trigger_across_groups() {
    let dir = write_config_dir(
        "duplicate",
        r#"
- mappings:
    CapsLock: LeftControl

- name: "override"
  mappings:
    CapsLock: Tab
"#,
    );
    let out = run_check_in_dir(&dir);
    assert!(out.contains("CapsLock appears in multiple groups"));
    std::fs::remove_dir_all(&dir).ok();
}

// ---------------------------------------------------------------------------
// Empty group detection
// ---------------------------------------------------------------------------

#[test]
fn check_empty_group() {
    let dir = write_config_dir(
        "empty_group",
        r#"
- name: "placeholder"
"#,
    );
    let out = run_check_in_dir(&dir);
    assert!(out.contains("has no mappings"));
    std::fs::remove_dir_all(&dir).ok();
}

// ---------------------------------------------------------------------------
// Circular pair detection
// ---------------------------------------------------------------------------

#[test]
fn check_circular_swap() {
    let dir = write_config_dir(
        "swap",
        r#"
- mappings:
    CapsLock: LeftControl
    LeftControl: CapsLock
"#,
    );
    let out = run_check_in_dir(&dir);
    assert!(out.contains("form a circular pair (swap)"));
    std::fs::remove_dir_all(&dir).ok();
}

// ---------------------------------------------------------------------------
// Multiple diagnostics
// ---------------------------------------------------------------------------

#[test]
fn check_multiple_issues() {
    let dir = write_config_dir(
        "multiple",
        r#"
- name: "empty"

- name: "duplicates"
  mappings:
    CapsLock: A
    Tab: Tab

- name: "more"
  mappings:
    CapsLock: LeftControl
    LeftControl: CapsLock
"#,
    );
    let out = run_check_in_dir(&dir);

    assert!(out.contains("has no mappings"));
    assert!(out.contains("remaps to itself (no-op)"));
    assert!(out.contains("CapsLock appears in multiple groups"));
    assert!(out.contains("form a circular pair (swap)"));
    std::fs::remove_dir_all(&dir).ok();
}

// ---------------------------------------------------------------------------
// config list subcommand
// ---------------------------------------------------------------------------

#[test]
fn config_list_prints_content() {
    let content = r#"
- mappings:
    CapsLock: LeftControl
"#;
    let dir = write_config_dir("list_test", content.trim());
    let output = Command::new(bin_path())
        .args(["config", "list"])
        .current_dir(&dir)
        .output()
        .expect("failed to run keymapper");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("CapsLock"));
    assert!(stdout.contains("LeftControl"));

    std::fs::remove_dir_all(&dir).ok();
}

// ---------------------------------------------------------------------------
// Usage / error handling
// ---------------------------------------------------------------------------

#[test]
fn no_args_shows_usage() {
    let output = Command::new(bin_path())
        .output()
        .expect("failed to run keymapper");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Usage: keymapper config"));
}

#[test]
fn unknown_subcommand_shows_error() {
    let output = Command::new(bin_path())
        .args(["config", "foo"])
        .output()
        .expect("failed to run keymapper");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Unknown subcommand"));
}

#[test]
fn check_invalid_key_names() {
    let dir = write_config_dir(
        "bad_keys",
        r#"
- mappings:
    NoSuchKey: CapsLock
"#,
    );
    let stderr = run_check_fails_in_dir(&dir);
    assert!(stderr.contains("Failed to parse"));
    std::fs::remove_dir_all(&dir).ok();
}
