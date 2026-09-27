// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Test helpers for keymapper's e2e test harness: the key event injector,
//! which feeds a virtual keyboard into the daemon's capture path.
//!
//! This is a dev-only helper crate (a `dev-dependency` of `keymapper`); it
//! is never linked into a release binary.

pub mod key_injector;
