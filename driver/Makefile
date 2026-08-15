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

# Pin the DerivedData output so build artifacts land in Build/Products/.
DERIVED_DATA := -derivedDataPath Build

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
	  $(DERIVED_DATA) \
	  $(COMMON_FLAGS)

release:
	xcodebuild -project $(XCODE_PROJ) \
	  -scheme $(SCHEME) \
	  -configuration Release \
	  ARCHS="arm64 x86_64" \
	  ONLY_ACTIVE_ARCH=NO \
	  $(DERIVED_DATA) \
	  $(COMMON_FLAGS)

ci:
	xcodebuild -project $(XCODE_PROJ) \
	  -scheme $(SCHEME) \
	  -configuration Release \
	  ARCHS="arm64 x86_64" \
	  ONLY_ACTIVE_ARCH=NO \
	  $(DERIVED_DATA) \
	  $(NOSIGN_FLAGS)

install: release
	mkdir -p "$(EXTENSIONS_DIR)"
	cp -R Build/Products/Release/$(KEXT_NAME) "$(EXTENSIONS_DIR)/"
	@echo ""
	@echo "Driver installed to $(EXTENSIONS_DIR)/$(KEXT_NAME)"
	@echo "Approve it in System Settings > Privacy & Security on first run."

install-ci: ci
	mkdir -p "$(EXTENSIONS_DIR)"
	cp -R Build/Products/Release/$(KEXT_NAME) "$(EXTENSIONS_DIR)/"
	@echo ""
	@echo "Driver installed to $(EXTENSIONS_DIR)/$(KEXT_NAME)"

clean:
	xcodebuild -project $(XCODE_PROJ) \
	  -scheme $(SCHEME) \
	  clean
	@echo "Clean complete."
