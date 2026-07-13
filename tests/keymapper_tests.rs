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
    assert!(stderr.contains("failed to parse"));
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
    // clap prints "Usage: keymapper" with the command list.
    assert!(stderr.contains("Usage: keymapper"));
}

#[test]
fn unknown_subcommand_shows_error() {
    let output = Command::new(bin_path())
        .args(["config", "foo"])
        .output()
        .expect("failed to run keymapper");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    // clap reports unknown subcommands with "error:".
    assert!(stderr.starts_with("error:"));
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
    assert!(stderr.contains("failed to parse"));
    std::fs::remove_dir_all(&dir).ok();
}

// ---------------------------------------------------------------------------
// config create subcommand
// ---------------------------------------------------------------------------

#[test]
fn config_creates_empty_file() {
    let dir = env::temp_dir().join("keymapper_test_create");
    std::fs::create_dir_all(&dir).expect("failed to create temp dir");

    // Use the CWD-based config path so we can control the location.
    let output = Command::new(bin_path())
        .args(["config", "add", "CapsLock", "LeftControl"])
        .current_dir(&dir)
        .output()
        .expect("failed to run keymapper");

    assert!(
        output.status.success(),
        "keymapper exited with {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Added"));

    // Verify the file was created and contains the mapping.
    let config_path = dir.join("config.yaml");
    assert!(config_path.is_file());
    let contents = std::fs::read_to_string(&config_path).unwrap();
    assert!(contents.contains("CapsLock"));
    assert!(contents.contains("LeftControl"));

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn config_add_to_existing_file() {
    let dir = write_config_dir(
        "add_existing",
        r#"
- name: "my rules"
  mappings:
    CapsLock: LeftControl
"#,
    );

    // Add a second mapping to the same group.
    let output = Command::new(bin_path())
        .args(["config", "add", "--group", "my rules", "Tab", "Backspace"])
        .current_dir(&dir)
        .output()
        .expect("failed to run keymapper");

    assert!(
        output.status.success(),
        "keymapper exited with {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Added"));

    // Verify the file contains both mappings.
    let contents = std::fs::read_to_string(dir.join("config.yaml")).unwrap();
    assert!(contents.contains("CapsLock"));
    assert!(contents.contains("Tab"));
    assert!(contents.contains("Backspace"));

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn config_add_creates_new_group() {
    let dir = write_config_dir(
        "add_new_group",
        r#"
- name: "existing"
  mappings:
    CapsLock: LeftControl
"#,
    );

    // Add a mapping to a new group.
    let output = Command::new(bin_path())
        .args([
            "config",
            "add",
            "--group",
            "new group",
            "Ctrl+H",
            "LeftArrow",
        ])
        .current_dir(&dir)
        .output()
        .expect("failed to run keymapper");

    assert!(
        output.status.success(),
        "keymapper exited with {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    // Verify the file contains both groups.
    let contents = std::fs::read_to_string(dir.join("config.yaml")).unwrap();
    assert!(contents.contains("existing"));
    assert!(contents.contains("new group"));
    // "Ctrl" normalizes to "LeftControl" on all platforms.
    assert!(contents.contains("LeftControl+H"));
    assert!(contents.contains("LeftArrow"));

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn config_add_invalid_trigger_fails() {
    let dir = write_config_dir(
        "add_bad_trigger",
        r#"
- name: "test"
  mappings:
    CapsLock: LeftControl
"#,
    );

    let output = Command::new(bin_path())
        .args(["config", "add", "NoSuchKey", "CapsLock"])
        .current_dir(&dir)
        .output()
        .expect("failed to run keymapper");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("invalid trigger"));

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn config_add_invalid_output_fails() {
    let dir = write_config_dir(
        "add_bad_output",
        r#"
- name: "test"
  mappings:
    CapsLock: LeftControl
"#,
    );

    let output = Command::new(bin_path())
        .args(["config", "add", "CapsLock", "FakeKey"])
        .current_dir(&dir)
        .output()
        .expect("failed to run keymapper");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("invalid output"));

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn config_add_with_apps() {
    let dir = write_config_dir("add_apps", "groups: []");

    let output = Command::new(bin_path())
        .args([
            "config",
            "add",
            "--group",
            "iterm",
            "--apps",
            "iTerm2",
            "Ctrl+H",
            "LeftArrow",
        ])
        .current_dir(&dir)
        .output()
        .expect("failed to run keymapper");

    assert!(
        output.status.success(),
        "keymapper exited with {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    let contents = std::fs::read_to_string(dir.join("config.yaml")).unwrap();
    assert!(contents.contains("iTerm2"));

    std::fs::remove_dir_all(&dir).ok();
}

// ---------------------------------------------------------------------------
// server status / start subcommands
// ---------------------------------------------------------------------------

#[test]
fn server_status_not_running() {
    // keymapperd is unlikely to be running in the test environment.
    let output = Command::new(bin_path())
        .args(["server", "status"])
        .output()
        .expect("failed to run keymapper");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Either "is running" or "is not running" is valid; the important thing
    // is that the command exits successfully.
    assert!(
        stdout.contains("keymapperd"),
        "output should mention keymapperd"
    );
}

#[test]
fn server_start_not_found() {
    // keymapperd is not on PATH in the test environment, so starting it
    // should fail with a clear error message.
    let output = Command::new(bin_path())
        .args(["server", "start"])
        .output()
        .expect("failed to run keymapper");

    // The command should fail because keymapperd is not on PATH.
    assert!(
        !output.status.success(),
        "server start should fail when keymapperd is not on PATH"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("failed to start") || stderr.contains("No such file"),
        "error message: {}",
        stderr
    );
}
