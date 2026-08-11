//! Timestamps. GitLab hands out ISO8601 strings; the UI wants ages.

/// Parse a UTC ISO8601 timestamp (e.g. "2026-08-05T14:30:00.000Z") into Unix
/// seconds. GitLab always returns UTC, so the offset is ignored.
fn parse_iso8601(s: &str) -> Option<i64> {
    if s.len() < 19 {
        return None;
    }
    let num = |a: usize, z: usize| s.get(a..z)?.parse::<i64>().ok();
    let year = num(0, 4)?;
    let month = num(5, 7)?;
    let day = num(8, 10)?;
    let hour = num(11, 13)?;
    let min = num(14, 16)?;
    let sec = num(17, 19)?;
    // days-from-civil (Howard Hinnant): number of days since 1970-01-01.
    let y = if month <= 2 { year - 1 } else { year };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days * 86400 + hour * 3600 + min * 60 + sec)
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Compact age from an ISO8601 timestamp: "just now" / "5m" / "3h" / "2d" / "4w".
pub(crate) fn rel_age(iso: &str) -> String {
    let Some(t) = parse_iso8601(iso) else {
        return "-".to_string();
    };
    rel_age_secs((now_unix() - t).max(0))
}

/// Same bucketing as `rel_age`, from a raw second count instead of an
/// ISO8601 string — for ages measured off something other than a GitLab
/// timestamp (e.g. a file's mtime).
pub(crate) fn rel_age_secs(secs: i64) -> String {
    if secs < 60 {
        "just now".to_string()
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        format!("{}h", secs / 3600)
    } else if secs < 7 * 86400 {
        format!("{}d", secs / 86400)
    } else {
        format!("{}w", secs / (7 * 86400))
    }
}

/// Whole days since the timestamp (used to highlight staleness).
pub(crate) fn age_days(iso: &str) -> i64 {
    match parse_iso8601(iso) {
        Some(t) => (now_unix() - t).max(0) / 86400,
        None => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rel_age_secs_buckets_by_magnitude() {
        assert_eq!(rel_age_secs(0), "just now");
        assert_eq!(rel_age_secs(59), "just now");
        assert_eq!(rel_age_secs(60), "1m");
        assert_eq!(rel_age_secs(3599), "59m");
        assert_eq!(rel_age_secs(3600), "1h");
        assert_eq!(rel_age_secs(86399), "23h");
        assert_eq!(rel_age_secs(86400), "1d");
        assert_eq!(rel_age_secs(7 * 86400 - 1), "6d");
        assert_eq!(rel_age_secs(7 * 86400), "1w");
    }

    #[test]
    fn rel_age_and_rel_age_secs_agree_on_a_known_timestamp() {
        // rel_age(iso) is rel_age_secs(now - t) — pin one data point without
        // depending on wall-clock time in the test itself: an ISO8601
        // timestamp far enough in the past that it always lands in "…w".
        let out = rel_age("2000-01-01T00:00:00.000Z");
        assert!(out.ends_with('w'), "{out}");
    }

    #[test]
    fn rel_age_falls_back_to_dash_on_unparseable_input() {
        assert_eq!(rel_age("not a timestamp"), "-");
        assert_eq!(rel_age(""), "-");
    }
}
