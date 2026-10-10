//! Saved Inbox Views — a faithful port of the reference's
//! `src/server/inbox/viewEngine.ts` + `src/server/services/dateRange.ts` +
//! `src/server/database/repositories/inboxViewRepo.ts` (v1.7.0/v1.8.0).
//!
//! Safety model (reference plan Phase 6: "structured condition definitions,
//! NOT generated SQL"):
//! - Stored definitions are pure JSON trees (schema-validated at the route
//!   boundary by [`parse_view_definition`], the Zod port).
//! - At evaluation time each node compiles to a fixed SQL template chosen from
//!   a closed match on `kind`; every VALUE is a bound parameter, every
//!   IDENTIFIER is a whitelisted constant ([`activity_field_column`],
//!   [`age_metric_sql`], [`RESPONSE_STATE_SQL`]). User input can never become
//!   SQL.
//! - Views are DYNAMIC: calendar date modes ("today") resolve to concrete UTC
//!   instants at OPEN time, so a saved view means "today whenever it is
//!   opened".
//!
//! Column-name adaptations (documented port-wide, see `db_breadth.rs` /
//! `search.rs`): `tag_local_id`→`tag_id`,
//! `field_local_id`→`field_id`, `known_issue_conversations`→
//! `known_issue_links`, `ai_runs.output`→`ai_runs.response_json`, and
//! `conversations.remote_created_at`→`conversations.created_at` (the port's
//! mirror stores the remote creation stamp in `created_at`). The three
//! conversation FK columns keep the reference names after DB-03 (M047):
//! `mailbox_local_id` / `assignee_local_id` / `customer_local_id`.

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

use chrono::{Datelike, Timelike};

use crate::error::Result;

/// Maximum depth of a condition tree (reference viewEngine.ts:72).
pub const MAX_TREE_DEPTH: u32 = 10;

// ---------------------------------------------------------------------------
// ViewCompileError
// ---------------------------------------------------------------------------

/// A definition that cannot be compiled to SQL. The routes surface this as
/// 422 at SAVE time and at PREVIEW time — an unevaluable view is never
/// persisted (reference `ViewCompileError`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewCompileError {
    /// The human-facing message (byte-identical to the reference's).
    pub message: String,
}

impl ViewCompileError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ViewCompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ViewCompileError {}

// ---------------------------------------------------------------------------
// Date-range resolver (reference services/dateRange.ts)
// ---------------------------------------------------------------------------

/// How a resolved window's boundaries were derived (honest display).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateRangeKind {
    /// Local wall-clock calendar-day boundaries (DST-correct).
    Calendar,
    /// Exact now-minus hour windows.
    Rolling,
    /// Explicit user-supplied dates.
    Exact,
}

impl DateRangeKind {
    /// The display string used in notes (reference `kind`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Calendar => "calendar",
            Self::Rolling => "rolling",
            Self::Exact => "exact",
        }
    }
}

/// A resolved date window: inclusive UTC `from`, exclusive UTC `to`.
#[derive(Debug, Clone, PartialEq)]
pub struct DateRange {
    /// Inclusive lower bound (UTC ISO, millisecond precision).
    pub from: String,
    /// Exclusive upper bound (UTC ISO, millisecond precision).
    pub to: String,
    /// Human label describing the resolved window.
    pub label: String,
    /// How the boundaries were derived.
    pub kind: DateRangeKind,
}

/// The resolver input (reference `DateRangeInput`).
#[derive(Debug, Clone, Copy, Default)]
pub struct DateRangeInput<'a> {
    /// One of the 15 date modes (validated against the catalog).
    pub mode: &'a str,
    /// User IANA timezone.
    pub timezone: &'a str,
    /// For exact_date / custom_range: YYYY-MM-DD (assumed in `timezone`).
    pub from: Option<&'a str>,
    /// For custom_range: YYYY-MM-DD.
    pub to: Option<&'a str>,
    /// Optional time-of-day bound "HH:mm" applied to the window start.
    pub from_time: Option<&'a str>,
    /// Optional time-of-day bound "HH:mm" applied to the window end.
    pub to_time: Option<&'a str>,
    /// Injectable clock for tests (epoch ms).
    pub now: Option<i64>,
}

/// True when the zone resolves (reference `isValidTimezone`). Delegates to
/// the SLA engine's probe — `chrono-tz` embeds the IANA database, the Rust
/// equivalent of the reference's `Intl.DateTimeFormat` check.
#[must_use]
pub fn is_valid_timezone(timezone: &str) -> bool {
    crate::business_hours::is_valid_timezone(timezone)
}

/// Resolve the user's timezone: explicit param > stored setting > system >
/// UTC (reference `resolveTimezone`).
#[must_use]
pub fn resolve_timezone(explicit: Option<&str>, stored_setting: Option<&str>) -> String {
    for candidate in [explicit, stored_setting].into_iter().flatten() {
        if candidate != "system" && is_valid_timezone(candidate) {
            return candidate.to_string();
        }
    }
    // System zone: the TZ env var when it names a valid IANA zone (the
    // closest Rust equivalent of Intl's resolved system zone), else UTC.
    if let Ok(tz) = std::env::var("TZ") {
        if is_valid_timezone(&tz) {
            return tz;
        }
    }
    "UTC".to_string()
}

/// Convert "HH:mm" to minutes-after-local-midnight; `None` for invalid input
/// (reference `parseTimeOfDay`).
fn parse_time_of_day(v: Option<&str>) -> Option<i64> {
    let v = v?;
    let bytes = v.as_bytes();
    if bytes.len() != 5 || bytes[2] != b':' {
        return None;
    }
    if !bytes
        .iter()
        .enumerate()
        .all(|(i, b)| i == 2 || b.is_ascii_digit())
    {
        return None;
    }
    let h: i64 = v.get(0..2)?.parse().ok()?;
    let min: i64 = v.get(3..5)?.parse().ok()?;
    if h > 23 || min > 59 {
        return None;
    }
    Some(h * 60 + min)
}

/// Wall-clock parts of an instant in a zone: (y, m, d, minute-of-day,
/// weekday-from-Sunday).
fn wall_parts(ts_ms: i64, tz: &chrono_tz::Tz) -> Option<(i32, u32, u32, i64, u32)> {
    use chrono::TimeZone;
    let utc = chrono::Utc.timestamp_millis_opt(ts_ms).single()?;
    let local = utc.with_timezone(tz);
    let minute = i64::from(local.hour()) * 60 + i64::from(local.minute());
    Some((
        local.year(),
        local.month(),
        local.day(),
        minute,
        local.weekday().num_days_from_sunday(),
    ))
}

/// Epoch ms for a wall-clock day + minute-of-day in a timezone, DST-aware.
///
/// Ports the reference's `dayjs.tz` wall-to-instant parse:
/// - an UNAMBIGUOUS wall time resolves through the zone's offset in force
///   for that local time (a 23h or 25h local day still yields exactly one
///   local midnight — verified against Australia/Lord_Howe 2024-04-07,
///   where midnight is +11:00 even though the day ends in +10:30);
/// - an AMBIGUOUS wall time (the repeated hour of a fall-back) resolves to
///   the EARLIEST instant;
/// - a nonexistent wall time (a spring-forward gap) applies the offset in
///   force just before the gap, shifting the instant past the gap — the
///   documented dayjs behavior.
fn local_instant_ms(tz: &chrono_tz::Tz, y: i32, mo: u32, d: u32, minute: i64) -> Option<i64> {
    use chrono::{Offset, TimeZone};
    let naive = chrono::NaiveDate::from_ymd_opt(y, mo, d)?
        .and_hms_opt(0, 0, 0)?
        .and_utc()
        + chrono::Duration::milliseconds(minute * 60_000);
    match tz.from_local_datetime(&naive.naive_utc()) {
        chrono::LocalResult::Single(dt) => Some(dt.timestamp_millis()),
        chrono::LocalResult::Ambiguous(earliest, _latest) => Some(earliest.timestamp_millis()),
        chrono::LocalResult::None => {
            // Gap: the dayjs parse applies the pre-gap offset to the naive
            // wall time (its single-pass guess). The offset in force just
            // before the transition is the one that maps the naive instant
            // backwards into real time.
            let offset = tz.offset_from_utc_datetime(&naive.naive_utc()).fix();
            let guessed = naive - chrono::Duration::seconds(i64::from(offset.local_minus_utc()));
            // One correction pass for zones where the naive instant sits far
            // from the gap.
            let offset2 = tz.offset_from_utc_datetime(&guessed.naive_utc()).fix();
            let corrected = naive - chrono::Duration::seconds(i64::from(offset2.local_minus_utc()));
            Some(corrected.timestamp_millis())
        }
    }
}

fn days_in_month(y: i32, mo: u32) -> u32 {
    let (next_y, next_m) = if mo == 12 { (y + 1, 1) } else { (y, mo + 1) };
    let first_of_next = chrono::NaiveDate::from_ymd_opt(next_y, next_m, 1);
    first_of_next
        .and_then(|d| d.pred_opt())
        .map_or(30, |d| d.day())
}

/// `Date.toISOString()` — UTC ISO with millisecond precision.
fn iso_utc(ts_ms: i64) -> String {
    use chrono::TimeZone;
    chrono::Utc
        .timestamp_millis_opt(ts_ms)
        .single()
        .map(|t| t.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
        .unwrap_or_default()
}

/// The public face of [`iso_utc`] (the Operations snapshot's `generated_at`).
#[must_use]
pub fn iso_utc_pub(ts_ms: i64) -> String {
    iso_utc(ts_ms)
}

/// Resolve a date filter to `[from, to)` UTC ISO instants (reference
/// `resolveDateRange`). Returns `None` when the mode needs explicit dates
/// that were not supplied — the caller answers 422, never silently widens.
#[must_use]
pub fn resolve_date_range(input: &DateRangeInput<'_>) -> Option<DateRange> {
    let mode = crate::catalog::DateMode::ALL
        .iter()
        .find(|m| m.as_str() == input.mode)
        .copied()?;
    let tz: chrono_tz::Tz = if is_valid_timezone(input.timezone) {
        input.timezone.parse().ok()?
    } else {
        "UTC".parse().ok()?
    };
    let now_ms = input.now.unwrap_or_else(now_ms_epoch);
    let (y, m, d, _, dow) = wall_parts(now_ms, &tz)?;

    // Day arithmetic on a NEUTRAL date (immune to offsets), then convert the
    // resulting local midnight via the DST-correct wall->instant parse.
    let midnight_at = |day_offset: i64| -> Option<i64> {
        let base = chrono::NaiveDate::from_ymd_opt(y, m, d)? + chrono::Duration::days(day_offset);
        local_instant_ms(&tz, base.year(), base.month(), base.day(), 0)
    };
    let month_midnight_at = |month_offset: i32, day_of_month: u32| -> Option<i64> {
        let first = chrono::NaiveDate::from_ymd_opt(y, m, 1)?;
        let shifted = first
            .checked_add_months(chrono::Months::new(month_offset.unsigned_abs()))
            .filter(|_| month_offset >= 0)
            .or_else(|| {
                first.checked_sub_months(chrono::Months::new(month_offset.unsigned_abs()))
            })?;
        let day = day_of_month.min(days_in_month(shifted.year(), shifted.month()));
        let clamped = shifted.with_day(day)?;
        local_instant_ms(&tz, clamped.year(), clamped.month(), clamped.day(), 0)
    };
    // Week start = Sunday 00:00 local (dayjs convention).
    let week_midnight_at = |week_offset: i64| midnight_at(week_offset * 7 - i64::from(dow));

    let with_time_bounds = |from: i64, to: i64, kind: DateRangeKind, label: &str| -> DateRange {
        let mut f = from;
        let mut t = to;
        if let Some(from_min) = parse_time_of_day(input.from_time) {
            if let Some((fy, fmo, fd, _, _)) = wall_parts(f, &tz) {
                if let Some(inst) = local_instant_ms(&tz, fy, fmo, fd, from_min) {
                    f = inst;
                }
            }
        }
        if let Some(to_min) = parse_time_of_day(input.to_time) {
            // "to 17:00" = exclusive end at 17:00 on the LAST INCLUDED day;
            // when the window end already lands on midnight-of-next-day, step
            // back one day.
            if let Some((ly, lmo, ld, _, _)) = wall_parts(t - 1, &tz) {
                if let Some(inst) = local_instant_ms(&tz, ly, lmo, ld, to_min) {
                    t = inst;
                }
            }
        }
        DateRange {
            from: iso_utc(f),
            to: iso_utc(t),
            label: label.to_string(),
            kind,
        }
    };

    let iso_date = |v: Option<&str>| -> Option<(i32, u32, u32)> {
        let v = v?;
        let bytes = v.as_bytes();
        if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
            return None;
        }
        let yy: i32 = v.get(0..4)?.parse().ok()?;
        let mo: u32 = v.get(5..7)?.parse().ok()?;
        let dd: u32 = v.get(8..10)?.parse().ok()?;
        if !(1..=12).contains(&mo) || !(1..=31).contains(&dd) {
            return None;
        }
        // Rejects Feb 30 etc.: probe noon local and check the round trip.
        let probe = local_instant_ms(&tz, yy, mo, dd, 12)?;
        let (py, pmo, pd, _, _) = wall_parts(probe, &tz)?;
        if py != yy || pmo != mo || pd != dd {
            return None;
        }
        Some((yy, mo, dd))
    };

    match mode {
        // ---- Calendar-day modes (local boundaries, DST-correct) ----
        crate::catalog::DateMode::Today => Some(with_time_bounds(
            midnight_at(0)?,
            midnight_at(1)?,
            DateRangeKind::Calendar,
            "Today",
        )),
        crate::catalog::DateMode::Yesterday => Some(with_time_bounds(
            midnight_at(-1)?,
            midnight_at(0)?,
            DateRangeKind::Calendar,
            "Yesterday",
        )),
        crate::catalog::DateMode::Tomorrow => Some(with_time_bounds(
            midnight_at(1)?,
            midnight_at(2)?,
            DateRangeKind::Calendar,
            "Tomorrow",
        )),
        crate::catalog::DateMode::ThisWeek => Some(with_time_bounds(
            week_midnight_at(0)?,
            week_midnight_at(1)?,
            DateRangeKind::Calendar,
            "This week",
        )),
        crate::catalog::DateMode::LastWeek => Some(with_time_bounds(
            week_midnight_at(-1)?,
            week_midnight_at(0)?,
            DateRangeKind::Calendar,
            "Last week",
        )),
        crate::catalog::DateMode::ThisMonth => Some(with_time_bounds(
            month_midnight_at(0, 1)?,
            month_midnight_at(1, 1)?,
            DateRangeKind::Calendar,
            "This month",
        )),
        crate::catalog::DateMode::LastMonth => Some(with_time_bounds(
            month_midnight_at(-1, 1)?,
            month_midnight_at(0, 1)?,
            DateRangeKind::Calendar,
            "Last month",
        )),
        // ---- Rolling modes (exact now-minus windows) ----
        crate::catalog::DateMode::Last24h => Some(rolling(now_ms, 24, "Last 24 hours")),
        crate::catalog::DateMode::Last48h => Some(rolling(now_ms, 48, "Last 48 hours")),
        crate::catalog::DateMode::Last7d => Some(rolling(now_ms, 24 * 7, "Last 7 days")),
        crate::catalog::DateMode::Last14d => Some(rolling(now_ms, 24 * 14, "Last 14 days")),
        crate::catalog::DateMode::Last30d => Some(rolling(now_ms, 24 * 30, "Last 30 days")),
        crate::catalog::DateMode::Last90d => Some(rolling(now_ms, 24 * 90, "Last 90 days")),
        // ---- Exact modes ----
        crate::catalog::DateMode::ExactDate => {
            let (dy, dmo, dd) = iso_date(input.from)?;
            let start = local_instant_ms(&tz, dy, dmo, dd, 0)?;
            let end = local_instant_ms(&tz, dy, dmo, dd, 0)? + 86_400_000;
            Some(with_time_bounds(
                start,
                end,
                DateRangeKind::Exact,
                &format!("On {}", input.from.unwrap_or_default()),
            ))
        }
        crate::catalog::DateMode::CustomRange => {
            let a = iso_date(input.from)?;
            let b = iso_date(input.to)?;
            let start_a = local_instant_ms(&tz, a.0, a.1, a.2, 0)?;
            let start_b = local_instant_ms(&tz, b.0, b.1, b.2, 0)?;
            let (lo, hi) = if start_a < start_b {
                (start_a, start_b + 86_400_000)
            } else {
                (start_b, start_a + 86_400_000)
            };
            Some(with_time_bounds(
                lo,
                hi,
                DateRangeKind::Exact,
                &format!(
                    "{} to {}",
                    input.from.unwrap_or_default(),
                    input.to.unwrap_or_default()
                ),
            ))
        }
    }
}

fn rolling(now_ms: i64, hours: i64, label: &str) -> DateRange {
    DateRange {
        from: iso_utc(now_ms - hours * 3_600_000),
        to: iso_utc(now_ms),
        label: label.to_string(),
        kind: DateRangeKind::Rolling,
    }
}

fn now_ms_epoch() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Whitelisted SQL constants (every identifier is a literal — never input)
// ---------------------------------------------------------------------------

