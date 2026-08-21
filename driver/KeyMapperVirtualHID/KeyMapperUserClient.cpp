// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

/// @file
/// Implementation of the user client for the virtual HID keyboard driver.

#include "KeyMapperUserClient.h"

#include <string.h>

#include <DriverKit/IOBufferMemoryDescriptor.h>
#include <DriverKit/OSData.h>
#include <HIDDriverKit/IOHIDDevice.h>

#include "KeyMapperDriver.h"
#include "KeyMapperProtocol.h"

OSDefineMetaClassAndStructors(KeyMapperUserClient, IOUserClient)

kern_return_t KeyMapperUserClient::ExternalMethod(
    uint64_t selector,
    IOUserClientMethodArguments * arguments,
    const IOUserClientMethodDispatch * dispatch,
    OSObject * target,
    void * reference)
{
    // Only the send-report selector is supported.
    if (selector != kKeyMapperSendReportSelector) {
        return kIOReturnUnsupported;
    }

    // The report bytes are passed as the structure input.
    OSData * reportData = arguments->structureInput;
    if (!reportData || reportData->getLength() == 0) {
        return kIOReturnBadArgument;
    }

    const size_t length = reportData->getLength();
    const void * bytes = reportData->getBytesNoCopy();

    // Wrap the report bytes in a memory descriptor for handleReport(). The
    // descriptor is readable (kIOMemoryDirectionOut) because the HID event
    // system reads the report from it.
    IOBufferMemoryDescriptor * memDesc = nullptr;
    kern_return_t kr = IOBufferMemoryDescriptor::Create(
        kIOMemoryDirectionOut, length, 0, &memDesc);
    if (kr != kIOReturnSuccess) {
        return kr;
    }
    if (!memDesc) {
        return kIOReturnNoMemory;
    }

    // Copy the report bytes into the descriptor's buffer. The buffer is
    // allocated in the driver's address space, so the address returned by
    // GetAddressRange() can be used directly.
    IOAddressSegment range;
    kr = memDesc->GetAddressRange(&range);
    if (kr != kIOReturnSuccess) {
        memDesc->release();
        return kr;
    }
    memcpy(reinterpret_cast<void *>(range.address), bytes, length);
    memDesc->SetLength(length);

    // Feed the report into the HID event system through the owning driver.
    // The driver is the provider of this user client (see
    // KeyMapperDriver::NewUserClient).
    auto * driver = OSDynamicCast(KeyMapperDriver, GetProvider());
    if (!driver) {
        memDesc->release();
        return kIOReturnError;
    }

    kr = driver->handleReport(0, memDesc, static_cast<uint32_t>(length), kIOHIDReportTypeInput, 0);

    memDesc->release();

    return kr;
}
