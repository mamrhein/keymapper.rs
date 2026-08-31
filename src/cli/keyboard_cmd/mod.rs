// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision:

//! Keyboard device listing command.  Prints a table of all detected keyboards
//! with name, vendor, model, port type, and device identifier.

use crate::platform::list_keyboards;

/// Print a table of all detected keyboard devices.
pub fn list() {
    let keyboards = match list_keyboards() {
        Ok(kb) => kb,
        Err(e) => {
            eprintln!("Warning: {e}");
            return;
        }
    };

    if keyboards.is_empty() {
        println!("No keyboard devices found.");
        return;
    }

    // Calculate column widths.
    let name_width = width_for_column(
        "NAME",
        &keyboards
            .iter()
            .map(|k| k.name.as_str())
            .collect::<Vec<_>>(),
    );
    let vendor_width = width_for_column(
        "VENDOR",
        &keyboards
            .iter()
            .map(|k| k.vendor.as_str())
            .collect::<Vec<_>>(),
    );
    let model_width = width_for_column(
        "MODEL",
        &keyboards
            .iter()
            .map(|k| k.model.as_str())
            .collect::<Vec<_>>(),
    );
    let port_width = width_for_column(
        "PORT",
        &keyboards
            .iter()
            .map(|k| k.port.as_deref().unwrap_or(""))
            .collect::<Vec<_>>(),
    );
    let device_width = width_for_column(
        "DEVICE",
        &keyboards
            .iter()
            .map(|k| k.device.as_str())
            .collect::<Vec<_>>(),
    );

    // Print header.
    print_padded("NAME", name_width);
    print!("  ");
    print_padded("VENDOR", vendor_width);
    print!("  ");
    print_padded("MODEL", model_width);
    print!("  ");
    print_padded("PORT", port_width);
    print!("  ");
    println!("DEVICE");

    // Print separator.
    let sep = format!(
        "{}  {}  {}  {}  {}",
        "\u{2500}".repeat(name_width),
        "\u{2500}".repeat(vendor_width),
        "\u{2500}".repeat(model_width),
        "\u{2500}".repeat(port_width),
        // DEVICE column extends to the end of line.
        "\u{2500}".repeat(device_width.max(10)),
    );
    println!("{sep}");

    // Print rows.
    for kb in &keyboards {
        print_padded(&kb.name, name_width);
        print!("  ");
        print_padded(&kb.vendor, vendor_width);
        print!("  ");
        print_padded(&kb.model, model_width);
        print!("  ");
        let port_str = kb.port.as_deref().unwrap_or("");
        print_padded(port_str, port_width);
        print!("  ");
        println!("{}", kb.device);
    }

    println!();
    println!("Total: {} keyboard(s)", keyboards.len());
}

/// Print a string padded to the given width, truncating if necessary.
fn print_padded(s: &str, width: usize) {
    print!("{}", pad(s, width));
}

/// Pad a string to the given width (in characters), truncating with an
/// ellipsis when necessary.  Truncation happens on character boundaries so
/// multi-byte UTF-8 values (e.g. non-ASCII vendor names) cannot panic.
fn pad(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        return format!("{s:<width$}", width = width);
    }
    if width <= 3 {
        s.chars().take(width).collect()
    } else {
        format!("{}…", s.chars().take(width - 1).collect::<String>())
    }
}

/// Calculate the optimal column width for a header and a list of values.
///
/// Widths are in characters so they line up with `pad` for multi-byte
/// UTF-8 values.
fn width_for_column(header: &str, values: &[&str]) -> usize {
    let max_value =
        values.iter().map(|s| s.chars().count()).max().unwrap_or(0);
    header.chars().count().max(max_value).min(40)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::keyboard::KeyboardInfo;

    #[test]
    fn width_for_column_header_only() {
        let w = width_for_column("VENDOR", &[]);
        assert_eq!(w, 6);
    }

    #[test]
    fn width_for_column_long_value() {
        let values = vec!["Logitech"];
        let w = width_for_column("VENDOR", &values);
        assert_eq!(w, 8); // "Logitech" is longer than "VENDOR"
    }

    #[test]
    fn width_for_column_capped() {
        let long = "a".repeat(100);
        let values = vec![long.as_str()];
        let w = width_for_column("X", &values);
        assert_eq!(w, 40); // capped at 40
    }

    #[test]
    fn pad_pads_short_values() {
        assert_eq!(pad("Test", 8), "Test    ");
    }

    #[test]
    fn pad_truncates_long_values_with_ellipsis() {
        assert_eq!(pad("Logitech", 6), "Logit…");
    }

    #[test]
    fn pad_truncates_without_ellipsis_when_too_narrow() {
        assert_eq!(pad("abcd", 3), "abc");
    }

    #[test]
    fn pad_handles_multibyte_utf8_without_panic() {
        // Truncation points that would land mid-character in bytes.
        assert_eq!(pad("ÄÖÜ", 2), "ÄÖ");
        assert_eq!(pad("ÄÖÜ", 4), "ÄÖÜ ");
        let vendor = "Müller&Söhne";
        assert_eq!(pad(vendor, 6).chars().count(), 6);
    }

    #[test]
    fn keyboard_info_debug_output() {
        let kb = KeyboardInfo::new(
            "Test".into(),
            "Vendor".into(),
            "Model".into(),
            "device0".into(),
            Some("USB".to_string()),
        );

        assert_eq!(kb.name, "Test");
        assert_eq!(kb.vendor, "Vendor");
        assert_eq!(kb.model, "Model");
        assert_eq!(kb.device, "device0");
        assert_eq!(kb.port, Some("USB".to_string()));
    }
}
