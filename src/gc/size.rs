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

    // `as u64` saturates, so a negative value would land on 0 rather than fail. A
    // cap of 0 is not a no-op here: `select_for_size` reads it as "free
    // everything", so a typo like `-5G` would quietly clear the cache instead
    // of being rejected.
    // `is_sign_negative` rather than `< 0.0`: `-0` and `-0.0` parse to negative
    // zero, for which `< 0.0` is false, so `-0G` would otherwise still reach
    // the cast and land on the same dangerous 0.
    if !base.is_finite() || base.is_sign_negative() {
        return Err(HoldError::InvalidMetadataSize(
            s.to_string(),
            "Size must be a non-negative, finite number".to_string(),
        ));
    }

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
    fn parse_size_rejects_negative_values_instead_of_clamping_to_zero() {
        // `(-5.0 * 1024f64.powi(3)) as u64` is 0, and a max size of 0 makes
        // `select_for_size` free the whole cache, so this has to be an error.
        // `-0` and `-0.0` parse to negative zero, where `< 0.0` is false: an explicitly
        // negative input must not reach the cast and become the 0 cap either.
        for input in [
            "-1", "-5G", "-0.5G", "-500M", "-1T", "-1KiB", "-0", "-0.0", "-0G",
        ] {
            let result = parse_size(input);
            assert!(
                result.is_err(),
                "{input} should be rejected, got {result:?}"
            );
        }
    }

    #[test]
    fn parse_size_still_accepts_zero_and_the_boundary() {
        // Rejecting negatives must not reject a deliberate 0, which is a valid
        // (if aggressive) cap, nor anything that parsed before.
        assert_eq!(parse_size("0").unwrap(), 0);
        assert_eq!(parse_size("0G").unwrap(), 0);
        assert_eq!(parse_size("0.0M").unwrap(), 0);
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