/// The deterministic response-state CASE (reference
/// `src/server/inbox/responseState.ts` RESPONSE_STATE_SQL, verbatim — the
/// port's conversations columns carry the same names). The SAME expression
/// drives the saved-view `response_state` condition and the Operations
/// Center tile fragments, so a badge in the list can never disagree with the
/// detail view.
pub const RESPONSE_STATE_SQL: &str = "CASE
  WHEN c.status = 'closed' THEN 'closed'
  WHEN c.status = 'spam' THEN 'closed'
  WHEN c.status = 'active' AND c.snoozed_until IS NOT NULL AND c.snoozed_until > datetime('now') THEN 'snoozed'
  WHEN c.status = 'active' AND c.first_customer_message_at IS NOT NULL AND c.first_response_at IS NULL THEN 'needs_first_response'
  WHEN c.status = 'active'
       AND c.last_customer_reply_at IS NOT NULL
       AND (c.last_human_agent_response_at IS NULL OR c.last_customer_reply_at > c.last_human_agent_response_at) THEN 'customer_waiting'
  WHEN c.status IN ('active', 'pending')
       AND c.last_human_agent_response_at IS NOT NULL
       AND (c.last_customer_reply_at IS NULL OR c.last_human_agent_response_at > c.last_customer_reply_at)
       AND (julianday('now') - julianday(c.last_human_agent_response_at)) * 1440 <= 1440 THEN 'recently_responded'
  WHEN c.status IN ('active', 'pending')
       AND c.last_human_agent_response_at IS NOT NULL
       AND (c.last_customer_reply_at IS NULL OR c.last_human_agent_response_at > c.last_customer_reply_at) THEN 'agent_waiting'
  WHEN c.activity_history_complete = 0
       AND c.first_customer_message_at IS NULL
       AND c.first_response_at IS NULL THEN 'unknown'
  ELSE 'never_responded'
END";

/// The 14 activity fields -> whitelisted SQL column expressions (reference
/// `ACTIVITY_FIELD_COLUMN`; `created_at` maps to the port's `c.created_at`,
/// which carries the reference's `remote_created_at` stamp).
#[must_use]
pub fn activity_field_column(field: &str) -> Option<&'static str> {
    match field {
        "created_at" => Some("c.created_at"),
        "first_customer_message_at" => Some("c.first_customer_message_at"),
        "first_response_at" => Some("c.first_response_at"),
        "last_customer_reply_at" => Some("c.last_customer_reply_at"),
        "last_human_agent_response_at" => Some("c.last_human_agent_response_at"),
        "last_system_response_at" => Some("c.last_system_response_at"),
        "last_note_at" => Some("c.last_note_at"),
        "last_activity_at" => Some("COALESCE(c.last_activity_at, c.created_at)"),
        "closed_at" => Some("c.closed_at"),
        "customer_waiting_since" => Some("c.customer_waiting_since"),
        "last_status_change_at" => Some("c.last_status_change_at"),
        "last_assignment_change_at" => Some("c.last_assignment_change_at"),
        "last_tag_change_at" => Some("c.last_tag_change_at"),
        "last_custom_field_change_at" => Some("c.last_custom_field_change_at"),
        _ => None,
    }
}

/// Response-age metric -> (SQL minutes expression, label) (reference
/// `AGE_METRIC_SQL`; `c.remote_created_at` adapted to the port's
/// `c.created_at`).
#[must_use]
pub fn age_metric_sql(metric: &str) -> Option<(&'static str, &'static str)> {
    match metric {
        "time_since_customer_reply" => Some((
            "(julianday('now') - COALESCE(julianday(c.last_customer_reply_at), julianday('now'))) * 1440",
            "Time since customer reply",
        )),
        "time_since_agent_response" => Some((
            "(julianday('now') - COALESCE(julianday(c.last_human_agent_response_at), julianday('now'))) * 1440",
            "Time since agent response",
        )),
        "customer_waiting_duration" => Some((
            "(julianday('now') - COALESCE(julianday(c.customer_waiting_since), julianday('now'))) * 1440",
            "Customer waiting",
        )),
        "first_response_delay" => Some((
            "(COALESCE(julianday(c.first_response_at), julianday('now')) - COALESCE(julianday(c.created_at), julianday('now'))) * 1440",
            "First response delay",
        )),
        "resolution_duration" => Some((
            "(COALESCE(julianday(c.closed_at), julianday('now')) - COALESCE(julianday(c.created_at), julianday('now'))) * 1440",
            "Resolution duration",
        )),
        "conversation_age" => Some((
            "(julianday('now') - COALESCE(julianday(c.created_at), julianday('now'))) * 1440",
            "Conversation age",
        )),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// View definition model (shared/activity.ts)
// ---------------------------------------------------------------------------

/// The 22 condition kinds, in catalog order (reference `viewConditionSchema`).
pub const VIEW_CONDITION_KINDS: [&str; 22] = [
    "status",
    "assignee",
    "team",
    "mailbox",
    "channel",
    "tags",
    "custom_field",
    "customer_property",
    "customer_text",
    "date_activity",
    "response_state",
    "response_age",
    "sla",
    "priority",
    "ticket_state",
    "known_issue",
    "ai_analyzed",
    "interaction_signal",
    "ai_attribute",
    "unread",
    "snoozed",
    "customer",
];

/// A condition-tree node: an AND/OR group or one of the 22 conditions.
/// Wire format matches the reference exactly (internally tagged on `kind`,
/// camelCase member names).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ViewNode {
    /// An AND/OR group with children.
    Group {
        /// "all" (AND) or "any" (OR).
        combinator: String,
        /// Child nodes.
        children: Vec<ViewNode>,
    },
    /// Help Scout conversation statuses.
    Status {
        /// active/pending/closed/spam.
        statuses: Vec<String>,
    },
    /// Assignee filter.
    Assignee {
        /// Local user ids.
        #[serde(rename = "assigneeLocalIds")]
        assignee_local_ids: Vec<i64>,
        /// Also match unassigned conversations.
        #[serde(rename = "includeUnassigned")]
        include_unassigned: bool,
    },
    /// Team filter.
    Team {
        /// Local team ids.
        #[serde(rename = "teamLocalIds")]
        team_local_ids: Vec<i64>,
    },
    /// Mailbox filter.
    Mailbox {
        /// Local mailbox ids.
        #[serde(rename = "mailboxLocalIds")]
        mailbox_local_ids: Vec<i64>,
    },
    /// Channel filter.
    Channel {
        /// email/chat.
        channels: Vec<String>,
    },
    /// Tag filter.
    Tags {
        /// Tag names (matched case-insensitively).
        tags: Vec<String>,
        /// any/all/none.
        mode: String,
    },
    /// Custom conversation field.
    CustomField {
        /// Local field id.
        #[serde(rename = "fieldLocalId")]
        field_local_id: i64,
        /// equals/not_equals/contains/is_empty/is_not_empty.
        op: String,
        /// The comparison value (bound parameter).
        value: Option<String>,
    },
    /// Customer property.
    CustomerProperty {
        /// Property definition id.
        #[serde(rename = "definitionId")]
        definition_id: i64,
        /// equals/not_equals/contains/is_empty/is_not_empty/gt/gte/lt/lte.
        op: String,
        /// The comparison value.
        value: Option<String>,
    },
    /// Customer free text (name/email/organization).
    CustomerText {
        /// name/email/organization.
        field: String,
        /// contains/equals/not_contains/is_empty/is_not_empty.
        op: String,
        /// The comparison value.
        value: Option<String>,
    },
    /// Activity timestamp window.
    DateActivity {
        /// One of the 14 activity fields.
        #[serde(rename = "activityField")]
        activity_field: String,
        /// One of the 15 date modes.
        mode: String,
        /// YYYY-MM-DD for exact modes.
        from: Option<String>,
        /// YYYY-MM-DD for custom_range.
        to: Option<String>,
        /// Optional HH:mm start bound.
        #[serde(rename = "fromTime")]
        from_time: Option<String>,
        /// Optional HH:mm end bound.
        #[serde(rename = "toTime")]
        to_time: Option<String>,
    },
    /// Deterministic response state.
    ResponseState {
        /// Response states (the 8-value closed set).
        states: Vec<String>,
    },
    /// Response age in minutes.
    ResponseAge {
        /// One of the 6 age metrics.
        metric: String,
        /// gt/gte/lt/lte.
        op: String,
        /// Minute threshold.
        minutes: f64,
    },
    /// Live SLA state (resolved against the business-hours engine).
    Sla {
        /// at_risk/breached.
        states: Vec<String>,
        /// Invert the match.
        negate: bool,
    },
    /// SupportOS priority.
    Priority {
        /// none/low/medium/high/urgent.
        priorities: Vec<String>,
    },
    /// SupportOS ticket state.
    TicketState {
        /// Local state ids.
        #[serde(rename = "stateIds")]
        state_ids: Vec<i64>,
        /// Also match stateless conversations.
        #[serde(rename = "includeNoState")]
        include_no_state: bool,
    },
    /// Known-issue link.
    KnownIssue {
        /// Any known issue.
        any: bool,
        /// Specific known-issue ids.
        #[serde(rename = "knownIssueIds")]
        known_issue_ids: Option<Vec<i64>>,
    },
    /// AI analyzed (completed ticket analysis exists).
    AiAnalyzed {
        /// true = analyzed, false = not.
        analyzed: bool,
    },
    /// Per-conversation interaction signal.
    InteractionSignal {
        /// Signal dimension.
        dimension: String,
        /// Signal value.
        value: String,
        /// Invert the match.
        negate: bool,
    },
    /// Current local AI attribute (closed 14-key catalog).
    AiAttribute {
        /// One of the 14 keys.
        attribute: String,
        /// equals/not_equals/contains/not_contains/gt/gte/lt/lte.
        op: String,
        /// The comparison value.
        value: String,
    },
    /// Unread flag.
    Unread {
        /// true = unread.
        unread: bool,
    },
    /// Snoozed flag.
    Snoozed {
        /// true = currently snoozed.
        snoozed: bool,
    },
    /// Customer filter.
    Customer {
        /// Local customer ids.
        #[serde(rename = "customerLocalIds")]
        customer_local_ids: Vec<i64>,
    },
}

/// A saved-view definition: a combinator over condition nodes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ViewDefinition {
    /// "all" (AND) or "any" (OR).
    pub combinator: String,
    /// The condition nodes.
    pub conditions: Vec<ViewNode>,
}

/// A stored saved view (reference `SavedInboxView`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedInboxView {
    pub id: i64,
    pub name: String,
    pub description: Option<String>,
    pub definition: ViewDefinition,
    pub sort_order: i64,
    pub folder: Option<String>,
    pub version: i64,
    pub created_at: String,
    pub updated_at: String,
}

// ---------------------------------------------------------------------------
// Route-boundary validation (the viewDefinitionSchema / viewRequestSchema port)
// ---------------------------------------------------------------------------

/// One Zod-style validation issue (path + message).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewIssue {
    /// The dotted issue path ("" for the root).
    pub path: String,
    /// The Zod v3 message.
    pub message: String,
}

/// The parsed create-view request body.
#[derive(Debug, Clone)]
pub struct CreateViewRequest {
    pub name: String,
    pub description: Option<String>,
    pub definition: ViewDefinition,
    pub sort_order: Option<i64>,
    pub folder: Option<String>,
}

/// The parsed update-view request body (all fields optional; `description`
/// and `folder` keep the 3-state absent/`null`/value distinction the
/// reference's `.nullable().optional()` Zod fields have — PATCH `null`
/// CLEARS the stored value).
#[derive(Debug, Clone, Default)]
pub struct UpdateViewRequest {
    pub name: Option<String>,
    pub description: Option<Option<String>>,
    pub definition: Option<ViewDefinition>,
    pub sort_order: Option<i64>,
    pub folder: Option<Option<String>>,
}

const RESPONSE_STATES: [&str; 8] = [
    "needs_first_response",
    "customer_waiting",
    "agent_waiting",
    "recently_responded",
    "never_responded",
    "closed",
    "snoozed",
    "unknown",
];
const AGE_METRICS: [&str; 6] = [
    "time_since_customer_reply",
    "time_since_agent_response",
    "customer_waiting_duration",
    "first_response_delay",
    "resolution_duration",
    "conversation_age",
];
const TICKET_PRIORITIES: [&str; 5] = ["none", "low", "medium", "high", "urgent"];
const CONVERSATION_STATUSES: [&str; 4] = ["active", "pending", "closed", "spam"];
const CHANNELS: [&str; 2] = ["email", "chat"];
const TAG_MODES: [&str; 3] = ["any", "all", "none"];
const FIELD_OPS: [&str; 5] = [
    "equals",
    "not_equals",
    "contains",
    "is_empty",
    "is_not_empty",
];
const PROPERTY_OPS: [&str; 9] = [
    "equals",
    "not_equals",
    "contains",
    "is_empty",
    "is_not_empty",
    "gt",
    "gte",
    "lt",
    "lte",
];
const TEXT_OPS: [&str; 5] = [
    "contains",
    "equals",
    "not_contains",
    "is_empty",
    "is_not_empty",
];
const CUSTOMER_TEXT_FIELDS: [&str; 3] = ["name", "email", "organization"];
const SLA_STATES: [&str; 2] = ["at_risk", "breached"];
const AGE_OPS: [&str; 4] = ["gt", "gte", "lt", "lte"];
const AI_ATTRIBUTE_OPS: [&str; 8] = [
    "equals",
    "not_equals",
    "contains",
    "not_contains",
    "gt",
    "gte",
    "lt",
    "lte",
];

fn json_type_name(v: &Json) -> &'static str {
    match v {
        Json::String(_) => "string",
        Json::Number(_) => "number",
        Json::Bool(_) => "boolean",
        Json::Array(_) => "array",
        Json::Object(_) => "object",
        Json::Null => "null",
    }
}

fn enum_message(variants: &[&str], received: &str) -> String {
    let expected = variants
        .iter()
        .map(|v| format!("'{v}'"))
        .collect::<Vec<_>>()
        .join(" | ");
    format!("Invalid enum value. Expected {expected}, received '{received}'")
}

struct IssueSink {
    issues: Vec<ViewIssue>,
}

impl IssueSink {
    fn push(&mut self, path: &str, message: impl Into<String>) {
        self.issues.push(ViewIssue {
            path: path.to_string(),
            message: message.into(),
        });
    }

    fn is_empty(&self) -> bool {
        self.issues.is_empty()
    }
}

/// `z.string().min(1).max(hi)`.
fn parse_bounded_string(
    v: Option<&Json>,
    path: &str,
    lo: usize,
    hi: usize,
    sink: &mut IssueSink,
) -> Option<String> {
    match v {
        None | Some(Json::Null) => {
            sink.push(path, "Required");
            None
        }
        Some(Json::String(s)) => {
            if s.chars().count() < lo {
                sink.push(
                    path,
                    format!("String must contain at least {lo} character(s)"),
                );
                None
            } else if s.chars().count() > hi {
                sink.push(
                    path,
                    format!("String must contain at most {hi} character(s)"),
                );
                None
            } else {
                Some(s.clone())
            }
        }
        Some(other) => {
            sink.push(
                path,
                format!("Expected string, received {}", json_type_name(other)),
            );
            None
        }
    }
}

/// `z.string().max(hi).nullable().optional()` — value or null-or-missing.
fn parse_nullable_max_string(
    v: Option<&Json>,
    path: &str,
    hi: usize,
    sink: &mut IssueSink,
) -> Option<Option<String>> {
    match v {
        None | Some(Json::Null) => Some(None),
        Some(Json::String(s)) => {
            if s.chars().count() > hi {
                sink.push(
                    path,
                    format!("String must contain at most {hi} character(s)"),
                );
                Some(None)
            } else {
                Some(Some(s.clone()))
            }
        }
        Some(other) => {
            sink.push(
                path,
                format!("Expected string, received {}", json_type_name(other)),
            );
            Some(None)
        }
    }
}

/// `z.boolean()`.
fn parse_bool(v: Option<&Json>, path: &str, sink: &mut IssueSink) -> Option<bool> {
    match v {
        None | Some(Json::Null) => {
            sink.push(path, "Required");
            None
        }
        Some(Json::Bool(b)) => Some(*b),
        Some(other) => {
            sink.push(
                path,
                format!("Expected boolean, received {}", json_type_name(other)),
            );
            None
        }
    }
}

/// `z.number().int().positive()`.
fn parse_positive_int(v: Option<&Json>, path: &str, sink: &mut IssueSink) -> Option<i64> {
    match v {
        None | Some(Json::Null) => {
            sink.push(path, "Required");
            None
        }
        Some(Json::Number(n)) => {
            if let Some(i) = n.as_i64() {
                if i > 0 {
                    Some(i)
                } else {
                    sink.push(path, "Number must be greater than 0");
                    None
                }
            } else {
                sink.push(path, "Expected int, received float");
                None
            }
        }
        Some(other) => {
            sink.push(
                path,
                format!("Expected number, received {}", json_type_name(other)),
            );
            None
        }
    }
}

/// `z.array(z.enum(variants)).min(lo).max(hi)`.
fn parse_enum_array(
    v: Option<&Json>,
    path: &str,
    variants: &[&str],
    lo: usize,
    hi: usize,
    sink: &mut IssueSink,
) -> Option<Vec<String>> {
    let items = parse_array(v, path, lo, hi, sink)?;
    let mut out = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        match item.as_str() {
            Some(s) if variants.contains(&s) => out.push(s.to_string()),
            Some(s) => {
                sink.push(&format!("{path}.{i}"), enum_message(variants, s));
                return None;
            }
            None => {
                sink.push(
                    &format!("{path}.{i}"),
                    format!("Expected string, received {}", json_type_name(item)),
                );
                return None;
            }
        }
    }
    Some(out)
}

