# Homebrew cask for the Karabiner DriverKit VirtualHIDDevice package (pqrs).
#
# keymapper's macOS output path emits remapped keys through this driver.
# The cask installs the package only; activating the DriverKit extension and
# registering the daemon LaunchDaemon require root, so the keymapper formula
# does that via scripts/install-karabiner-macos.sh.
#
# Keep the version and checksum in sync with scripts/install-karabiner-macos.sh
# and scripts/package-macos.sh.

class KarabinerDriverkitVirtualhiddevice < Cask
    desc "DriverKit virtual HID device driver (used by keymapper)"
    homepage "https://github.com/pqrs-org/Karabiner-DriverKit-VirtualHIDDevice"

    version "8.2.0"
    sha256 "7faf4c33046c2274726da9e29da795fb2d2ad81796557db0fcc1686c611eeafc"

    url "https://github.com/pqrs-org/Karabiner-DriverKit-VirtualHIDDevice/releases/download/v#{version}/Karabiner-DriverKit-VirtualHIDDevice-#{version}.pkg"
    name "Karabiner DriverKit VirtualHIDDevice"

    pkg "Karabiner-DriverKit-VirtualHIDDevice-#{version}.pkg"

    uninstall script: {
        sudo: true,
        executable: "/bin/bash",
        args: [
            "-c",
            <<~EOS
                /Applications/.Karabiner-VirtualHIDDevice-Manager.app/Contents/MacOS/Karabiner-VirtualHIDDevice-Manager deactivate || true
                rm -rf "/Applications/.Karabiner-VirtualHIDDevice-Manager.app"
                rm -rf "/Library/Application Support/org.pqrs/Karabiner-DriverKit-VirtualHIDDevice"
            EOS
        ],
    }

    caveats do
        <<~EOS
            The DriverKit extension must be enabled once in:
            System Settings > General > Login Items & Extensions > Driver Extensions.

            The keymapper formula activates the extension and registers the
            daemon LaunchDaemon during installation.
        EOS
    end
end
