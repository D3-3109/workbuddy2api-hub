//! Time helpers. "Local" means the process timezone (`TZ`), as in Python's
//! `time.localtime`.

use chrono::{
    Datelike, Duration, Local, LocalResult, NaiveDate, NaiveDateTime, TimeZone, Timelike, Utc,
};

/// `time.time()`
pub fn now() -> f64 {
    let n = Utc::now();
    n.timestamp() as f64 + n.timestamp_subsec_micros() as f64 / 1e6
}

pub fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

fn local_from_naive(dt: NaiveDateTime) -> f64 {
    match Local.from_local_datetime(&dt) {
        LocalResult::Single(t) => t.timestamp() as f64,
        LocalResult::Ambiguous(a, _) => a.timestamp() as f64,
        LocalResult::None => Local.from_utc_datetime(&dt).timestamp() as f64,
    }
}

/// Local midnight `days_back` days before `ts`.
pub fn local_midnight(ts: f64, days_back: i64) -> f64 {
    let lt = Local
        .timestamp_opt(ts as i64, 0)
        .single()
        .unwrap_or_else(Local::now);
    let d = lt.date_naive() - Duration::days(days_back);
    local_from_naive(d.and_hms_opt(0, 0, 0).unwrap())
}

pub fn today_midnight() -> f64 {
    local_midnight(now(), 0)
}

/// Seconds until the next local midnight, at least 60.
pub fn seconds_until_local_midnight() -> i64 {
    let n = now();
    let next = local_midnight(n, -1);
    ((next - n) as i64).max(60)
}

/// Next local 04:00 (today if it is still before 04:00).
pub fn next_local_4am() -> f64 {
    let n = Local::now();
    let today4 = n.date_naive().and_hms_opt(4, 0, 0).unwrap();
    let target = if n.naive_local() < today4 {
        today4
    } else {
        today4 + Duration::days(1)
    };
    local_from_naive(target)
}

/// Local Monday 00:00 of the current week.
pub fn local_week_start() -> f64 {
    let n = Local::now();
    let d = n.date_naive() - Duration::days(n.weekday().num_days_from_monday() as i64);
    local_from_naive(d.and_hms_opt(0, 0, 0).unwrap())
}

/// Local 1st of month 00:00.
pub fn local_month_start() -> f64 {
    let n = Local::now();
    let d = NaiveDate::from_ymd_opt(n.year(), n.month(), 1).unwrap();
    local_from_naive(d.and_hms_opt(0, 0, 0).unwrap())
}

/// Start of the local hour containing `ts`.
pub fn local_hour_start(ts: f64) -> f64 {
    let lt = Local
        .timestamp_opt(ts as i64, 0)
        .single()
        .unwrap_or_else(Local::now);
    let naive = lt
        .naive_local()
        .with_minute(0)
        .unwrap()
        .with_second(0)
        .unwrap();
    local_from_naive(naive)
}

/// `time.strftime("%Y-%m-%dT%H:%M:%S", time.localtime(ts))`
pub fn local_iso(ts: f64) -> String {
    fmt_local(ts, "%Y-%m-%dT%H:%M:%S")
}

/// `time.strftime("%Y-%m-%d %H:%M:%S", time.localtime(ts))`
pub fn local_stamp(ts: f64) -> String {
    fmt_local(ts, "%Y-%m-%d %H:%M:%S")
}

pub fn fmt_local(ts: f64, fmt: &str) -> String {
    Local
        .timestamp_opt(ts as i64, 0)
        .single()
        .map(|t| t.format(fmt).to_string())
        .unwrap_or_default()
}

pub fn local_day_key(ts: f64) -> String {
    fmt_local(ts, "%Y-%m-%d")
}

pub fn local_hour(ts: f64) -> u32 {
    Local
        .timestamp_opt(ts as i64, 0)
        .single()
        .map(|t| t.hour())
        .unwrap_or(0)
}

pub fn local_minute(ts: f64) -> u32 {
    Local
        .timestamp_opt(ts as i64, 0)
        .single()
        .map(|t| t.minute())
        .unwrap_or(0)
}

/// Parse `YYYY-MM-DD HH:MM:SS` (or `T` separator; first 19 chars) as local time.
pub fn parse_local_stamp(s: &str) -> Option<f64> {
    let s = s.trim().replace('T', " ");
    let s = s.get(..19)?;
    let dt = NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").ok()?;
    Some(local_from_naive(dt))
}

/// Parse `YYYY-MM-DD HH:MM:SS` as UTC, returning epoch seconds (`calendar.timegm`).
pub fn timegm_stamp(s: &str) -> Option<f64> {
    let s = s.trim().replace('T', " ");
    let dt = NaiveDateTime::parse_from_str(s.get(..19)?, "%Y-%m-%d %H:%M:%S").ok()?;
    Some(dt.and_utc().timestamp() as f64)
}

/// Next 00:00 in fixed UTC+8 (prompt degrade window).
pub fn next_utc8_midnight() -> f64 {
    let tz = chrono::FixedOffset::east_opt(8 * 3600).unwrap();
    let n = Utc::now().with_timezone(&tz);
    let next = (n.date_naive() + Duration::days(1))
        .and_hms_opt(0, 0, 0)
        .unwrap();
    tz.from_local_datetime(&next)
        .single()
        .map(|t| t.timestamp() as f64)
        .unwrap_or_else(now)
}

/// `_human_left` from wb_accounts.py.
pub fn human_left(secs: f64) -> String {
    let s = secs.max(0.0) as i64;
    if s < 60 {
        return format!("{s}s");
    }
    if s < 3600 {
        let (m, r) = (s / 60, s % 60);
        return if r == 0 {
            format!("{m}m")
        } else {
            format!("{m}m{r:02}s")
        };
    }
    if s < 86400 {
        let (h, m) = (s / 3600, (s % 3600) / 60);
        return if m == 0 {
            format!("{h}h")
        } else {
            format!("{h}h{m:02}m")
        };
    }
    format!("{:.1} days", secs / 86400.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_left_formats() {
        assert_eq!(human_left(5.0), "5s");
        assert_eq!(human_left(120.0), "2m");
        assert_eq!(human_left(125.0), "2m05s");
        assert_eq!(human_left(3600.0), "1h");
        assert_eq!(human_left(3720.0), "1h02m");
        assert_eq!(human_left(86400.0 * 2.5), "2.5 days");
    }

    #[test]
    fn timegm() {
        assert_eq!(timegm_stamp("2026-10-06 21:00:09"), Some(1791320409.0));
    }
}
