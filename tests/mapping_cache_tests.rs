// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

use std::env;

use keymapper::daemon::mapping_cache::RuntimeLookupCache;

/// Write *content* to a temp file, return its path.  Each invocation gets
// a unique filename keyed by *label* to avoid races when tests run in
// parallel.  The caller must delete the file (or let the process exit).
fn write_temp_config(label: &str, content: &str) -> String {
    let dir = env::temp_dir();
    let path = dir.join(format!("keymapperd_test_{}.yaml", label));
    std::fs::write(&path, content).expect("failed to write temp config");
    path.to_string_lossy().into_owned()
}

#[test]
fn compile_from_path_valid_file() {
    let yaml = r#"
- mappings:
    CapsLock: LeftControl
"#;
    let path = write_temp_config("valid", yaml);
    let result = RuntimeLookupCache::compile_from_path(&path);
    std::fs::remove_file(&path).ok();

    assert!(result.is_ok());
}

#[test]
fn compile_from_path_nonexistent_file() {
    let result =
        RuntimeLookupCache::compile_from_path("/nonexistent/path/config.yaml");
    assert!(result.is_err());
}

#[test]
fn compile_from_path_invalid_yaml() {
    let yaml = ":::\n  - invalid: [yaml: content:";
    let path = write_temp_config("invalid_yaml", yaml);
    let result = RuntimeLookupCache::compile_from_path(&path);
    std::fs::remove_file(&path).ok();

    assert!(result.is_err());
}

#[test]
fn compile_from_path_invalid_keys() {
    let yaml = r#"
- mappings:
    BadKeyThatDoesNotExist: CapsLock
"#;
    let path = write_temp_config("invalid_keys", yaml);
    let result = RuntimeLookupCache::compile_from_path(&path);
    std::fs::remove_file(&path).ok();

    assert!(result.is_err());
}

#[test]
fn compile_from_path_empty_file() {
    // An empty file is valid YAML but not a valid AppConfig (no sequence).
    let path = write_temp_config("empty", "");
    let result = RuntimeLookupCache::compile_from_path(&path);
    std::fs::remove_file(&path).ok();

    assert!(result.is_err());
}

#[test]
fn compile_from_path_utf8_content() {
    // Config with Unicode in the group name — should parse fine.
    let yaml = r#"
- name: "тест / 测试 / 🚀"
  mappings:
    CapsLock: LeftControl
"#;
    let path = write_temp_config("utf8", yaml);
    let result = RuntimeLookupCache::compile_from_path(&path);
    std::fs::remove_file(&path).ok();

    assert!(result.is_ok());
}
