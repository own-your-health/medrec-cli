//! Times in `_status/` and `_requests/`: RFC 3339 in UTC, to the second.

/// Seconds since the Unix epoch, now.
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn fmt(secs: i64) -> String {
    pk_cli_core::dates::fmt_rfc3339(secs)
}

/// `YYYY-MM-DDTHH:MM:SS`, with optional fractional seconds, then `Z` or
/// `±HH:MM`: the forms `auth-status/v1` and medrec write. `None` for
/// anything else.
pub fn parse(s: &str) -> Option<i64> {
    let s = s.trim();
    let b = s.as_bytes();
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b't' | b' ') {
        return None;
    }
    if b[13] != b':' || b[16] != b':' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> {
        let t = s.get(r)?;
        t.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| t.parse().ok())?
    };
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, sec) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&mo) || d < 1 || d > days_in(y, mo) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    let mut rest = &s[19..];
    if let Some(frac) = rest.strip_prefix('.') {
        let n = frac.bytes().take_while(u8::is_ascii_digit).count();
        if n == 0 {
            return None;
        }
        rest = &frac[n..];
    }
    let offset = match rest {
        "Z" | "z" => 0,
        _ => {
            let rb = rest.as_bytes();
            if rb.len() != 6 || !matches!(rb[0], b'+' | b'-') || rb[3] != b':' {
                return None;
            }
            let (oh, om) = (
                num(20 + n_frac(s)..22 + n_frac(s))?,
                num(23 + n_frac(s)..25 + n_frac(s))?,
            );
            if oh > 23 || om > 59 {
                return None;
            }
            let o = oh * 3600 + om * 60;
            if rb[0] == b'-' {
                -o
            } else {
                o
            }
        }
    };
    Some(days_from_civil(y, mo, d) * 86400 + h * 3600 + mi * 60 + sec - offset)
}

/// The length of the fractional-seconds part (`.` and digits) after the
/// seconds of an RFC 3339 time.
fn n_frac(s: &str) -> usize {
    match s.get(19..).and_then(|r| r.strip_prefix('.')) {
        Some(f) => 1 + f.bytes().take_while(u8::is_ascii_digit).count(),
        None => 0,
    }
}

fn days_in(y: i64, m: i64) -> i64 {
    match m {
        4 | 6 | 9 | 11 => 30,
        2 if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        2 => 28,
        _ => 31,
    }
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's
/// algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// The month file an attempt started in: `YYYY-MM`.
pub fn month(secs: i64) -> String {
    fmt(secs)[..7].to_string()
}

/// `YYYYMMDDTHHMMSSZ`, a request file's leading time.
pub fn compact(secs: i64) -> String {
    let t = fmt(secs);
    format!(
        "{}{}{}T{}{}{}Z",
        &t[0..4],
        &t[5..7],
        &t[8..10],
        &t[11..13],
        &t[14..16],
        &t[17..19]
    )
}

/// A duration a person types: `90s`, `30m`, `6h`, `1d`, or seconds.
pub fn parse_duration(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let (n, unit) = match s.find(|c: char| !c.is_ascii_digit()) {
        Some(i) => (&s[..i], &s[i..]),
        None => (s, "s"),
    };
    let n: u64 = n
        .parse()
        .map_err(|_| format!("`{s}` is not a duration (90s, 30m, 6h, 1d)"))?;
    let mult = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86400,
        _ => return Err(format!("`{s}` is not a duration (90s, 30m, 6h, 1d)")),
    };
    n.checked_mul(mult)
        .filter(|v| *v > 0)
        .ok_or_else(|| format!("`{s}` is not a duration (90s, 30m, 6h, 1d)"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_times_read_back_as_written() {
        for secs in [0, 1_795_000_000, 1_800_000_123] {
            assert_eq!(parse(&fmt(secs)), Some(secs));
        }
        assert_eq!(parse("1970-01-01T00:00:00Z"), Some(0));
        assert!(parse("2024-02-29T00:00:00Z").is_some());
        assert_eq!(
            parse("2026-07-12T05:00:00.25+02:00"),
            parse("2026-07-12T03:00:00Z")
        );
        assert_eq!(
            parse("2026-07-12T03:00:00.123Z"),
            parse("2026-07-12T03:00:00Z")
        );
        assert_eq!(
            parse("2026-07-12T05:00:00+02:00"),
            parse("2026-07-12T03:00:00Z")
        );
        assert_eq!(
            parse("2026-07-11T22:00:00-05:00"),
            parse("2026-07-12T03:00:00Z")
        );
        for bad in [
            "",
            "2026-07-12",
            "2026-13-01T00:00:00Z",
            "2026-07-12T03:00:00",
            "soon",
            "2026-02-31T00:00:00Z",
            "2025-02-29T00:00:00Z",
            "2026-04-31T00:00:00Z",
            "2026-07-12T03:00:00+-1:00",
            "2026-07-12T03:00:00++1:00",
            "2026-07-12T03:00:00+24:00",
            "2026-07-12T03:00:00+01:60",
            "2026-07-12T03:00:00.5+-1:00",
        ] {
            assert_eq!(parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn months_compact_times_and_durations() {
        let t = parse("2026-11-22T20:03:12Z").unwrap();
        assert_eq!(month(t), "2026-11");
        assert_eq!(compact(t), "20261122T200312Z");
        assert_eq!(parse_duration("90").unwrap(), 90);
        assert_eq!(parse_duration("30m").unwrap(), 1800);
        assert_eq!(parse_duration("6h").unwrap(), 21600);
        assert_eq!(parse_duration("1d").unwrap(), 86400);
        for bad in ["", "0", "h", "1w", "-1h"] {
            assert!(parse_duration(bad).is_err(), "{bad}");
        }
    }
}