/// `z.array(z.number().int().positive()).min(lo).max(hi)`.
fn parse_id_array(
    v: Option<&Json>,
    path: &str,
    lo: usize,
    hi: usize,
    sink: &mut IssueSink,
) -> Option<Vec<i64>> {
    let items = parse_array(v, path, lo, hi, sink)?;
    let mut out = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        out.push(parse_positive_int(
            Some(item),
            &format!("{path}.{i}"),
            sink,
        )?);
    }
    Some(out)
}

fn parse_array<'v>(
    v: Option<&'v Json>,
    path: &str,
    lo: usize,
    hi: usize,
    sink: &mut IssueSink,
) -> Option<&'v Vec<Json>> {
    match v {
        None | Some(Json::Null) => {
            sink.push(path, "Required");
            None
        }
        Some(Json::Array(items)) => {
            if items.len() < lo {
                sink.push(
                    path,
                    format!(
                        "Array must contain at least {lo} element(s), but the input had {}",
                        items.len()
                    ),
                );
                None
            } else if items.len() > hi {
                sink.push(
                    path,
                    format!(
                        "Array must contain at most {hi} element(s), but the input had {}",
                        items.len()
                    ),
                );
                None
            } else {
                Some(items)
            }
        }
        Some(other) => {
            sink.push(
                path,
                format!("Expected array, received {}", json_type_name(other)),
            );
            None
        }
    }
}

/// `z.string().regex(/^(?:[01]\d|2[0-3]):[0-5]\d$/, 'time must be a valid
/// HH:mm')` — strict HH:mm, nullable, optional.
fn parse_time_of_day_field(
    v: Option<&Json>,
    path: &str,
    sink: &mut IssueSink,
) -> Option<Option<String>> {
    match v {
        None | Some(Json::Null) => Some(None),
        Some(Json::String(s)) => {
            let valid = s.len() == 5
                && s.as_bytes().iter().enumerate().all(|(i, b)| match i {
                    2 => *b == b':',
                    _ => b.is_ascii_digit(),
                })
                && {
                    let h: u32 = s[0..2].parse().unwrap_or(99);
                    let mi: u32 = s[3..5].parse().unwrap_or(99);
                    h < 24 && mi < 60
                };
            if valid {
                Some(Some(s.clone()))
            } else {
                sink.push(path, "time must be a valid HH:mm");
                Some(None)
            }
        }
        Some(other) => {
            sink.push(
                path,
                format!("Expected string, received {}", json_type_name(other)),
            );
            Some(None)
        }
    }
}

/// One of the closed-catalog strings.
fn parse_enum_str(
    v: Option<&Json>,
    path: &str,
    variants: &[&str],
    sink: &mut IssueSink,
) -> Option<String> {
    match v {
        None | Some(Json::Null) => {
            sink.push(path, "Required");
            None
        }
        Some(Json::String(s)) => {
            if variants.contains(&s.as_str()) {
                Some(s.clone())
            } else {
                sink.push(path, enum_message(variants, s));
                None
            }
        }
        Some(other) => {
            sink.push(
                path,
                format!("Expected string, received {}", json_type_name(other)),
            );
            None
        }
    }
}

/// The viewNodeSchema port: a group or one of the 22 conditions.
fn parse_view_node(v: &Json, path: &str, sink: &mut IssueSink) -> Option<ViewNode> {
    let Some(obj) = v.as_object() else {
        sink.push(
            path,
            format!("Expected object, received {}", json_type_name(v)),
        );
        return None;
    };
    let kind = match obj.get("kind") {
        Some(Json::String(s)) => s.clone(),
        _ => {
            sink.push(path, "Required");
            return None;
        }
    };
    let get = |k: &str| obj.get(k);
    match kind.as_str() {
        "group" => {
            let combinator = parse_enum_str(
                get("combinator"),
                &format!("{path}.combinator"),
                &["all", "any"],
                sink,
            )?;
            let children = get("children").and_then(Json::as_array);
            let child_path = format!("{path}.children");
            match children {
                None => {
                    sink.push(&child_path, "Required");
                    None
                }
                Some(items) => {
                    if items.is_empty() {
                        sink.push(
                            &child_path,
                            "Array must contain at least 1 element(s), but the input had 0",
                        );
                        return None;
                    }
                    if items.len() > 25 {
                        sink.push(
                            &child_path,
                            format!(
                                "Array must contain at most 25 element(s), but the input had {}",
                                items.len()
                            ),
                        );
                        return None;
                    }
                    let mut nodes = Vec::with_capacity(items.len());
                    for (i, child) in items.iter().enumerate() {
                        nodes.push(parse_view_node(child, &format!("{child_path}.{i}"), sink)?);
                    }
                    Some(ViewNode::Group {
                        combinator,
                        children: nodes,
                    })
                }
            }
        }
        "status" => Some(ViewNode::Status {
            statuses: parse_enum_array(
                get("statuses"),
                &format!("{path}.statuses"),
                &CONVERSATION_STATUSES,
                1,
                4,
                sink,
            )?,
        }),
        "assignee" => Some(ViewNode::Assignee {
            assignee_local_ids: parse_id_array(
                get("assigneeLocalIds"),
                &format!("{path}.assigneeLocalIds"),
                1,
                200,
                sink,
            )?,
            include_unassigned: parse_bool(
                get("includeUnassigned"),
                &format!("{path}.includeUnassigned"),
                sink,
            )?,
        }),
        "team" => Some(ViewNode::Team {
            team_local_ids: parse_id_array(
                get("teamLocalIds"),
                &format!("{path}.teamLocalIds"),
                1,
                200,
                sink,
            )?,
        }),
        "mailbox" => Some(ViewNode::Mailbox {
            mailbox_local_ids: parse_id_array(
                get("mailboxLocalIds"),
                &format!("{path}.mailboxLocalIds"),
                1,
                200,
                sink,
            )?,
        }),
        "channel" => Some(ViewNode::Channel {
            channels: parse_enum_array(
                get("channels"),
                &format!("{path}.channels"),
                &CHANNELS,
                1,
                2,
                sink,
            )?,
        }),
        "tags" => {
            let tags = parse_array(get("tags"), &format!("{path}.tags"), 1, 50, sink)?;
            let mut out = Vec::with_capacity(tags.len());
            for (i, t) in tags.iter().enumerate() {
                out.push(parse_bounded_string(
                    Some(t),
                    &format!("{path}.tags.{i}"),
                    1,
                    200,
                    sink,
                )?);
            }
            Some(ViewNode::Tags {
                tags: out,
                mode: parse_enum_str(get("mode"), &format!("{path}.mode"), &TAG_MODES, sink)?,
            })
        }
        "custom_field" => Some(ViewNode::CustomField {
            field_local_id: parse_positive_int(
                get("fieldLocalId"),
                &format!("{path}.fieldLocalId"),
                sink,
            )?,
            op: parse_enum_str(get("op"), &format!("{path}.op"), &FIELD_OPS, sink)?,
            value: parse_nullable_max_string(get("value"), &format!("{path}.value"), 500, sink)?,
        }),
        "customer_property" => Some(ViewNode::CustomerProperty {
            definition_id: parse_positive_int(
                get("definitionId"),
                &format!("{path}.definitionId"),
                sink,
            )?,
            op: parse_enum_str(get("op"), &format!("{path}.op"), &PROPERTY_OPS, sink)?,
            value: parse_nullable_max_string(get("value"), &format!("{path}.value"), 500, sink)?,
        }),
        "customer_text" => Some(ViewNode::CustomerText {
            field: parse_enum_str(
                get("field"),
                &format!("{path}.field"),
                &CUSTOMER_TEXT_FIELDS,
                sink,
            )?,
            op: parse_enum_str(get("op"), &format!("{path}.op"), &TEXT_OPS, sink)?,
            value: parse_nullable_max_string(get("value"), &format!("{path}.value"), 300, sink)?,
        }),
        "date_activity" => {
            let activity_field = parse_enum_str(
                get("activityField"),
                &format!("{path}.activityField"),
                &crate::catalog::ActivityField::ALL.map(|f| f.as_str()),
                sink,
            )?;
            Some(ViewNode::DateActivity {
                activity_field,
                mode: parse_enum_str(
                    get("mode"),
                    &format!("{path}.mode"),
                    &crate::catalog::DateMode::ALL.map(|m| m.as_str()),
                    sink,
                )?,
                from: parse_nullable_max_string(get("from"), &format!("{path}.from"), 40, sink)?,
                to: parse_nullable_max_string(get("to"), &format!("{path}.to"), 40, sink)?,
                from_time: parse_time_of_day_field(
                    get("fromTime"),
                    &format!("{path}.fromTime"),
                    sink,
                )?,
                to_time: parse_time_of_day_field(get("toTime"), &format!("{path}.toTime"), sink)?,
            })
        }
        "response_state" => Some(ViewNode::ResponseState {
            // The reference schema caps this array only from below; the high
            // bound is a defensive safety cap (the repo-wide id arrays cap at
            // 200), far above any real filter.
            states: parse_enum_array(
                get("states"),
                &format!("{path}.states"),
                &RESPONSE_STATES,
                1,
                10_000,
                sink,
            )?,
        }),
        "response_age" => {
            let metric =
                parse_enum_str(get("metric"), &format!("{path}.metric"), &AGE_METRICS, sink)?;
            let op = parse_enum_str(get("op"), &format!("{path}.op"), &AGE_OPS, sink)?;
            let minutes = match get("minutes") {
                None | Some(Json::Null) => {
                    sink.push(&format!("{path}.minutes"), "Required");
                    return None;
                }
                Some(Json::Number(n)) => {
                    let f = n.as_f64().unwrap_or(f64::NAN);
                    if !f.is_finite() {
                        sink.push(&format!("{path}.minutes"), "Expected number, received NaN");
                        return None;
                    }
                    if f < 0.0 {
                        sink.push(
                            &format!("{path}.minutes"),
                            "Number must be greater than or equal to 0",
                        );
                        return None;
                    }
                    if f > 525_600.0 {
                        sink.push(
                            &format!("{path}.minutes"),
                            "Number must be less than or equal to 525600",
                        );
                        return None;
                    }
                    f
                }
                Some(other) => {
                    sink.push(
                        &format!("{path}.minutes"),
                        format!("Expected number, received {}", json_type_name(other)),
                    );
                    return None;
                }
            };
            Some(ViewNode::ResponseAge {
                metric,
                op,
                minutes,
            })
        }
        "sla" => Some(ViewNode::Sla {
            states: parse_enum_array(
                get("states"),
                &format!("{path}.states"),
                &SLA_STATES,
                1,
                10_000,
                sink,
            )?,
            negate: parse_bool(get("negate"), &format!("{path}.negate"), sink)?,
        }),
        "priority" => Some(ViewNode::Priority {
            priorities: parse_enum_array(
                get("priorities"),
                &format!("{path}.priorities"),
                &TICKET_PRIORITIES,
                1,
                10_000,
                sink,
            )?,
        }),
        "ticket_state" => Some(ViewNode::TicketState {
            state_ids: parse_id_array(get("stateIds"), &format!("{path}.stateIds"), 1, 200, sink)?,
            include_no_state: parse_bool(
                get("includeNoState"),
                &format!("{path}.includeNoState"),
                sink,
            )?,
        }),
        "known_issue" => Some(ViewNode::KnownIssue {
            any: parse_bool(get("any"), &format!("{path}.any"), sink)?,
            known_issue_ids: match get("knownIssueIds") {
                None | Some(Json::Null) => None,
                Some(v) => Some(parse_id_array(
                    Some(v),
                    &format!("{path}.knownIssueIds"),
                    1,
                    200,
                    sink,
                )?),
            },
        }),
        "ai_analyzed" => Some(ViewNode::AiAnalyzed {
            analyzed: parse_bool(get("analyzed"), &format!("{path}.analyzed"), sink)?,
        }),
        "interaction_signal" => Some(ViewNode::InteractionSignal {
            dimension: parse_bounded_string(
                get("dimension"),
                &format!("{path}.dimension"),
                1,
                40,
                sink,
            )?,
            value: parse_bounded_string(get("value"), &format!("{path}.value"), 1, 60, sink)?,
            negate: parse_bool(get("negate"), &format!("{path}.negate"), sink)?,
        }),
        "ai_attribute" => Some(ViewNode::AiAttribute {
            attribute: parse_enum_str(
                get("attribute"),
                &format!("{path}.attribute"),
                &crate::catalog::AiAttributeKey::ALL.map(|k| k.as_str()),
                sink,
            )?,
            op: parse_enum_str(get("op"), &format!("{path}.op"), &AI_ATTRIBUTE_OPS, sink)?,
            value: parse_bounded_string(get("value"), &format!("{path}.value"), 1, 120, sink)?,
        }),
        "unread" => Some(ViewNode::Unread {
            unread: parse_bool(get("unread"), &format!("{path}.unread"), sink)?,
        }),
        "snoozed" => Some(ViewNode::Snoozed {
            snoozed: parse_bool(get("snoozed"), &format!("{path}.snoozed"), sink)?,
        }),
        "customer" => Some(ViewNode::Customer {
            customer_local_ids: parse_id_array(
                get("customerLocalIds"),
                &format!("{path}.customerLocalIds"),
                1,
                200,
                sink,
            )?,
        }),
        other => {
            let mut all: Vec<&str> = vec!["group"];
            all.extend(VIEW_CONDITION_KINDS);
            sink.push(
                &format!("{path}.kind"),
                format!(
                    "Invalid discriminator value. Expected {}, received '{other}'",
                    all.iter()
                        .map(|k| format!("'{k}'"))
                        .collect::<Vec<_>>()
                        .join(" | ")
                ),
            );
            None
        }
    }
}

/// The viewDefinitionSchema port.
///
/// # Errors
/// Returns the full Zod-style issue list (empty = valid).
pub fn parse_view_definition(v: &Json) -> std::result::Result<ViewDefinition, Vec<ViewIssue>> {
    let mut sink = IssueSink { issues: Vec::new() };
    let Some(obj) = v.as_object() else {
        return Err(vec![ViewIssue {
            path: String::new(),
            message: format!("Expected object, received {}", json_type_name(v)),
        }]);
    };
    let combinator = parse_enum_str(
        obj.get("combinator"),
        "combinator",
        &["all", "any"],
        &mut sink,
    );
    let conditions = match obj.get("conditions") {
        None | Some(Json::Null) => {
            sink.push("conditions", "Required");
            None
        }
        Some(Json::Array(items)) => {
            if items.len() > 50 {
                sink.push(
                    "conditions",
                    format!(
                        "Array must contain at most 50 element(s), but the input had {}",
                        items.len()
                    ),
                );
                None
            } else {
                Some(items.clone())
            }
        }
        Some(other) => {
            sink.push(
                "conditions",
                format!("Expected array, received {}", json_type_name(other)),
            );
            None
        }
    };
    let mut nodes = Vec::new();
    if let Some(items) = conditions {
        for (i, item) in items.iter().enumerate() {
            if let Some(n) = parse_view_node(item, &format!("conditions.{i}"), &mut sink) {
                nodes.push(n);
            }
        }
    }
    if sink.is_empty() {
        Ok(ViewDefinition {
            combinator: combinator.expect("validated"),
            conditions: nodes,
        })
    } else {
        Err(sink.issues)
    }
}

/// The createViewRequestSchema port.
///
/// # Errors
/// Returns the full Zod-style issue list (empty = valid).
pub fn parse_create_view_request(
    v: &Json,
) -> std::result::Result<CreateViewRequest, Vec<ViewIssue>> {
    let mut sink = IssueSink { issues: Vec::new() };
    let Some(obj) = v.as_object() else {
        return Err(vec![ViewIssue {
            path: String::new(),
            message: format!("Expected object, received {}", json_type_name(v)),
        }]);
    };
    let name = parse_bounded_string(obj.get("name"), "name", 1, 120, &mut sink);
    let description =
        parse_nullable_max_string(obj.get("description"), "description", 500, &mut sink);
    let definition = match obj.get("definition") {
        None | Some(Json::Null) => {
            sink.push("definition", "Required");
            None
        }
        Some(d) => match parse_view_definition(d) {
            Ok(def) => Some(def),
            Err(issues) => {
                for issue in issues {
                    sink.push(&format!("definition.{}", issue.path), issue.message);
                }
                None
            }
        },
    };
    let sort_order = match obj.get("sort_order") {
        None | Some(Json::Null) => None,
        Some(Json::Number(n)) => match n.as_i64() {
            Some(i) if (0..=9999).contains(&i) => Some(i),
            Some(_) => {
                sink.push("sort_order", "Number must be between 0 and 9999");
                None
            }
            None => {
                sink.push("sort_order", "Expected int, received float");
                None
            }
        },
        Some(other) => {
            sink.push(
                "sort_order",
                format!("Expected number, received {}", json_type_name(other)),
            );
            None
        }
    };
    let folder = parse_nullable_max_string(obj.get("folder"), "folder", 80, &mut sink);
    if sink.is_empty() {
        Ok(CreateViewRequest {
            name: name.expect("validated"),
            description: description.flatten(),
            definition: definition.expect("validated"),
            sort_order,
            folder: folder.flatten(),
        })
    } else {
        Err(sink.issues)
    }
}

