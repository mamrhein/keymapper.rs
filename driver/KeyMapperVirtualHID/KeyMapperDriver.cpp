// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

/// @file
/// Implementation of the virtual HID keyboard driver.

#include "KeyMapperDriver.h"

#include <DriverKit/IOLib.h>
#include <DriverKit/OSData.h>
#include <DriverKit/OSDictionary.h>
#include <DriverKit/OSNumber.h>
#include <DriverKit/OSString.h>
#include <HIDDriverKit/IOHIDDeviceKeys.h>

#include "HIDReportDescriptor.h"

OSDefineMetaClassAndStructors(KeyMapperDriver, IOUserHIDDevice)

bool KeyMapperDriver::handleStart(IOService * provider)
{
    // Call the parent's handleStart through IOUserHIDDevice. This is marked
    // LOCALONLY but since we're in the same binary it's accessible.
    bool result = IOUserHIDDevice::handleStart(provider);

    if (result) {
        IOLog("KeyMapperDriver: virtual keyboard started\n");
    } else {
        IOLog("KeyMapperDriver: failed to start\n");
    }

    return result;
}

OSDictionary * KeyMapperDriver::newDeviceDescription()
{
    // Build the device identity dictionary. These keys are consumed by IOKit
    // to publish the device in the registry and make it discoverable by
    // user-space clients through IOServiceGetMatchingService().
    auto * dict = OSDictionary::withCapacity(7);
    if (!dict) {
        return nullptr;
    }

    dict->setObject(kIOHIDVendorIDKey, OSNumber::withNumber(0x05AC, 32));        // Apple
    dict->setObject(kIOHIDProductIDKey, OSNumber::withNumber(0x1234, 32));       // Virtual keyboard
    dict->setObject(kIOHIDVersionNumberKey, OSNumber::withNumber(0x0100, 32));    // v1.0
    dict->setObject(kIOHIDCountryCodeKey, OSNumber::withNumber(0x21, 32));        // USA
    dict->setObject(kIOHIDManufacturerKey, OSString::withCString("adrhinum"));
    dict->setObject(kIOHIDProductKey, OSString::withCString("KeyMapper Virtual Keyboard"));
    dict->setObject(kIOHIDTransportKey, OSString::withCString("USB"));

    return dict;
}

OSData * KeyMapperDriver::newReportDescriptor()
{
    // Standard USB keyboard report descriptor: modifier byte + reserved byte
    // + 6 key-code slots, plus LED output report and Consumer Control page.
    return OSData::withBytes(kKeyboardReportDescriptor, sizeof(kKeyboardReportDescriptor));
}
