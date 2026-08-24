# Windows — Low-Level Hook Capture and VHF Virtual HID Output

Status: **planned**. Input capture is implemented; event emission currently uses
`SendInput` and will be replaced by a Virtual HID Framework (VHF) HID source
driver, written in Rust on top of
[windows-drivers-rs](https://github.com/microsoft/windows-drivers-rs), so that
the architecture matches Linux and macOS: the daemon emits remapped key events
through a virtual keyboard device that Windows treats as a real physical
keyboard.

## How it works

### Input capture (implemented)

`keymapperd` installs a `WH_KEYBOARD_LL` hook on a dedicated hook thread and a
raw-input window on a second thread. A worker thread correlates both event
streams to identify the source keyboard, performs the mapping lookup, and
decides whether to swallow or pass through the event. This stage is unchanged
by the VHF work.

### Output emission (planned, VHF HID source driver)

The VHF driver exposes a virtual keyboard (plus a consumer-control) device.
When a remapped key is produced, `keymapperd` opens the driver's device
interface and submits the resulting HID report with a single
`DeviceIoControl` call, mirroring the `IOConnectCallMethod` call on macOS.
The Windows HID stack then treats the report as input from a physical
keyboard, so it works in all applications, including those that consume raw
input or filter injected `SendInput` events.

## The VHF stack

VHF (in-box `Vhf.sys`, Windows 10+) replaces the whole HID transport
minidriver layer. You write a small HID source driver, and VHF builds the rest
of the stack underneath it:

```text
keymapperd (daemon)
    |  DeviceIoControl: submit HID report
    v
HID source driver (our driver, UMDF 2)
    |  VhfReadReportSubmit()
    v
Vhf.sys (in-box, lower filter via INF LowerFilters="vhf")
    |
    v
Hidclass.sys / Mshidkmdf.sys (in-box, enumerates top-level collections)
    |
    v
HID clients (keyboard class driver, raw input, Win32 HID)
```

Driver lifecycle:

1. `WdfDeviceCreate` in the device-add callback.
2. `VHF_CONFIG_INIT` + `VhfCreate` (PASSIVE_LEVEL) with the report
   descriptor, vendor/product IDs, and a fixed container ID.
3. `VhfStart` — VHF enumerates one child device per top-level collection.
4. Emit reports with `VhfReadReportSubmit(handle, &packet)`, where
   `HID_XFER_PACKET` is `{ reportBuffer, reportBufferLen, reportId }`.
5. Tear down with `VhfDelete(handle, TRUE)` in the device cleanup callback.

VHF automatically answers `IOCTL_HID_GET_STRING`,
`IOCTL_HID_GET_DEVICE_ATTRIBUTES`, `IOCTL_HID_GET_DEVICE_DESCRIPTOR`, and
`IOCTL_HID_GET_REPORT_DESCRIPTOR`. Get/set feature, get input report, and
write (output) report requests are forwarded to registered callbacks, or
completed with `STATUS_NOT_SUPPORTED` if none is registered. By default VHF
buffers read reports, which fits our use case (one report per key state
change); the alternative `EvtVhfReadyForNextReadReport` flow is only needed if
report coalescing must be tuned.

## Driver design

### UMDF 2, no KMDF fallback

The VHF documentation is inconsistent about user-mode support: the overview
page says "in this release, VHF supports a HID source driver only in kernel
mode," while the `vhf.h` header index says the interface is for "both User
mode and Kernel mode," and `VHF_CONFIG.FileHandle` is documented as required
for user-mode (UMDF) drivers. The WDK settles the question: WDK 10.0.26100
ships both `Lib/10.0.26100.0/km/x64/vhfkm.lib` and
`Lib/10.0.26100.0/um/x64/VhfUm.lib`; the latter exports the full VHF API
(`VhfCreate`, `VhfStart`, `VhfReadReportSubmit`, `VhfDelete`,
`VhfAsyncOperationComplete`) and binds to an in-box `VhfUm.dll`. The UMDF
source-driver path therefore exists in the current toolchain.

Decision: **UMDF 2 only.** A KMDF driver is not an option because kernel
drivers require Microsoft partner attestation (or WHQL) for distribution and
test signing for development, which the project does not accept. UMDF 2
additionally has these advantages:

- It is a user-mode binary: a bug crashes the UMDF host process, not the
  kernel.
- Distribution requires ordinary code signing, not Microsoft partner
  attestation or WHQL.
- The daemon-side protocol (device interface + `DeviceIoControl`) is
  identical to what a KMDF driver would use.

The remaining risk is that all official VHF samples are kernel-mode (see
references) and the UMDF setup flow (the
`FileHandle`/`WdfIoTargetOpenLocalTargetByFile` machinery) has to be worked
out from `vhf.h` during bring-up. Mitigations: `windows-drivers-rs` ships
first-class VHF support (below), and the kernel-mode `CfuVirtualHid` sample
remains a reference for the VHF flow.

### Implementation: Rust via `windows-drivers-rs`

The driver is written in Rust with
[microsoft/windows-drivers-rs](https://github.com/microsoft/windows-drivers-rs),
the official Microsoft platform for Windows driver development in Rust
(crates `wdk-build`, `wdk-sys`, `wdk`, `wdk-alloc`, `wdk-panic`). This keeps
the entire codebase, daemon and driver, in one language and toolchain.

Verified against the repository (August 2026):

- The `wdk-sys` `hid` feature bindgen-binds the VHF API: the HID header
  subset is `hidclass.h`, `hidsdi.h`, `hidpi.h`, and `vhf.h`, so `VhfCreate`,
  `VhfStart`, `VhfReadReportSubmit`, `VhfDelete`, and `VHF_CONFIG` are
  available as Rust types.
- `wdk-build` links the correct VHF library per driver model: `VhfKm` for
  WDM/KMDF and `VhfUm` for UMDF, each covered by a unit test.
- Minimal WDM, KMDF, and UMDF example drivers live in `examples/` of the
  repository; `examples/sample-umdf-driver` is the direct template for this
driver.

Caveat: the project's README states it is "in early stages of development and
not yet recommended for production use." The driver is small and its flow is
simple, but the dependency should be pinned to a specific revision and kept in
sync with upstream fixes.

### Layout

```text
driver/windows/
    KeyMapperVhf/
        Cargo.toml              # cdylib, [package.metadata.wdk.driver-model] UMDF, hid feature
        build.rs                # wdk_build::configure_wdk_binary_build()
        Makefile.toml           # cargo-make packaging (inf2cat, signing)
        src/
            lib.rs              # DriverEntry, device add/cleanup, VhfCreate/Start/Delete
            interface.rs        # device interface + IOCTL dispatch
            report_descriptor.rs # report descriptor bytes, generated from the shared source
        KeyMapperVhf.inx        # INF: LowerFilters="vhf", hardware ID, interface
```

The report descriptor is the same byte stream as
`driver/KeyMapperVirtualHID/HIDReportDescriptor.h` (keyboard top-level
collection, report ID 1, six key slots; consumer collection, report ID 2;
LED output report). A small script generates the Rust byte array so both
platforms share one source of truth.

### Daemon protocol

The driver creates a device interface with a project-owned GUID and handles
one custom control code, `IOCTL_KEYMAPPER_SUBMIT_REPORT`, whose input buffer
mirrors `HID_XFER_PACKET`:

```c
typedef struct _KM_SUBMIT_REPORT {
    ULONG  reportBufferLen;  // length of the report in data.
    UCHAR  reportId;         // 1 for keyboard, 2 for consumer control.
    UCHAR  data[32];         // fixed report area (keyboard report is 9 bytes).
} KM_SUBMIT_REPORT;
```

The handler validates the report ID and length, copies the bytes into a
preallocated `HID_XFER_PACKET`, and calls `VhfReadReportSubmit`. No callback
for the LED output report is registered in the first version, so LED writes
from the keyboard class driver complete with `STATUS_NOT_SUPPORTED` and the
Num/Caps/Scroll LEDs stay off; this behavior must be verified during
bring-up.

### Fixed device identity

All identity values live in one header shared by the driver and the daemon:

- `VendorID` / `ProductID` / `VersionNumber` (pick stable values; register a
  vendor ID before distribution).
- `ContainerID` — a fixed GUID the daemon uses for self-exclusion and
  monitoring.
- Hardware ID `HID\KEYMAPPER_VIRT_KBD`.

Installation follows the pattern of the Rust driver samples and the official
`CfuVirtualHid` VHF sample (from `microsoft/CFU`):

```bat
pnputil /add-driver KeyMapperVhf.inf /install
devgen /add /hardwareid "HID\KEYMAPPER_VIRT_KBD"
```

`devcon install KeyMapperVhf.inf HID\KEYMAPPER_VIRT_KBD` is an equivalent
one-step form. Device Manager then shows the source device, a "Virtual HID
Framework (VHF) HID device" child, and an "HID-compliant device" per
collection.

## Daemon design

- New `src/platform/windows/virt_kbd.rs`, mirroring
  `src/platform/macos/hid_virt_kbd_conn.rs`: discover the device with
  SetupDi (match the hardware ID or container ID), open the device
  interface, wait/retry loop at startup, one `DeviceIoControl` per report,
  reconnect on device removal.
- `build_keyboard_report`, `build_consumer_report`, and `modifier_to_hid`
  move from `src/platform/macos/hid_virt_kbd_conn.rs` to `src/common/` —
  they are pure byte builders over the shared descriptor.
- `key.rs` gains a `vk_to_hid_usage` conversion (inverse of the existing
  `hid_to_vk`) so the Windows `NativeKey` values can be emitted as report ID
  1; consumer usage IDs already flow through the platform-agnostic mapping
  engine.
- `src/platform/windows/mapping.rs` switches its emission path from
  `SendInput` to `VirtKbdConn`, keeping `SendInput` as a fallback when the
  driver is not installed (the driver is user-installed, unlike `uinput` on
  Linux). `CAPTURE_TAG` stamping stays only on the fallback path.
- **Self-exclusion.** On Linux the daemon simply never opens the virtual
  device; on Windows the low-level hook is global, so the virtual
  keyboard's events also reach it. Exclusion happens at event level: the
  raw-input thread resolves each event's device handle to a container ID and
  the worker passes through (never swallows, never remaps) events from the
  virtual device, preventing the re-emission feedback loop.

## Build, signing, install

- Toolchain: Rust plus the Enterprise WDK (eWDK) build environment
  (`LaunchBuildEnv.cmd`) and LLVM for `bindgen`. The upstream README pins
  LLVM 17 because of an ARM64 `bindgen` bug that is fixed in LLVM 19; x64
  builds work with current LLVM. `cargo-make` runs the post-build packaging
  (`inf2cat`, signing); a `cargo-wdk` extension is in development to replace
  it. Caveat: the `Microsoft.Windows.WDK.x64` NuGet package contains
  `vhfkm.lib`/`VhfUm.lib` but not `vhf.h`, so the eWDK environment is
  required.
- Development: `cargo make` produces a driver package signed with a
  generated test certificate (`WDRLocalTestCert.cer`). Loading the package
  requires test signing (`bcdedit /set testsigning on`) and, on machines with
  Secure Boot or BitLocker, the suspension steps documented in the Rust
  driver samples.
- Distribution: the UMDF 2 driver is signed like an ordinary user-mode
  binary; no Microsoft partner attestation is required.
- Uninstall removes both the driver package (`pnputil /delete-driver`) and
  the device instance, otherwise a ghost device remains.

## Observability

The e2e monitor currently captures daemon output via the `CAPTURE_TAG` in
`dwExtraInfo`, which a VHF device cannot stamp. Since the virtual keyboard
is a real device, the monitor should instead read it directly: open the
device and consume input reports, or correlate raw-input events by container
ID. The `CAPTURE_TAG` mechanism remains only for the `SendInput` fallback
path.

## Trade-offs vs `SendInput`

| Aspect | `SendInput` | VHF virtual keyboard |
|--------|-------------|----------------------|
| Event source | Marked as injected (`LLKHF_INJECTED`, `dwExtraInfo`) | Appears as a real keyboard device |
| Raw-input applications | May treat events as synthetic | See an ordinary HID keyboard |
| Extra component | None | User-installed driver |
| Signing | None | Code-signed driver (no kernel attestation) |
| Failure mode | In-process | Driver not loaded (daemon falls back to `SendInput`) |

Note that VHF makes the *input* indistinguishable from hardware; it does not
hide the driver itself from systems that scan the device list.

## Open items

1. Bring up the UMDF 2 VHF source driver: the `FileHandle`/
   `WdfIoTargetOpenLocalTargetByFile` setup flow is documented only in
   `vhf.h` and needs to be followed in a minimal prototype.
2. `windows-drivers-rs` is early stage: pin the dependency to a specific
   revision, re-test against upstream updates, and keep the C
   `CfuVirtualHid` sample as the reference implementation for the VHF flow.
3. Verify that the keyboard class driver tolerates an unsupported LED output
   report during enumeration.
4. Community reports (Microsoft Q&A) include at least one VHF virtual
   keyboard that produced no events; budget for a bring-up debugging phase.
5. Register a vendor ID before shipping; use development values until then.

## References

- [Write a HID source driver by using VHF](https://learn.microsoft.com/en-us/windows-hardware/drivers/hid/virtual-hid-framework--vhf-)
- [vhf.h header reference](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/vhf/)
- [`VHF_CONFIG` structure](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/vhf/ns-vhf-_vhf_config)
- [`VhfReadReportSubmit`](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/vhf/nf-vhf-vhfreadreportsubmit)
- [CFU virtual HID device simulation (VHF sample + `devcon` install)](https://learn.microsoft.com/en-us/windows-hardware/drivers/cfu/cfu-firmware-update-simulation)
- [microsoft/CFU repository (`CfuVirtualHid` solution)](https://github.com/microsoft/CFU)
- [Install the WDK using NuGet](https://learn.microsoft.com/en-us/windows-hardware/drivers/install-the-wdk-using-nuget)
- [microsoft/windows-drivers-rs (Rust driver development platform)](https://github.com/microsoft/windows-drivers-rs)
- [microsoft/Windows-rust-driver-samples](https://github.com/microsoft/Windows-rust-driver-samples)