/// The updateViewRequestSchema port (every field optional).
///
/// # Errors
/// Returns the full Zod-style issue list (empty = valid).
pub fn parse_update_view_request(
    v: &Json,
) -> std::result::Result<UpdateViewRequest, Vec<ViewIssue>> {
    let mut sink = IssueSink { issues: Vec::new() };
    let Some(obj) = v.as_object() else {
        return Err(vec![ViewIssue {
            path: String::new(),
            message: format!("Expected object, received {}", json_type_name(v)),
        }]);
    };
    let name = match obj.get("name") {
        None | Some(Json::Null) => None,
        Some(_) => parse_bounded_string(obj.get("name"), "name", 1, 120, &mut sink),
    };
    // 3-state: absent (no change) vs explicit null (CLEAR) vs string.
    let description = match obj.get("description") {
        None => None,
        Some(Json::Null) => Some(None),
        Some(_) => parse_nullable_max_string(obj.get("description"), "description", 500, &mut sink)
            .flatten()
            .map(Some),
    };
    let definition = match obj.get("definition") {
        None | Some(Json::Null) => None,
        Some(d) => match parse_view_definition(d) {
            Ok(def) => Some(def),
            Err(issues) => {
                for issue in issues {
                    sink.push(&format!("definition.{}", issue.path), issue.message);
                }
                None
            }
        },
    };
    let sort_order = match obj.get("sort_order") {
        None | Some(Json::Null) => None,
        Some(Json::Number(n)) => match n.as_i64() {
            Some(i) if (0..=9999).contains(&i) => Some(i),
            Some(_) => {
                sink.push("sort_order", "Number must be between 0 and 9999");
                None
            }
            None => {
                sink.push("sort_order", "Expected int, received float");
                None
            }
        },
        Some(other) => {
            sink.push(
                "sort_order",
                format!("Expected number, received {}", json_type_name(other)),
            );
            None
        }
    };
    // 3-state: absent (no change) vs explicit null (CLEAR) vs string.
    let folder = match obj.get("folder") {
        None => None,
        Some(Json::Null) => Some(None),
        Some(_) => parse_nullable_max_string(obj.get("folder"), "folder", 80, &mut sink)
            .flatten()
            .map(Some),
    };
    if sink.is_empty() {
        Ok(UpdateViewRequest {
            name,
            description,
            definition,
            sort_order,
            folder,
        })
    } else {
        Err(sink.issues)
    }
}

// ---------------------------------------------------------------------------
// The compiler (reference ViewEngine)
// ---------------------------------------------------------------------------

/// A bound SQL parameter list.
pub type SqlParams = Vec<rusqlite::types::Value>;

/// One compiled fragment: WHERE content (no leading WHERE) + bound params.
struct Fragment {
    sql: String,
    params: SqlParams,
}

impl Fragment {
    fn empty() -> Self {
        Self {
            sql: String::new(),
            params: Vec::new(),
        }
    }
}

/// The result of compiling a definition.
#[derive(Debug, Clone, Default)]
pub struct CompiledView {
    /// SQL WHERE content (no leading WHERE); safe to embed as `AND ({sql})`.
    /// `"1=1"` means "no filtering".
    pub where_sql: String,
    /// Bound parameters in order.
    pub params: SqlParams,
    /// Honest notes about how the tree was evaluated (surfaced by the API).
    pub notes: Vec<String>,
}

/// Resolves SLA states to the conversations currently in them (the route
/// wires `sla::sla_alerts` here, exactly like the reference injects
/// `ctx.sla.slaAlerts()`).
pub type SlaResolver<'a> = Box<dyn Fn(&[&str]) -> Vec<i64> + 'a>;

/// The view compiler. Constructed per evaluation with the user's timezone and
/// (optionally) an injectable clock and SLA resolver.
pub struct ViewEngine<'a> {
    timezone: String,
    now: Option<i64>,
    resolve_sla: Option<SlaResolver<'a>>,
}

impl<'a> ViewEngine<'a> {
    /// A new engine for a timezone (reference `new ViewEngine(db, {timezone})`).
    #[must_use]
    pub fn new(timezone: impl Into<String>) -> Self {
        Self {
            timezone: timezone.into(),
            now: None,
            resolve_sla: None,
        }
    }

    /// Injectable clock (tests).
    #[must_use]
    pub fn with_now(mut self, now_ms: i64) -> Self {
        self.now = Some(now_ms);
        self
    }

    /// Injectable SLA resolution (routes).
    #[must_use]
    pub fn with_sla_resolver(mut self, resolver: SlaResolver<'a>) -> Self {
        self.resolve_sla = Some(resolver);
        self
    }

    /// Compile a definition to parameterized SQL.
    ///
    /// # Errors
    /// [`ViewCompileError`] when the definition cannot be evaluated — the
    /// caller answers 422 and never persists it.
    pub fn compile(
        &self,
        def: &ViewDefinition,
    ) -> std::result::Result<CompiledView, ViewCompileError> {
        let mut notes = Vec::new();
        let fragment =
            self.compile_nodes(&def.conditions, def.combinator == "all", &mut notes, 0)?;
        if fragment.sql.is_empty() {
            // Empty condition list = no filtering (all conversations).
            return Ok(CompiledView {
                where_sql: "1=1".to_string(),
                params: Vec::new(),
                notes,
            });
        }
        Ok(CompiledView {
            where_sql: fragment.sql,
            params: fragment.params,
            notes,
        })
    }

    fn compile_nodes(
        &self,
        nodes: &[ViewNode],
        intersect: bool,
        notes: &mut Vec<String>,
        depth: u32,
    ) -> std::result::Result<Fragment, ViewCompileError> {
        if depth > MAX_TREE_DEPTH {
            return Err(ViewCompileError::new(
                "Condition tree nesting deeper than supported (max 10).",
            ));
        }
        let mut fragments = Vec::with_capacity(nodes.len());
        for node in nodes {
            fragments.push(self.compile_node(node, notes, depth)?);
        }
        let non_empty: Vec<&Fragment> = fragments.iter().filter(|f| !f.sql.is_empty()).collect();
        if non_empty.is_empty() {
            return Ok(Fragment::empty());
        }
        let joiner = if intersect { " AND " } else { " OR " };
        let sql = non_empty
            .iter()
            .map(|f| format!("({})", f.sql))
            .collect::<Vec<_>>()
            .join(joiner);
        let mut params = Vec::new();
        for f in non_empty {
            params.extend(f.params.iter().cloned());
        }
        Ok(Fragment { sql, params })
    }

    fn compile_node(
        &self,
        node: &ViewNode,
        notes: &mut Vec<String>,
        depth: u32,
    ) -> std::result::Result<Fragment, ViewCompileError> {
        if let ViewNode::Group {
            combinator,
            children,
        } = node
        {
            if children.is_empty() {
                return Ok(Fragment::empty());
            }
            return self.compile_nodes(children, combinator == "all", notes, depth + 1);
        }
        self.compile_condition(node, notes)
    }

