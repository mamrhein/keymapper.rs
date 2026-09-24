# Homebrew cask for keymapper (precompiled binaries).
#
# Installs the precompiled release binaries for the current architecture, so
# no Rust toolchain is required.  On macOS, remapped keys are emitted through
# the Karabiner DriverKit VirtualHIDDevice driver; the installer script
# installs that pinned driver package, registers the virtkbdd LaunchDaemon
# (root, emits mapped keys) and the keymapperd LaunchAgent (user domain,
# captures keyboard events), activates the DriverKit extension, and registers
# the Karabiner daemon LaunchDaemon.  The services are managed by launchd and
# controlled with `keymapper daemon status|start|stop` (not by `brew
# services`).
#
# Install:
#   brew install --cask mamrhein/keymapper/keymapper-bin

cask "keymapper-bin" do
  arch arm: "aarch64-apple-darwin", intel: "x86_64-apple-darwin"

  version "0.2.0-alpha.4"
  # Replaced by the homebrew-tap workflow with the release asset checksums.
  sha256 arm:   "0000000000000000000000000000000000000000000000000000000000000000",
         intel: "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"

  # The url interpolates the version, so only the version and checksums need
  # updating on a release.
  url "https://github.com/mamrhein/keymapper.rs/releases/download/v#{version}/keymapper-v#{version}-#{arch}.tar.xz"
  desc "Cross-platform key-remapping daemon (precompiled binaries)"
  homepage "https://github.com/mamrhein/keymapper.rs"

  # The release archive extracts to dist/keymapper/v<version>/ in the staging
  # directory; all paths below are relative to it.  The installer runs before
  # the binaries are linked, so it takes the staged daemon paths as arguments.
  # It registers the virtkbdd LaunchDaemon and the keymapperd LaunchAgent,
  # installs the Karabiner DriverKit package, activates the extension, and
  # registers the Karabiner daemon LaunchDaemon.  Requires sudo.  The script
  # runs from the staging directory so it can find its sibling scripts and
  # the launchd plist templates.
  installer script: {
    executable: "dist/keymapper/v#{version}/install-macos.sh",
    args:       ["#{staged_path}/dist/keymapper/v#{version}/keymapperd",
                 "#{staged_path}/dist/keymapper/v#{version}/virtkbdd"],
    sudo:       true,
  }
  # Only the CLI is linked into Homebrew's bin — the daemons are installed by
  # the script to their canonical locations (/Library/Application Support/
  # keymapper/virtkbdd and ~/.local/bin/keymapperd).  In particular, virtkbdd
  # is kept out of /usr/local/bin, which is admin-writable on Intel Macs and
  # would allow replacing the root-run daemon binary.
  binary "dist/keymapper/v#{version}/keymapper"

  # Runs before the staged files are removed, so the script (and its sibling
  # uninstall-karabiner-macos.sh) is still available.  Requires sudo.
  uninstall script: {
    executable: "dist/keymapper/v#{version}/uninstall-macos.sh",
    sudo:       true,
  }

  caveats do
    <<~EOS
      On first run, the Karabiner DriverKit extension may need to be
      enabled once in:
      System Settings > General > Login Items & Extensions > Driver Extensions.

      No reboot is required.

      keymapperd also needs the Input Monitoring and Accessibility
      permissions (System Settings > Privacy & Security).

      The keymapperd and virtkbdd services are managed by launchd; control
      them with:
        keymapper daemon status | start | stop
    EOS
  end
end
