// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Test helpers for the e2e test harness: the key event injector and the
//! `keymapper_reader` stdin recorder (the "normal app" that receives the
//! daemon's output through the OS's regular input path).

pub mod key_injector;
pub mod reader;
