# ---------------------------------------------------------------------------
# Builds and installs the KeyMapperVirtualHID DriverKit extension for local
# development.  Uses ad-hoc signing (no certificate required).
#
# Targets:
#   make build     — Debug build, active architecture only (default)
#   make release   — Release build, all architectures
#   make ci        — CI build (no code signing) and install
#   make install   — Build and copy the .kext to ~/Library/Extensions/
#   make clean     — Remove build artifacts
# ---------------------------------------------------------------------------

XCODE_PROJ := KeyMapperVirtualHID.xcodeproj
SCHEME     := KeyMapperVirtualHID
KEXT_NAME  := KeyMapperVirtualHID.kext

EXTENSIONS_DIR := $(HOME)/Library/Extensions

# Default to ad-hoc signing for local development.  Override with
# CODE_SIGN_IDENTITY= for distribution builds, or use the 'ci' target
# which disables signing entirely (DriverKit 25.5 rejects ad-hoc signing).
ifdef CODE_SIGN_IDENTITY
  SIGN_FLAGS := \
		CODE_SIGN_IDENTITY="$(CODE_SIGN_IDENTITY)" \
		CODE_SIGN_ENTITLEMENTS=KeyMapperVirtualHID/KeyMapperVirtualHID.entitlements \
		CODE_SIGNING_REQUIRED=YES \
		CODE_SIGNING_ALLOWED=YES
else
  SIGN_FLAGS := \
		CODE_SIGN_IDENTITY=- \
		CODE_SIGN_ENTITLEMENTS=KeyMapperVirtualHID/KeyMapperVirtualHID.entitlements \
		CODE_SIGNING_REQUIRED=YES \
		CODE_SIGNING_ALLOWED=YES
endif

NOSIGN_FLAGS := \
		CODE_SIGNING_REQUIRED=NO \
		CODE_SIGNING_ALLOWED=NO \
		CODE_SIGN_IDENTITY=""

COMMON_FLAGS ?= $(SIGN_FLAGS)

.PHONY: build release ci install clean

build:
	xcodebuild -project $(XCODE_PROJ) \
	  -scheme $(SCHEME) \
	  -configuration Debug \
	  ONLY_ACTIVE_ARCH=YES \
	  $(COMMON_FLAGS)

release:
	xcodebuild -project $(XCODE_PROJ) \
	  -scheme $(SCHEME) \
	  -configuration Release \
	  ARCHS="arm64 x86_64" \
	  ONLY_ACTIVE_ARCH=NO \
	  $(COMMON_FLAGS)

ci:
	xcodebuild -project $(XCODE_PROJ) \
	  -scheme $(SCHEME) \
	  -configuration Release \
	  ARCHS="arm64 x86_64" \
	  ONLY_ACTIVE_ARCH=NO \
	  $(NOSIGN_FLAGS)

# Find the latest built .kext by scanning both local DerivedData and the
# system-wide Xcode DerivedData directory.  This avoids hardcoding the
# xcodebuild output path, which varies depending on SDK version and host.
find_kext = \
  find . ~/Library/Developer/Xcode/DerivedData \
    -maxdepth 10 -name "$(KEXT_NAME)" -type d 2>/dev/null | head -1

install: release
	@KEXT=$$($(find_kext)); \
	if [ -z "$$KEXT" ]; then echo "error: built $(KEXT_NAME) not found" >&2; exit 1; fi; \
	mkdir -p "$(EXTENSIONS_DIR)"; \
	cp -R "$$KEXT" "$(EXTENSIONS_DIR)/"; \
	echo ""; \
	echo "Driver installed to $(EXTENSIONS_DIR)/$(KEXT_NAME)"; \
	echo "Approve it in System Settings > Privacy & Security on first run."

install-ci: ci
	@KEXT=$$($(find_kext)); \
	if [ -z "$$KEXT" ]; then echo "error: built $(KEXT_NAME) not found" >&2; exit 1; fi; \
	mkdir -p "$(EXTENSIONS_DIR)"; \
	cp -R "$$KEXT" "$(EXTENSIONS_DIR)/"; \
	echo ""; \
	echo "Driver installed to $(EXTENSIONS_DIR)/$(KEXT_NAME)"

clean:
	xcodebuild -project $(XCODE_PROJ) \
	  -scheme $(SCHEME) \
	  clean
	@echo "Clean complete."
