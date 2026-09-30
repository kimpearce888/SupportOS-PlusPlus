//! Client interaction intelligence — deterministic signals + recency (M7-T01).
//!
//! Per spec M7: "client interaction intelligence."
//! Per the reference notes:
//! - "Maintains per-customer interaction signals: response style preferences,
//!   question count, technical familiarity, frustration cues, escalation intent."
//! - "Deterministic by default (zero AI): observable from message metadata."
//! - "Preference requires ≥3 observations before counting as a pattern
//!   (INTERACTION_MIN_OBSERVATIONS_FOR_PREFERENCE = 3)."
//! - "Recency weighting half-life: 90 days (INTERACTION_RECENCY_HALF_LIFE_DAYS)."
//! - "Change significance threshold: 0.34."
//!
//! ## Design
//!
//! The interaction engine is DETERMINISTIC — it derives signals from message
//! metadata (message count, word count, sentiment keywords, question marks,
//! etc.) without calling any AI provider. The AI layer (M6) fills
//! AI-designated slots only when LM Studio is enabled; this module provides
//! the deterministic baseline that always works.

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::error::Result;

/// The recency weighting half-life in days. Per the reference notes:
/// `INTERACTION_RECENCY_HALF_LIFE_DAYS = 90`.
pub const RECENCY_HALF_LIFE_DAYS: f64 = 90.0;

/// The minimum number of observations before counting as a pattern.
/// Per the reference notes: `INTERACTION_MIN_OBSERVATIONS_FOR_PREFERENCE = 3`.
pub const MIN_OBSERVATIONS_FOR_PREFERENCE: u32 = 3;

/// The change significance threshold. Per the reference notes: 0.34.
/// A change in a signal value is considered "significant" if the weighted
/// difference exceeds this threshold.
pub const CHANGE_SIGNIFICANCE_THRESHOLD: f64 = 0.34;

/// The M014 migration: creates the `interaction_signals` table.
pub const M014_SQL: &str = r#"
    CREATE TABLE IF NOT EXISTS interaction_signals (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        customer_id     INTEGER NOT NULL,
        signal_key      TEXT NOT NULL,
        signal_value    TEXT NOT NULL,
        observed_at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
        source_conversation_id INTEGER
    );
    CREATE INDEX IF NOT EXISTS idx_interaction_signals_customer
        ON interaction_signals (customer_id, signal_key, observed_at);

    UPDATE app_state SET schema_version = 14 WHERE id = 1;
"#;

/// Apply M014 migration. Idempotent.
pub fn apply_m014(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS interaction_signals (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            customer_id     INTEGER NOT NULL,
            signal_key      TEXT NOT NULL,
            signal_value    TEXT NOT NULL,
            observed_at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
            source_conversation_id INTEGER
        );
        CREATE INDEX IF NOT EXISTS idx_interaction_signals_customer
            ON interaction_signals (customer_id, signal_key, observed_at);",
    )?;
    let _ = conn.execute("UPDATE app_state SET schema_version = 14 WHERE id = 1", []);
    Ok(())
}

/// The closed vocabulary of interaction signal keys.
/// Per the reference notes: "response style preferences, question count,
/// technical familiarity, frustration cues, escalation intent."
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalKey {
    /// How the customer prefers to communicate (e.g., "formal", "casual").
    ResponseStyle,
    /// The number of questions in the customer's messages.
    QuestionCount,
    /// The customer's technical familiarity level (e.g., "beginner", "expert").
    TechnicalFamiliarity,
    /// Frustration cues detected (e.g., "calm", "frustrated", "angry").
    FrustrationCues,
    /// Whether the customer is signaling escalation intent (e.g., "yes", "no").
    EscalationIntent,
}

impl SignalKey {
    /// All variants in spec order.
    pub const ALL: [Self; 5] = [
        Self::ResponseStyle,
        Self::QuestionCount,
        Self::TechnicalFamiliarity,
        Self::FrustrationCues,
        Self::EscalationIntent,
    ];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ResponseStyle => "response_style",
            Self::QuestionCount => "question_count",
            Self::TechnicalFamiliarity => "technical_familiarity",
            Self::FrustrationCues => "frustration_cues",
            Self::EscalationIntent => "escalation_intent",
        }
    }
}

