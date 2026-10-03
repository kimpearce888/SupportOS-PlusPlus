//! Business-hours engine — the port of `src/server/analytics/businessHours.ts`
//! (v1.4.0): pure, dependency-free time math.
//!
//! Design decisions (mirrored from the reference):
//! - All computation happens on INSTANTS (epoch ms) converted through the
//!   mailbox's IANA timezone, so daylight-saving transitions are handled by
//!   the tz database (the reference uses `Intl` + the platform ICU; the port
//!   uses `chrono-tz` + the same IANA database, the standard Rust equivalent
//!   already used by the settings routes).
//! - The wall->instant conversion ports the reference's classic
//!   guess-and-correct trick verbatim (two passes converge for all real
//!   zones; for DST gaps and ambiguous wall times it lands on the earliest
//!   possible instant).
//! - Business minutes are REAL minutes that fall inside the configured
//!   schedule: nights, weekends and non-configured weekdays contribute zero.
//!   A reply that arrives Saturday 9am after a Friday 5pm ticket has aged
//!   ZERO business minutes - which is the honest answer for SLA purposes.
//! - The engine never panics for weird zones: invalid timezones surface as
//!   `None` and the caller (the SLA service) falls back to wall minutes,
//!   labeled as such in the report.
//!
//! Port mapping notes:
//! - `days` keeps JS `Date.getDay()` semantics: 0=Sunday .. 6=Saturday.
//! - `Date.UTC(y, mo-1, d, h, mi)` rollover (e.g. minute 1440 → next day
//!   midnight) is reproduced via `NaiveDate + Duration::minutes`.
//! - `Date.parse` is approximated by [`parse_instant_ms`]: RFC 3339
//!   (with/without offset), zoneless ISO (read as UTC), date-only (UTC
//!   midnight, the JS rule) and SQLite's `YYYY-MM-DD HH:MM:SS` shape.
//!   Anything else is `None` — the reference's `NaN` honest-fallback path.

use chrono::{Datelike, NaiveDate, NaiveDateTime, TimeZone, Timelike, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// The per-mailbox business-hours schedule (reference
/// `BusinessHoursConfig`). `days` uses JS `getDay()` values: 0=Sunday .. 6=Saturday.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BusinessHoursConfig {
    /// IANA timezone, e.g. 'America/New_York'.
    pub timezone: String,
    /// Active weekdays as JS `Date.getDay()` values: 0=Sunday .. 6=Saturday.
    pub days: Vec<i64>,
    /// Schedule start, minutes after local midnight (e.g. 540 = 09:00).
    pub start_minute: i64,
    /// Schedule end, minutes after local midnight (e.g. 1020 = 17:00).
    pub end_minute: i64,
}

/// Per-mailbox SLA targets (reference `SlaTargets`): minutes, `None` = not set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SlaTargets {
    /// First-response target in business minutes.
    pub first_response_target_min: Option<i64>,
    /// Resolution target in business minutes.
    pub resolution_target_min: Option<i64>,
}

/// The default schedule (reference `DEFAULT_BUSINESS_HOURS` const: UTC,
/// Mon-Fri 9-17). A Rust `const` cannot own `String`/`Vec`, so the constant
/// is surfaced as a constructor.
#[must_use]
pub fn default_business_hours() -> BusinessHoursConfig {
    BusinessHoursConfig {
        timezone: "UTC".to_string(),
        days: vec![1, 2, 3, 4, 5],
        start_minute: 540,
        end_minute: 1020,
    }
}

/// Safety bound: spans longer than this fall back to wall minutes
/// (reference `MAX_SPAN_DAYS`).
const MAX_SPAN_DAYS: i64 = 400;

/// SLA verdict for a measured duration against a target (reference `slaStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlaVerdict {
    /// Measured duration at or under the target.
    Met,
    /// Measured duration over the target.
    Missed,
    /// A duration exists but no target is configured.
    NoTarget,
    /// The duration is unknown (invalid input, untrusted span).
    Unmeasured,
}

