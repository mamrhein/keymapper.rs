// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! macOS implementation of the `keymapper driver` commands.
//!
//! Handles building and installing the DriverKit extension from source,
//! checking its IOKit registry state, and probing socket connectivity.

use std::path::PathBuf;

use super::DriverStatus;

/// The name of the `.kext` bundle produced by the Xcode project.
const KEXT_NAME: &str = "KeyMapperVirtualHID.kext";

/// The driver class name advertised in the IOKit registry.
const DRIVER_CLASS: &str = "KeyMapperDriver";

/// Returns the local development install directory for the driver.
///
/// Resolves to `~/Library/Application Support/keymapper/`.
fn local_install_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(format!("{home}/Library/Application Support/keymapper"))
}

/// Tries to locate the Homebrew prefix by resolving the path of the running
/// binary.  If the binary is under a Homebrew `bin/` directory, it walks up
/// to find the prefix root.
///
/// Returns `None` if the binary does not appear to be installed via Homebrew.
fn try_homebrew_prefix() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;

    // Walk up the path looking for a `bin/` component, then check if its
    // parent looks like a Homebrew prefix (contains `opt/` or `Cellar/`).
    let mut current = exe.parent()?;
    loop {
        let parent = current.parent()?;
        if current.file_name().and_then(|n| n.to_str()) == Some("bin") {
            // Check if this looks like a Homebrew prefix.  Valid prefixes
            // contain `opt/` (for opt-linked formulae) or `Cellar/`.
            if parent.join("opt").is_dir() || parent.join("Cellar").is_dir() {
                return Some(parent.to_path_buf());
            }
        }
        current = parent;
        if current == parent.parent()? {
            break;
        }
    }

    None
}

/// Returns the Homebrew install path for the driver, if applicable.
///
/// Checks `<prefix>/lib/keymapper/KeyMapperVirtualHID.kext`.  Returns `None`
/// if the binary does not appear to be a Homebrew installation.
fn homebrew_install_path() -> Option<PathBuf> {
    let prefix = try_homebrew_prefix()?;
    Some(prefix.join("lib").join("keymapper").join(KEXT_NAME))
}

/// Returns the local install path for the driver.
fn local_install_path() -> PathBuf {
    local_install_dir().join(KEXT_NAME)
}

/// Checks whether the driver `.kext` exists at either known install location.
///
/// Returns the path if found, `None` otherwise.
fn find_installed_kext() -> Option<PathBuf> {
    // Check Homebrew path first (if this is a Homebrew install).
    if let Some(hp) = homebrew_install_path()
        && hp.join("Contents/Info.plist").is_file()
    {
        return Some(hp);
    }

    // Check local development path.
    let lp = local_install_path();
    if lp.join("Contents/Info.plist").is_file() {
        return Some(lp);
    }

    None
}

/// Verifies that `xcodebuild` is available on the system.  Returns an error
/// string if it is not found.
fn verify_xcodebuild() -> Result<(), String> {
    let output = std::process::Command::new("xcodebuild")
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    match output {
        Ok(status) if status.success() => Ok(()),
        Ok(_) => Err("xcodebuild returned an error. Ensure Xcode is \
                      installed and `xcode-select --install` has been run."
            .to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err("xcodebuild not found. Install Xcode Command Line Tools: \
                 `xcode-select --install`"
                .to_string())
        }
        Err(e) => Err(format!("failed to run xcodebuild: {e}")),
    }
}

/// Locate the `driver/` directory relative to the running binary.  Tries:
/// 1. The `driver/` directory next to the binary's parent (cargo dev layout).
/// 2. Walking up from the binary to find a directory containing `driver/`.
fn find_driver_dir() -> Result<PathBuf, String> {
    let exe = std::env::current_exe()
        .map_err(|e| format!("cannot resolve exe path: {e}"))?;

    // Strategy 1: Look for `driver/` alongside the project root.  In a dev
    // build the binary lives at `target/debug/keymapper`, so the project root
    // is two levels up.  In a release build it's `target/release/keymapper`.
    let project_root = exe.parent().and_then(|p| p.parent());
    if let Some(root) = project_root {
        let driver_dir = root.join("driver");
        if driver_dir.is_dir() {
            return Ok(driver_dir);
        }
    }

    // Strategy 2: Walk up from the binary looking for `driver/`.
    let mut current = exe.parent().unwrap_or(&exe);
    loop {
        let candidate = current.join("driver");
        if candidate.is_dir() {
            return Ok(candidate);
        }
        let parent = current.parent();
        if parent.is_none() || parent == Some(current) {
            break;
        }
        current = parent.unwrap();
    }

    Err(
        "driver/ directory not found. The DriverKit extension source must be \
         available alongside the binary."
            .to_string(),
    )
}

