# ---------------------------------------------------------------------------
# Builds and installs the KeyMapperVirtualHID DriverKit extension for local
# development.  Uses ad-hoc signing (no certificate required).
#
# Targets:
#   make build     — Debug build, active architecture only (default)
#   make release   — Release build, all architectures
#   make install   — Build and copy the .kext to ~/Library/Application Support/keymapper/
#   make clean     — Remove build artifacts
# ---------------------------------------------------------------------------

XCODE_PROJ := KeyMapperVirtualHID.xcodeproj
SCHEME     := KeyMapperVirtualHID
KEXT_NAME  := KeyMapperVirtualHID.kext

APP_SUPPORT_DIR := $(HOME)/Library/Application Support/keymapper

COMMON_FLAGS := \
	CODE_SIGN_IDENTITY=- \
	CODE_SIGN_ENTITLEMENTS=KeyMapperVirtualHID/KeyMapperVirtualHID.entitlements \
	CODE_SIGNING_REQUIRED=YES \
	CODE_SIGNING_ALLOWED=YES

.PHONY: build release install clean

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

install: release
	mkdir -p "$(APP_SUPPORT_DIR)"
	cp -R Build/Products/Release/$(KEXT_NAME) "$(APP_SUPPORT_DIR)/"
	@echo ""
	@echo "Driver installed to $(APP_SUPPORT_DIR)/$(KEXT_NAME)"

clean:
	xcodebuild -project $(XCODE_PROJ) \
	  -scheme $(SCHEME) \
	  clean
	@echo "Clean complete."