impl SlaVerdict {
    /// The reference's string values (used by tests and debugging).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Met => "met",
            Self::Missed => "missed",
            Self::NoTarget => "no_target",
            Self::Unmeasured => "unmeasured",
        }
    }
}

/// True when the zone resolves (reference `isValidTimezone` — guards the
/// wall-minutes fallback path for bad input). `chrono-tz` embeds the IANA
/// database, the Rust equivalent of the reference's `Intl.DateTimeFormat`
/// probe.
#[must_use]
pub fn is_valid_timezone(timezone: &str) -> bool {
    timezone.parse::<Tz>().is_ok()
}

/// Wall-clock parts of an instant in a timezone (reference `partsInZone`).
/// Minute-of-day is minute-granular — the reference reads only hour+minute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WallParts {
    y: i32,
    mo: u32,
    d: u32,
    /// Minutes after local midnight.
    mi: i64,
    /// JS `getDay()`: 0=Sunday .. 6=Saturday.
    dow: u8,
}

fn parts_in_zone(ts_ms: i64, tz: &Tz) -> Option<WallParts> {
    let utc = Utc.timestamp_millis_opt(ts_ms).single()?;
    let local = utc.with_timezone(tz);
    Some(WallParts {
        y: local.year(),
        mo: local.month(),
        d: local.day(),
        mi: i64::from(local.hour()) * 60 + i64::from(local.minute()),
        dow: local.weekday().num_days_from_sunday() as u8,
    })
}

/// `Date.UTC(y, mo-1, d, h, mi)` in epoch ms — with the same rollover
/// semantics (minute may exceed 1440 and rolls into the next day).
fn naive_utc_ms(y: i32, mo: u32, d: u32, minute: i64) -> Option<i64> {
    let midnight = NaiveDate::from_ymd_opt(y, mo, d)?.and_hms_opt(0, 0, 0)?;
    let dt = midnight + chrono::Duration::minutes(minute);
    Some(dt.and_utc().timestamp_millis())
}

/// Offset (minutes) to ADD to an epoch ms value to get wall time in the zone
/// (reference `offsetAt`). Minute-granular like the reference's version.
fn offset_minutes_at(ts_ms: i64, tz: &Tz) -> Option<f64> {
    let p = parts_in_zone(ts_ms, tz)?;
    let as_utc = naive_utc_ms(p.y, p.mo, p.d, p.mi)?;
    Some((as_utc - ts_ms) as f64 / 60_000.0)
}

/// Epoch ms for a wall-clock day + minute-of-day in a timezone (DST-aware).
///
/// Ports the reference's `instantForWall` two-pass guess-and-correct verbatim:
/// start from the wall time read as UTC, then twice subtract the offset
/// observed at that instant. For every real zone the second pass converges;
/// for DST gaps and ambiguous wall times it resolves to the EARLIEST possible
/// instant, matching the reference's arithmetic exactly.
fn instant_for_wall(y: i32, mo: u32, d: u32, minute: i64, tz: &Tz) -> Option<i64> {
    let naive = naive_utc_ms(y, mo, d, minute)?;
    let mut ts = naive;
    for _ in 0..2 {
        let off = offset_minutes_at(ts, tz)?;
        ts = naive - (off * 60_000.0) as i64;
    }
    Some(ts)
}

/// `Date.parse` for the shapes the SLA engine meets: RFC 3339 (Z or numeric
/// offset, optional fractional seconds), zoneless ISO read as UTC, date-only
/// as UTC midnight, and SQLite's `YYYY-MM-DD HH:MM:SS`. `None` = NaN (the
/// reference's honest "cannot parse" answer).
#[must_use]
pub fn parse_instant_ms(raw: &str) -> Option<i64> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    // Date-only forms are UTC in JS Date.parse.
    if let Ok(day) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return day
            .and_hms_opt(0, 0, 0)
            .map(|ndt| ndt.and_utc().timestamp_millis());
    }
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(dt.timestamp_millis());
    }
    if let Ok(ndt) = NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f") {
        return Some(ndt.and_utc().timestamp_millis());
    }
    if let Ok(ndt) = NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%.f") {
        return Some(ndt.and_utc().timestamp_millis());
    }
    None
}