/// A stored interaction signal observation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InteractionSignal {
    pub id: Option<i64>,
    pub customer_id: i64,
    pub signal_key: String,
    pub signal_value: String,
    pub observed_at: String,
    pub source_conversation_id: Option<i64>,
}

/// Record an interaction signal observation.
pub fn record_signal(
    conn: &Connection,
    customer_id: i64,
    key: SignalKey,
    value: &str,
    source_conversation_id: Option<i64>,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO interaction_signals (customer_id, signal_key, signal_value, source_conversation_id)
         VALUES (?1, ?2, ?3, ?4)",
        params![customer_id, key.as_str(), value, source_conversation_id],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Get all signals for a customer, ordered oldest-first.
pub fn get_signals(conn: &Connection, customer_id: i64) -> Result<Vec<InteractionSignal>> {
    let mut stmt = conn.prepare(
        "SELECT id, customer_id, signal_key, signal_value, observed_at, source_conversation_id
         FROM interaction_signals
         WHERE customer_id = ?1
         ORDER BY observed_at ASC",
    )?;
    let rows = stmt
        .query_map(params![customer_id], |r| {
            Ok(InteractionSignal {
                id: r.get(0)?,
                customer_id: r.get(1)?,
                signal_key: r.get(2)?,
                signal_value: r.get(3)?,
                observed_at: r.get(4)?,
                source_conversation_id: r.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Get signals for a specific key for a customer.
pub fn get_signals_for_key(
    conn: &Connection,
    customer_id: i64,
    key: SignalKey,
) -> Result<Vec<InteractionSignal>> {
    let mut stmt = conn.prepare(
        "SELECT id, customer_id, signal_key, signal_value, observed_at, source_conversation_id
         FROM interaction_signals
         WHERE customer_id = ?1 AND signal_key = ?2
         ORDER BY observed_at ASC",
    )?;
    let rows = stmt
        .query_map(params![customer_id, key.as_str()], |r| {
            Ok(InteractionSignal {
                id: r.get(0)?,
                customer_id: r.get(1)?,
                signal_key: r.get(2)?,
                signal_value: r.get(3)?,
                observed_at: r.get(4)?,
                source_conversation_id: r.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Count observations for a specific key. Used to check if the minimum
/// observation count is met (≥3 per the reference notes).
pub fn count_observations(conn: &Connection, customer_id: i64, key: SignalKey) -> Result<u32> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM interaction_signals
         WHERE customer_id = ?1 AND signal_key = ?2",
        params![customer_id, key.as_str()],
        |r| r.get(0),
    )?;
    Ok(u32::try_from(count).unwrap_or(0))
}

/// Whether a signal key qualifies as a confirmed pattern (≥3 observations).
/// Per the reference notes: INTERACTION_MIN_OBSERVATIONS_FOR_PREFERENCE = 3.
pub fn is_confirmed_pattern(conn: &Connection, customer_id: i64, key: SignalKey) -> Result<bool> {
    Ok(count_observations(conn, customer_id, key)? >= MIN_OBSERVATIONS_FOR_PREFERENCE)
}

/// Compute the recency weight for an observation. Per the reference notes:
/// recency weighting half-life = 90 days. The weight is 0.5^((now - observed_at) / half_life).
///
/// Pure function — testable. `days_ago` is the number of days between the
/// observation and "now."
#[must_use]
pub fn recency_weight(days_ago: f64) -> f64 {
    if days_ago < 0.0 {
        // Future observation — full weight.
        return 1.0;
    }
    0.5_f64.powf(days_ago / RECENCY_HALF_LIFE_DAYS)
}

/// Whether a change in signal value is "significant." Per the reference notes:
/// change significance threshold = 0.34. A change is significant if the
/// weighted proportion of the new value exceeds the threshold.
///
/// Pure function — testable.
#[must_use]
pub fn is_significant_change(new_value_ratio: f64) -> bool {
    new_value_ratio.abs() > CHANGE_SIGNIFICANCE_THRESHOLD
}

/// Derive a deterministic signal from message text. Per the reference notes:
/// "Deterministic by default (zero AI): observable from message metadata."
///
/// This function uses simple heuristics:
/// - Question count: count of `?` in the text.
/// - Frustration cues: presence of frustration keywords ("angry", "frustrated",
///   "unacceptable", "ridiculous", "worst", "terrible").
/// - Escalation intent: presence of escalation keywords ("manager", "supervisor",
///   "escalate", "lawsuit", "cancel", "refund now").
/// - Response style: average word count per message (short=casual, long=formal).
/// - Technical familiarity: presence of technical keywords ("API", "endpoint",
///   "webhook", "JSON", "stack trace", "log").
///
/// Pure function — testable without a DB.
#[must_use]
pub fn derive_signal(key: SignalKey, message_text: &str) -> String {
    let text_lower = message_text.to_lowercase();
    match key {
        SignalKey::QuestionCount => {
            let count = message_text.matches('?').count();
            count.to_string()
        }
        SignalKey::FrustrationCues => {
            let frustration_words = [
                "angry",
                "frustrated",
                "unacceptable",
                "ridiculous",
                "worst",
                "terrible",
                "horrible",
                "disappointed",
                "annoyed",
                "furious",
            ];
            let found = frustration_words
                .iter()
                .filter(|w| text_lower.contains(*w))
                .count();
            if found >= 3 {
                "angry".to_string()
            } else if found >= 1 {
                "frustrated".to_string()
            } else {
                "calm".to_string()
            }
        }
        SignalKey::EscalationIntent => {
            let escalation_words = [
                "manager",
                "supervisor",
                "escalate",
                "lawsuit",
                "cancel",
                "refund now",
                "better business",
                "attorney",
            ];
            let found = escalation_words
                .iter()
                .filter(|w| text_lower.contains(*w))
                .count();
            if found >= 1 {
                "yes".to_string()
            } else {
                "no".to_string()
            }
        }
        SignalKey::TechnicalFamiliarity => {
            let tech_words = [
                "api",
                "endpoint",
                "webhook",
                "json",
                "stack trace",
                "log",
                "debug",
                "http",
                "ssl",
                "certificate",
                "dns",
                "firewall",
            ];
            let found = tech_words
                .iter()
                .filter(|w| text_lower.contains(*w))
                .count();
            if found >= 3 {
                "expert".to_string()
            } else if found >= 1 {
                "intermediate".to_string()
            } else {
                "beginner".to_string()
            }
        }
        SignalKey::ResponseStyle => {
            let word_count = message_text.split_whitespace().count();
            if word_count > 100 {
                "formal".to_string()
            } else if word_count > 30 {
                "detailed".to_string()
            } else {
                "casual".to_string()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    fn fresh_db() -> Connection {
        let f = NamedTempFile::new()
            .unwrap()
            .into_temp_path()
            .keep()
            .unwrap();
        let mut conn = crate::db::open(&f).unwrap();
        crate::db::ensure_migrations_table(&conn).unwrap();
        crate::migrations::run_all(&mut conn).unwrap();
        apply_m014(&conn).unwrap();
        conn
    }

    // ---- M014 migration ---------------------------------------------------

    #[test]
    fn m014_creates_interaction_signals_table() {
        let conn = fresh_db();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM interaction_signals", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn m014_is_idempotent() {
        let conn = fresh_db();
        apply_m014(&conn).unwrap();
    }

    // ---- SignalKey ---------------------------------------------------------

    #[test]
    fn signal_key_all_has_five_variants() {
        assert_eq!(SignalKey::ALL.len(), 5);
    }

    #[test]
    fn signal_key_as_str() {
        assert_eq!(SignalKey::ResponseStyle.as_str(), "response_style");
        assert_eq!(SignalKey::QuestionCount.as_str(), "question_count");
        assert_eq!(
            SignalKey::TechnicalFamiliarity.as_str(),
            "technical_familiarity"
        );
        assert_eq!(SignalKey::FrustrationCues.as_str(), "frustration_cues");
        assert_eq!(SignalKey::EscalationIntent.as_str(), "escalation_intent");
    }

    // ---- record_signal + get_signals ---------------------------------------

    #[test]
    fn record_signal_stores_and_retrieves() {
        let conn = fresh_db();
        let id = record_signal(&conn, 2001, SignalKey::QuestionCount, "3", Some(1001)).unwrap();
        assert!(id > 0);
        let signals = get_signals(&conn, 2001).unwrap();
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].signal_key, "question_count");
        assert_eq!(signals[0].signal_value, "3");
        assert_eq!(signals[0].source_conversation_id, Some(1001));
    }

    #[test]
    fn get_signals_returns_empty_for_nonexistent_customer() {
        let conn = fresh_db();
        assert!(get_signals(&conn, 9999).unwrap().is_empty());
    }

    #[test]
    fn get_signals_for_key_filters_correctly() {
        let conn = fresh_db();
        record_signal(&conn, 2001, SignalKey::QuestionCount, "2", None).unwrap();
        record_signal(&conn, 2001, SignalKey::FrustrationCues, "calm", None).unwrap();
        record_signal(&conn, 2001, SignalKey::QuestionCount, "5", None).unwrap();

        let question_signals = get_signals_for_key(&conn, 2001, SignalKey::QuestionCount).unwrap();
        assert_eq!(question_signals.len(), 2);
        for s in &question_signals {
            assert_eq!(s.signal_key, "question_count");
        }
    }

    // ---- count_observations + is_confirmed_pattern ------------------------

    #[test]
    fn count_observations_returns_correct_count() {
        let conn = fresh_db();
        assert_eq!(
            count_observations(&conn, 2001, SignalKey::QuestionCount).unwrap(),
            0
        );
        record_signal(&conn, 2001, SignalKey::QuestionCount, "1", None).unwrap();
        record_signal(&conn, 2001, SignalKey::QuestionCount, "2", None).unwrap();
        assert_eq!(
            count_observations(&conn, 2001, SignalKey::QuestionCount).unwrap(),
            2
        );
    }

    #[test]
    fn is_confirmed_pattern_requires_three_observations() {
        let conn = fresh_db();
        assert!(!is_confirmed_pattern(&conn, 2001, SignalKey::FrustrationCues).unwrap());
        record_signal(&conn, 2001, SignalKey::FrustrationCues, "calm", None).unwrap();
        assert!(!is_confirmed_pattern(&conn, 2001, SignalKey::FrustrationCues).unwrap());
        record_signal(&conn, 2001, SignalKey::FrustrationCues, "calm", None).unwrap();
        assert!(!is_confirmed_pattern(&conn, 2001, SignalKey::FrustrationCues).unwrap());
        record_signal(&conn, 2001, SignalKey::FrustrationCues, "calm", None).unwrap();
        assert!(is_confirmed_pattern(&conn, 2001, SignalKey::FrustrationCues).unwrap());
    }

    // ---- recency_weight ----------------------------------------------------

    #[test]
    fn recency_weight_zero_days_is_one() {
        assert!((recency_weight(0.0) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn recency_weight_at_half_life_is_half() {
        // At 90 days (the half-life), the weight should be 0.5.
        assert!((recency_weight(90.0) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn recency_weight_at_two_half_lives_is_quarter() {
        // At 180 days (2 × half-life), the weight should be 0.25.
        assert!((recency_weight(180.0) - 0.25).abs() < 1e-9);
    }

    #[test]
    fn recency_weight_negative_days_returns_one() {
        // Future observation → full weight.
        assert!((recency_weight(-10.0) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn recency_weight_decreases_with_age() {
        let w1 = recency_weight(10.0);
        let w2 = recency_weight(50.0);
        let w3 = recency_weight(100.0);
        assert!(w1 > w2);
        assert!(w2 > w3);
    }

    // ---- is_significant_change ---------------------------------------------

    #[test]
    fn is_significant_change_above_threshold() {
        assert!(is_significant_change(0.5));
        assert!(is_significant_change(-0.5));
    }

    #[test]
    fn is_significant_change_below_threshold() {
        assert!(!is_significant_change(0.3));
        assert!(!is_significant_change(-0.3));
        assert!(!is_significant_change(0.0));
    }

    #[test]
    fn is_significant_change_at_threshold_is_false() {
        // The threshold is 0.34; exactly at it is NOT significant (strictly greater).
        assert!(!is_significant_change(0.34));
    }

    // ---- derive_signal (deterministic, zero AI) ----------------------------

    #[test]
    fn derive_question_count_counts_question_marks() {
        assert_eq!(
            derive_signal(SignalKey::QuestionCount, "Hello? How are you? Why?"),
            "3"
        );
        assert_eq!(
            derive_signal(SignalKey::QuestionCount, "No questions here"),
            "0"
        );
    }

    #[test]
    fn derive_frustration_cues_detects_keywords() {
        assert_eq!(
            derive_signal(
                SignalKey::FrustrationCues,
                "This is unacceptable and ridiculous"
            ),
            "frustrated".to_string()
        );
        assert_eq!(
            derive_signal(
                SignalKey::FrustrationCues,
                "I am angry, frustrated, and furious about this"
            ),
            "angry"
        );
        assert_eq!(
            derive_signal(SignalKey::FrustrationCues, "Thanks for the help!"),
            "calm".to_string()
        );
    }

    #[test]
    fn derive_escalation_intent_detects_keywords() {
        assert_eq!(
            derive_signal(
                SignalKey::EscalationIntent,
                "I want to speak to your manager"
            ),
            "yes".to_string()
        );
        assert_eq!(
            derive_signal(SignalKey::EscalationIntent, "I'm going to escalate this"),
            "yes".to_string()
        );
        assert_eq!(
            derive_signal(SignalKey::EscalationIntent, "Thanks for the quick reply"),
            "no".to_string()
        );
    }

    #[test]
    fn derive_technical_familiarity_detects_keywords() {
        assert_eq!(
            derive_signal(
                SignalKey::TechnicalFamiliarity,
                "Check the API endpoint and webhook log for the JSON response"
            ),
            "expert".to_string()
        );
        assert_eq!(
            derive_signal(
                SignalKey::TechnicalFamiliarity,
                "Can you check the API logs?"
            ),
            "intermediate".to_string()
        );
        assert_eq!(
            derive_signal(SignalKey::TechnicalFamiliarity, "My internet isn't working"),
            "beginner".to_string()
        );
    }

    #[test]
    fn derive_response_style_by_word_count() {
        // 2 words → casual (≤ 30)
        let short = "Hi there";
        // 35 words → detailed (> 30, ≤ 100)
        let medium: String = std::iter::repeat_n("word ", 35).collect();
        // 105 words → formal (> 100)
        let long: String = std::iter::repeat_n("word ", 105).collect();

        assert_eq!(derive_signal(SignalKey::ResponseStyle, short), "casual");
        assert_eq!(derive_signal(SignalKey::ResponseStyle, &medium), "detailed");
        assert_eq!(derive_signal(SignalKey::ResponseStyle, &long), "formal");
    }

    #[test]
    fn derive_signal_is_deterministic() {
        // Same input → same output (zero AI — deterministic).
        let text = "Can you check the API? I'm frustrated with this.";
        let s1 = derive_signal(SignalKey::QuestionCount, text);
        let s2 = derive_signal(SignalKey::QuestionCount, text);
        assert_eq!(s1, s2, "deterministic: same input → same output");
    }

    // ---- constants ---------------------------------------------------------

    #[test]
    fn recency_half_life_is_90_days() {
        assert_eq!(RECENCY_HALF_LIFE_DAYS, 90.0);
    }

    #[test]
    fn min_observations_for_preference_is_3() {
        assert_eq!(MIN_OBSERVATIONS_FOR_PREFERENCE, 3);
    }

    #[test]
    fn change_significance_threshold_is_0_34() {
        assert!((CHANGE_SIGNIFICANCE_THRESHOLD - 0.34).abs() < 1e-9);
    }

    // ---- InteractionSignal serde -------------------------------------------

    #[test]
    fn interaction_signal_serializes() {
        let s = InteractionSignal {
            id: Some(1),
            customer_id: 2001,
            signal_key: "question_count".to_string(),
            signal_value: "3".to_string(),
            observed_at: "2026-10-01T10:00:00Z".to_string(),
            source_conversation_id: Some(1001),
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"signal_key\":\"question_count\""));
        assert!(json.contains("\"signal_value\":\"3\""));
    }
}
