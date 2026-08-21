// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

/// @file
/// User client for the virtual HID keyboard driver.
///
/// A `KeyMapperUserClient` is created for each user-space connection opened
/// with `IOServiceOpen()`. It receives HID reports via `IOConnectCallMethod()`
/// and feeds them into the HID event system through the owning driver.

#pragma once

#include <DriverKit/IOUserClient.h>

#include "OSStructors.h"

class KeyMapperUserClient : public IOUserClient
{
    OSDeclareDefaultStructors(KeyMapperUserClient)

public:

    /// Receives arguments from `IOConnectCallMethod()` calls made by the
    /// user-space client.
    ///
    /// @param selector    The selector identifying the method.
    /// @param arguments   The arguments passed by the caller.
    /// @param dispatch    NULL when called in the driver.
    /// @param target      Target for the dispatch function.
    /// @param reference   Reference constant for the dispatch function.
    /// @return            `kIOReturnSuccess` on success.
    virtual kern_return_t ExternalMethod(
        uint64_t selector,
        IOUserClientMethodArguments * arguments,
        const IOUserClientMethodDispatch * dispatch,
        OSObject * target,
        void * reference) override;
};