/// Real minutes between two instants that fall inside the schedule
/// (reference `businessMinutesBetween`).
///
/// Returns `None` when the computation cannot be trusted (unparsable input,
/// `end < start`, invalid timezone, a span beyond [`MAX_SPAN_DAYS`], no
/// active weekdays, or a non-positive window) — the caller then falls back to
/// wall minutes and says so in the report.
#[must_use]
pub fn business_minutes_between(
    start_iso: &str,
    end_iso: &str,
    cfg: &BusinessHoursConfig,
) -> Option<f64> {
    let start = parse_instant_ms(start_iso)?;
    let end = parse_instant_ms(end_iso)?;
    if end < start {
        return None;
    }
    let tz: Tz = cfg.timezone.parse().ok()?;
    if end - start > MAX_SPAN_DAYS * 86_400_000 {
        return None;
    }

    let active: HashSet<i64> = cfg
        .days
        .iter()
        .copied()
        .filter(|d| (0..=6).contains(d))
        .collect();
    if active.is_empty() {
        return None;
    }
    let day_start = cfg.start_minute.clamp(0, 1440);
    let day_end = std::cmp::max(day_start, cfg.end_minute.clamp(0, 1440));
    if day_end <= day_start {
        return None;
    }

    // Walk local calendar days from the start's day to the end's day.
    let start_parts = parts_in_zone(start, &tz)?;
    let end_parts = parts_in_zone(end, &tz)?;
    let (mut cy, mut cmo, mut cd) = (start_parts.y, start_parts.mo, start_parts.d);
    let mut total_ms: i64 = 0;
    for _guard in 0..=(MAX_SPAN_DAYS + 2) {
        let day_ts = instant_for_wall(cy, cmo, cd, 0, &tz)?;
        if active.contains(&i64::from(parts_in_zone(day_ts, &tz)?.dow)) {
            let win_start = instant_for_wall(cy, cmo, cd, day_start, &tz)?;
            let win_end = instant_for_wall(cy, cmo, cd, day_end, &tz)?;
            let overlap_start = std::cmp::max(start, win_start);
            let overlap_end = std::cmp::min(end, win_end);
            if overlap_end > overlap_start {
                total_ms += overlap_end - overlap_start;
            }
        }
        if (cy, cmo, cd) == (end_parts.y, end_parts.mo, end_parts.d) {
            break;
        }
        // next calendar day
        let next_midnight = instant_for_wall(cy, cmo, cd, 1439, &tz)? + 60_000;
        let np = parts_in_zone(next_midnight, &tz)?;
        cy = np.y;
        cmo = np.mo;
        cd = np.d;
    }
    Some(total_ms as f64 / 60_000.0)
}

/// Wall-clock minutes between two instants (reference `wallMinutesBetween` —
/// the honest fallback / comparison).
#[must_use]
pub fn wall_minutes_between(start_iso: &str, end_iso: &str) -> Option<f64> {
    let start = parse_instant_ms(start_iso)?;
    let end = parse_instant_ms(end_iso)?;
    if end < start {
        return None;
    }
    Some((end - start) as f64 / 60_000.0)
}

/// SLA verdict for a measured duration against a target (reference
/// `slaStatus`): `minutes <= target` is met, no target is `no_target`
/// (when a duration exists), unknown duration is `unmeasured`.
#[must_use]
pub fn sla_status(minutes: Option<f64>, target_min: Option<i64>) -> SlaVerdict {
    match (target_min, minutes) {
        (None, Some(_)) => SlaVerdict::NoTarget,
        (None, None) => SlaVerdict::Unmeasured,
        (Some(_), None) => SlaVerdict::Unmeasured,
        (Some(target), Some(minutes)) => {
            if minutes <= target as f64 {
                SlaVerdict::Met
            } else {
                SlaVerdict::Missed
            }
        }
    }
}

