//! UTC timestamps without forking `date` and without a calendar dependency.
//!
//! `now_iso` used to shell out to `/bin/date -u` and end in
//! `.unwrap_or_default()`, so a failed fork produced an **empty string** — and
//! that value is the `ts` of every timeline event, every queue record, the lock
//! body and the credential manifest. `conversations::parse` sorts on it, so an
//! empty timestamp silently reordered history. A timestamp must not be able to
//! fail quietly, and it must not cost a process.

use std::time::{SystemTime, UNIX_EPOCH};

/// Seconds since the Unix epoch, clamped at 0.
pub fn now_unix() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// ISO-8601 UTC, second precision: `2026-07-24T10:43:12Z`.
///
/// A clock before the Unix epoch is not a case worth branching on: it clamps to
/// the epoch rather than returning an error nobody can act on.
pub fn now_iso() -> String {
    iso_from_unix(now_unix())
}

/// Format a Unix timestamp as ISO-8601 UTC.
///
/// Civil date from days is Howard Hinnant's `civil_from_days`, shifted to a
/// March-based year so leap day lands at the end of the cycle and no
/// special-casing of February is needed.
pub fn iso_from_unix(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    // Shift the epoch from 1970-01-01 to 0000-03-01.
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097); // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11], March = 0
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_the_epoch() {
        assert_eq!(iso_from_unix(0), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn formats_a_known_timestamp() {
        // 2026-07-24T10:43:12Z — cross-checked against `date -u -r 1784889792`.
        assert_eq!(iso_from_unix(1_784_889_792), "2026-07-24T10:43:12Z");
    }

    /// Leap day is the case a hand-rolled calendar gets wrong, and 2000 is the
    /// century that is a leap year while 1900 and 2100 are not.
    #[test]
    fn handles_leap_days() {
        assert_eq!(iso_from_unix(951_782_400), "2000-02-29T00:00:00Z");
        // 2024-02-29T23:59:59Z
        assert_eq!(iso_from_unix(1_709_251_199), "2024-02-29T23:59:59Z");
        // 2100 is NOT a leap year: 2100-02-28 is followed by 2100-03-01.
        assert_eq!(iso_from_unix(4_107_456_000), "2100-02-28T00:00:00Z");
        assert_eq!(iso_from_unix(4_107_456_000 + 86_400), "2100-03-01T00:00:00Z");
    }

    /// A year boundary exercises the March-shift in both directions at once.
    #[test]
    fn handles_year_boundaries() {
        assert_eq!(iso_from_unix(1_767_225_599), "2025-12-31T23:59:59Z");
        assert_eq!(iso_from_unix(1_767_225_600), "2026-01-01T00:00:00Z");
    }

    /// A clock before the epoch must still produce a parseable stamp rather
    /// than an empty string, which is the whole reason this module exists.
    #[test]
    fn a_pre_epoch_clock_still_formats() {
        assert_eq!(iso_from_unix(-86_400), "1969-12-31T00:00:00Z");
    }

    #[test]
    fn now_is_shaped_like_a_timestamp_and_never_empty() {
        let s = now_iso();
        assert_eq!(s.len(), 20, "expected YYYY-MM-DDTHH:MM:SSZ, got {s:?}");
        assert!(s.ends_with('Z'), "{s:?}");
        assert!(s.starts_with("20"), "{s:?}");
    }
}