    fn compile_condition(
        &self,
        cond: &ViewNode,
        notes: &mut Vec<String>,
    ) -> std::result::Result<Fragment, ViewCompileError> {
        match cond {
            ViewNode::Group { .. } => unreachable!("groups handled in compile_node"),
            ViewNode::Status { statuses } => {
                if statuses.is_empty() {
                    return Ok(Fragment::empty());
                }
                Ok(Fragment {
                    sql: format!("c.status IN ({})", placeholders(statuses.len()).join(",")),
                    params: statuses.iter().map(|s| text(s)).collect(),
                })
            }
            ViewNode::Assignee {
                assignee_local_ids,
                include_unassigned,
            } => {
                let mut parts: Vec<String> = Vec::new();
                let mut params = SqlParams::new();
                if !assignee_local_ids.is_empty() {
                    parts.push(format!(
                        "c.assignee_local_id IN ({})",
                        placeholders(assignee_local_ids.len()).join(",")
                    ));
                    params.extend(assignee_local_ids.iter().map(|id| integer(*id)));
                }
                if *include_unassigned {
                    parts.push("c.assignee_local_id IS NULL".to_string());
                }
                if parts.is_empty() {
                    return Ok(Fragment::empty());
                }
                Ok(Fragment {
                    sql: parts.join(" OR "),
                    params,
                })
            }
            ViewNode::Team { team_local_ids } => {
                if team_local_ids.is_empty() {
                    return Ok(Fragment::empty());
                }
                Ok(Fragment {
                    sql: format!(
                        "c.assigned_team_local_id IN ({})",
                        placeholders(team_local_ids.len()).join(",")
                    ),
                    params: team_local_ids.iter().map(|id| integer(*id)).collect(),
                })
            }
            ViewNode::Mailbox { mailbox_local_ids } => {
                if mailbox_local_ids.is_empty() {
                    return Ok(Fragment::empty());
                }
                Ok(Fragment {
                    sql: format!(
                        "c.mailbox_local_id IN ({})",
                        placeholders(mailbox_local_ids.len()).join(",")
                    ),
                    params: mailbox_local_ids.iter().map(|id| integer(*id)).collect(),
                })
            }
            ViewNode::Channel { channels } => {
                if channels.is_empty() {
                    return Ok(Fragment::empty());
                }
                Ok(Fragment {
                    sql: format!("c.type IN ({})", placeholders(channels.len()).join(",")),
                    params: channels.iter().map(|c| text(c)).collect(),
                })
            }
            ViewNode::Tags { tags, mode } => {
                let lowered: Vec<String> = tags.iter().map(|t| t.to_lowercase()).collect();
                if lowered.is_empty() {
                    return Ok(Fragment::empty());
                }
                let tag_exists = |names: &[String]| {
                    Fragment {
                    sql: format!(
                        "EXISTS (SELECT 1 FROM conversation_tags ct JOIN tags t ON t.id = ct.tag_id WHERE ct.conversation_id = c.id AND LOWER(t.name) IN ({}))",
                        placeholders(names.len()).join(",")
                    ),
                    params: names.iter().map(|n| text(n)).collect(),
                }
                };
                if mode == "any" {
                    return Ok(tag_exists(&lowered));
                }
                if mode == "none" {
                    let any = tag_exists(&lowered);
                    return Ok(Fragment {
                        sql: format!("NOT {}", any.sql),
                        params: any.params,
                    });
                }
                // all: every tag must appear on the SAME conversation.
                let parts = lowered
                    .iter()
                    .map(|_| "EXISTS (SELECT 1 FROM conversation_tags ct JOIN tags t ON t.id = ct.tag_id WHERE ct.conversation_id = c.id AND LOWER(t.name) = ?)")
                    .collect::<Vec<_>>();
                Ok(Fragment {
                    sql: parts.join(" AND "),
                    params: lowered.iter().map(|n| text(n)).collect(),
                })
            }
            ViewNode::CustomField {
                field_local_id,
                op,
                value,
            } => {
                let base = "cf.field_id = ?";
                match op.as_str() {
                    "is_empty" => Ok(Fragment {
                        sql: format!("NOT EXISTS (SELECT 1 FROM conversation_fields cf WHERE cf.conversation_id = c.id AND {base} AND cf.value IS NOT NULL AND cf.value <> '')"),
                        params: vec![integer(*field_local_id)],
                    }),
                    "is_not_empty" => Ok(Fragment {
                        sql: format!("EXISTS (SELECT 1 FROM conversation_fields cf WHERE cf.conversation_id = c.id AND {base} AND cf.value IS NOT NULL AND cf.value <> '')"),
                        params: vec![integer(*field_local_id)],
                    }),
                    "equals" => Ok(Fragment {
                        sql: format!("EXISTS (SELECT 1 FROM conversation_fields cf WHERE cf.conversation_id = c.id AND {base} AND LOWER(cf.value) = LOWER(?))"),
                        params: vec![integer(*field_local_id), text(value.as_deref().unwrap_or(""))],
                    }),
                    "not_equals" => Ok(Fragment {
                        sql: format!("NOT EXISTS (SELECT 1 FROM conversation_fields cf WHERE cf.conversation_id = c.id AND {base} AND LOWER(cf.value) = LOWER(?))"),
                        params: vec![integer(*field_local_id), text(value.as_deref().unwrap_or(""))],
                    }),
                    "contains" => Ok(Fragment {
                        sql: format!("EXISTS (SELECT 1 FROM conversation_fields cf WHERE cf.conversation_id = c.id AND {base} AND cf.value LIKE ? ESCAPE '\\')"),
                        params: vec![integer(*field_local_id), text(&format!("%{}%", escape_like(value.as_deref().unwrap_or(""))))],
                    }),
                    other => Err(ViewCompileError::new(format!(
                        "Unsupported custom_field operator: {other}"
                    ))),
                }
            }
            ViewNode::CustomerProperty {
                definition_id,
                op,
                value,
            } => {
                let def = "cp.definition_id = ?";
                let cust_join = "cp.customer_id = c.customer_local_id";
                match op.as_str() {
                    "is_empty" => Ok(Fragment {
                        sql: format!("c.customer_local_id IS NULL OR NOT EXISTS (SELECT 1 FROM customer_properties cp WHERE {cust_join} AND {def} AND cp.value IS NOT NULL AND cp.value <> '')"),
                        params: vec![integer(*definition_id)],
                    }),
                    "is_not_empty" => Ok(Fragment {
                        sql: format!("EXISTS (SELECT 1 FROM customer_properties cp WHERE {cust_join} AND {def} AND cp.value IS NOT NULL AND cp.value <> '')"),
                        params: vec![integer(*definition_id)],
                    }),
                    "equals" => Ok(Fragment {
                        sql: format!("EXISTS (SELECT 1 FROM customer_properties cp WHERE {cust_join} AND {def} AND LOWER(cp.value) = LOWER(?))"),
                        params: vec![integer(*definition_id), text(value.as_deref().unwrap_or(""))],
                    }),
                    "not_equals" => Ok(Fragment {
                        sql: format!("NOT EXISTS (SELECT 1 FROM customer_properties cp WHERE {cust_join} AND {def} AND LOWER(cp.value) = LOWER(?))"),
                        params: vec![integer(*definition_id), text(value.as_deref().unwrap_or(""))],
                    }),
                    "contains" => Ok(Fragment {
                        sql: format!("EXISTS (SELECT 1 FROM customer_properties cp WHERE {cust_join} AND {def} AND cp.value LIKE ? ESCAPE '\\')"),
                        params: vec![integer(*definition_id), text(&format!("%{}%", escape_like(value.as_deref().unwrap_or(""))))],
                    }),
                    "gt" | "gte" | "lt" | "lte" => {
                        let n = value
                            .as_deref()
                            .and_then(|v| v.parse::<f64>().ok())
                            .filter(|n| n.is_finite());
                        let Some(n) = n else {
                            return Err(ViewCompileError::new(
                                "customer_property numeric operator requires a numeric value.",
                            ));
                        };
                        let op_sql = match op.as_str() {
                            "gt" => ">",
                            "gte" => ">=",
                            "lt" => "<",
                            _ => "<=",
                        };
                        Ok(Fragment {
                            sql: format!("EXISTS (SELECT 1 FROM customer_properties cp WHERE {cust_join} AND {def} AND CAST(cp.value AS REAL) {op_sql} ?)"),
                            params: vec![integer(*definition_id), real(n)],
                        })
                    }
                    other => Err(ViewCompileError::new(format!(
                        "Unsupported customer_property operator: {other}"
                    ))),
                }
            }
            ViewNode::CustomerText { field, op, value } => {
                let v = value.as_deref().unwrap_or("").trim();
                match field.as_str() {
                    "name" => {
                        let name_expr =
                            "TRIM(COALESCE(cu.first_name, '') || ' ' || COALESCE(cu.last_name, ''))";
                        match op.as_str() {
                            "is_empty" => Ok(Fragment {
                                sql: format!("c.customer_local_id IS NULL OR NOT EXISTS (SELECT 1 FROM customers cu WHERE cu.id = c.customer_local_id AND {name_expr} <> '')"),
                                params: Vec::new(),
                            }),
                            "is_not_empty" => Ok(Fragment {
                                sql: format!("EXISTS (SELECT 1 FROM customers cu WHERE cu.id = c.customer_local_id AND {name_expr} <> '')"),
                                params: Vec::new(),
                            }),
                            "contains" => Ok(Fragment {
                                sql: format!("EXISTS (SELECT 1 FROM customers cu WHERE cu.id = c.customer_local_id AND {name_expr} LIKE ? ESCAPE '\\')"),
                                params: vec![text(&format!("%{}%", escape_like(v)))],
                            }),
                            "equals" => Ok(Fragment {
                                sql: format!("EXISTS (SELECT 1 FROM customers cu WHERE cu.id = c.customer_local_id AND {name_expr} = ?)"),
                                params: vec![text(v)],
                            }),
                            "not_contains" => Ok(Fragment {
                                sql: format!("c.customer_local_id IS NULL OR NOT EXISTS (SELECT 1 FROM customers cu WHERE cu.id = c.customer_local_id AND {name_expr} LIKE ? ESCAPE '\\')"),
                                params: vec![text(&format!("%{}%", escape_like(v)))],
                            }),
                            other => Err(ViewCompileError::new(format!(
                                "Unsupported customer_text operator: {other}"
                            ))),
                        }
                    }
                    "email" => match op.as_str() {
                        "is_empty" => Ok(Fragment {
                            sql: "NOT EXISTS (SELECT 1 FROM customer_emails ce WHERE ce.customer_id = c.customer_local_id)".to_string(),
                            params: Vec::new(),
                        }),
                        "is_not_empty" => Ok(Fragment {
                            sql: "EXISTS (SELECT 1 FROM customer_emails ce WHERE ce.customer_id = c.customer_local_id)".to_string(),
                            params: Vec::new(),
                        }),
                        "contains" => Ok(Fragment {
                            sql: "EXISTS (SELECT 1 FROM customer_emails ce WHERE ce.customer_id = c.customer_local_id AND ce.value LIKE ? ESCAPE '\\')".to_string(),
                            params: vec![text(&format!("%{}%", escape_like(v)))],
                        }),
                        "equals" => Ok(Fragment {
                            sql: "EXISTS (SELECT 1 FROM customer_emails ce WHERE ce.customer_id = c.customer_local_id AND ce.value = ?)".to_string(),
                            params: vec![text(v)],
                        }),
                        "not_contains" => Ok(Fragment {
                            sql: "c.customer_local_id IS NULL OR NOT EXISTS (SELECT 1 FROM customer_emails ce WHERE ce.customer_id = c.customer_local_id AND ce.value LIKE ? ESCAPE '\\')".to_string(),
                            params: vec![text(&format!("%{}%", escape_like(v)))],
                        }),
                        other => Err(ViewCompileError::new(format!(
                            "Unsupported customer_text operator: {other}"
                        ))),
                    },
                    "organization" => match op.as_str() {
                        "is_empty" => Ok(Fragment {
                            sql: "c.customer_local_id IS NULL OR NOT EXISTS (SELECT 1 FROM customers cu WHERE cu.id = c.customer_local_id AND cu.organization_id IS NOT NULL AND EXISTS (SELECT 1 FROM organizations o WHERE o.id = cu.organization_id AND o.name <> ''))".to_string(),
                            params: Vec::new(),
                        }),
                        "is_not_empty" => Ok(Fragment {
                            sql: "EXISTS (SELECT 1 FROM customers cu JOIN organizations o ON o.id = cu.organization_id WHERE cu.id = c.customer_local_id AND o.name <> '')".to_string(),
                            params: Vec::new(),
                        }),
                        "contains" => Ok(Fragment {
                            sql: "EXISTS (SELECT 1 FROM customers cu JOIN organizations o ON o.id = cu.organization_id WHERE cu.id = c.customer_local_id AND o.name LIKE ? ESCAPE '\\')".to_string(),
                            params: vec![text(&format!("%{}%", escape_like(v)))],
                        }),
                        "equals" => Ok(Fragment {
                            sql: "EXISTS (SELECT 1 FROM customers cu JOIN organizations o ON o.id = cu.organization_id WHERE cu.id = c.customer_local_id AND o.name = ?)".to_string(),
                            params: vec![text(v)],
                        }),
                        "not_contains" => Ok(Fragment {
                            sql: "c.customer_local_id IS NULL OR NOT EXISTS (SELECT 1 FROM customers cu JOIN organizations o ON o.id = cu.organization_id WHERE cu.id = c.customer_local_id AND o.name LIKE ? ESCAPE '\\')".to_string(),
                            params: vec![text(&format!("%{}%", escape_like(v)))],
                        }),
                        other => Err(ViewCompileError::new(format!(
                            "Unsupported customer_text operator: {other}"
                        ))),
                    },
                    other => Err(ViewCompileError::new(format!(
                        "Unsupported customer_text field: {other}"
                    ))),
                }
            }
            ViewNode::DateActivity {
                activity_field,
                mode,
                from,
                to,
                from_time,
                to_time,
            } => {
                let range = resolve_date_range(&DateRangeInput {
                    mode,
                    timezone: &self.timezone,
                    from: from.as_deref(),
                    to: to.as_deref(),
                    from_time: from_time.as_deref(),
                    to_time: to_time.as_deref(),
                    now: self.now,
                });
                let Some(range) = range else {
                    return Err(ViewCompileError::new(format!(
                        "Date filter '{mode}' requires valid from/to dates (YYYY-MM-DD)."
                    )));
                };
                let Some(column) = activity_field_column(activity_field) else {
                    return Err(ViewCompileError::new(format!(
                        "Unknown activity field: {activity_field}"
                    )));
                };
                notes.push(format!(
                    "Date filter '{}' resolved to {} ({} to {}, {} boundaries, {}).",
                    activity_field,
                    range.label,
                    range.from,
                    range.to,
                    range.kind.as_str(),
                    self.timezone
                ));
                // NULL timestamp = unknown, never "matches" a date window.
                Ok(Fragment {
                    sql: format!("{column} IS NOT NULL AND {column} >= ? AND {column} < ?"),
                    params: vec![text(&range.from), text(&range.to)],
                })
            }
            ViewNode::ResponseState { states } => {
                if states.is_empty() {
                    return Ok(Fragment::empty());
                }
                Ok(Fragment {
                    sql: format!(
                        "({RESPONSE_STATE_SQL}) IN ({})",
                        placeholders(states.len()).join(",")
                    ),
                    params: states.iter().map(|s| text(s)).collect(),
                })
            }
            ViewNode::ResponseAge {
                metric,
                op,
                minutes,
            } => {
                let Some((metric_sql, _label)) = age_metric_sql(metric) else {
                    return Err(ViewCompileError::new(format!(
                        "Unsupported response_age metric: {metric}"
                    )));
                };
                let op_sql = match op.as_str() {
                    "gt" => ">",
                    "gte" => ">=",
                    "lt" => "<",
                    _ => "<=",
                };
                Ok(Fragment {
                    sql: format!("{metric_sql} {op_sql} ?"),
                    params: vec![real(*minutes)],
                })
            }
            ViewNode::Sla { states, negate } => {
                let Some(resolve) = &self.resolve_sla else {
                    return Err(ViewCompileError::new(
                        "SLA conditions require the SLA service.",
                    ));
                };
                let queried: Vec<&str> = states.iter().map(String::as_str).collect();
                let ids = resolve(&queried);
                notes.push(format!(
                    "SLA state evaluated live against current mailbox business-hours ({} conversation(s) currently {}).",
                    ids.len(),
                    states.join("/")
                ));
                if ids.is_empty() {
                    return Ok(Fragment {
                        sql: if *negate { "1=1" } else { "1=0" }.to_string(),
                        params: Vec::new(),
                    });
                }
                let in_list = placeholders(ids.len()).join(",");
                Ok(Fragment {
                    sql: if *negate {
                        format!("c.id NOT IN ({in_list})")
                    } else {
                        format!("c.id IN ({in_list})")
                    },
                    params: ids.iter().map(|id| integer(*id)).collect(),
                })
            }
            ViewNode::Priority { priorities } => {
                if priorities.is_empty() {
                    return Ok(Fragment::empty());
                }
                Ok(Fragment {
                    sql: format!(
                        "c.supportos_priority IN ({})",
                        placeholders(priorities.len()).join(",")
                    ),
                    params: priorities.iter().map(|p| text(p)).collect(),
                })
            }
            ViewNode::TicketState {
                state_ids,
                include_no_state,
            } => {
                let mut parts: Vec<String> = Vec::new();
                let mut params = SqlParams::new();
                if !state_ids.is_empty() {
                    parts.push(format!(
                        "c.supportos_state_id IN ({})",
                        placeholders(state_ids.len()).join(",")
                    ));
                    params.extend(state_ids.iter().map(|id| integer(*id)));
                }
                if *include_no_state {
                    parts.push("c.supportos_state_id IS NULL".to_string());
                }
                if parts.is_empty() {
                    return Ok(Fragment::empty());
                }
                Ok(Fragment {
                    sql: parts.join(" OR "),
                    params,
                })
            }
            ViewNode::KnownIssue {
                any,
                known_issue_ids,
            } => {
                if *any {
                    return Ok(Fragment {
                        sql: "EXISTS (SELECT 1 FROM known_issue_links kic WHERE kic.conversation_id = c.id)".to_string(),
                        params: Vec::new(),
                    });
                }
                let Some(ids) = known_issue_ids else {
                    return Ok(Fragment::empty());
                };
                if ids.is_empty() {
                    return Ok(Fragment::empty());
                }
                Ok(Fragment {
                    sql: format!(
                        "EXISTS (SELECT 1 FROM known_issue_links kic WHERE kic.conversation_id = c.id AND kic.known_issue_id IN ({}))",
                        placeholders(ids.len()).join(",")
                    ),
                    params: ids.iter().map(|id| integer(*id)).collect(),
                })
            }
            ViewNode::AiAnalyzed { analyzed } => Ok(Fragment {
                sql: if *analyzed {
                    "EXISTS (SELECT 1 FROM ai_runs ar WHERE ar.conversation_id = c.id AND ar.type = 'ticket_analysis' AND ar.status = 'completed')".to_string()
                } else {
                    "NOT EXISTS (SELECT 1 FROM ai_runs ar WHERE ar.conversation_id = c.id AND ar.type = 'ticket_analysis' AND ar.status = 'completed')".to_string()
                },
                params: Vec::new(),
            }),
            ViewNode::InteractionSignal {
                dimension,
                value,
                negate,
            } => {
                let frag = Fragment {
                    sql: "EXISTS (SELECT 1 FROM client_current_signals s, json_each(s.signals_json) je WHERE s.conversation_id = c.id AND json_extract(je.value, '$.dimension') = ? AND json_extract(je.value, '$.value') = ?)".to_string(),
                    params: vec![text(dimension), text(value)],
                };
                Ok(Fragment {
                    sql: if *negate {
                        format!("NOT {}", frag.sql)
                    } else {
                        frag.sql
                    },
                    params: frag.params,
                })
            }
            ViewNode::AiAttribute {
                attribute,
                op,
                value,
            } => {
                // Keys are the closed catalog (whitelisted); every value is a
                // bound parameter — user input can never become SQL. Honest
                // unknown: a missing current row IS 'unknown'.
                let Some(def) = crate::catalog::AiAttributeKey::parse(attribute) else {
                    return Err(ViewCompileError::new(format!(
                        "Unknown AI attribute: {attribute}"
                    )));
                };
                let attr = attribute.as_str();
                if value.to_lowercase() == "unknown" {
                    if op != "equals" {
                        return Err(ViewCompileError::new(
                            "Only 'equals unknown' is supported for the unknown value.",
                        ));
                    }
                    return Ok(Fragment {
                        sql: "NOT EXISTS (SELECT 1 FROM ai_attributes aa WHERE aa.conversation_id = c.id AND aa.attribute = ? AND aa.superseded_at IS NULL)".to_string(),
                        params: vec![text(attr)],
                    });
                }
                if def.value_type() == crate::catalog::AttributeValueType::Number {
                    let Ok(n) = value.parse::<f64>() else {
                        return Err(ViewCompileError::new(
                            "ai_attribute numeric operator requires a numeric value.",
                        ));
                    };
                    if !n.is_finite() {
                        return Err(ViewCompileError::new(
                            "ai_attribute numeric operator requires a numeric value.",
                        ));
                    }
                    let op_sql = match op.as_str() {
                        "gt" => ">",
                        "gte" => ">=",
                        "lt" => "<",
                        "lte" => "<=",
                        _ => {
                            return Err(ViewCompileError::new(format!(
                                "ai_attribute (number) does not support operator {op}."
                            )));
                        }
                    };
                    return Ok(Fragment {
                        sql: format!("EXISTS (SELECT 1 FROM ai_attributes aa WHERE aa.conversation_id = c.id AND aa.attribute = ? AND aa.superseded_at IS NULL AND CAST(aa.value AS REAL) {op_sql} ?)"),
                        params: vec![text(attr), real(n)],
                    });
                }
                match op.as_str() {
                    "equals" => Ok(Fragment {
                        sql: "EXISTS (SELECT 1 FROM ai_attributes aa WHERE aa.conversation_id = c.id AND aa.attribute = ? AND aa.superseded_at IS NULL AND LOWER(aa.value) = LOWER(?))".to_string(),
                        params: vec![text(attr), text(value)],
                    }),
                    "not_equals" => Ok(Fragment {
                        sql: "NOT EXISTS (SELECT 1 FROM ai_attributes aa WHERE aa.conversation_id = c.id AND aa.attribute = ? AND aa.superseded_at IS NULL AND LOWER(aa.value) = LOWER(?))".to_string(),
                        params: vec![text(attr), text(value)],
                    }),
                    "contains" => Ok(Fragment {
                        sql: "EXISTS (SELECT 1 FROM ai_attributes aa WHERE aa.conversation_id = c.id AND aa.attribute = ? AND aa.superseded_at IS NULL AND aa.value LIKE ? ESCAPE '\\')".to_string(),
                        params: vec![text(attr), text(&format!("%{}%", escape_like(value)))],
                    }),
                    "not_contains" => Ok(Fragment {
                        sql: "NOT EXISTS (SELECT 1 FROM ai_attributes aa WHERE aa.conversation_id = c.id AND aa.attribute = ? AND aa.superseded_at IS NULL AND aa.value LIKE ? ESCAPE '\\')".to_string(),
                        params: vec![text(attr), text(&format!("%{}%", escape_like(value)))],
                    }),
                    "gt" | "gte" | "lt" | "lte" => {
                        // Ordered enum vocabularies support comparisons by position.
                        let vocab = def.values();
                        let Some(idx) = vocab.iter().position(|v| *v == value) else {
                            return Err(ViewCompileError::new(format!(
                                "ai_attribute '{attr}' comparison requires a value from its vocabulary ({}).",
                                vocab.join(", ")
                            )));
                        };
                        let bounds: Vec<&str> = match op.as_str() {
                            "gt" => vocab[idx + 1..].to_vec(),
                            "gte" => vocab[idx..].to_vec(),
                            "lt" => vocab[..idx].to_vec(),
                            _ => vocab[..idx + 1].to_vec(),
                        };
                        if bounds.is_empty() {
                            return Ok(Fragment {
                                sql: "1=0".to_string(),
                                params: Vec::new(),
                            });
                        }
                        let mut params = vec![text(attr)];
                        params.extend(bounds.iter().map(|b| text(b)));
                        Ok(Fragment {
                            sql: format!(
                                "EXISTS (SELECT 1 FROM ai_attributes aa WHERE aa.conversation_id = c.id AND aa.attribute = ? AND aa.superseded_at IS NULL AND aa.value IN ({}))",
                                placeholders(bounds.len()).join(",")
                            ),
                            params,
                        })
                    }
                    other => Err(ViewCompileError::new(format!(
                        "Unsupported ai_attribute operator: {other}"
                    ))),
                }
            }
            ViewNode::Unread { unread } => Ok(Fragment {
                sql: "c.is_unread = ?".to_string(),
                params: vec![integer(i64::from(*unread))],
            }),
            ViewNode::Snoozed { snoozed } => Ok(Fragment {
                sql: if *snoozed {
                    "c.snoozed_until IS NOT NULL AND c.snoozed_until > datetime('now')".to_string()
                } else {
                    "c.snoozed_until IS NULL OR c.snoozed_until <= datetime('now')".to_string()
                },
                params: Vec::new(),
            }),
            ViewNode::Customer { customer_local_ids } => {
                if customer_local_ids.is_empty() {
                    return Ok(Fragment::empty());
                }
                Ok(Fragment {
                    sql: format!(
                        "c.customer_local_id IN ({})",
                        placeholders(customer_local_ids.len()).join(",")
                    ),
                    params: customer_local_ids.iter().map(|id| integer(*id)).collect(),
                })
            }
        }
    }
}

fn placeholders(n: usize) -> Vec<&'static str> {
    vec!["?"; n]
}

fn text(s: &str) -> rusqlite::types::Value {
    rusqlite::types::Value::Text(s.to_string())
}

fn integer(i: i64) -> rusqlite::types::Value {
    rusqlite::types::Value::Integer(i)
}

fn real(f: f64) -> rusqlite::types::Value {
    rusqlite::types::Value::Real(f)
}

/// Escape LIKE metacharacters in a user value (same policy as the reference
/// viewEngine/segmentEngine `escapeLike`).
#[must_use]
pub fn escape_like(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    for ch in v.chars() {
        if matches!(ch, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

// ---------------------------------------------------------------------------
// Storage (reference InboxViewRepository)
// ---------------------------------------------------------------------------

/// Create the `inbox_views` table (reference migration 011). Idempotent.
///
/// # Errors
/// Returns [`crate::error::Error::Sqlite`] when the DDL fails.
pub fn ensure_inbox_views_table(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS inbox_views (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            description TEXT,
            definition TEXT NOT NULL,
            sort_order INTEGER NOT NULL DEFAULT 0,
            folder TEXT,
            version INTEGER NOT NULL DEFAULT 1,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        );",
    )?;
    Ok(())
}

/// `new Date().toISOString()` (reference `nowIso`).
fn now_iso() -> String {
    iso_utc(now_ms_epoch())
}

fn row_to_view(row: &rusqlite::Row<'_>) -> rusqlite::Result<SavedInboxView> {
    let definition_json: String = row.get(3)?;
    let definition: ViewDefinition = serde_json::from_str(&definition_json).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(e))
    })?;
    Ok(SavedInboxView {
        id: row.get(0)?,
        name: row.get(1)?,
        description: row.get(2)?,
        definition,
        sort_order: row.get(4)?,
        folder: row.get(5)?,
        version: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

const VIEW_COLUMNS: &str =
    "id, name, description, definition, sort_order, folder, version, created_at, updated_at";

/// List every saved view (ORDER BY sort_order ASC, name ASC).
///
/// # Errors
/// Returns [`crate::error::Error::Sqlite`] on query failure.
pub fn list_views(conn: &Connection) -> Result<Vec<SavedInboxView>> {
    ensure_inbox_views_table(conn)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {VIEW_COLUMNS} FROM inbox_views ORDER BY sort_order ASC, name ASC"
    ))?;
    let views = stmt
        .query_map([], row_to_view)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(views)
}

