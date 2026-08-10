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
    let secs = (now_unix() - t).max(0);
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
