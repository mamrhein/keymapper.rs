# Homebrew formula for keymapper.
#
# Builds the Rust crate and the DriverKit virtual HID extension from source.
# The driver is installed to ~/Library/Extensions/ and discovered at runtime
# via IOKit.
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
        depends_on "xcode" => :build
    end

    def install
        # Build and install both Rust binaries (keymapper, keymapperd).
        system "cargo", "install", "--root", prefix, "--locked"

        on_macos do
            # Build the DriverKit driver with ad-hoc signing.
            system "xcodebuild", \
                    "-project", "driver/KeyMapperVirtualHID.xcodeproj",
                    "-scheme", "KeyMapperVirtualHID",
                    "-configuration", "Release",
                    "-derivedDataPath", "driver-build",
                    "CODE_SIGN_IDENTITY=-",
                    "CODE_SIGN_ENTITLEMENTS=driver/KeyMapperVirtualHID/KeyMapperVirtualHID.entitlements",
                    "CODE_SIGNING_REQUIRED=YES",
                    "CODE_SIGNING_ALLOWED=YES"

            # Install the driver bundle to the user Extensions directory so
            # macOS loads it automatically.
            kext_dir = HOMEBREW_USER_EXTENSIONS
            kext_dir.mkpath
            cp_r "driver-build/Build/Products/Release/KeyMapperVirtualHID.kext",
                 kext_dir
        end
    end

    def caveats
        <<~EOS
            The KeyMapperVirtualHID driver was installed to
            #{HOMEBREW_USER_EXTENSIONS}.

            On first run, approve the driver in
            System Settings > Privacy & Security.
        EOS
    end

    service do
        run [opt_bin / "keymapperd"]
        keep_alive true
    end
end
