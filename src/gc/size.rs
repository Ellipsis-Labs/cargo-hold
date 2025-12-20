use crate::error::{HoldError, Result};

/// Parse a size string like "5G", "500M", "1024K" into bytes
pub(crate) fn parse_size(s: &str) -> Result<u64> {
    let s = s.trim();

    // Try to parse as raw number first
    if let Ok(bytes) = s.parse::<u64>() {
        return Ok(bytes);
    }

    // Otherwise parse with suffix
    let (num_part, suffix) = split_number_suffix(s)?;
    let multiplier = match suffix.to_uppercase().as_str() {
        "B" | "" => 1,
        "K" | "KB" | "KIB" => 1024,
        "M" | "MB" | "MIB" => 1024 * 1024,
        "G" | "GB" | "GIB" => 1024 * 1024 * 1024,
        "T" | "TB" | "TIB" => 1024_u64.pow(4),
        _ => {
            return Err(HoldError::InvalidMetadataSize(
                s.to_string(),
                format!("Unknown size suffix: {suffix}"),
            ));
        }
    };

    let base: f64 = num_part.parse().map_err(|_| {
        HoldError::InvalidMetadataSize(s.to_string(), "Invalid number format".to_string())
    })?;

    Ok((base * multiplier as f64) as u64)
}

/// Split a size string into number and suffix parts
fn split_number_suffix(s: &str) -> Result<(&str, &str)> {
    let mut split_pos = s.len();
    for (i, ch) in s.char_indices() {
        if ch.is_alphabetic() {
            split_pos = i;
            break;
        }
    }

    let (num, suffix) = s.split_at(split_pos);
    if num.is_empty() {
        return Err(HoldError::InvalidMetadataSize(
            s.to_string(),
            "No number found".to_string(),
        ));
    }

    Ok((num, suffix))
}

/// Format size in human-readable format
pub(crate) fn format_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB"];
    let mut size = bytes as f64;
    let mut unit_idx = 0;

    while size >= 1024.0 && unit_idx < UNITS.len() - 1 {
        size /= 1024.0;
        unit_idx += 1;
    }

    if unit_idx == 0 {
        format!("{} {}", bytes, UNITS[0])
    } else {
        format!("{:.1} {}", size, UNITS[unit_idx])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_size() {
        assert_eq!(parse_size("100").unwrap(), 100);
        assert_eq!(parse_size("100B").unwrap(), 100);
        assert_eq!(parse_size("1K").unwrap(), 1024);
        assert_eq!(parse_size("1KB").unwrap(), 1024);
        assert_eq!(parse_size("1KiB").unwrap(), 1024);
        assert_eq!(parse_size("2M").unwrap(), 2 * 1024 * 1024);
        assert_eq!(parse_size("2MB").unwrap(), 2 * 1024 * 1024);
        assert_eq!(parse_size("2MiB").unwrap(), 2 * 1024 * 1024);
        assert_eq!(parse_size("3G").unwrap(), 3 * 1024 * 1024 * 1024);
        assert_eq!(parse_size("3GB").unwrap(), 3 * 1024 * 1024 * 1024);
        assert_eq!(parse_size("3GiB").unwrap(), 3 * 1024 * 1024 * 1024);
        assert_eq!(
            parse_size("1.5G").unwrap(),
            (1.5 * 1024.0 * 1024.0 * 1024.0) as u64
        );

        assert!(parse_size("").is_err());
        assert!(parse_size("abc").is_err());
        assert!(parse_size("100X").is_err());
    }

    #[test]
    fn test_format_size() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(100), "100 B");
        assert_eq!(format_size(1024), "1.0 KiB");
        assert_eq!(format_size(1536), "1.5 KiB");
        assert_eq!(format_size(1024 * 1024), "1.0 MiB");
        assert_eq!(format_size(1024 * 1024 * 1024), "1.0 GiB");
        assert_eq!(format_size(1024_u64.pow(4)), "1.0 TiB");
    }
}