/// Fetch one saved view by id.
///
/// # Errors
/// Returns `Ok(None)` when the view does not exist;
/// [`crate::error::Error::Sqlite`] on query failure.
pub fn get_view(conn: &Connection, id: i64) -> Result<Option<SavedInboxView>> {
    ensure_inbox_views_table(conn)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {VIEW_COLUMNS} FROM inbox_views WHERE id = ?1"
    ))?;
    let view = stmt.query_row(rusqlite::params![id], row_to_view).ok();
    Ok(view)
}

/// Create a view (the definition is stored as a JSON condition tree — never
/// SQL).
///
/// # Errors
/// Returns [`crate::error::Error::Sqlite`] on failure.
pub fn create_view(
    conn: &Connection,
    name: &str,
    description: Option<&str>,
    definition: &ViewDefinition,
    sort_order: i64,
    folder: Option<&str>,
) -> Result<SavedInboxView> {
    ensure_inbox_views_table(conn)?;
    let definition_json = serde_json::to_string(definition)?;
    conn.execute(
        "INSERT INTO inbox_views (name, description, definition, sort_order, folder) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![name, description, definition_json, sort_order, folder],
    )?;
    let id = conn.last_insert_rowid();
    get_view(conn, id)?.ok_or_else(|| {
        crate::error::Error::Config(format!("saved view {id} vanished after insert"))
    })
}

/// Patch a view. `version` bumps only when the definition actually changed.
///
/// # Errors
/// Returns `Ok(None)` when the view does not exist;
/// [`crate::error::Error::Sqlite`] on failure.
pub fn update_view(
    conn: &Connection,
    id: i64,
    name: Option<&str>,
    description: Option<Option<&str>>,
    definition: Option<&ViewDefinition>,
    sort_order: Option<i64>,
    folder: Option<Option<&str>>,
) -> Result<Option<SavedInboxView>> {
    let Some(existing) = get_view(conn, id)? else {
        return Ok(None);
    };
    let new_definition_json = definition.map(serde_json::to_string).transpose()?;
    let definition_changed = new_definition_json.as_deref().is_some_and(|new| {
        Some(new) != serde_json::to_string(&existing.definition).ok().as_deref()
    });
    let new_name = name.unwrap_or(&existing.name);
    let new_description = description
        .map(|d| d.map(String::from))
        .unwrap_or_else(|| existing.description.clone());
    let new_definition = new_definition_json
        .unwrap_or_else(|| serde_json::to_string(&existing.definition).unwrap_or_default());
    let new_sort = sort_order.unwrap_or(existing.sort_order);
    let new_folder = folder
        .map(|f| f.map(String::from))
        .unwrap_or_else(|| existing.folder.clone());
    conn.execute(
        "UPDATE inbox_views SET
           name = ?1, description = ?2, definition = ?3, sort_order = ?4, folder = ?5,
           version = CASE WHEN ?6 THEN version + 1 ELSE version END,
           updated_at = ?7
         WHERE id = ?8",
        rusqlite::params![
            new_name,
            new_description,
            new_definition,
            new_sort,
            new_folder,
            i64::from(definition_changed),
            now_iso(),
            id
        ],
    )?;
    get_view(conn, id)
}

