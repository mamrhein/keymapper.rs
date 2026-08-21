// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

/// @file
/// Virtual HID keyboard driver using DriverKit's IOUserHIDDevice.
///
/// Exposes a standard USB keyboard interface through DriverKit's user-space
/// HID driver framework. User-space clients open the driver with
/// `IOServiceOpen()` and send input reports through a `KeyMapperUserClient`
/// to inject key events.

#pragma once

#include <DriverKit/OSObject.h>
#include <HIDDriverKit/IOUserHIDDevice.h>

#include "OSStructors.h"

class IOUserClient;

class KeyMapperDriver : public IOUserHIDDevice
{
    OSDeclareDefaultStructors(KeyMapperDriver)

public:

    /// Handles device startup. Configures and publishes the virtual keyboard
    /// with the system.
    ///
    /// @param provider  The parent IOService provider.
    /// @return          `true` on success.
    virtual bool handleStart(IOService * provider) override;

    /// Creates a user client for a user-space connection opened with
    /// `IOServiceOpen()`.
    ///
    /// @param type        The type passed to `IOServiceOpen()`.
    /// @param userClient  The created `KeyMapperUserClient` on success.
    /// @return            `kIOReturnSuccess` on success.
    virtual kern_return_t NewUserClient(
        uint32_t type,
        IOUserClient ** userClient);

    /// Returns the device identity dictionary consumed by IOKit to publish
    /// the device in the registry.
    ///
    /// @return  An `OSDictionary` with vendor ID, product ID, version,
    ///          manufacturer name, and product name.
    virtual OSDictionary * newDeviceDescription() override;

    /// Returns the USB keyboard report descriptor.
    ///
    /// @return  An `OSData` object containing the standard USB keyboard
    ///          report descriptor.
    virtual OSData * newReportDescriptor() override;
};

// Global meta-class symbol consumed by OSDynamicCast(KeyMapperDriver, ...).
OSDeclareMetaClass(KeyMapperDriver);
