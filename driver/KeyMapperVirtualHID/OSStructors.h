// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

/// @file
/// Minimal OSMetaClass structor macros for DriverKit extensions.
///
/// The kernel extension SDK provides these in libkern/c++/OSMetaClass.h, but
/// that header drags in kernel-specific dependencies (Mach types like thread_t)
/// that are unavailable in the DriverKit SDK. These standalone definitions
/// provide just what's needed for class registration in DriverKit extensions.
///
/// Usage:
///   - Header: Place OSDeclareDefaultStructors(MyClass) after the opening
///             brace of your class declaration.
///   - Source: Place OSDefineMetaClassAndStructors(MyClass, SuperClass) at the
///             top of your .cpp file.

#pragma once

#include <DriverKit/OSMetaClass.h>

/// Declares structors for a concrete (non-abstract) DriverKit class.
#define OSDeclareDefaultStructors(className)                         \
private:                                                             \
    static const OSMetaClass * const superClass;                     \
public:                                                              \
    static const OSMetaClass * const metaClass;                      \
    static class MetaClass : public OSMetaClass {                    \
    public:                                                          \
        MetaClass();                                                 \
        virtual kern_return_t New(OSObject * instance) override;     \
    } gMetaClass;                                                    \
    friend class className :: MetaClass;                             \
    virtual const OSMetaClass * getMetaClass() const override;       \
protected:                                                           \
    className ();                                                    \
    virtual ~className ();

/// Defines structors for a concrete DriverKit class.
#define OSDefineMetaClassAndStructors(className, superclassName)     \
                                                                     \
/* ── Class global data ──────────────────────────────────────────── */\
                                                                     \
className :: MetaClass className :: gMetaClass;                      \
const OSMetaClass * const className :: superClass =                  \
    g ## superclassName ## MetaClass;                                \
const OSMetaClass * const className :: metaClass =                   \
    & className :: gMetaClass;                                       \
                                                                     \
/* ── Meta-class constructor ─────────────────────────────────────── */\
                                                                     \
className :: MetaClass::MetaClass()                                  \
{                                                                    \
}                                                                    \
                                                                     \
/* ── Meta-class New (placement new for allocated instances) ─────── */\
                                                                     \
kern_return_t                                                        \
className :: MetaClass::New(OSObject * instance)                     \
{                                                                    \
    className * obj = new (instance) className;                      \
    return kIOReturnSuccess;                                         \
}                                                                    \
                                                                     \
/* ── Class member functions ─────────────────────────────────────── */\
                                                                     \
const OSMetaClass *                                                  \
className :: getMetaClass() const                                    \
{                                                                    \
    return &gMetaClass;                                              \
}                                                                    \
                                                                     \
/* ── Constructor / destructor ───────────────────────────────────── */\
                                                                     \
className :: className ()                                            \
{                                                                    \
}                                                                    \
                                                                     \
className ::~className ()                                            \
{                                                                    \
}