/// Delete a view; `false` when it did not exist.
///
/// # Errors
/// Returns [`crate::error::Error::Sqlite`] on failure.
pub fn delete_view(conn: &Connection, id: i64) -> Result<bool> {
    ensure_inbox_views_table(conn)?;
    let changes = conn.execute(
        "DELETE FROM inbox_views WHERE id = ?1",
        rusqlite::params![id],
    )?;
    Ok(changes > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;
    use tempfile::NamedTempFile;

    fn fresh_db() -> Connection {
        let f = NamedTempFile::new()
            .unwrap()
            .into_temp_path()
            .keep()
            .unwrap();
        let mut conn = crate::db::open(&f).unwrap();
        // The canonical boot chain: every column the compiler's fragments
        // reference (supportos_priority, deleted_at, activity columns, the
        // join tables) exists after apply_all.
        crate::bootstrap::apply_all(&mut conn).unwrap();
        conn
    }

    fn def(combinator: &str, conditions: Vec<ViewNode>) -> ViewDefinition {
        ViewDefinition {
            combinator: combinator.to_string(),
            conditions,
        }
    }

    fn compile(def: &ViewDefinition) -> CompiledView {
        ViewEngine::new("UTC").compile(def).unwrap()
    }

    fn param_str(c: &CompiledView, i: usize) -> String {
        match &c.params[i] {
            rusqlite::types::Value::Text(s) => s.clone(),
            other => panic!("expected text param, got {other:?}"),
        }
    }

    fn param_i64(c: &CompiledView, i: usize) -> i64 {
        match &c.params[i] {
            rusqlite::types::Value::Integer(i) => *i,
            other => panic!("expected integer param, got {other:?}"),
        }
    }

    // ---- per-kind compile tests (SQL shape + parameter binding) ----------

    #[test]
    fn status_compiles_to_in_list() {
        let c = compile(&def(
            "all",
            vec![ViewNode::Status {
                statuses: vec!["active".into(), "closed".into()],
            }],
        ));
        assert_eq!(c.where_sql, "(c.status IN (?,?))");
        assert_eq!(param_str(&c, 0), "active");
        assert_eq!(param_str(&c, 1), "closed");
    }

    #[test]
    fn assignee_ids_or_unassigned() {
        let c = compile(&def(
            "all",
            vec![ViewNode::Assignee {
                assignee_local_ids: vec![3, 7],
                include_unassigned: true,
            }],
        ));
        assert_eq!(
            c.where_sql,
            "(c.assignee_local_id IN (?,?) OR c.assignee_local_id IS NULL)"
        );
        assert_eq!(param_i64(&c, 0), 3);
        assert_eq!(param_i64(&c, 1), 7);

        let only_ids = compile(&def(
            "all",
            vec![ViewNode::Assignee {
                assignee_local_ids: vec![9],
                include_unassigned: false,
            }],
        ));
        assert_eq!(only_ids.where_sql, "(c.assignee_local_id IN (?))");
    }

    #[test]
    fn team_mailbox_channel_customer_in_lists() {
        let c = compile(&def(
            "all",
            vec![
                ViewNode::Team {
                    team_local_ids: vec![1],
                },
                ViewNode::Mailbox {
                    mailbox_local_ids: vec![2, 3],
                },
                ViewNode::Channel {
                    channels: vec!["chat".into()],
                },
                ViewNode::Customer {
                    customer_local_ids: vec![42],
                },
            ],
        ));
        assert_eq!(
            c.where_sql,
            "(c.assigned_team_local_id IN (?)) AND (c.mailbox_local_id IN (?,?)) AND (c.type IN (?)) AND (c.customer_local_id IN (?))"
        );
        assert_eq!(c.params.len(), 5);
    }

    #[test]
    fn tags_any_all_none() {
        let any = compile(&def(
            "all",
            vec![ViewNode::Tags {
                tags: vec!["Vip".into()],
                mode: "any".into(),
            }],
        ));
        assert_eq!(
            any.where_sql,
            "(EXISTS (SELECT 1 FROM conversation_tags ct JOIN tags t ON t.id = ct.tag_id WHERE ct.conversation_id = c.id AND LOWER(t.name) IN (?)))"
        );
        assert_eq!(param_str(&any, 0), "vip", "tag lowered");

        let all = compile(&def(
            "all",
            vec![ViewNode::Tags {
                tags: vec!["a".into(), "b".into()],
                mode: "all".into(),
            }],
        ));
        assert_eq!(
            all.where_sql,
            "(EXISTS (SELECT 1 FROM conversation_tags ct JOIN tags t ON t.id = ct.tag_id WHERE ct.conversation_id = c.id AND LOWER(t.name) = ?) AND EXISTS (SELECT 1 FROM conversation_tags ct JOIN tags t ON t.id = ct.tag_id WHERE ct.conversation_id = c.id AND LOWER(t.name) = ?))"
        );

        let none = compile(&def(
            "all",
            vec![ViewNode::Tags {
                tags: vec!["a".into()],
                mode: "none".into(),
            }],
        ));
        assert!(none
            .where_sql
            .starts_with("(NOT EXISTS (SELECT 1 FROM conversation_tags"));
    }

    #[test]
    fn custom_field_all_operators() {
        let cases: Vec<(&str, &str, Option<&str>, &str, usize)> = vec![
            ("is_empty", "", None, "NOT EXISTS (SELECT 1 FROM conversation_fields cf WHERE cf.conversation_id = c.id AND cf.field_id = ? AND cf.value IS NOT NULL AND cf.value <> '')", 1),
            ("is_not_empty", "", None, "EXISTS (SELECT 1 FROM conversation_fields cf WHERE cf.conversation_id = c.id AND cf.field_id = ? AND cf.value IS NOT NULL AND cf.value <> '')", 1),
            ("equals", "Pro", Some("Pro"), "EXISTS (SELECT 1 FROM conversation_fields cf WHERE cf.conversation_id = c.id AND cf.field_id = ? AND LOWER(cf.value) = LOWER(?))", 2),
            ("not_equals", "Pro", Some("Pro"), "NOT EXISTS (SELECT 1 FROM conversation_fields cf WHERE cf.conversation_id = c.id AND cf.field_id = ? AND LOWER(cf.value) = LOWER(?))", 2),
            ("contains", "50%off", Some("%50\\%off%"), "EXISTS (SELECT 1 FROM conversation_fields cf WHERE cf.conversation_id = c.id AND cf.field_id = ? AND cf.value LIKE ? ESCAPE '\\')", 2),
        ];
        for (op, raw, expected_value, expected_sql, expected_params) in cases {
            let c = compile(&def(
                "all",
                vec![ViewNode::CustomField {
                    field_local_id: 5,
                    op: op.into(),
                    value: Some(raw.to_string()),
                }],
            ));
            assert_eq!(c.where_sql, format!("({expected_sql})"), "op {op}");
            assert_eq!(c.params.len(), expected_params, "op {op}");
            if let Some(v) = expected_value {
                assert_eq!(param_str(&c, 1), v, "op {op}");
            }
        }
    }

    #[test]
    fn custom_field_unknown_operator_errors() {
        let err = ViewEngine::new("UTC")
            .compile(&def(
                "all",
                vec![ViewNode::CustomField {
                    field_local_id: 5,
                    op: "gt".into(),
                    value: None,
                }],
            ))
            .unwrap_err();
        assert_eq!(err.message, "Unsupported custom_field operator: gt");
    }

    #[test]
    fn customer_property_operators_including_numeric() {
        let c = compile(&def(
            "all",
            vec![ViewNode::CustomerProperty {
                definition_id: 2,
                op: "gte".into(),
                value: Some("10".into()),
            }],
        ));
        assert_eq!(
            c.where_sql,
            "(EXISTS (SELECT 1 FROM customer_properties cp WHERE cp.customer_id = c.customer_local_id AND cp.definition_id = ? AND CAST(cp.value AS REAL) >= ?))"
        );
        assert_eq!(param_i64(&c, 0), 2);
        assert_eq!(c.params[1], rusqlite::types::Value::Real(10.0));

        let empty = compile(&def(
            "all",
            vec![ViewNode::CustomerProperty {
                definition_id: 2,
                op: "is_empty".into(),
                value: None,
            }],
        ));
        assert!(empty
            .where_sql
            .starts_with("(c.customer_local_id IS NULL OR NOT EXISTS"));
        assert_eq!(empty.params.len(), 1);
    }

    #[test]
    fn customer_property_numeric_requires_number() {
        let err = ViewEngine::new("UTC")
            .compile(&def(
                "all",
                vec![ViewNode::CustomerProperty {
                    definition_id: 2,
                    op: "gt".into(),
                    value: Some("not-a-number".into()),
                }],
            ))
            .unwrap_err();
        assert_eq!(
            err.message,
            "customer_property numeric operator requires a numeric value."
        );
    }

    #[test]
    fn customer_text_name_email_organization() {
        let name = compile(&def(
            "all",
            vec![ViewNode::CustomerText {
                field: "name".into(),
                op: "contains".into(),
                value: Some("Emma L".into()),
            }],
        ));
        assert_eq!(
            name.where_sql,
            "(EXISTS (SELECT 1 FROM customers cu WHERE cu.id = c.customer_local_id AND TRIM(COALESCE(cu.first_name, '') || ' ' || COALESCE(cu.last_name, '')) LIKE ? ESCAPE '\\'))"
        );
        assert_eq!(param_str(&name, 0), "%Emma L%");

        let email_empty = compile(&def(
            "all",
            vec![ViewNode::CustomerText {
                field: "email".into(),
                op: "is_empty".into(),
                value: None,
            }],
        ));
        assert_eq!(
            email_empty.where_sql,
            "(NOT EXISTS (SELECT 1 FROM customer_emails ce WHERE ce.customer_id = c.customer_local_id))"
        );

        let org = compile(&def(
            "all",
            vec![ViewNode::CustomerText {
                field: "organization".into(),
                op: "equals".into(),
                value: Some("Acme".into()),
            }],
        ));
        assert_eq!(
            org.where_sql,
            "(EXISTS (SELECT 1 FROM customers cu JOIN organizations o ON o.id = cu.organization_id WHERE cu.id = c.customer_local_id AND o.name = ?))"
        );
        assert_eq!(param_str(&org, 0), "Acme");
    }

    #[test]
    fn customer_text_unknown_field_and_op_error() {
        let err = ViewEngine::new("UTC")
            .compile(&def(
                "all",
                vec![ViewNode::CustomerText {
                    field: "nope".into(),
                    op: "equals".into(),
                    value: None,
                }],
            ))
            .unwrap_err();
        assert_eq!(err.message, "Unsupported customer_text field: nope");
    }

    #[test]
    fn date_activity_compiles_range_scan_with_note() {
        let c = ViewEngine::new("UTC")
            .with_now(1_710_072_000_000) // 2024-03-10T12:00:00Z
            .compile(&def(
                "all",
                vec![ViewNode::DateActivity {
                    activity_field: "created_at".into(),
                    mode: "today".into(),
                    from: None,
                    to: None,
                    from_time: None,
                    to_time: None,
                }],
            ))
            .unwrap();
        assert_eq!(
            c.where_sql,
            "(c.created_at IS NOT NULL AND c.created_at >= ? AND c.created_at < ?)"
        );
        assert_eq!(param_str(&c, 0), "2024-03-10T00:00:00.000Z");
        assert_eq!(param_str(&c, 1), "2024-03-11T00:00:00.000Z");
        assert_eq!(
            c.notes,
            vec![
                "Date filter 'created_at' resolved to Today (2024-03-10T00:00:00.000Z to 2024-03-11T00:00:00.000Z, calendar boundaries, UTC)."
            ]
        );
    }

    #[test]
    fn date_activity_per_field_columns() {
        for (field, column) in [
            ("created_at", "c.created_at"),
            (
                "last_activity_at",
                "COALESCE(c.last_activity_at, c.created_at)",
            ),
            ("closed_at", "c.closed_at"),
            ("customer_waiting_since", "c.customer_waiting_since"),
        ] {
            let c = ViewEngine::new("UTC")
                .with_now(1_710_072_000_000)
                .compile(&def(
                    "all",
                    vec![ViewNode::DateActivity {
                        activity_field: field.into(),
                        mode: "last_24h".into(),
                        from: None,
                        to: None,
                        from_time: None,
                        to_time: None,
                    }],
                ))
                .unwrap();
            assert!(
                c.where_sql.contains(&format!("{column} IS NOT NULL")),
                "{field}"
            );
        }
    }

    #[test]
    fn date_activity_exact_requires_valid_dates() {
        let err = ViewEngine::new("UTC")
            .compile(&def(
                "all",
                vec![ViewNode::DateActivity {
                    activity_field: "created_at".into(),
                    mode: "exact_date".into(),
                    from: Some("2024-02-30".into()),
                    to: None,
                    from_time: None,
                    to_time: None,
                }],
            ))
            .unwrap_err();
        assert_eq!(
            err.message,
            "Date filter 'exact_date' requires valid from/to dates (YYYY-MM-DD)."
        );
    }

    #[test]
    fn response_state_uses_the_case_expression() {
        let c = compile(&def(
            "all",
            vec![ViewNode::ResponseState {
                states: vec!["customer_waiting".into(), "closed".into()],
            }],
        ));
        // compile_nodes wraps each fragment in parens, then the condition
        // itself wraps the CASE: "((CASE ... END) IN (?,?))".
        assert!(c.where_sql.starts_with("((CASE"));
        assert!(c.where_sql.ends_with("IN (?,?))"));
        assert!(c
            .where_sql
            .contains("WHEN c.status = 'closed' THEN 'closed'"));
        assert!(c.where_sql.contains("THEN 'needs_first_response'"));
        assert_eq!(param_str(&c, 0), "customer_waiting");
    }

    #[test]
    fn response_age_metric_and_operator() {
        let c = compile(&def(
            "all",
            vec![ViewNode::ResponseAge {
                metric: "customer_waiting_duration".into(),
                op: "gte".into(),
                minutes: 90.5,
            }],
        ));
        assert_eq!(
            c.where_sql,
            "((julianday('now') - COALESCE(julianday(c.customer_waiting_since), julianday('now'))) * 1440 >= ?)"
        );
        assert_eq!(c.params[0], rusqlite::types::Value::Real(90.5));

        let err = ViewEngine::new("UTC")
            .compile(&def(
                "all",
                vec![ViewNode::ResponseAge {
                    metric: "bogus".into(),
                    op: "gt".into(),
                    minutes: 1.0,
                }],
            ))
            .unwrap_err();
        assert_eq!(err.message, "Unsupported response_age metric: bogus");
    }

    #[test]
    fn sla_without_resolver_errors_like_the_reference() {
        // Reference quirk kept verbatim: create/update compile WITHOUT the SLA
        // resolver, so a view containing an SLA condition cannot be SAVED.
        let err = ViewEngine::new("UTC")
            .compile(&def(
                "all",
                vec![ViewNode::Sla {
                    states: vec!["at_risk".into()],
                    negate: false,
                }],
            ))
            .unwrap_err();
        assert_eq!(err.message, "SLA conditions require the SLA service.");
    }

    #[test]
    fn sla_with_resolver_lists_ids_and_notes() {
        let engine = ViewEngine::new("UTC").with_sla_resolver(Box::new(|states| {
            assert!(states == ["at_risk"] || states == ["breached"]);
            vec![11, 22]
        }));
        let c = engine
            .compile(&def(
                "all",
                vec![ViewNode::Sla {
                    states: vec!["at_risk".into()],
                    negate: false,
                }],
            ))
            .unwrap();
        assert_eq!(c.where_sql, "(c.id IN (?,?))");
        assert_eq!(param_i64(&c, 0), 11);
        assert_eq!(
            c.notes,
            vec![
                "SLA state evaluated live against current mailbox business-hours (2 conversation(s) currently at_risk)."
            ]
        );

        let negated = engine
            .compile(&def(
                "all",
                vec![ViewNode::Sla {
                    states: vec!["breached".into()],
                    negate: true,
                }],
            ))
            .unwrap();
        assert!(negated.where_sql.contains("NOT IN"));

        let empty = ViewEngine::new("UTC").with_sla_resolver(Box::new(|_| vec![]));
        let c = empty
            .compile(&def(
                "all",
                vec![ViewNode::Sla {
                    states: vec!["breached".into()],
                    negate: false,
                }],
            ))
            .unwrap();
        assert_eq!(c.where_sql, "(1=0)");
    }

    #[test]
    fn priority_ticket_state_known_issue() {
        let c = compile(&def(
            "all",
            vec![ViewNode::Priority {
                priorities: vec!["urgent".into()],
            }],
        ));
        assert_eq!(c.where_sql, "(c.supportos_priority IN (?))");

        let ts = compile(&def(
            "all",
            vec![ViewNode::TicketState {
                state_ids: vec![2],
                include_no_state: true,
            }],
        ));
        assert_eq!(
            ts.where_sql,
            "(c.supportos_state_id IN (?) OR c.supportos_state_id IS NULL)"
        );

        let ki = compile(&def(
            "all",
            vec![ViewNode::KnownIssue {
                any: false,
                known_issue_ids: Some(vec![4, 5]),
            }],
        ));
        assert!(ki.where_sql.contains("known_issue_links kic"));
        assert!(ki.where_sql.contains("kic.known_issue_id IN (?,?)"));

        let any = compile(&def(
            "all",
            vec![ViewNode::KnownIssue {
                any: true,
                known_issue_ids: None,
            }],
        ));
        assert_eq!(
            any.where_sql,
            "(EXISTS (SELECT 1 FROM known_issue_links kic WHERE kic.conversation_id = c.id))"
        );
    }

    #[test]
    fn ai_analyzed_and_interaction_signal() {
        let analyzed = compile(&def("all", vec![ViewNode::AiAnalyzed { analyzed: true }]));
        assert_eq!(
            analyzed.where_sql,
            "(EXISTS (SELECT 1 FROM ai_runs ar WHERE ar.conversation_id = c.id AND ar.type = 'ticket_analysis' AND ar.status = 'completed'))"
        );
        let not_analyzed = compile(&def("all", vec![ViewNode::AiAnalyzed { analyzed: false }]));
        assert!(not_analyzed
            .where_sql
            .starts_with("(NOT EXISTS (SELECT 1 FROM ai_runs"));

        let signal = compile(&def(
            "all",
            vec![ViewNode::InteractionSignal {
                dimension: "frustration".into(),
                value: "strong".into(),
                negate: true,
            }],
        ));
        assert!(signal
            .where_sql
            .starts_with("(NOT EXISTS (SELECT 1 FROM client_current_signals"));
        assert_eq!(param_str(&signal, 0), "frustration");
        assert_eq!(param_str(&signal, 1), "strong");
    }

    #[test]
    fn ai_attribute_text_and_enum_ops() {
        let equals = compile(&def(
            "all",
            vec![ViewNode::AiAttribute {
                attribute: "product".into(),
                op: "equals".into(),
                value: "Reports".into(),
            }],
        ));
        assert_eq!(
            equals.where_sql,
            "(EXISTS (SELECT 1 FROM ai_attributes aa WHERE aa.conversation_id = c.id AND aa.attribute = ? AND aa.superseded_at IS NULL AND LOWER(aa.value) = LOWER(?)))"
        );
        assert_eq!(param_str(&equals, 0), "product");
        assert_eq!(param_str(&equals, 1), "Reports");

        // Enum vocab comparison: urgency gte 'moderate' -> ['moderate','high'].
        let gte = compile(&def(
            "all",
            vec![ViewNode::AiAttribute {
                attribute: "urgency".into(),
                op: "gte".into(),
                value: "moderate".into(),
            }],
        ));
        assert!(
            gte.where_sql.ends_with("AND aa.value IN (?,?)))"),
            "{}",
            gte.where_sql
        );
        assert_eq!(param_str(&gte, 1), "moderate");
        assert_eq!(param_str(&gte, 2), "high");

        // lt off the bottom of the vocab matches nothing.
        let lt = compile(&def(
            "all",
            vec![ViewNode::AiAttribute {
                attribute: "urgency".into(),
                op: "lt".into(),
                value: "none".into(),
            }],
        ));
        assert_eq!(lt.where_sql, "(1=0)");
    }

    #[test]
    fn ai_attribute_number_and_unknown() {
        let n = compile(&def(
            "all",
            vec![ViewNode::AiAttribute {
                attribute: "question_count".into(),
                op: "gt".into(),
                value: "3".into(),
            }],
        ));
        assert!(n.where_sql.contains("CAST(aa.value AS REAL) > ?"));
        assert_eq!(c_number(&n, 1), 3.0);

        let unknown = compile(&def(
            "all",
            vec![ViewNode::AiAttribute {
                attribute: "product".into(),
                op: "equals".into(),
                value: "Unknown".into(),
            }],
        ));
        assert_eq!(
            unknown.where_sql,
            "(NOT EXISTS (SELECT 1 FROM ai_attributes aa WHERE aa.conversation_id = c.id AND aa.attribute = ? AND aa.superseded_at IS NULL))"
        );

        let err = ViewEngine::new("UTC")
            .compile(&def(
                "all",
                vec![ViewNode::AiAttribute {
                    attribute: "question_count".into(),
                    op: "gt".into(),
                    value: "many".into(),
                }],
            ))
            .unwrap_err();
        assert_eq!(
            err.message,
            "ai_attribute numeric operator requires a numeric value."
        );

        let err = ViewEngine::new("UTC")
            .compile(&def(
                "all",
                vec![ViewNode::AiAttribute {
                    attribute: "product".into(),
                    op: "not_equals".into(),
                    value: "unknown".into(),
                }],
            ))
            .unwrap_err();
        assert_eq!(
            err.message,
            "Only 'equals unknown' is supported for the unknown value."
        );

        let err = ViewEngine::new("UTC")
            .compile(&def(
                "all",
                vec![ViewNode::AiAttribute {
                    attribute: "urgency".into(),
                    op: "gt".into(),
                    value: "bogus".into(),
                }],
            ))
            .unwrap_err();
        assert_eq!(
            err.message,
            "ai_attribute 'urgency' comparison requires a value from its vocabulary (none, low, moderate, high)."
        );
    }

    fn c_number(c: &CompiledView, i: usize) -> f64 {
        match &c.params[i] {
            rusqlite::types::Value::Real(f) => *f,
            other => panic!("expected real param, got {other:?}"),
        }
    }

    #[test]
    fn unread_and_snoozed() {
        let unread = compile(&def("all", vec![ViewNode::Unread { unread: true }]));
        assert_eq!(unread.where_sql, "(c.is_unread = ?)");
        assert_eq!(param_i64(&unread, 0), 1);

        let snoozed = compile(&def("all", vec![ViewNode::Snoozed { snoozed: true }]));
        assert_eq!(
            snoozed.where_sql,
            "(c.snoozed_until IS NOT NULL AND c.snoozed_until > datetime('now'))"
        );
        let awake = compile(&def("all", vec![ViewNode::Snoozed { snoozed: false }]));
        assert_eq!(
            awake.where_sql,
            "(c.snoozed_until IS NULL OR c.snoozed_until <= datetime('now'))"
        );
    }

    // ---- AND/OR tree walking ----------------------------------------------

    #[test]
    fn empty_definition_is_1eq1_and_empty_groups_vanish() {
        let c = compile(&def("all", vec![]));
        assert_eq!(c.where_sql, "1=1");
        assert!(c.params.is_empty());
        assert!(c.notes.is_empty());

        let with_empty_group = compile(&def(
            "all",
            vec![ViewNode::Group {
                combinator: "all".into(),
                children: vec![],
            }],
        ));
        assert_eq!(with_empty_group.where_sql, "1=1");

        // A status condition with an empty list is also a no-op fragment.
        let only_empty_status = compile(&def("all", vec![ViewNode::Status { statuses: vec![] }]));
        assert_eq!(only_empty_status.where_sql, "1=1");
    }

    #[test]
    fn and_or_trees_wrap_and_join_like_the_reference() {
        let c = compile(&def(
            "all",
            vec![
                ViewNode::Status {
                    statuses: vec!["active".into()],
                },
                ViewNode::Group {
                    combinator: "any".into(),
                    children: vec![
                        ViewNode::Priority {
                            priorities: vec!["urgent".into()],
                        },
                        ViewNode::Priority {
                            priorities: vec!["high".into()],
                        },
                    ],
                },
            ],
        ));
        assert_eq!(
            c.where_sql,
            "(c.status IN (?)) AND ((c.supportos_priority IN (?)) OR (c.supportos_priority IN (?)))"
        );
        assert_eq!(c.params.len(), 3);

        let any = compile(&def(
            "any",
            vec![
                ViewNode::Status {
                    statuses: vec!["active".into()],
                },
                ViewNode::Status {
                    statuses: vec!["pending".into()],
                },
            ],
        ));
        assert_eq!(any.where_sql, "(c.status IN (?)) OR (c.status IN (?))");
    }

    #[test]
    fn depth_cap_at_ten() {
        let deep = |n: u32| -> ViewNode {
            let mut node = ViewNode::Status {
                statuses: vec!["active".into()],
            };
            for _ in 0..n {
                node = ViewNode::Group {
                    combinator: "all".into(),
                    children: vec![node],
                };
            }
            node
        };
        // 12 nested groups -> the 11th nesting level exceeds the cap.
        let err = ViewEngine::new("UTC")
            .compile(&def("all", vec![deep(12)]))
            .unwrap_err();
        assert_eq!(
            err.message,
            "Condition tree nesting deeper than supported (max 10)."
        );
        // 8 nested groups compile fine.
        assert!(ViewEngine::new("UTC")
            .compile(&def("all", vec![deep(8)]))
            .is_ok());
    }

    #[test]
    fn injection_shaped_values_stay_parameters() {
        let evil = "x'; DROP TABLE conversations;--";
        let c = compile(&def(
            "all",
            vec![ViewNode::Tags {
                tags: vec![evil.into()],
                mode: "any".into(),
            }],
        ));
        assert!(!c.where_sql.contains("DROP TABLE"));
        assert_eq!(param_str(&c, 0), evil.to_lowercase());
    }

    // ---- date-range resolver (reference tests/unit/activity.test.ts) ------

    const T0: i64 = 1_710_072_000_000; // 2024-03-10T12:00:00Z (US spring-forward)

    #[test]
    fn today_resolves_in_user_timezone() {
        let utc = resolve_date_range(&DateRangeInput {
            mode: "today",
            timezone: "UTC",
            now: Some(T0),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(utc.from, "2024-03-10T00:00:00.000Z");
        assert_eq!(utc.to, "2024-03-11T00:00:00.000Z");
        assert_eq!(utc.kind, DateRangeKind::Calendar);

        let ny = resolve_date_range(&DateRangeInput {
            mode: "today",
            timezone: "America/New_York",
            now: Some(T0),
            ..Default::default()
        })
        .unwrap();
        // Midnight NY on 2024-03-10 is 05:00Z (EST); midnight on 03-11 is
        // 04:00Z (EDT — the boundary CROSSED the DST jump).
        assert_eq!(ny.from, "2024-03-10T05:00:00.000Z");
        assert_eq!(ny.to, "2024-03-11T04:00:00.000Z");
    }

    #[test]
    fn spring_forward_day_is_23_hours() {
        let ny = resolve_date_range(&DateRangeInput {
            mode: "today",
            timezone: "America/New_York",
            now: Some(T0),
            ..Default::default()
        })
        .unwrap();
        let hours = (parse_ms(&ny.to) - parse_ms(&ny.from)) / 3_600_000;
        assert_eq!(hours, 23);
    }

    #[test]
    fn fall_back_day_is_25_hours() {
        let t1 = 1_730_635_200_000; // 2024-11-03T12:00:00Z (US fall-back day)
        let ny = resolve_date_range(&DateRangeInput {
            mode: "today",
            timezone: "America/New_York",
            now: Some(t1),
            ..Default::default()
        })
        .unwrap();
        let hours = (parse_ms(&ny.to) - parse_ms(&ny.from)) / 3_600_000;
        assert_eq!(hours, 25);
        assert_eq!(ny.from, "2024-11-03T04:00:00.000Z");
        assert_eq!(ny.to, "2024-11-04T05:00:00.000Z");
    }

    #[test]
    fn non_quarter_hour_offset_kathmandu() {
        let ktm = resolve_date_range(&DateRangeInput {
            mode: "today",
            timezone: "Asia/Kathmandu",
            now: Some(T0),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(ktm.from, "2024-03-09T18:15:00.000Z");
        assert_eq!(ktm.to, "2024-03-10T18:15:00.000Z");
    }

    #[test]
    fn half_hour_dst_shift_lord_howe() {
        let t = 1_712_491_200_000; // 2024-04-07T12:00:00Z
        let lhi = resolve_date_range(&DateRangeInput {
            mode: "today",
            timezone: "Australia/Lord_Howe",
            now: Some(t),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(lhi.from, "2024-04-06T13:00:00.000Z");
        assert_eq!(lhi.to, "2024-04-07T13:30:00.000Z");
        let hours = (parse_ms(&lhi.to) - parse_ms(&lhi.from)) as f64 / 3_600_000.0;
        assert!((hours - 24.5).abs() < 1e-9);
    }

    #[test]
    fn weeks_start_sunday() {
        let t = 1_710_336_000_000; // Wednesday 2024-03-13T12:00:00Z
        let utc = resolve_date_range(&DateRangeInput {
            mode: "this_week",
            timezone: "UTC",
            now: Some(t),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(utc.from, "2024-03-10T00:00:00.000Z");
        assert_eq!(utc.to, "2024-03-17T00:00:00.000Z");
    }

    #[test]
    fn yesterday_and_tomorrow_are_adjacent_local_days() {
        let y = resolve_date_range(&DateRangeInput {
            mode: "yesterday",
            timezone: "UTC",
            now: Some(T0),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(y.from, "2024-03-09T00:00:00.000Z");
        let t = resolve_date_range(&DateRangeInput {
            mode: "tomorrow",
            timezone: "UTC",
            now: Some(T0),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(t.from, "2024-03-11T00:00:00.000Z");
    }

    #[test]
    fn this_month_and_last_month() {
        let m = resolve_date_range(&DateRangeInput {
            mode: "this_month",
            timezone: "UTC",
            now: Some(T0),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(m.from, "2024-03-01T00:00:00.000Z");
        assert_eq!(m.to, "2024-04-01T00:00:00.000Z");
        let lm = resolve_date_range(&DateRangeInput {
            mode: "last_month",
            timezone: "UTC",
            now: Some(T0),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(lm.from, "2024-02-01T00:00:00.000Z");
        assert_eq!(lm.to, "2024-03-01T00:00:00.000Z");
        // Month clamping: July 31 - 1 month = June 30.
        let t = 1_722_427_200_000; // 2024-07-31T12:00:00Z
        let lm = resolve_date_range(&DateRangeInput {
            mode: "last_month",
            timezone: "UTC",
            now: Some(t),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(lm.from, "2024-06-01T00:00:00.000Z");
        assert_eq!(lm.to, "2024-07-01T00:00:00.000Z");
    }

    #[test]
    fn rolling_modes_are_exact_now_minus_windows() {
        let r = resolve_date_range(&DateRangeInput {
            mode: "last_24h",
            timezone: "UTC",
            now: Some(T0),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(r.from, "2024-03-09T12:00:00.000Z");
        assert_eq!(r.to, "2024-03-10T12:00:00.000Z");
        assert_eq!(r.kind, DateRangeKind::Rolling);
        assert_eq!(r.label, "Last 24 hours");

        let d7 = resolve_date_range(&DateRangeInput {
            mode: "last_7d",
            timezone: "America/New_York",
            now: Some(T0),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(parse_ms(&d7.to) - parse_ms(&d7.from), 7 * 24 * 3_600_000);
    }

    #[test]
    fn exact_date_resolves_local_calendar_day() {
        let r = resolve_date_range(&DateRangeInput {
            mode: "exact_date",
            timezone: "America/New_York",
            from: Some("2024-07-04"),
            now: Some(T0),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(r.from, "2024-07-04T04:00:00.000Z");
        assert_eq!(r.to, "2024-07-05T04:00:00.000Z");
        assert_eq!(r.label, "On 2024-07-04");
        assert_eq!(r.kind, DateRangeKind::Exact);
    }

    #[test]
    fn invalid_dates_are_rejected() {
        for (mode, from, to) in [
            ("exact_date", Some("2024-02-30"), None),
            ("exact_date", Some("2024-13-01"), None),
            ("exact_date", Some("not-a-date"), None),
            ("custom_range", Some("2024-01-01"), None),
        ] {
            let r = resolve_date_range(&DateRangeInput {
                mode,
                timezone: "UTC",
                from,
                to,
                now: Some(T0),
                ..Default::default()
            });
            assert!(r.is_none(), "{mode} {from:?} {to:?}");
        }
    }

    #[test]
    fn custom_range_is_inclusive_and_order_safe() {
        let r = resolve_date_range(&DateRangeInput {
            mode: "custom_range",
            timezone: "UTC",
            from: Some("2024-03-08"),
            to: Some("2024-03-10"),
            now: Some(T0),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(r.from, "2024-03-08T00:00:00.000Z");
        assert_eq!(r.to, "2024-03-11T00:00:00.000Z");
        assert_eq!(r.label, "2024-03-08 to 2024-03-10");

        let flipped = resolve_date_range(&DateRangeInput {
            mode: "custom_range",
            timezone: "UTC",
            from: Some("2024-03-10"),
            to: Some("2024-03-08"),
            now: Some(T0),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(flipped.from, "2024-03-08T00:00:00.000Z");
    }

    #[test]
    fn time_of_day_bounds_apply_inside_the_local_day() {
        // 09:00 on 2024-03-10 NY is AFTER the 02:00->03:00 jump: EDT (-4).
        let r = resolve_date_range(&DateRangeInput {
            mode: "today",
            timezone: "America/New_York",
            from_time: Some("09:00"),
            to_time: Some("17:00"),
            now: Some(T0),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(r.from, "2024-03-10T13:00:00.000Z");
        assert_eq!(r.to, "2024-03-10T21:00:00.000Z");
    }

    #[test]
    fn time_of_day_before_dst_jump_uses_pre_jump_offset() {
        // 01:30 on 2024-03-10 NY is BEFORE the jump: EST (-5).
        let r = resolve_date_range(&DateRangeInput {
            mode: "today",
            timezone: "America/New_York",
            from_time: Some("01:30"),
            to_time: Some("01:45"),
            now: Some(T0),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(r.from, "2024-03-10T06:30:00.000Z");
        assert_eq!(r.to, "2024-03-10T06:45:00.000Z");
    }

    #[test]
    fn invalid_timezone_falls_back_to_utc() {
        let r = resolve_date_range(&DateRangeInput {
            mode: "today",
            timezone: "Not/AZone",
            now: Some(T0),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(r.from, "2024-03-10T00:00:00.000Z");
    }

    #[test]
    fn resolve_timezone_prefers_explicit_then_stored() {
        assert_eq!(
            resolve_timezone(Some("Europe/Berlin"), None),
            "Europe/Berlin"
        );
        assert_eq!(resolve_timezone(None, Some("Asia/Tokyo")), "Asia/Tokyo");
        let system = resolve_timezone(None, Some("system"));
        assert_ne!(system, "system");
        let garbage = resolve_timezone(Some("garbage!"), None);
        assert_ne!(garbage, "garbage!");
        assert!(is_valid_timezone("UTC"));
        assert!(!is_valid_timezone("Mars/Olympus"));
    }

    fn parse_ms(iso: &str) -> i64 {
        chrono::DateTime::parse_from_rfc3339(iso)
            .unwrap()
            .timestamp_millis()
    }

    // ---- dynamic re-resolution (reference integration/activity.test.ts) ---

    #[test]
    fn date_conditions_are_dynamic_with_the_clock() {
        let defn = def(
            "all",
            vec![ViewNode::DateActivity {
                activity_field: "created_at".into(),
                mode: "today".into(),
                from: None,
                to: None,
                from_time: None,
                to_time: None,
            }],
        );
        let c1 = ViewEngine::new("UTC")
            .with_now(1_717_246_400_000) // 2024-06-01T12:00:00Z
            .compile(&defn)
            .unwrap();
        assert_eq!(param_str(&c1, 0), "2024-06-01T00:00:00.000Z");
        let c2 = ViewEngine::new("UTC")
            .with_now(1_717_331_200_000) // 2024-06-02T12:00:00Z
            .compile(&defn)
            .unwrap();
        assert_eq!(param_str(&c2, 0), "2024-06-02T00:00:00.000Z");
        assert!(c2.notes.join(" ").contains("Today"));
    }

    // ---- schema validation --------------------------------------------------

    #[test]
    fn parse_accepts_a_well_formed_nested_definition() {
        let v = serde_json::json!({
            "combinator": "all",
            "conditions": [
                { "kind": "status", "statuses": ["active", "pending"] },
                { "kind": "group", "combinator": "any", "children": [
                    { "kind": "tags", "tags": ["vip"], "mode": "any" },
                    { "kind": "date_activity", "activityField": "created_at", "mode": "last_7d" }
                ]}
            ]
        });
        let defn = parse_view_definition(&v).unwrap();
        assert_eq!(defn.combinator, "all");
        assert_eq!(defn.conditions.len(), 2);
        // Round-trips through the stored-JSON form.
        let stored = serde_json::to_string(&defn).unwrap();
        let back: ViewDefinition = serde_json::from_str(&stored).unwrap();
        assert_eq!(defn, back);
    }

    #[test]
    fn parse_rejects_unknown_kinds_and_bad_values() {
        let v = serde_json::json!({
            "combinator": "all",
            "conditions": [{ "kind": "bogus_kind" }]
        });
        let err = parse_view_definition(&v).unwrap_err();
        assert!(err[0].message.contains("Invalid discriminator value"));

        let v = serde_json::json!({
            "combinator": "all",
            "conditions": [{ "kind": "status", "statuses": ["bogus"] }]
        });
        let err = parse_view_definition(&v).unwrap_err();
        assert!(err[0].message.contains("Invalid enum value"));
        assert_eq!(err[0].path, "conditions.0.statuses.0");

        let v = serde_json::json!({
            "combinator": "all",
            "conditions": [{ "kind": "date_activity", "activityField": "created_at", "mode": "last_7d", "fromTime": "99:99" }]
        });
        let err = parse_view_definition(&v).unwrap_err();
        assert_eq!(err[0].message, "time must be a valid HH:mm");

        let v = serde_json::json!({
            "combinator": "all",
            "conditions": [{ "kind": "response_age", "metric": "bogus", "op": "gt", "minutes": 5 }]
        });
        assert!(parse_view_definition(&v).is_err());
    }

    #[test]
    fn parse_rejects_oversized_width_and_empty_groups() {
        let mut children = Vec::new();
        for _ in 0..26 {
            children.push(serde_json::json!({ "kind": "unread", "unread": true }));
        }
        let v = serde_json::json!({
            "combinator": "all",
            "conditions": [{ "kind": "group", "combinator": "all", "children": children }]
        });
        let err = parse_view_definition(&v).unwrap_err();
        assert!(err[0].message.contains("at most 25"));

        let v = serde_json::json!({
            "combinator": "all",
            "conditions": [{ "kind": "group", "combinator": "all", "children": [] }]
        });
        let err = parse_view_definition(&v).unwrap_err();
        assert!(err[0].message.contains("at least 1"));
    }

    // ---- storage -----------------------------------------------------------

    #[test]
    fn storage_round_trips_and_bumps_version_on_definition_change() {
        let conn = fresh_db();
        let defn = def(
            "all",
            vec![
                ViewNode::Status {
                    statuses: vec!["active".into()],
                },
                ViewNode::ResponseState {
                    states: vec!["customer_waiting".into()],
                },
            ],
        );
        let view = create_view(&conn, "Waiting room", None, &defn, 0, None).unwrap();
        assert_eq!(view.version, 1);
        assert_eq!(get_view(&conn, view.id).unwrap().unwrap().definition, defn);

        update_view(&conn, view.id, Some("Renamed"), None, None, None, None).unwrap();
        assert_eq!(
            get_view(&conn, view.id).unwrap().unwrap().version,
            1,
            "name-only change: no bump"
        );

        let new_def = def(
            "all",
            vec![ViewNode::Status {
                statuses: vec!["pending".into()],
            }],
        );
        update_view(&conn, view.id, None, None, Some(&new_def), None, None).unwrap();
        assert_eq!(
            get_view(&conn, view.id).unwrap().unwrap().version,
            2,
            "definition change bumps"
        );

        assert!(delete_view(&conn, view.id).unwrap());
        assert!(!delete_view(&conn, view.id).unwrap());
        assert!(get_view(&conn, view.id).unwrap().is_none());
    }

    #[test]
    fn storage_lists_in_sort_order() {
        let conn = fresh_db();
        let defn = def("all", vec![]);
        create_view(&conn, "Beta", None, &defn, 2, None).unwrap();
        create_view(&conn, "Alpha", None, &defn, 1, Some("f1")).unwrap();
        let views = list_views(&conn).unwrap();
        assert_eq!(views.len(), 2);
        assert_eq!(views[0].name, "Alpha");
        assert_eq!(views[0].folder.as_deref(), Some("f1"));
        assert_eq!(views[1].sort_order, 2);
    }

    #[test]
    fn compiled_sql_matches_expected_rows() {
        let conn = fresh_db();
        // Fixture: one waiting, one closed-urgent, one unassigned.
        // DB-03 (M047): conversations.mailbox_local_id/customer_local_id are
        // real FKs now (db::open sets foreign_keys=ON) — seed the parents the
        // fixture's concrete ids point at.
        conn.execute(
            "INSERT OR IGNORE INTO mailboxes (id, remote_id, name) VALUES (101, 101, 'Support')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO customers (id, remote_id, first_name) VALUES (2001, 2001, 'Fixture')",
            [],
        )
        .unwrap();
        let insert = |remote: i64, status: &str, subject: &str| {
            conn.execute(
                "INSERT INTO conversations (remote_id, number, status, subject, mailbox_local_id, customer_local_id)
                 VALUES (?1, ?1, ?2, ?3, 101, 2001)",
                params![remote, status, subject],
            )
            .unwrap();
            conn.last_insert_rowid()
        };
        let waiting = insert(9905, "active", "Waiting ticket");
        conn.execute(
            "UPDATE conversations SET first_customer_message_at = ?1, first_response_at = ?2,
                last_customer_reply_at = ?3, last_human_agent_response_at = ?4, customer_waiting_since = ?3
             WHERE id = ?5",
            params![
                "2024-03-10T07:00:00.000Z",
                "2024-03-10T08:00:00.000Z",
                "2024-03-10T10:00:00.000Z",
                "2024-03-10T08:00:00.000Z",
                waiting
            ],
        )
        .unwrap();
        let closed = insert(9906, "closed", "Closed urgent");
        conn.execute(
            "UPDATE conversations SET supportos_priority = 'urgent' WHERE id = ?1",
            params![closed],
        )
        .unwrap();
        let unassigned = insert(9907, "active", "Unassigned");

        let ids_for = |defn: &ViewDefinition| -> Vec<i64> {
            let compiled = ViewEngine::new("UTC").compile(defn).unwrap();
            let sql = format!(
                "SELECT c.id FROM conversations c WHERE c.deleted_at IS NULL AND c.merged_into_conversation_id IS NULL AND ({})",
                compiled.where_sql
            );
            let mut stmt = conn.prepare(&sql).unwrap();
            let mut ids: Vec<i64> = stmt
                .query_map(rusqlite::params_from_iter(compiled.params.iter()), |r| {
                    r.get(0)
                })
                .unwrap()
                .filter_map(|r| r.ok())
                .collect();
            ids.sort();
            ids
        };

        // response_state via the CASE expression.
        assert!(ids_for(&def(
            "all",
            vec![ViewNode::ResponseState {
                states: vec!["customer_waiting".into()]
            }]
        ))
        .contains(&waiting));
        // priority
        assert_eq!(
            ids_for(&def(
                "all",
                vec![ViewNode::Priority {
                    priorities: vec!["urgent".into()]
                }]
            )),
            vec![closed]
        );
        // unassigned via includeUnassigned
        assert!(ids_for(&def(
            "all",
            vec![ViewNode::Assignee {
                assignee_local_ids: vec![999_999],
                include_unassigned: true
            }]
        ))
        .contains(&unassigned));
        // response_age: waiting > 0 minutes
        assert!(ids_for(&def(
            "all",
            vec![ViewNode::ResponseAge {
                metric: "customer_waiting_duration".into(),
                op: "gt".into(),
                minutes: 0.0
            }]
        ))
        .contains(&waiting));
        // ai_analyzed false matches rows with no completed runs
        assert_eq!(
            ids_for(&def("all", vec![ViewNode::AiAnalyzed { analyzed: false }])).len(),
            3
        );
    }

    #[test]
    fn every_condition_kind_compiles_or_errors_deliberately() {
        // The 22 kinds must never hit the "unknown kind" arm — each either
        // compiles or fails with its own message.
        let sample: Vec<ViewNode> = vec![
            ViewNode::Status {
                statuses: vec!["active".into()],
            },
            ViewNode::Assignee {
                assignee_local_ids: vec![1],
                include_unassigned: false,
            },
            ViewNode::Team {
                team_local_ids: vec![1],
            },
            ViewNode::Mailbox {
                mailbox_local_ids: vec![1],
            },
            ViewNode::Channel {
                channels: vec!["email".into()],
            },
            ViewNode::Tags {
                tags: vec!["a".into()],
                mode: "any".into(),
            },
            ViewNode::CustomField {
                field_local_id: 1,
                op: "equals".into(),
                value: Some("v".into()),
            },
            ViewNode::CustomerProperty {
                definition_id: 1,
                op: "equals".into(),
                value: Some("v".into()),
            },
            ViewNode::CustomerText {
                field: "name".into(),
                op: "contains".into(),
                value: Some("v".into()),
            },
            ViewNode::DateActivity {
                activity_field: "created_at".into(),
                mode: "today".into(),
                from: None,
                to: None,
                from_time: None,
                to_time: None,
            },
            ViewNode::ResponseState {
                states: vec!["closed".into()],
            },
            ViewNode::ResponseAge {
                metric: "conversation_age".into(),
                op: "gt".into(),
                minutes: 10.0,
            },
            ViewNode::Priority {
                priorities: vec!["high".into()],
            },
            ViewNode::TicketState {
                state_ids: vec![1],
                include_no_state: false,
            },
            ViewNode::KnownIssue {
                any: true,
                known_issue_ids: None,
            },
            ViewNode::AiAnalyzed { analyzed: true },
            ViewNode::InteractionSignal {
                dimension: "frustration".into(),
                value: "strong".into(),
                negate: false,
            },
            ViewNode::AiAttribute {
                attribute: "product".into(),
                op: "equals".into(),
                value: "x".into(),
            },
            ViewNode::Unread { unread: false },
            ViewNode::Snoozed { snoozed: false },
            ViewNode::Customer {
                customer_local_ids: vec![1],
            },
        ];
        for node in sample {
            let defn = def("all", vec![node]);
            let result = ViewEngine::new("UTC").compile(&defn);
            assert!(
                result.is_ok(),
                "kind must compile: {:?}",
                result.err().map(|e| e.message)
            );
        }
        // SLA (22nd kind) compiles only with the resolver — reference quirk.
        let sla_def = def(
            "all",
            vec![ViewNode::Sla {
                states: vec!["at_risk".into()],
                negate: false,
            }],
        );
        assert!(ViewEngine::new("UTC")
            .with_sla_resolver(Box::new(|_| vec![1]))
            .compile(&sla_def)
            .is_ok());
    }
}
