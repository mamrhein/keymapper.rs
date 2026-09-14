# Homebrew formula for keymapper.
#
# Builds the Rust crate from source.  On macOS, remapped keys are emitted
# through the Karabiner DriverKit VirtualHIDDevice driver (installed via the
# karabiner-driverkit-virtualhiddevice cask dependency); the formula then
# registers the virtkbdd LaunchDaemon (root, emits mapped keys) and the
# keymapperd LaunchAgent (user domain, captures keyboard events), activates
# the DriverKit extension, and registers the Karabiner daemon LaunchDaemon.
# On Linux, it registers the keymapperd systemd user service.  The services
# are managed by launchd / systemctl --user and controlled with
# `keymapper daemon status|start|stop` (not by `brew services`).
#
# Install:
#   brew install mamrhein/keymapper/keymapper

class Keymapper < Formula
  desc "Cross-platform key-remapping daemon"
  homepage "https://github.com/mamrhein/keymapper.rs"

  # Update version for each release (the url interpolates it).
  version "0.2.0-alpha.4"
  url "https://github.com/mamrhein/keymapper.rs/archive/refs/tags/v#{version}.tar.gz"

  license "BSD-3-Clause"

  depends_on "rust" => :build

  on_macos do
    # The driver through which virtkbdd emits remapped keys.  The cask
    # installs the package only; activation happens in install below.
    depends_on "mamrhein/keymapper/karabiner-driverkit-virtualhiddevice"
  end

  def install
    # Build and install all Rust binaries (keymapper, keymapperd, virtkbdd,
    # keymapper_reader).
    system "cargo", "install", "--path", ".", "--root", prefix, "--locked"

    # Keep the uninstall scripts in the prefix so `brew uninstall` can stop
    # and remove the services registered below.
    on_macos do
      libexec.install "scripts/uninstall-macos.sh", "scripts/uninstall-karabiner-macos.sh"
    end
    on_linux do
      libexec.install "scripts/uninstall-linux.sh"
    end

    on_macos do
      # Register the virtkbdd LaunchDaemon and the keymapperd LaunchAgent,
      # install the Karabiner DriverKit package (if not already installed by
      # the cask), activate the extension, and register the Karabiner daemon
      # LaunchDaemon.  Requires sudo.
      system "sudo", "scripts/install-macos.sh",
        prefix/"bin/keymapperd", prefix/"bin/virtkbdd"
    end

    on_linux do
      # Register the keymapperd systemd user service (no root required).
      system "scripts/install-linux.sh", prefix/"bin/keymapperd"
    end
  end

  def uninstall
    on_macos do
      system "sudo", libexec/"uninstall-macos.sh" if (libexec/"uninstall-macos.sh").exist?
    end
    on_linux do
      system libexec/"uninstall-linux.sh" if (libexec/"uninstall-linux.sh").exist?
    end
  end

  def caveats
    <<~EOS
      On first run, the Karabiner DriverKit extension may need to be
      enabled once in:
      System Settings > General > Login Items & Extensions > Driver Extensions.

      No reboot is required.

      keymapperd also needs the Input Monitoring and Accessibility
      permissions (System Settings > Privacy & Security).

      The keymapperd and virtkbdd services are managed by launchd (macOS)
      or the systemd user session (Linux); control them with:
        keymapper daemon status | start | stop
    EOS
  end
end
