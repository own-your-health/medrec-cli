//! FHIR `instant`, `dateTime` and `date` values as points in time, so two
//! versions of a resource compare by when they were current, whatever
//! offset or precision each source writes.

/// Seconds since the Unix epoch and nanoseconds: `YYYY-MM-DD` (its start,
/// UTC), or `YYYY-MM-DDThh:mm[:ss[.f…]]` with `Z` or `±hh:mm`. `None` for
/// anything else, a date-time without a zone included.
pub fn instant(s: &str) -> Option<(i64, u32)> {
    let s = s.trim();
    let date = s.get(..10)?;
    let days = days(date)?;
    let rest = &s[10..];
    if rest.is_empty() {
        return Some((days * 86_400, 0));
    }
    let rest = rest.strip_prefix(['T', 't'])?;
    let zone_at = rest.find(['Z', 'z', '+', '-'])?;
    let (clock, zone) = rest.split_at(zone_at);
    let offset = match zone {
        "Z" | "z" => 0,
        _ => {
            let sign = if zone.starts_with('-') { -1 } else { 1 };
            let (h, m) = zone[1..].split_once(':')?;
            sign * (field(h, 0, 23)? * 3600 + field(m, 0, 59)? * 60)
        }
    };
    let mut parts = clock.splitn(3, ':');
    let hour = field(parts.next()?, 0, 23)?;
    let minute = field(parts.next()?, 0, 59)?;
    let (second, nanos) = match parts.next() {
        None => (0, 0),
        Some(sec) => {
            let (whole, frac) = sec.split_once('.').unwrap_or((sec, ""));
            if !frac.chars().all(|c| c.is_ascii_digit()) || frac.len() > 9 {
                return None;
            }
            let nanos = if frac.is_empty() {
                0
            } else {
                format!("{frac:0<9}").parse::<u32>().ok()?
            };
            // A leap second counts as the last second of its minute.
            (field(whole, 0, 60)?.min(59), nanos)
        }
    };
    Some((
        days * 86_400 + hour * 3600 + minute * 60 + second - offset,
        nanos,
    ))
}

/// Whether `a` is strictly earlier than `b`, when both are readable.
pub fn earlier(a: Option<&str>, b: Option<&str>) -> bool {
    match (a.and_then(instant), b.and_then(instant)) {
        (Some(a), Some(b)) => a < b,
        _ => false,
    }
}

fn field(s: &str, min: i64, max: i64) -> Option<i64> {
    if s.len() != 2 || !s.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let n: i64 = s.parse().ok()?;
    (min..=max).contains(&n).then_some(n)
}

/// Days from 1970-01-01 to an ISO date (proleptic Gregorian).
fn days(date: &str) -> Option<i64> {
    let civil = pk_cli_core::dates::parse_iso(date).ok()?;
    let (y, m, d) = (civil.0, i64::from(civil.1), i64::from(civil.2));
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let n = era * 146_097 + doe - 719_468;
    // `parse_iso` takes any day up to 31: a day the month lacks is refused.
    (pk_cli_core::dates::civil_from_unix(n * 86_400) == civil).then_some(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_time_reads_the_same_in_any_zone_and_precision() {
        assert_eq!(instant("1970-01-01"), Some((0, 0)));
        assert_eq!(instant("1970-01-01T00:00:00Z"), Some((0, 0)));
        let t = instant("2026-03-10T14:30:00Z").unwrap();
        assert_eq!(instant("2026-03-10T09:30:00-05:00"), Some(t));
        assert_eq!(instant("2026-03-10T16:30:00+02:00"), Some(t));
        assert_eq!(instant("2026-03-10T14:30Z"), Some(t));
        assert_eq!(instant("2026-03-10T14:30:00.25Z"), Some((t.0, 250_000_000)));
        assert!(earlier(
            Some("2026-03-10T14:30:00Z"),
            Some("2026-03-10T14:30:00.5Z")
        ));
        assert!(earlier(Some("2026-03-10"), Some("2026-03-10T00:00:01Z")));
        assert!(!earlier(
            Some("2026-03-10T14:30:00Z"),
            Some("2026-03-10T09:30:00-05:00")
        ));
        assert!(!earlier(None, Some("2026-03-10")));
        assert!(!earlier(Some("2026-03-10"), Some("not a time")));
        for bad in [
            "2026-03-10T14:30:00",
            "2026-13-01",
            "2026-02-30",
            "2026-03-10T24:00:00Z",
            "2026-03-10T14:3:00Z",
            "2026-03-10T14:30:00.x1Z",
            "2026-03-10 14:30:00Z",
            "",
        ] {
            assert_eq!(instant(bad), None, "{bad}");
        }
        // Agrees with the formatter at every day of several centuries.
        for secs in (-4_000_000_000i64..8_000_000_000).step_by(86_400 * 37 + 3_601) {
            let s = pk_cli_core::dates::fmt_rfc3339(secs);
            assert_eq!(instant(&s), Some((secs, 0)), "{s}");
        }
    }
}
