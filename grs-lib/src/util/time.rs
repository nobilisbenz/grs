//! Time helpers. Canonical time is epoch-milliseconds (integers); ISO-8601 is
//! only for human display (`timestamp_iso` in snap JSON, log output).

use chrono::{SecondsFormat, TimeZone, Utc};

/// Milliseconds since the Unix epoch.
pub type Millis = i64;

/// Current time as epoch milliseconds.
pub fn now_ms() -> Millis {
    Utc::now().timestamp_millis()
}

/// ISO-8601 (UTC, millisecond precision, `Z` suffix) for display.
pub fn iso(millis: Millis) -> String {
    Utc
        .timestamp_millis_opt(millis)
        .single()
        .map(|t| t.to_rfc3339_opts(SecondsFormat::Millis, true))
        .unwrap_or_default()
}

/// A rough, human-readable "time ago" string for `grs log`/`grs status`.
pub fn time_ago(millis: Millis, now: Millis) -> String {
    let mut delta = now - millis;
    if delta < 0 {
        delta = 0;
    }
    let secs = delta / 1000;
    if secs < 60 {
        return format!("{secs}s ago");
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins}m ago");
    }
    let hours = mins / 60;
    if hours < 24 {
        return format!("{hours}h ago");
    }
    let days = hours / 24;
    if days == 1 {
        return "yesterday".to_string();
    }
    if days < 30 {
        return format!("{days}d ago");
    }
    let months = days / 30;
    if months < 12 {
        return format!("{months}mo ago");
    }
    format!("{}y ago", days / 365)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_format() {
        // 1719236589441 ms = 2024-06-24T13:43:09.441Z (UTC)
        assert_eq!(iso(1719236589441), "2024-06-24T13:43:09.441Z");
    }

    #[test]
    fn time_ago_buckets() {
        let now = 10_000_000;
        assert_eq!(time_ago(now - 5_000, now), "5s ago");
        assert_eq!(time_ago(now - 120_000, now), "2m ago");
        assert_eq!(time_ago(now - 3_600_000, now), "1h ago");
        assert_eq!(time_ago(now - 86_400_000, now), "yesterday");
        assert_eq!(time_ago(now - 3 * 86_400_000, now), "3d ago");
    }
}
