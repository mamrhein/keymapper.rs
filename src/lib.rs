// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

pub mod cli;
pub mod common;
pub mod daemon;
pub mod platform;
pub mod test_util;

// Re-export the HID-centric key identity so downstream code (and tests)
// can refer to it via the crate root.
pub use common::HidUsage;