/// Resolve the project directory (parent of `driver/`).
fn find_project_dir() -> Result<PathBuf, String> {
    let driver_dir = find_driver_dir()?;
    driver_dir
        .parent()
        .map(|p| p.to_path_buf())
        .ok_or("cannot determine project directory".to_string())
}

/// Query IOKit to check if the virtual HID driver is currently loaded.
///
/// Returns `true` if a service matching the driver class name is found in the
/// IOKit registry.
fn is_driver_loaded_in_iokit() -> bool {
    // Use `ioreg` to query the IOKit registry for our driver class.  This
    // avoids linking against IOKit directly in the CLI binary (which already
    // links dynamically for the hid_socket module but keeps things cleaner).
    let output = std::process::Command::new("ioreg")
        .args(["-c", DRIVER_CLASS, "-r", "-l1"])
        .output()
        .ok();

    if let Some(output) = output
        && output.status.success()
    {
        let stdout = String::from_utf8_lossy(&output.stdout);
        if !stdout.is_empty() {
            return true;
        }
    }

    false
}

/// Attempt to connect a socket to the virtual HID driver.
///
/// Uses the `HidSocket::discover_and_open` API from the platform module.
/// Returns `true` if a socket connection succeeds.
#[cfg(feature = "driverkit")]
fn is_socket_connected() -> bool {
    use crate::platform::HidSocket;
    HidSocket::discover_and_open().is_ok()
}

#[cfg(not(feature = "driverkit"))]
fn is_socket_connected() -> bool {
    // Without the driverkit feature, socket connectivity cannot be tested.
    false
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Build and install the DriverKit extension for local development.
pub fn install() -> Result<(), String> {
    // Check if the driver is already installed via Homebrew.
    if let Some(hp) = homebrew_install_path()
        && hp.join("Contents/Info.plist").is_file()
    {
        println!("Driver is already installed via Homebrew at:");
        println!("  {}", hp.display());
        println!("Run `keymapper driver status` for details.");
        return Ok(());
    }

    // Verify xcodebuild is available.
    verify_xcodebuild()?;

    let project_dir = find_project_dir()?;
    let driver_dir = project_dir.join("driver");

    println!("Building DriverKit extension...");
    let build_status = std::process::Command::new("make")
        .current_dir(&driver_dir)
        .arg("install")
        .status()
        .map_err(|e| format!("failed to run make: {e}"))?;

    if !build_status.success() {
        return Err(format!(
            "Driver build failed (exit code {}). Check the build output \
             above for details.",
            build_status.code().unwrap_or(-1)
        ));
    }

    let install_path = local_install_path();
    if !install_path.join("Contents/Info.plist").is_file() {
        return Err("Build appeared to succeed but the .kext was not copied \
                    to the install location."
            .to_string());
    }

    println!();
    println!("Driver installed successfully.");
    println!(
        "First launch will prompt in System Settings → Privacy & Security."
    );
    println!("Run `keymapper driver status` to verify.");

    Ok(())
}

/// Query the current state of the virtual HID driver.
pub fn status() -> DriverStatus {
    let installed_path = find_installed_kext();
    let loaded_in_iokit = is_driver_loaded_in_iokit();
    let socket_connected = if loaded_in_iokit {
        is_socket_connected()
    } else {
        false
    };

    DriverStatus {
        installed: installed_path.is_some(),
        installed_path,
        loaded_in_iokit,
        socket_connected,
        signing: "ad-hoc".to_string(),
    }
}
