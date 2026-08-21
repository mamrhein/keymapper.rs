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
#include <DriverKit/IOUserClient.h>
#include <HIDDriverKit/IOHIDDeviceKeys.h>

#include "HIDReportDescriptor.h"

/// Name of the registry property describing the user client. Consumed by
/// `NewUserClient()` to create a `KeyMapperUserClient` for each connection.
static const char * const kKeyMapperUserClientProperty = "KeyMapperUserClient";

OSDefineMetaClassAndStructors(KeyMapperDriver, IOUserHIDDevice)

bool KeyMapperDriver::handleStart(IOService * provider)
{
    // Call the parent's handleStart through IOUserHIDDevice. This is marked
    // LOCALONLY but since we're in the same binary it's accessible.
    bool result = IOUserHIDDevice::handleStart(provider);

    if (result) {
        // Register the property describing the user client. NewUserClient()
        // consumes it via IOService::Create() to instantiate a
        // KeyMapperUserClient for each user-space connection.
        auto * userClientDict = OSDictionary::withCapacity(2);
        if (userClientDict) {
            auto * ioClass = OSString::withCString("IOUserClient");
            auto * ioUserClass = OSString::withCString("KeyMapperUserClient");
            if (ioClass && ioUserClass) {
                userClientDict->setObject("IOClass", ioClass);
                userClientDict->setObject("IOUserClass", ioUserClass);

                // IOHIDDevice::setProperty() takes OSObject keys, so wrap the
                // property name in an OSString.
                auto * userClientKey = OSString::withCString(kKeyMapperUserClientProperty);
                if (userClientKey) {
                    setProperty(userClientKey, userClientDict);
                    userClientKey->release();
                }
            }
            if (ioClass) {
                ioClass->release();
            }
            if (ioUserClass) {
                ioUserClass->release();
            }
            userClientDict->release();
        }

        IOLog("KeyMapperDriver: virtual keyboard started\n");
    } else {
        IOLog("KeyMapperDriver: failed to start\n");
    }

    return result;
}

kern_return_t KeyMapperDriver::NewUserClient(
    uint32_t type,
    IOUserClient ** userClient)
{
    // Instantiate the user client from the registry property set in
    // handleStart(). The created object is registered as a child of this
    // driver, so it can reach the driver through getProvider().
    IOService * service = nullptr;
    kern_return_t kr = Create(this, kKeyMapperUserClientProperty, &service);
    if (kr != kIOReturnSuccess || !service) {
        return (kr != kIOReturnSuccess) ? kr : kIOReturnError;
    }

    *userClient = OSDynamicCast(IOUserClient, service);
    if (!*userClient) {
        service->release();
        return kIOReturnError;
    }

    // The IOKit framework takes ownership of the user client and releases it
    // when the connection is closed.
    return kIOReturnSuccess;
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