/// Minutes since an instant, business-adjusted (reference
/// `businessMinutesSince` — "currently waiting" aging up to now).
#[must_use]
pub fn business_minutes_since(iso: &str, cfg: &BusinessHoursConfig) -> Option<f64> {
    let now = now_iso_millis();
    business_minutes_between(iso, &now, cfg)
}

/// `new Date().toISOString()` — UTC with millisecond precision, the exact
/// format the reference's route layer and `generated_at` fields emit.
#[must_use]
pub fn now_iso_millis() -> String {
    Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

/// `new Date(Date.now() - days * 86400000).toISOString()` (reference
/// `isoDaysAgo`).
#[must_use]
pub fn iso_days_ago(days: i64) -> String {
    let ts = Utc::now().timestamp_millis() - days * 86_400_000;
    Utc.timestamp_millis_opt(ts)
        .single()
        .map(|t| t.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
        .unwrap_or_else(|| Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Ports tests/unit/businessHours.test.ts (v1.4.0 SLA engine: pure
    // business-hours math. Every case pins behavior the SLA report depends
    // on: weekend/night exclusion, DST transitions, half-hour zones, and the
    // honest null fallbacks).
    //
    // 2026 calendar anchors (all UTC unless stated):
    // Mar 6 2026 = Friday, Mar 7 = Saturday, Mar 8 = Sunday (US DST starts),
    // Mar 9 = Monday.

    fn weekdays_9_17() -> BusinessHoursConfig {
        BusinessHoursConfig {
            timezone: "UTC".to_string(),
            days: vec![1, 2, 3, 4, 5],
            start_minute: 540,
            end_minute: 1020,
        }
    }

    fn bm(start: &str, end: &str, cfg: &BusinessHoursConfig) -> Option<f64> {
        business_minutes_between(start, end, cfg)
    }

    #[test]
    fn counts_minutes_inside_the_window_on_the_same_day() {
        assert_eq!(
            bm(
                "2026-03-06T12:00:00Z",
                "2026-03-06T15:00:00Z",
                &weekdays_9_17()
            ),
            Some(180.0)
        );
    }

    #[test]
    fn excludes_time_before_the_window_opens() {
        // Fri 07:00 -> 09:30 crosses the open: only 09:00-09:30 counts
        assert_eq!(
            bm(
                "2026-03-06T07:00:00Z",
                "2026-03-06T09:30:00Z",
                &weekdays_9_17()
            ),
            Some(30.0)
        );
    }

    #[test]
    fn excludes_time_after_the_window_closes() {
        // Fri 16:50 -> 18:00: only 16:50-17:00 counts
        assert_eq!(
            bm(
                "2026-03-06T16:50:00Z",
                "2026-03-06T18:00:00Z",
                &weekdays_9_17()
            ),
            Some(10.0)
        );
    }

    #[test]
    fn weekend_and_night_contribute_zero() {
        // Friday 17:05 -> Monday 09:05 = 5 minutes
        assert_eq!(
            bm(
                "2026-03-06T17:05:00Z",
                "2026-03-09T09:05:00Z",
                &weekdays_9_17()
            ),
            Some(5.0)
        );
    }

    #[test]
    fn sums_a_multi_day_span() {
        // Fri 12:00-17:00 = 300; Mon 9-17 = 480; Tue 9-12 = 180 -> 960
        assert_eq!(
            bm(
                "2026-03-06T12:00:00Z",
                "2026-03-10T12:00:00Z",
                &weekdays_9_17()
            ),
            Some(960.0)
        );
    }

    #[test]
    fn skips_configured_off_weekdays_entirely() {
        let mon_only = BusinessHoursConfig {
            days: vec![1],
            ..weekdays_9_17().clone()
        };
        // Fri 12:00 -> Tue 12:00 -> only Monday 9-17 counts = 480
        assert_eq!(
            bm("2026-03-06T12:00:00Z", "2026-03-10T12:00:00Z", &mon_only),
            Some(480.0)
        );
    }

    #[test]
    fn measures_the_wall_clock_alongside() {
        // (for honesty in the report)
        assert_eq!(
            wall_minutes_between("2026-03-06T12:00:00Z", "2026-03-10T12:00:00Z"),
            Some(4.0 * 24.0 * 60.0)
        );
    }

    #[test]
    fn handles_a_dst_spring_forward_transition() {
        // (America/New_York, spring forward 2026-03-08)
        // Fri Mar 6 16:00 EST (UTC-5) -> Mon Mar 9 10:00 EDT (UTC-4)
        // Business minutes: Fri 16:00-17:00 (60) + Mon 09:00-10:00 (60) = 120
        let ny = BusinessHoursConfig {
            timezone: "America/New_York".to_string(),
            days: vec![1, 2, 3, 4, 5],
            start_minute: 540,
            end_minute: 1020,
        };
        assert_eq!(
            bm("2026-03-06T21:00:00Z", "2026-03-09T14:00:00Z", &ny),
            Some(120.0)
        );
    }

    #[test]
    fn handles_a_dst_fall_back_transition() {
        // America/New_York, fall back 2026-11-01 (02:00 EDT -> 01:00 EST).
        // Fri Oct 30 16:30 EDT (20:30Z) -> Mon Nov 2 09:30 EST (14:30Z):
        // Fri 16:30-17:00 (30) + Mon 09:00-09:30 (30) = 60 business minutes
        // while the wall clock spans 3 days + 18 hours.
        let ny = BusinessHoursConfig {
            timezone: "America/New_York".to_string(),
            days: vec![1, 2, 3, 4, 5],
            start_minute: 540,
            end_minute: 1020,
        };
        assert_eq!(
            bm("2026-10-30T20:30:00Z", "2026-11-02T14:30:00Z", &ny),
            Some(60.0)
        );
    }

    #[test]
    fn handles_half_hour_offset_zones() {
        // (Asia/Kolkata, UTC+5:30)
        // Kolkata window 09:00-17:00 IST = 03:30-11:30 UTC
        let kolkata = BusinessHoursConfig {
            timezone: "Asia/Kolkata".to_string(),
            days: vec![1, 2, 3, 4, 5],
            start_minute: 540,
            end_minute: 1020,
        };
        // Mon Mar 9 05:00 UTC = 10:30 IST -> 06:30 UTC = 12:00 IST = 90 minutes
        assert_eq!(
            bm("2026-03-09T05:00:00Z", "2026-03-09T06:30:00Z", &kolkata),
            Some(90.0)
        );
    }

    #[test]
    fn returns_none_for_invalid_input_instead_of_lying() {
        // (honest fallback)
        let bad_tz = BusinessHoursConfig {
            timezone: "Not/AZone".to_string(),
            ..weekdays_9_17().clone()
        };
        let no_days = BusinessHoursConfig {
            days: vec![],
            ..weekdays_9_17().clone()
        };
        let no_window = BusinessHoursConfig {
            end_minute: 540,
            ..weekdays_9_17().clone()
        };
        assert_eq!(
            bm("2026-03-06T12:00:00Z", "2026-03-06T15:00:00Z", &bad_tz),
            None
        );
        assert_eq!(
            bm("2026-03-06T12:00:00Z", "2026-03-06T15:00:00Z", &no_days),
            None
        );
        assert_eq!(
            bm("2026-03-06T12:00:00Z", "2026-03-06T15:00:00Z", &no_window),
            None
        );
        assert_eq!(bm("nope", "2026-03-06T15:00:00Z", &weekdays_9_17()), None);
        assert_eq!(
            bm(
                "2026-03-06T15:00:00Z",
                "2026-03-06T12:00:00Z",
                &weekdays_9_17()
            ),
            None
        );
    }

    #[test]
    fn caps_absurdly_long_spans_with_none() {
        // (bounded computation)
        assert_eq!(
            bm(
                "2020-01-01T00:00:00Z",
                "2030-01-01T00:00:00Z",
                &weekdays_9_17()
            ),
            None
        );
    }

    #[test]
    fn validates_timezones_without_panicking() {
        assert!(is_valid_timezone("Europe/Berlin"));
        assert!(!is_valid_timezone("Mars/Olympus"));
    }

    #[test]
    fn business_minutes_since_measures_aging_up_to_now() {
        let an_hour_ago = {
            let ts = Utc::now().timestamp_millis() - 3_600_000;
            Utc.timestamp_millis_opt(ts)
                .single()
                .unwrap()
                .format("%Y-%m-%dT%H:%M:%S%.3fZ")
                .to_string()
        };
        let aged = business_minutes_since(&an_hour_ago, &weekdays_9_17());
        assert!(aged.is_some_and(|m| m >= 0.0), "aged: {aged:?}");
    }

    #[test]
    fn all_day_schedule_counts_everything() {
        // 24/7 config (the e2e tests configure exactly this): business == wall.
        let all_day = BusinessHoursConfig {
            timezone: "UTC".to_string(),
            days: vec![0, 1, 2, 3, 4, 5, 6],
            start_minute: 0,
            end_minute: 1440,
        };
        assert_eq!(
            bm("2026-03-06T12:00:00Z", "2026-03-09T12:00:00Z", &all_day),
            Some(3.0 * 24.0 * 60.0)
        );
    }

    #[test]
    fn closed_schedule_returns_none() {
        // days configured but the window is empty (start == end) and an
        // overnight window (end < start) both degrade to the wall fallback.
        let empty_window = BusinessHoursConfig {
            timezone: "UTC".to_string(),
            days: vec![1, 2, 3, 4, 5],
            start_minute: 540,
            end_minute: 540,
        };
        let overnight = BusinessHoursConfig {
            timezone: "UTC".to_string(),
            days: vec![1, 2, 3, 4, 5],
            start_minute: 1020,
            end_minute: 540,
        };
        assert_eq!(
            bm(
                "2026-03-06T12:00:00Z",
                "2026-03-09T12:00:00Z",
                &empty_window
            ),
            None
        );
        assert_eq!(
            bm("2026-03-06T12:00:00Z", "2026-03-09T12:00:00Z", &overnight),
            None
        );
    }

    #[test]
    fn sub_minute_instants_count_fractional_minutes() {
        // Seconds inside the window are real ms overlap, like the reference.
        assert_eq!(
            bm(
                "2026-03-06T12:00:00Z",
                "2026-03-06T12:00:30Z",
                &weekdays_9_17()
            ),
            Some(0.5)
        );
    }

    // ---- slaStatus (reference describe block) ------------------------------

    #[test]
    fn sla_status_classifies_against_the_target() {
        assert_eq!(sla_status(Some(10.0), Some(60)), SlaVerdict::Met);
        assert_eq!(sla_status(Some(60.0), Some(60)), SlaVerdict::Met);
        assert_eq!(sla_status(Some(61.0), Some(60)), SlaVerdict::Missed);
    }

    #[test]
    fn sla_status_reports_no_target_when_no_target_is_configured() {
        assert_eq!(sla_status(Some(999.0), None), SlaVerdict::NoTarget);
    }

    #[test]
    fn sla_status_reports_unmeasured_when_the_duration_is_unknown() {
        assert_eq!(sla_status(None, Some(60)), SlaVerdict::Unmeasured);
        assert_eq!(sla_status(None, None), SlaVerdict::Unmeasured);
    }
}
