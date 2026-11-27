//! Minimal RFC 3339 (UTC, second precision) helpers so the workspace needs no date crate.

use crate::CatalogError;

/// Days since 1970-01-01 for a proleptic Gregorian date (Hinnant's `days_from_civil`).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Inverse of [`days_from_civil`].
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// Parses `YYYY-MM-DDTHH:MM:SS[.fff](Z|±HH:MM)` into milliseconds since the Unix epoch.
pub fn parse_rfc3339_ms(value: &str) -> Result<i64, CatalogError> {
    let err = |reason| CatalogError::InvalidTimestamp {
        value: value.to_owned(),
        reason,
    };
    let b = value.as_bytes();
    if b.len() < 20 || !value.is_ascii() {
        return Err(err("expected YYYY-MM-DDTHH:MM:SSZ"));
    }
    let num = |from: usize, to: usize| -> Result<i64, CatalogError> {
        value[from..to]
            .parse::<i64>()
            .map_err(|_| err("non-numeric field"))
    };
    if b[4] != b'-'
        || b[7] != b'-'
        || !matches!(b[10], b'T' | b't' | b' ')
        || b[13] != b':'
        || b[16] != b':'
    {
        return Err(err("expected YYYY-MM-DDTHH:MM:SSZ"));
    }
    let (year, month, day) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (hour, minute, second) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return Err(err("field out of range"));
    }
    let mut rest = &value[19..];
    let mut millis = 0;
    if let Some(frac) = rest.strip_prefix('.') {
        let digits = frac.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return Err(err("empty fractional seconds"));
        }
        let padded = format!("{:0<3}", &frac[..digits.min(3)]);
        millis = padded
            .parse::<i64>()
            .map_err(|_| err("non-numeric field"))?;
        rest = &frac[digits..];
    }
    let offset_s = match rest {
        "Z" | "z" => 0,
        _ if rest.len() == 6
            && (rest.starts_with('+') || rest.starts_with('-'))
            && &rest[3..4] == ":" =>
        {
            let sign = if rest.starts_with('-') { -1 } else { 1 };
            let h = rest[1..3].parse::<i64>().map_err(|_| err("bad offset"))?;
            let m = rest[4..6].parse::<i64>().map_err(|_| err("bad offset"))?;
            sign * (h * 3600 + m * 60)
        }
        _ => return Err(err("missing `Z` or `±HH:MM` offset")),
    };
    let secs =
        days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second - offset_s;
    Ok(secs * 1000 + millis)
}

/// Formats milliseconds since the epoch as `YYYY-MM-DDTHH:MM:SSZ` (sub-second part dropped).
pub fn format_rfc3339(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    let rem = secs.rem_euclid(86_400);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_date_matches_prototype_epoch() {
        // prototype/out/obs.models.dev.jsonl: observedAtMs for revision 2026-11-26T08:32:56Z
        assert_eq!(
            parse_rfc3339_ms("2026-11-26T08:32:56Z").unwrap(),
            1_795_681_976_000
        );
    }

    #[test]
    fn round_trips_and_handles_offsets_and_fractions() {
        let ms = parse_rfc3339_ms("2024-02-29T23:59:59Z").unwrap();
        assert_eq!(format_rfc3339(ms), "2024-02-29T23:59:59Z");
        assert_eq!(parse_rfc3339_ms("1970-01-01T01:00:00+01:00").unwrap(), 0);
        assert_eq!(parse_rfc3339_ms("1970-01-01T00:00:00.25Z").unwrap(), 250);
        assert_eq!(format_rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_rfc3339(-1000), "1969-12-31T23:59:59Z");
    }

    #[test]
    fn rejects_garbage() {
        for bad in [
            "",
            "yesterday",
            "2026-13-01T00:00:00Z",
            "2026-11-26T08:32:56",
            "2026-11-26T08:32:56+0100",
        ] {
            assert!(parse_rfc3339_ms(bad).is_err(), "{bad}");
        }
    }
}
