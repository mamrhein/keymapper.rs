# Homebrew formula for keymapper.
#
# Builds the Rust crate from source.  On macOS, remapped keys are emitted
# through the Karabiner DriverKit VirtualHIDDevice driver (installed via the
# karabiner-driverkit-virtualhiddevice cask dependency); the formula then
# activates the driver and registers its daemon LaunchDaemon.
#
# Install locally (not yet in a tap):
#   brew install --build-from-source path/to/brew/keymapper.rb

class Keymapper < Formula
    desc "Cross-platform key-remapping daemon"
    homepage "https://github.com/mamrhein/keymapper.rs"

    # Update url and version for each release.
    url "https://github.com/mamrhein/keymapper.rs/archive/refs/tags/v0.1.0.tar.gz"
    version "0.1.0"

    license "SEE LICENSE IN LICENSE.TXT"

    depends_on "rust" => :build

    on_macos do
        # The driver through which keymapperd emits remapped keys.  The cask
        # installs the package only; activation happens in install below.
        depends_on "mamrhein/keymapper/karabiner-driverkit-virtualhiddevice"
    end

    def install
        # Build and install both Rust binaries (keymapper, keymapperd).
        system "cargo", "install", "--root", prefix, "--locked"

        on_macos do
            # Install the Karabiner DriverKit package (if not already
            # installed), activate the extension, and register the daemon
            # LaunchDaemon.  Requires sudo.
            system "sudo", "scripts/install-karabiner-macos.sh"
        end
    end

    def caveats
        <<~EOS
            On first run, the Karabiner DriverKit extension may need to be
            enabled once in:
            System Settings > General > Login Items & Extensions > Driver Extensions.

            No reboot is required.
        EOS
    end

    service do
        run [opt_bin / "keymapperd"]
        keep_alive true
        sudo true  # LaunchDaemon is required for IOKit device seizure.
    end
end
