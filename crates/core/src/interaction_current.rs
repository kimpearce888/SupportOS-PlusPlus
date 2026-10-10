//! Client current-signals — the per-conversation interaction snapshot
//! (reference `ai/interaction/heuristics.ts` + the current-signals write path
//! of `ai/interaction/engine.ts` + `interactionRepo.saveCurrentInteraction`).
//!
//! Deterministic by design (interaction spec #6/#10): pure functions over
//! customer-authored message text, ZERO AI. The snapshot feeds:
//!
//! - the Operations Center `high_effort` tile (tileFragments.ts),
//! - the saved-view `interaction_signal` condition (viewEngine.ts),
//!
//! and is refreshed after the initial sync for every conversation
//! (reference workers.ts:629-636) and on customer activity
//! (workers.ts:744-747).
//!
//! Only the CURRENT-signals layer lives here; the baseline/observation/
//! coaching machinery (engine.ts beyond `recordCurrentInteraction`) is a
//! separate domain.

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::error::Result;

// ---------------------------------------------------------------------------
// Table (reference migration 005 + 006's unique cap)
// ---------------------------------------------------------------------------

const CLIENT_CURRENT_SIGNALS_SQL: &str = "
        CREATE TABLE IF NOT EXISTS client_current_signals (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        conversation_id INTEGER NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
        customer_id INTEGER REFERENCES customers(id) ON DELETE CASCADE,
        signals_json TEXT NOT NULL,
        message_stats_json TEXT,
        customer_goal TEXT,
        sources TEXT NOT NULL DEFAULT 'heuristic',
        analysis_version TEXT,
        generated_at TEXT NOT NULL DEFAULT (datetime('now')),
        provenance TEXT NOT NULL DEFAULT 'heuristic',
        UNIQUE (conversation_id)
        );
        CREATE INDEX IF NOT EXISTS idx_client_current_signals_customer
            ON client_current_signals(customer_id);
        CREATE UNIQUE INDEX IF NOT EXISTS idx_client_current_signals_unique
            ON client_current_signals(conversation_id);
";

/// Ensure the table exists (idempotent).
///
/// # Errors
/// Returns [`crate::error::Error::Sqlite`] when the DDL fails.
pub fn ensure_client_current_signals_table(conn: &Connection) -> Result<()> {
    conn.execute_batch(CLIENT_CURRENT_SIGNALS_SQL)?;
    Ok(())
}

/// The `interaction_evidence` table (AI-19 / audit C7): one row per signal
/// observation carrying evidence, keyed by the LOCAL conversation id. Written
/// by [`record_current_interaction`] (replace-per-conversation, so the table
/// always mirrors the live snapshot) and served by
/// `GET /api/interaction/:conversationId/evidence`.
const INTERACTION_EVIDENCE_SQL: &str = "
        CREATE TABLE IF NOT EXISTS interaction_evidence (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        conversation_id INTEGER NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
        evidence_type TEXT NOT NULL,
        evidence_data TEXT NOT NULL,
        created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
        );
        CREATE INDEX IF NOT EXISTS idx_interaction_evidence_conversation
            ON interaction_evidence(conversation_id);
";

/// Ensure the `interaction_evidence` table exists (idempotent).
///
/// # Errors
/// Returns [`crate::error::Error::Sqlite`] when the DDL fails.
pub fn ensure_interaction_evidence_table(conn: &Connection) -> Result<()> {
    conn.execute_batch(INTERACTION_EVIDENCE_SQL)?;
    Ok(())
}

/// Replace the conversation's evidence rows from the freshly computed
/// snapshot (one row per signal that carries evidence).
fn replace_evidence_rows(
    conn: &Connection,
    conversation_id: i64,
    signals: &[InteractionSignal],
) -> Result<()> {
    ensure_interaction_evidence_table(conn)?;
    conn.execute(
        "DELETE FROM interaction_evidence WHERE conversation_id = ?1",
        params![conversation_id],
    )?;
    for s in signals {
        let Some(ev) = &s.evidence else { continue };
        conn.execute(
            "INSERT INTO interaction_evidence (conversation_id, evidence_type, evidence_data)
             VALUES (?1, ?2, ?3)",
            params![
                conversation_id,
                s.dimension,
                serde_json::json!({
                    "excerpt": ev.excerpt,
                    "thread_local_id": ev.thread_local_id,
                    "conversation_local_id": ev.conversation_local_id,
                    "value": s.value,
                    "confidence": s.confidence,
                    "source": s.source,
                })
                .to_string()
            ],
        )?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Vocabularies (reference shared/constants.ts — closed observable unions)
// ---------------------------------------------------------------------------

pub const INTERACTION_DIMENSIONS: [&str; 9] = [
    "tone",
    "directness",
    "detail",
    "technical_language",
    "question_structure",
    "urgency",
    "frustration",
    "expectation",
    "response_preference",
];

const TONE_VALUES: [&str; 8] = [
    "neutral",
    "friendly",
    "frustrated",
    "appreciative",
    "disappointed",
    "confrontational",
    "urgent",
    "uncertain",
];
const DIRECTNESS_VALUES: [&str; 4] = ["indirect", "conversational", "direct", "highly_direct"];
const DETAIL_VALUES: [&str; 5] = ["very_low", "low", "moderate", "high", "very_high"];
const TECHNICAL_VALUES: [&str; 4] = ["non_technical", "mixed", "technical", "highly_technical"];
const QUESTION_STRUCTURE_VALUES: [&str; 5] = [
    "single_question",
    "multiple_questions",
    "troubleshooting_oriented",
    "confirmation_oriented",
    "explanation_oriented",
];
const URGENCY_VALUES: [&str; 4] = ["none", "low", "moderate", "high"];
const FRUSTRATION_VALUES: [&str; 4] = ["none", "possible", "moderate", "strong"];
const EXPECTATION_VALUES: [&str; 7] = [
    "information",
    "explanation",
    "troubleshooting",
    "action",
    "immediate_resolution",
    "escalation",
    "confirmation",
];
const RESPONSE_PREFERENCE_VALUES: [&str; 6] = [
    "concise",
    "detailed",
    "step_by_step",
    "technical",
    "conversational",
    "outcome_focused",
];

/// Enum guard: heuristic values must always be in the observable vocabulary
/// (reference `isValidValue`).
/// The full ranked vocabulary for a dimension (engine.ts `RANKED_DIMENSIONS`)
/// — powers baseline dominance, change magnitude and rank spans.
pub fn dimension_vocabulary(dimension: &str) -> Option<&'static [&'static str]> {
    match dimension {
        "tone" => Some(&TONE_VALUES),
        "directness" => Some(&DIRECTNESS_VALUES),
        "detail" => Some(&DETAIL_VALUES),
        "technical_language" => Some(&TECHNICAL_VALUES),
        "question_structure" => Some(&QUESTION_STRUCTURE_VALUES),
        "urgency" => Some(&URGENCY_VALUES),
        "frustration" => Some(&FRUSTRATION_VALUES),
        "expectation" => Some(&EXPECTATION_VALUES),
        "response_preference" => Some(&RESPONSE_PREFERENCE_VALUES),
        _ => None,
    }
}

/// `needsDeEscalation` (heuristics.ts:195-197): strong frustration alone, or
/// high urgency combined with at least moderate frustration.
pub fn needs_de_escalation(signals: &[InteractionSignal]) -> bool {
    let frustration = |level: &str| {
        signals
            .iter()
            .any(|s| s.dimension == "frustration" && s.value == level)
    };
    frustration("strong")
        || (signals
            .iter()
            .any(|s| s.dimension == "urgency" && s.value == "high")
            && (frustration("moderate") || frustration("strong")))
}

pub fn is_valid_value(dimension: &str, value: &str) -> bool {
    let vocab: &[&str] = match dimension {
        "tone" => &TONE_VALUES,
        "directness" => &DIRECTNESS_VALUES,
        "detail" => &DETAIL_VALUES,
        "technical_language" => &TECHNICAL_VALUES,
        "question_structure" => &QUESTION_STRUCTURE_VALUES,
        "urgency" => &URGENCY_VALUES,
        "frustration" => &FRUSTRATION_VALUES,
        "expectation" => &EXPECTATION_VALUES,
        "response_preference" => &RESPONSE_PREFERENCE_VALUES,
        _ => return false,
    };
    vocab.contains(&value)
}

// ---------------------------------------------------------------------------
// Marker lists (heuristics.ts, verbatim)
// ---------------------------------------------------------------------------

const TECHNICAL_VOCAB: &[&str] = &[
    "api",
    "endpoint",
    "webhook",
    "payload",
    "json",
    "oauth",
    "token",
    "http",
    "https",
    "ssl",
    "tls",
    "dns",
    "timezone",
    "utc",
    "cron",
    "queue",
    "cache",
    "latency",
    "http status",
    "401",
    "403",
    "404",
    "500",
    "console",
    "log",
    "stack trace",
    "exception",
    "database",
    "sql",
    "index",
    "migration",
    "deploy",
    "build",
    "header",
    "request",
    "response",
    "callback",
    "integration",
    "sdk",
    "environment variable",
    "rate limit",
];
const URGENCY_MARKERS: &[&str] = &[
    "urgent",
    "asap",
    "immediately",
    "right now",
    "today",
    "as soon as possible",
    "deadline",
    "before our",
    "blocking",
    "production is down",
    "outage",
    "critical",
];
const FRUSTRATION_MARKERS: &[&str] = &[
    "again",
    "still",
    "already",
    "third time",
    "multiple times",
    "repeatedly",
    "frustrat",
    "unacceptable",
    "ridiculous",
    "seriously",
    "not working",
    "never works",
    "every time",
    "tired of",
    "disappointed",
    "kept",
    "nobody",
    "no one",
    "still not",
];
const DIRECT_MARKERS: &[&str] = &[
    "need you to",
    "fix",
    "tell me",
    "send me",
    "do not",
    "stop",
    "require",
    "must",
    "want",
    "instead of",
];
const INDIRECT_MARKERS: &[&str] = &[
    "i was wondering",
    "if possible",
    "could you perhaps",
    "when you have a moment",
    "sorry to bother",
    "not sure if",
    "might be able",
    "hopefully",
    "any chance",
];
const ACTION_EXPECTATION_MARKERS: &[&str] = &[
    "fix",
    "resolve",
    "restore",
    "refund",
    "escalate",
    "asap",
    "immediately",
    "compensation",
];
const ESCALATION_MARKERS: &[&str] = &[
    "escalate",
    "manager",
    "supervisor",
    "complaint",
    "legal",
    "cancel our",
    "terminate",
    "switching to",
];

// ---------------------------------------------------------------------------
// Message model + stats
// ---------------------------------------------------------------------------

/// One customer-authored message (reference `MessageForAnalysis`).
#[derive(Debug, Clone)]
pub struct MessageForAnalysis {
    pub text: String,
    pub thread_local_id: Option<i64>,
    pub conversation_local_id: Option<i64>,
    pub created_at: Option<String>,
}

/// Message metadata stats (reference `MessageStats`).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct MessageStats {
    pub customer_messages: usize,
    pub avg_message_length: usize,
    pub question_count: usize,
    pub exclamation_ratio: f64,
    pub caps_ratio: f64,
}

/// `computeMessageStats` (heuristics.ts:76-92).
#[must_use]
pub fn compute_message_stats(messages: &[MessageForAnalysis]) -> MessageStats {
    let texts: Vec<&str> = messages.iter().map(|m| m.text.as_str()).collect();
    let total_len: usize = texts.iter().map(|t| t.chars().count()).sum();
    let all = texts.join("\n");
    let question_count = all.matches('?').count();
    let exclamations = all.matches('!').count();
    let letters: String = all.chars().filter(|c| c.is_ascii_alphabetic()).collect();
    let caps_letters: String = all.chars().filter(|c| c.is_ascii_uppercase()).collect();
    // ([!?])\1{1,} — runs of doubled punctuation (the JS regex uses a
    // backreference, unsupported by Rust's regex crate; an explicit scan of
    // maximal same-char [!?] runs of length >= 2 counts the same matches).
    let (final_cur, final_len, repeated) =
        all.chars()
            .fold((None, 0usize, 0usize), |(cur, len, count), c| {
                if c == '!' || c == '?' {
                    match cur {
                        Some(prev) if prev == c => (Some(c), len + 1, count),
                        Some(_) => {
                            // a run of a DIFFERENT punctuation char just ended
                            (Some(c), 1, count + usize::from(len >= 2))
                        }
                        None => (Some(c), 1, count),
                    }
                } else {
                    (None, 0, count + usize::from(cur.is_some() && len >= 2))
                }
            });
    let repeated = repeated + usize::from(final_cur.is_some() && final_len >= 2);
    MessageStats {
        customer_messages: messages.len(),
        avg_message_length: if messages.is_empty() {
            0
        } else {
            total_len / messages.len()
        },
        question_count,
        exclamation_ratio: if texts.is_empty() {
            0.0
        } else {
            round2((exclamations + repeated * 2) as f64 / texts.len() as f64)
        },
        caps_ratio: if letters.is_empty() {
            0.0
        } else {
            round3(caps_letters.chars().count() as f64 / letters.chars().count() as f64)
        },
    }
}

/// JS `Number(x.toFixed(2))`.
fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// JS `Number(x.toFixed(3))`.
fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

// ---------------------------------------------------------------------------
// Marker matching (word-boundary, case-insensitive)
// ---------------------------------------------------------------------------

/// ASCII word character (JS `\w` = [A-Za-z0-9_]).
fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Word-boundary marker match: "against" must not match "again"
/// (reference `hasMarker` — `new RegExp(`\\b${escaped}\\b`, 'i')`).
/// Implemented as a manual boundary scan (no per-call regex compilation —
/// the sync path calls this per marker per message).
fn has_marker(text: &str, marker: &str) -> bool {
    let hay = text.to_lowercase();
    let needle = marker.to_lowercase();
    let hay_bytes = hay.as_bytes();
    let needle_bytes = needle.as_bytes();
    if needle_bytes.is_empty() || hay_bytes.len() < needle_bytes.len() {
        return false;
    }
    let mut start = 0;
    while let Some(pos) = hay[start..].find(&needle) {
        let at = start + pos;
        let end = at + needle_bytes.len();
        let before_ok = at == 0 || !is_word_char(hay_bytes[at - 1] as char);
        let after_ok = end == hay_bytes.len() || !is_word_char(hay_bytes[end] as char);
        if before_ok && after_ok {
            return true;
        }
        start = at + 1;
        if start >= hay_bytes.len() {
            break;
        }
    }
    false
}

fn count_markers(text: &str, markers: &[&str]) -> usize {
    markers.iter().filter(|m| has_marker(text, m)).count()
}

fn matches_any(text: &str, markers: &[&str]) -> bool {
    markers.iter().any(|m| has_marker(text, m))
}

fn excerpt(text: &str, max_len: usize) -> String {
    let clean: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if clean.chars().count() <= max_len {
        clean
    } else {
        let cut: String = clean.chars().take(max_len - 3).collect();
        format!("{cut}…")
    }
}

/// The evidence pointer (reference `InteractionSignal['evidence']`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Evidence {
    pub excerpt: String,
    pub thread_local_id: Option<i64>,
    pub conversation_local_id: Option<i64>,
}

/// One interaction signal (reference `InteractionSignal`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InteractionSignal {
    pub dimension: String,
    pub value: String,
    pub confidence: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<Evidence>,
    pub source: String,
}

// The classification regexes, compiled once (they run per message on the
// sync path — per-call compilation dominated the initial-sync time).
macro_rules! cached_regex {
    ($name:ident, $pattern:expr) => {
        fn $name() -> &'static regex::Regex {
            static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
            RE.get_or_init(|| regex::Regex::new($pattern).expect("valid classification regex"))
        }
    };
}
cached_regex!(
    tone_appreciative_re,
    r"thank(s| you)? for|appreciate|great (work|job|support|help)|awesome|excellent"
);
cached_regex!(tone_evidence_re, r"thank(s| you)? for|appreciate");
cached_regex!(
    question_troubleshooting_re,
    r"error|broken|failing|not work|issue|problem"
);
cached_regex!(urgency_severe_re, r"production is down|outage|emergency");
cached_regex!(
    expectation_info_re,
    r"how do i|how can i|what is|when will|where is|which|why|explain|reason|cause"
);
cached_regex!(
    expectation_info_strict_re,
    r"how do i|how can i|what is|when will|where is|which"
);
cached_regex!(expectation_explain_re, r"why|explain|reason|cause");
cached_regex!(
    expectation_evidence_re,
    r"why|explain|how do|how can|when will|where is|which|what is"
);
cached_regex!(
    pref_concise_re,
    r"keep (it |this )?(short|brief)|concise|short answer|no lengthy|brief answer|be brief"
);
cached_regex!(
    pref_step_by_step_re,
    r"step by step|step-by-step|walk me through|instructions"
);
cached_regex!(
    pref_detailed_re,
    r"detailed|thorough|in depth|in-depth|full explanation|comprehensive"
);

fn find_evidence(
    messages: &[MessageForAnalysis],
    predicate: impl Fn(&str) -> bool,
) -> Option<Evidence> {
    for m in messages {
        if predicate(&m.text.to_lowercase()) {
            return Some(Evidence {
                excerpt: excerpt(&m.text, 220),
                thread_local_id: m.thread_local_id,
                conversation_local_id: m.conversation_local_id,
            });
        }
    }
    None
}

/// The full-marker evidence predicate helper set (tone).
fn tone_evidence_predicate(t: &str) -> bool {
    matches_any(t, FRUSTRATION_MARKERS)
        || matches_any(t, URGENCY_MARKERS)
        || tone_evidence_re().is_match(t)
}

/// Deterministic current-ticket signals (reference `heuristicSignals`,
/// heuristics.ts:95-169). Confidence reflects heuristic reliability.
#[must_use]
pub fn heuristic_signals(
    messages: &[MessageForAnalysis],
    stats: &MessageStats,
) -> Vec<InteractionSignal> {
    if messages.is_empty() {
        return Vec::new();
    }
    let all: String = messages
        .iter()
        .map(|m| m.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
        .to_lowercase();
    let mut signals: Vec<InteractionSignal> = Vec::new();
    let mut push = |dimension: &str, value: &str, confidence: &str, evidence: Option<Evidence>| {
        if is_valid_value(dimension, value) {
            signals.push(InteractionSignal {
                dimension: dimension.to_string(),
                value: value.to_string(),
                confidence: confidence.to_string(),
                evidence,
                source: "heuristic".to_string(),
            });
        }
    };

    // Tone: observable markers only.
    let tone = if matches_any(&all, FRUSTRATION_MARKERS) {
        "frustrated"
    } else if matches_any(&all, URGENCY_MARKERS) {
        "urgent"
    } else if tone_appreciative_re().is_match(&all) {
        "appreciative"
    } else {
        "neutral"
    };
    push(
        "tone",
        tone,
        if tone == "neutral" { "low" } else { "medium" },
        find_evidence(messages, tone_evidence_predicate),
    );

    // Directness.
    let direct_hits = count_markers(&all, DIRECT_MARKERS);
    let indirect_hits = count_markers(&all, INDIRECT_MARKERS);
    let directness = if direct_hits >= 2 {
        "highly_direct"
    } else if direct_hits == 1 && indirect_hits == 0 {
        "direct"
    } else {
        "conversational"
    };
    // Evidence must point at whichever marker class drove the classification.
    push(
        "directness",
        directness,
        if direct_hits + indirect_hits >= 1 {
            "medium"
        } else {
            "low"
        },
        find_evidence(messages, |t| {
            matches_any(t, DIRECT_MARKERS) || matches_any(t, INDIRECT_MARKERS)
        }),
    );

    // Detail level from average message length.
    let avg_len = stats.avg_message_length;
    let detail = if avg_len > 900 {
        "very_high"
    } else if avg_len > 450 {
        "high"
    } else if avg_len > 150 {
        "moderate"
    } else if avg_len > 60 {
        "low"
    } else {
        "very_low"
    };
    let first_msg = messages.first();
    push(
        "detail",
        detail,
        if avg_len > 150 { "medium" } else { "low" },
        first_msg.map(|m| Evidence {
            excerpt: excerpt(&m.text, 220),
            thread_local_id: m.thread_local_id,
            conversation_local_id: m.conversation_local_id,
        }),
    );

    // Technical language vocabulary.
    let tech_hits = count_markers(&all, TECHNICAL_VOCAB);
    let technical = if tech_hits >= 4 {
        "highly_technical"
    } else if tech_hits >= 2 {
        "technical"
    } else if tech_hits == 1 {
        "mixed"
    } else {
        "non_technical"
    };
    push(
        "technical_language",
        technical,
        if tech_hits >= 1 { "medium" } else { "low" },
        find_evidence(messages, |t| matches_any(t, TECHNICAL_VOCAB)),
    );

    // Question structure.
    let question_structure = if stats.question_count >= 3 || stats.question_count == 2 {
        "multiple_questions"
    } else if stats.question_count == 1 {
        "single_question"
    } else if question_troubleshooting_re().is_match(&all) {
        "troubleshooting_oriented"
    } else {
        "explanation_oriented"
    };
    push(
        "question_structure",
        question_structure,
        if stats.question_count >= 1 {
            "medium"
        } else {
            "low"
        },
        find_evidence(messages, |t| t.contains('?')),
    );

    // Urgency.
    let urgency_hits = count_markers(&all, URGENCY_MARKERS);
    let urgency = if urgency_hits >= 3 || urgency_severe_re().is_match(&all) {
        "high"
    } else if urgency_hits >= 1 {
        "moderate"
    } else {
        "none"
    };
    push(
        "urgency",
        urgency,
        if urgency_hits >= 1 { "medium" } else { "low" },
        find_evidence(messages, |t| matches_any(t, URGENCY_MARKERS)),
    );

    // Frustration.
    let frustration_hits = count_markers(&all, FRUSTRATION_MARKERS);
    let frustration = if frustration_hits >= 3 {
        "strong"
    } else if frustration_hits >= 1 {
        "moderate"
    } else {
        "none"
    };
    push(
        "frustration",
        frustration,
        if frustration_hits >= 1 {
            "medium"
        } else {
            "low"
        },
        find_evidence(messages, |t| matches_any(t, FRUSTRATION_MARKERS)),
    );

    // Expectation — confidence follows whether a marker/phrase drove it.
    let expectation_marker_hit = matches_any(&all, ESCALATION_MARKERS)
        || count_markers(&all, ACTION_EXPECTATION_MARKERS) >= 1
        || expectation_info_re().is_match(&all);
    let expectation = if matches_any(&all, ESCALATION_MARKERS) {
        "escalation"
    } else if count_markers(&all, ACTION_EXPECTATION_MARKERS) >= 2
        || (urgency == "high" && matches_any(&all, ACTION_EXPECTATION_MARKERS))
    {
        "immediate_resolution"
    } else if matches_any(&all, ACTION_EXPECTATION_MARKERS) {
        "action"
    } else if expectation_info_strict_re().is_match(&all) {
        "information"
    } else if expectation_explain_re().is_match(&all) {
        "explanation"
    } else {
        "information"
    };
    push(
        "expectation",
        expectation,
        if expectation_marker_hit {
            "medium"
        } else {
            "low"
        },
        find_evidence(messages, |t| {
            matches_any(t, ACTION_EXPECTATION_MARKERS)
                || matches_any(t, ESCALATION_MARKERS)
                || expectation_evidence_re().is_match(t)
        }),
    );

    signals
}

/// The sanitizer result (reference safety.ts `SanitizationResult`, kept part).
#[derive(Debug, Clone)]
pub struct SanitizedSignals {
    pub signals: Vec<InteractionSignal>,
    pub removed: Vec<(String, String, String)>,
}

/// Sanitize a set of signals from ANY source (reference `sanitizeSignals`,
/// safety.ts:44-63): vocabulary guard + the evidence mandate (spec #8 —
/// significant signals without evidence are dropped) + dedup.
#[must_use]
pub fn sanitize_signals(signals: Vec<InteractionSignal>) -> SanitizedSignals {
    let mut kept: Vec<InteractionSignal> = Vec::new();
    let mut removed: Vec<(String, String, String)> = Vec::new();
    let mut seen: Vec<(String, String)> = Vec::new();
    for s in signals {
        let key = (s.dimension.clone(), s.value.clone());
        if seen.contains(&key) {
            continue;
        }
        if !is_valid_value(&s.dimension, &s.value) {
            removed.push((
                s.dimension,
                s.value,
                "value outside observable vocabulary (possible personality label)".to_string(),
            ));
            continue;
        }
        let has_evidence = s.evidence.as_ref().is_some_and(|e| !e.excerpt.is_empty());
        if (s.confidence == "high" || s.confidence == "medium") && !has_evidence {
            removed.push((
                s.dimension,
                s.value,
                "significant signal without evidence (spec #8)".to_string(),
            ));
            continue;
        }
        seen.push(key);
        kept.push(s);
    }
    SanitizedSignals {
        signals: kept,
        removed,
    }
}

/// Explicit in-message preference requests — top of the precedence chain
/// (reference `explicitCurrentPreference` / `heuristicResponsePreference`).
#[must_use]
pub fn explicit_current_preference(
    messages: &[MessageForAnalysis],
) -> Option<(String, Option<Evidence>)> {
    let all: String = messages
        .iter()
        .map(|m| m.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
        .to_lowercase();
    // (matcher, value) — every trigger phrase is also findable by the
    // evidence predicate below it (the reference's evidence mandate).
    let prefs: [(&regex::Regex, &str); 3] = [
        (pref_concise_re(), "concise"),
        (pref_step_by_step_re(), "step_by_step"),
        (pref_detailed_re(), "detailed"),
    ];
    for (re, value) in prefs {
        if re.is_match(&all) {
            let evidence = find_evidence(messages, |t| re.is_match(t));
            return Some((value.to_string(), evidence));
        }
    }
    None
}

/// The customer goal from the expectation signal (reference
/// `inferCustomerGoal`, engine.ts:733-746): a KNOWN expectation maps to its
/// goal text (unknown expectation values map to null); NO expectation falls
/// back to the subject; no subject = null.
#[must_use]
pub fn infer_customer_goal(signals: &[InteractionSignal], subject: Option<&str>) -> Option<String> {
    let expectation = signals
        .iter()
        .find(|s| s.dimension == "expectation")
        .map(|s| s.value.as_str());
    match expectation {
        Some(exp) => match exp {
            "immediate_resolution" => Some("Get the issue resolved immediately"),
            "action" => Some("Get a concrete action taken"),
            "escalation" => Some("Get the issue escalated"),
            "explanation" => Some("Understand why this is happening"),
            "information" => Some("Get specific information"),
            "troubleshooting" => Some("Get help troubleshooting"),
            "confirmation" => Some("Confirm a suspected behavior"),
            _ => None,
        }
        .map(String::from),
        None => subject.map(|s| format!("Address: {s}")),
    }
}

// ---------------------------------------------------------------------------
// The engine's current-signals path
// ---------------------------------------------------------------------------

/// The computed current interaction (reference `CurrentInteraction`, kept
/// part — `is_returning_client` comes from history the port's coaching
/// domain owns; here it is computed from the mirror).
#[derive(Debug, Clone, Serialize)]
pub struct CurrentInteraction {
    pub conversation_local_id: i64,
    pub customer_local_id: Option<i64>,
    pub is_returning_client: bool,
    pub signals: Vec<InteractionSignal>,
    pub customer_goal: Option<String>,
    pub message_stats: MessageStats,
    /// 'heuristic' | 'heuristic+ai' | 'ai' (reference field; the card
    /// surfaces it on `current`).
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub sources: String,
    /// When the snapshot was generated (reference field).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub generated_at: Option<String>,
}

/// One customer thread row pulled by [`customer_messages`].
type CustomerThreadRow = (i64, Option<String>, Option<String>, Option<String>, String);

/// Load the customer-authored messages of a conversation (reference
/// `customerMessages` — threads type='customer', state='published',
/// non-deleted, oldest first; `body_html ?? body_text` via htmlToText).
pub(crate) fn customer_messages(
    conn: &Connection,
    conversation_id: i64,
) -> Result<Vec<MessageForAnalysis>> {
    let mut stmt = conn.prepare(
        "SELECT id, body_html, body_text, remote_created_at, created_at
           FROM conversation_threads
          WHERE conversation_id = ?1
            AND type = 'customer'
            AND state = 'published'
          ORDER BY remote_created_at ASC",
    )?;
    let rows: Vec<CustomerThreadRow> = stmt
        .query_map(params![conversation_id], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get::<_, Option<String>>(4)?.unwrap_or_default(),
            ))
        })?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows
        .into_iter()
        .filter_map(|(id, body_html, body, remote_created_at, created_at)| {
            let raw = body_html
                .as_deref()
                .filter(|h| !h.is_empty())
                .or(body.as_deref())
                .unwrap_or("");
            let text = crate::demo::html_to_text(raw);
            if text.trim().is_empty() {
                None
            } else {
                Some(MessageForAnalysis {
                    text,
                    thread_local_id: Some(id),
                    conversation_local_id: Some(conversation_id),
                    created_at: remote_created_at.or(Some(created_at)),
                })
            }
        })
        .collect())
}

/// Compute (never persist) the current interaction (reference
/// `computeCurrentInteraction`, engine.ts:76-98).
///
/// # Errors
/// Returns [`crate::error::Error::Sqlite`] when the mirror read fails.
pub fn compute_current_interaction(
    conn: &Connection,
    conversation_id: i64,
) -> Result<Option<CurrentInteraction>> {
    let conv = conn
        .query_row(
            "SELECT customer_id, subject FROM conversations WHERE id = ?1 AND deleted_at IS NULL",
            params![conversation_id],
            |row| {
                Ok((
                    row.get::<_, Option<i64>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                ))
            },
        )
        .ok();
    let Some((customer_local_id, subject)) = conv else {
        return Ok(None);
    };
    let messages = customer_messages(conn, conversation_id)?;
    let stats = compute_message_stats(&messages);
    let signals = sanitize_signals(heuristic_signals(&messages, &stats)).signals;
    let mut signals = signals;
    if let Some((pref, evidence)) = explicit_current_preference(&messages) {
        signals.push(InteractionSignal {
            dimension: "response_preference".to_string(),
            value: pref,
            confidence: "high".to_string(),
            evidence,
            source: "heuristic".to_string(),
        });
    }
    let is_returning_client = customer_local_id
        .map(|cid| {
            conn.query_row(
                "SELECT COUNT(*) FROM conversations WHERE customer_id = ?1 AND id != ?2",
                params![cid, conversation_id],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n > 0)
            .unwrap_or(false)
        })
        .unwrap_or(false);
    let goal = infer_customer_goal(&signals, subject.as_deref());
    Ok(Some(CurrentInteraction {
        conversation_local_id: conversation_id,
        customer_local_id,
        is_returning_client,
        signals,
        customer_goal: goal,
        message_stats: stats,
        sources: "heuristic".to_string(),
        generated_at: Some(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)),
    }))
}

/// Persist the current-signals snapshot (reference `recordCurrentInteraction`
/// write path + `interactionRepo.saveCurrentInteraction` — ONE row per
/// conversation, upsert in place) AND record the snapshot's signals as
/// longitudinal observations for the customer (spec #29/#30 — idempotent
/// per conversation+dimension+source), rebuilding the baseline unless
/// `rebuild_baseline` is false (the history-backfill pass rebuilds ONCE
/// after its loop instead of per conversation).
///
/// # Errors
/// Returns [`crate::error::Error::Sqlite`] when the write fails.
pub fn record_current_interaction(
    conn: &Connection,
    conversation_id: i64,
) -> Result<Option<CurrentInteraction>> {
    record_current_interaction_opts(conn, conversation_id, true)
}

/// [`record_current_interaction`] with the reference's
/// `opts.rebuildBaseline` switch (engine.ts:104-141).
pub fn record_current_interaction_opts(
    conn: &Connection,
    conversation_id: i64,
    rebuild_baseline: bool,
) -> Result<Option<CurrentInteraction>> {
    let Some(current) = compute_current_interaction(conn, conversation_id)? else {
        return Ok(None);
    };
    ensure_client_current_signals_table(conn)?;
    let signals_json = serde_json::to_string(&current.signals)?;
    let stats_json = serde_json::to_string(&current.message_stats)?;
    conn.execute(
        "INSERT INTO client_current_signals
             (conversation_id, customer_id, signals_json, message_stats_json, customer_goal,
              sources, analysis_version, generated_at, provenance)
         VALUES (?1, ?2, ?3, ?4, ?5, 'heuristic', 'heuristic_v1', datetime('now'), 'heuristic')
         ON CONFLICT (conversation_id) DO UPDATE SET
           customer_id = excluded.customer_id,
           signals_json = excluded.signals_json,
           message_stats_json = excluded.message_stats_json,
           customer_goal = excluded.customer_goal,
           sources = excluded.sources,
           analysis_version = excluded.analysis_version,
           generated_at = datetime('now'),
           provenance = excluded.provenance",
        params![
            conversation_id,
            current.customer_local_id,
            signals_json,
            stats_json,
            current.customer_goal,
        ],
    )?;
    // AI-19 (C7): keep the evidence rows mirroring the fresh snapshot so
    // GET /api/interaction/:id/evidence serves real data. Failure to
    // refresh the evidence rows must not fail the snapshot write (the
    // reference engine treats evidence persistence as best-effort).
    let _ = replace_evidence_rows(conn, conversation_id, &current.signals);
    // AI-16: longitudinal observations (spec #29/#30) + baseline rebuild.
    // Observation failures never break the sync-path snapshot (the
    // reference wraps the whole record in try/catch on the backfill path).
    let _ = crate::interaction_engine::ensure_schema(conn);
    let _ = crate::interaction_engine::record_observations_for(conn, &current);
    if rebuild_baseline {
        if let Some(cid) = current.customer_local_id {
            let _ = crate::interaction_engine::rebuild_baseline(conn, cid);
        }
    }
    Ok(Some(current))
}

/// The stored current-signals snapshot (reference `getCurrentInteraction` —
/// the read side of interactionRepo; serves `GET /api/interaction/:id`).
#[derive(Debug, Clone, Serialize)]
pub struct StoredCurrentSignals {
    pub conversation_id: i64,
    pub customer_id: Option<i64>,
    pub signals: Vec<InteractionSignal>,
    pub customer_goal: Option<String>,
    pub message_stats: Option<MessageStats>,
    pub generated_at: Option<String>,
    pub provenance: Option<String>,
    pub analysis_version: Option<String>,
}

/// Load the stored snapshot for a conversation (reference
/// `getCurrentInteraction` — pure read, never computes).
///
/// # Errors
/// Returns [`crate::error::Error::Sqlite`] when the mirror read fails.
pub fn get_current_interaction(
    conn: &Connection,
    conversation_id: i64,
) -> Result<Option<StoredCurrentSignals>> {
    let row = conn
        .query_row(
            "SELECT customer_id, signals_json, customer_goal, message_stats_json,
                    generated_at, provenance, analysis_version
             FROM client_current_signals WHERE conversation_id = ?1",
            params![conversation_id],
            |r| {
                Ok((
                    r.get::<_, Option<i64>>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, Option<String>>(6)?,
                ))
            },
        )
        .ok();
    let Some((
        customer_id,
        signals_json,
        customer_goal,
        stats_json,
        generated_at,
        provenance,
        analysis_version,
    )) = row
    else {
        return Ok(None);
    };
    Ok(Some(StoredCurrentSignals {
        conversation_id,
        customer_id,
        signals: serde_json::from_str(&signals_json).unwrap_or_default(),
        customer_goal,
        message_stats: stats_json.and_then(|s| serde_json::from_str(&s).ok()),
        generated_at,
        provenance,
        analysis_version,
    }))
}

/// One dimension's aggregated value counts in the customer profile.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileDimensionValue {
    pub value: String,
    pub count: u32,
}

/// One dimension's summary in the customer profile.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileDimension {
    pub dimension: String,
    /// The most frequent value for the dimension.
    pub value: String,
    /// Total observations for the dimension.
    pub count: u32,
    /// Per-value counts (reference `signalBreakdown` richness).
    pub values: Vec<ProfileDimensionValue>,
}

/// The response-preference resolution in the customer profile.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileResponsePreference {
    pub value: String,
    /// "human" (explicit override) or "heuristic" (inferred majority).
    pub source: String,
    pub observations: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// The customer's interaction profile (AI-19 / C7: "serve profile from
/// signals" — aggregated from the client_current_signals layer).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InteractionProfile {
    pub customer_id: i64,
    pub total_signals: u32,
    pub conversations_analyzed: u32,
    pub signal_breakdown: Vec<ProfileDimension>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_preference: Option<ProfileResponsePreference>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub customer_goal: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_signal_at: Option<String>,
}

/// The human override row for the customer (interaction_overrides, m035).
#[derive(Debug, Clone, Serialize)]
pub struct InteractionOverrideRow {
    pub field: String,
    pub value: String,
    pub reason: Option<String>,
    pub updated_at: Option<String>,
}

/// Load the customer's explicit response-preference override.
pub fn get_interaction_override(
    conn: &Connection,
    customer_id: i64,
) -> Result<Option<InteractionOverrideRow>> {
    let row = conn
        .query_row(
            "SELECT field, value, reason, updated_at FROM interaction_overrides
             WHERE customer_id = ?1 AND field = 'response_preference'",
            params![customer_id],
            |r| {
                Ok(InteractionOverrideRow {
                    field: r.get(0)?,
                    value: r.get(1)?,
                    reason: r.get(2)?,
                    updated_at: r.get(3)?,
                })
            },
        )
        .ok();
    Ok(row)
}

/// Aggregate the customer's interaction profile from the current-signals
/// layer (`client_current_signals` rows of every conversation owned by the
/// customer). The response preference resolves the way the engine ranks it:
/// the explicit human override first, then the inferred majority once it has
/// >= [`MIN_OBSERVATIONS_FOR_PREFERENCE`] observations.
///
/// # Errors
/// Returns [`crate::error::Error::Sqlite`] when the mirror read fails.
pub fn interaction_profile(conn: &Connection, customer_id: i64) -> Result<InteractionProfile> {
    use std::collections::BTreeMap;
    let mut stmt = conn.prepare(
        "SELECT signals_json, customer_goal, generated_at
           FROM client_current_signals
          WHERE customer_id = ?1
          ORDER BY generated_at ASC",
    )?;
    let rows: Vec<(String, Option<String>, Option<String>)> = stmt
        .query_map(params![customer_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
        .filter_map(|r| r.ok())
        .collect();
    let mut per_dimension: BTreeMap<String, BTreeMap<String, u32>> = BTreeMap::new();
    let mut total_signals: u32 = 0;
    let mut last_goal: Option<String> = None;
    let mut last_signal_at: Option<String> = None;
    let conversations_analyzed = rows.len();
    for (signals_json, goal, generated_at) in &rows {
        let signals: Vec<InteractionSignal> =
            serde_json::from_str(signals_json).unwrap_or_default();
        total_signals += u32::try_from(signals.len()).unwrap_or(0);
        last_goal = goal.clone().or(last_goal);
        if generated_at.as_deref().is_some_and(|g| !g.is_empty()) {
            last_signal_at = generated_at.clone();
        }
        for s in signals {
            *per_dimension
                .entry(s.dimension)
                .or_default()
                .entry(s.value)
                .or_insert(0) += 1;
        }
    }
    let mut signal_breakdown: Vec<ProfileDimension> = per_dimension
        .into_iter()
        .map(|(dimension, values)| {
            let mut values: Vec<ProfileDimensionValue> = values
                .into_iter()
                .map(|(value, count)| ProfileDimensionValue { value, count })
                .collect();
            values.sort_by(|a, b| b.count.cmp(&a.count).then(a.value.cmp(&b.value)));
            let count: u32 = values.iter().map(|v| v.count).sum();
            let value = values.first().map(|v| v.value.clone()).unwrap_or_default();
            ProfileDimension {
                dimension,
                value,
                count,
                values,
            }
        })
        .collect();
    signal_breakdown.sort_by(|a, b| a.dimension.cmp(&b.dimension));

    // Response preference: explicit override first (the human always
    // wins), else the inferred majority once it is a confirmed pattern.
    let response_preference = if let Some(o) = get_interaction_override(conn, customer_id)? {
        Some(ProfileResponsePreference {
            value: o.value,
            source: "human".into(),
            observations: 1,
            reason: o.reason,
        })
    } else {
        signal_breakdown
            .iter()
            .find(|d| d.dimension == "response_preference")
            .and_then(|d| {
                let top = d.values.first()?;
                (top.count >= crate::intelligence::MIN_OBSERVATIONS_FOR_PREFERENCE).then_some(
                    ProfileResponsePreference {
                        value: top.value.clone(),
                        source: "heuristic".into(),
                        observations: top.count,
                        reason: None,
                    },
                )
            })
    };

    Ok(InteractionProfile {
        customer_id,
        total_signals,
        conversations_analyzed: u32::try_from(conversations_analyzed).unwrap_or(0),
        signal_breakdown,
        response_preference,
        customer_goal: last_goal,
        last_signal_at,
    })
}

/// Post-initial-sync backfill: refresh the snapshot for every non-deleted
/// conversation (reference workers.ts:629-636 — deterministic, no AI, so
/// profiles are populated immediately; spec #59). Errors on individual
/// conversations never break the sync (the reference wraps in try/catch).
///
/// # Errors
/// Returns the first error only when the conversation listing itself fails.
pub fn backfill_all(conn: &Connection) -> Result<usize> {
    let ids: Vec<i64> = {
        let mut stmt =
            conn.prepare("SELECT id FROM conversations WHERE deleted_at IS NULL ORDER BY id")?;
        let ids = stmt
            .query_map([], |r| r.get(0))?
            .filter_map(|r| r.ok())
            .collect();
        ids
    };
    let mut written = 0;
    for id in ids {
        if record_current_interaction(conn, id).is_ok_and(|c| c.is_some()) {
            written += 1;
        }
    }
    Ok(written)
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
        crate::bootstrap::apply_all(&mut conn).unwrap();
        conn
    }

    fn msg(text: &str) -> MessageForAnalysis {
        MessageForAnalysis {
            text: text.to_string(),
            thread_local_id: Some(1),
            conversation_local_id: Some(1),
            created_at: None,
        }
    }

    fn signal_of<'a>(signals: &'a [InteractionSignal], dim: &str) -> Option<&'a InteractionSignal> {
        signals.iter().find(|s| s.dimension == dim)
    }

    // ---- Message stats ------------------------------------------------------

    #[test]
    fn message_stats_basic() {
        let stats = compute_message_stats(&[msg("Hello there?"), msg("Hey!! Again?")]);
        assert_eq!(stats.customer_messages, 2);
        assert_eq!(stats.avg_message_length, 12); // 12 + 12 = 24 / 2
        assert_eq!(stats.question_count, 2);
        // (2 exclamations + 1 doubled-run * 2) / 2 messages = 2.0
        assert!((stats.exclamation_ratio - 2.0).abs() < 1e-9);
        // "Hellothere?HeyAgain" letters: 18, caps: H,H,A = 3 -> 0.167
        assert!(
            (stats.caps_ratio - 0.167).abs() < 1e-9,
            "{:?}",
            stats.caps_ratio
        );
        assert_eq!(compute_message_stats(&[]).customer_messages, 0);
    }

    // ---- Marker boundaries ---------------------------------------------------

    #[test]
    fn marker_word_boundary_against_not_again() {
        assert!(has_marker("this is still not working", "still"));
        assert!(!has_marker("against the wall", "again"));
        assert!(has_marker("AGAIN lost", "again")); // case-insensitive
        assert!(has_marker("error 401 unauthorized", "401"));
        assert!(!has_marker("error 4015", "401"));
    }

    // ---- Heuristic signals ----------------------------------------------------

    #[test]
    fn frustration_strong_at_three_markers() {
        let messages = vec![msg("This is STILL not working. I already called twice and nobody fixed it. Every time the same thing.")];
        let stats = compute_message_stats(&messages);
        let signals = heuristic_signals(&messages, &stats);
        let f = signal_of(&signals, "frustration").unwrap();
        assert_eq!(f.value, "strong");
        assert_eq!(f.confidence, "medium");
        assert!(f.evidence.as_ref().is_some_and(|e| !e.excerpt.is_empty()));
    }

    #[test]
    fn tone_appreciative_over_frustration_precedence() {
        // Frustration markers win over urgency markers win over thanks.
        let messages = vec![msg("Thanks for your help, this is great support!")];
        let stats = compute_message_stats(&messages);
        let signals = heuristic_signals(&messages, &stats);
        assert_eq!(signal_of(&signals, "tone").unwrap().value, "appreciative");

        let messages = vec![msg("Thanks! But this is urgent, deadline today.")];
        let stats = compute_message_stats(&messages);
        let signals = heuristic_signals(&messages, &stats);
        assert_eq!(signal_of(&signals, "tone").unwrap().value, "urgent");

        let messages = vec![msg("This is not working again, still broken.")];
        let stats = compute_message_stats(&messages);
        let signals = heuristic_signals(&messages, &stats);
        assert_eq!(signal_of(&signals, "tone").unwrap().value, "frustrated");
    }

    #[test]
    fn technical_language_ranks_vocabulary_hits() {
        let messages = vec![msg(
            "The API returns 401 when the OAuth token expires. Check the webhook payload and the HTTP headers.",
        )];
        let stats = compute_message_stats(&messages);
        let signals = heuristic_signals(&messages, &stats);
        let tech = signal_of(&signals, "technical_language").unwrap();
        // api, 401, oauth, token, webhook, payload, http, headers = 8 hits.
        assert_eq!(tech.value, "highly_technical");
    }

    #[test]
    fn detail_ranks_average_length() {
        // Thresholds: <=60 very_low, <=150 low, <=450 moderate, <=900 high,
        // else very_high (average message length in characters).
        for (len, expected) in [
            (30, "very_low"),
            (70, "low"),
            (200, "moderate"),
            (500, "high"),
            (1000, "very_high"),
        ] {
            let messages = vec![msg(&"a".repeat(len))];
            let stats = compute_message_stats(&messages);
            assert_eq!(stats.avg_message_length, len);
            let signals = heuristic_signals(&messages, &stats);
            assert_eq!(
                signal_of(&signals, "detail").unwrap().value,
                expected,
                "len {len}"
            );
        }
    }

    #[test]
    fn directness_highly_direct_at_two_markers() {
        let messages = vec![msg("I need you to fix this now. Tell me what is wrong.")];
        let stats = compute_message_stats(&messages);
        let signals = heuristic_signals(&messages, &stats);
        assert_eq!(
            signal_of(&signals, "directness").unwrap().value,
            "highly_direct"
        );

        let messages = vec![msg("I was wondering if you could perhaps help?")];
        let stats = compute_message_stats(&messages);
        let signals = heuristic_signals(&messages, &stats);
        assert_eq!(
            signal_of(&signals, "directness").unwrap().value,
            "conversational"
        );
    }

    #[test]
    fn expectation_escalation_beats_action() {
        let messages = vec![msg(
            "I want to speak to a manager and escalate this complaint immediately.",
        )];
        let stats = compute_message_stats(&messages);
        let signals = heuristic_signals(&messages, &stats);
        assert_eq!(
            signal_of(&signals, "expectation").unwrap().value,
            "escalation"
        );

        let messages = vec![msg("How do I reset my password? What is the process?")];
        let stats = compute_message_stats(&messages);
        let signals = heuristic_signals(&messages, &stats);
        assert_eq!(
            signal_of(&signals, "expectation").unwrap().value,
            "information"
        );
        assert_eq!(
            signal_of(&signals, "question_structure").unwrap().value,
            "multiple_questions"
        );
    }

    #[test]
    fn empty_messages_produce_no_signals() {
        assert!(heuristic_signals(&[], &MessageStats::default()).is_empty());
    }

    // ---- Sanitizer ---------------------------------------------------------------

    #[test]
    fn sanitize_drops_significant_signals_without_evidence() {
        let no_evidence = InteractionSignal {
            dimension: "frustration".into(),
            value: "strong".into(),
            confidence: "medium".into(),
            evidence: None,
            source: "heuristic".into(),
        };
        let out = sanitize_signals(vec![no_evidence]);
        assert!(out.signals.is_empty());
        assert_eq!(
            out.removed[0].2,
            "significant signal without evidence (spec #8)"
        );

        let low_ok = InteractionSignal {
            dimension: "frustration".into(),
            value: "none".into(),
            confidence: "low".into(),
            evidence: None,
            source: "heuristic".into(),
        };
        let out = sanitize_signals(vec![low_ok]);
        assert_eq!(out.signals.len(), 1); // low confidence carries no evidence mandate
    }

    #[test]
    fn sanitize_rejects_out_of_vocabulary_and_dedups() {
        let bad = InteractionSignal {
            dimension: "tone".into(),
            value: "psychopath".into(),
            confidence: "low".into(),
            evidence: None,
            source: "heuristic".into(),
        };
        let dup = InteractionSignal {
            dimension: "tone".into(),
            value: "neutral".into(),
            confidence: "low".into(),
            evidence: None,
            source: "heuristic".into(),
        };
        let out = sanitize_signals(vec![bad, dup.clone(), dup]);
        assert_eq!(out.signals.len(), 1);
        assert_eq!(
            out.removed[0].2,
            "value outside observable vocabulary (possible personality label)"
        );
    }

    // ---- Preferences + goal --------------------------------------------------------

    #[test]
    fn explicit_preference_variants() {
        let (pref, _) = explicit_current_preference(&[msg("Please keep it brief.")]).unwrap();
        assert_eq!(pref, "concise");
        let (pref, _) =
            explicit_current_preference(&[msg("Can you walk me through it step by step?")])
                .unwrap();
        assert_eq!(pref, "step_by_step");
        let (pref, _) =
            explicit_current_preference(&[msg("Give me a detailed explanation.")]).unwrap();
        assert_eq!(pref, "detailed");
        assert!(explicit_current_preference(&[msg("Hello")]).is_none());
    }

    #[test]
    fn goal_inference_from_expectation() {
        let signals = vec![InteractionSignal {
            dimension: "expectation".into(),
            value: "immediate_resolution".into(),
            confidence: "medium".into(),
            evidence: Some(Evidence {
                excerpt: "fix it now".into(),
                thread_local_id: None,
                conversation_local_id: None,
            }),
            source: "heuristic".into(),
        }];
        assert_eq!(
            infer_customer_goal(&signals, Some("Login broken")).as_deref(),
            Some("Get the issue resolved immediately")
        );
        let none = vec![];
        assert_eq!(
            infer_customer_goal(&none, Some("Login broken")).as_deref(),
            Some("Address: Login broken")
        );
        assert_eq!(infer_customer_goal(&none, None), None);
    }

    // ---- Persistence -----------------------------------------------------------------

    #[test]
    fn record_upserts_one_row_per_conversation() {
        let conn = fresh_db();
        conn.execute(
            "INSERT INTO customers (id, remote_id) VALUES (2001, 2001)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO conversations (remote_id, number, status, mailbox_id, customer_id)
             VALUES (1001, 1001, 'active', 1, 2001)",
            [],
        )
        .unwrap();
        let cid: i64 = conn
            .query_row(
                "SELECT id FROM conversations WHERE remote_id = 1001",
                [],
                |r| r.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO conversation_threads (conversation_id, type, state, body_text, from_type, created_at)
             VALUES (?1, 'customer', 'published', 'This is urgent, need you to fix the API today.', 'customer', datetime('now'))",
            params![cid],
        )
        .unwrap();

        let current = record_current_interaction(&conn, cid).unwrap().unwrap();
        assert!(!current.signals.is_empty());
        assert_eq!(current.message_stats.customer_messages, 1);

        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM client_current_signals", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(rows, 1);

        // Re-record: upsert in place (still one row, goal may change).
        conn.execute(
            "UPDATE conversation_threads SET body_text = 'Why does this happen? Please explain the cause.'
             WHERE conversation_id = ?1",
            params![cid],
        )
        .unwrap();
        record_current_interaction(&conn, cid).unwrap();
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM client_current_signals", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(rows, 1);

        // The stored signals_json round-trips the fragment's shape.
        let signals_json: String = conn
            .query_row(
                "SELECT signals_json FROM client_current_signals WHERE conversation_id = ?1",
                params![cid],
                |r| r.get(0),
            )
            .unwrap();
        let parsed: Vec<InteractionSignal> = serde_json::from_str(&signals_json).unwrap();
        assert!(parsed.iter().all(|s| s.source == "heuristic"));
    }

    #[test]
    fn record_skips_draft_and_non_customer_threads() {
        let conn = fresh_db();
        conn.execute(
            "INSERT INTO customers (id, remote_id) VALUES (2001, 2001)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO conversations (remote_id, number, status, mailbox_id, customer_id)
             VALUES (1002, 1002, 'active', 1, 2001)",
            [],
        )
        .unwrap();
        let cid: i64 = conn
            .query_row(
                "SELECT id FROM conversations WHERE remote_id = 1002",
                [],
                |r| r.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO conversation_threads (conversation_id, type, state, body_text, from_type, created_at)
             VALUES (?1, 'customer', 'draft', 'urgent fix now', 'customer', datetime('now'))",
            params![cid],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO conversation_threads (conversation_id, type, state, body_text, from_type, created_at)
             VALUES (?1, 'reply', 'published', 'we are on it', 'user', datetime('now'))",
            params![cid],
        )
        .unwrap();
        let current = record_current_interaction(&conn, cid).unwrap().unwrap();
        assert!(current.signals.is_empty()); // no published customer messages
        assert_eq!(current.message_stats.customer_messages, 0);
    }

    #[test]
    fn record_missing_conversation_is_none() {
        let conn = fresh_db();
        assert!(record_current_interaction(&conn, 999).unwrap().is_none());
    }

    #[test]
    fn backfill_populates_every_conversation() {
        let conn = fresh_db();
        conn.execute(
            "INSERT INTO customers (id, remote_id) VALUES (2001, 2001)",
            [],
        )
        .unwrap();
        for remote in [1001, 1002] {
            conn.execute(
                "INSERT INTO conversations (remote_id, number, status, mailbox_id, customer_id)
                 VALUES (?1, ?1, 'active', 1, 2001)",
                params![remote],
            )
            .unwrap();
            let cid: i64 = conn
                .query_row(
                    "SELECT id FROM conversations WHERE remote_id = ?1",
                    params![remote],
                    |r| r.get(0),
                )
                .unwrap();
            conn.execute(
                "INSERT INTO conversation_threads (conversation_id, type, state, body_text, from_type, created_at)
                 VALUES (?1, 'customer', 'published', 'hello, why is this broken?', 'customer', datetime('now'))",
                params![cid],
            )
            .unwrap();
        }
        let written = backfill_all(&conn).unwrap();
        assert_eq!(written, 2);
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM client_current_signals", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(rows, 2);
    }

    /// The demo world path: after the S5 sync the backfill finds real customer
    /// threads with real signal content (this is what feeds high_effort).
    #[tokio::test]
    async fn demo_world_backfill_feeds_the_high_effort_tile() {
        let f = tempfile::NamedTempFile::new()
            .unwrap()
            .into_temp_path()
            .keep()
            .unwrap();
        let mut conn = crate::db::open(&f).unwrap();
        crate::bootstrap::apply_all(&mut conn).unwrap();
        crate::settings::set_bool(&conn, "demo_mode", true).unwrap();
        let shared = std::sync::Arc::new(std::sync::Mutex::new(conn));
        let engine = crate::sync_engine::SyncEngine::new(
            shared.clone(),
            std::sync::Arc::new(crate::helpscout::FakeHelpScoutProvider::new_demo()),
        );
        engine.initial_sync().await.unwrap();
        let conn = shared.lock().unwrap();
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM client_current_signals", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(rows >= 15, "demo world conversations with signals: {rows}");
        // json_extract works over the stored shape (the tile's exact access).
        let strong: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM client_current_signals s, json_each(s.signals_json) je
                  WHERE json_extract(je.value, '$.dimension') = 'frustration'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            strong >= 1,
            "frustration signals in the demo world: {strong}"
        );
        let with_stats: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM client_current_signals WHERE message_stats_json IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(with_stats, rows);
    }

    // ---- AI-19 (C7): evidence rows + snapshot loader + profile ------------

    /// Fixture: customer 2001 with one conversation whose customer thread
    /// triggers deterministic signals with evidence.
    fn evidence_db() -> (Connection, i64) {
        let conn = fresh_db();
        conn.execute(
            "INSERT INTO customers (id, remote_id) VALUES (2001, 2001)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO conversations (remote_id, number, status, mailbox_id, customer_id)
             VALUES (1001, 1001, 'active', 1, 2001)",
            [],
        )
        .unwrap();
        let cid: i64 = conn
            .query_row(
                "SELECT id FROM conversations WHERE remote_id = 1001",
                [],
                |r| r.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO conversation_threads (conversation_id, type, state, body_text, from_type, created_at)
             VALUES (?1, 'customer', 'published',
                'This is STILL not working. I already called twice and nobody fixed it. Every time the same thing. Please walk me through step by step how to fix the API endpoint.',
                'customer', datetime('now'))",
            params![cid],
        )
        .unwrap();
        (conn, cid)
    }

    #[test]
    fn record_writes_evidence_rows_replace_per_conversation() {
        let (conn, cid) = evidence_db();
        let current = record_current_interaction(&conn, cid).unwrap().unwrap();
        let with_evidence = current
            .signals
            .iter()
            .filter(|s| s.evidence.is_some())
            .count();
        assert!(with_evidence >= 1, "signals carry evidence");

        let rows: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM interaction_evidence WHERE conversation_id = ?1",
                params![cid],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            rows,
            i64::try_from(with_evidence).unwrap(),
            "one evidence row per evidenced signal"
        );

        // The row shape: evidence_type = dimension, evidence_data parses as
        // the JSON object with the excerpt + pointers + signal context.
        let (etype, data): (String, String) = conn
            .query_row(
                "SELECT evidence_type, evidence_data FROM interaction_evidence
                  WHERE conversation_id = ?1 ORDER BY id LIMIT 1",
                params![cid],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&data).unwrap();
        assert!(v["excerpt"].as_str().is_some_and(|e| !e.is_empty()));
        assert_eq!(v["conversation_local_id"], serde_json::json!(cid));
        assert_eq!(
            v["source"], "heuristic",
            "evidence_data carries the signal context"
        );
        assert!(
            INTERACTION_DIMENSIONS.contains(&etype.as_str()),
            "evidence_type is a dimension: {etype}"
        );

        // Re-record after the message changes: rows are REPLACED, never
        // accumulated (the table mirrors the live snapshot).
        conn.execute(
            "UPDATE conversation_threads SET body_text = 'Thanks! This is great support.'
             WHERE conversation_id = ?1",
            params![cid],
        )
        .unwrap();
        let current2 = record_current_interaction(&conn, cid).unwrap().unwrap();
        let with_evidence2 = current2
            .signals
            .iter()
            .filter(|s| s.evidence.is_some())
            .count();
        let rows2: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM interaction_evidence WHERE conversation_id = ?1",
                params![cid],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rows2, i64::try_from(with_evidence2).unwrap());
    }

    #[test]
    fn get_current_interaction_round_trips_the_stored_snapshot() {
        let (conn, cid) = evidence_db();
        // No snapshot yet → None (pure read, never computes).
        assert!(get_current_interaction(&conn, cid).unwrap().is_none());

        let current = record_current_interaction(&conn, cid).unwrap().unwrap();
        let stored = get_current_interaction(&conn, cid).unwrap().unwrap();
        assert_eq!(stored.conversation_id, cid);
        assert_eq!(stored.customer_id, Some(2001));
        assert_eq!(stored.signals, current.signals);
        assert_eq!(stored.customer_goal, current.customer_goal);
        assert!(stored.message_stats.is_some());
        assert!(stored
            .generated_at
            .as_deref()
            .is_some_and(|g| !g.is_empty()));
        assert_eq!(stored.provenance.as_deref(), Some("heuristic"));
        assert_eq!(
            stored.analysis_version.as_deref(),
            Some("heuristic_v1"),
            "the loader serves the full stored row"
        );
    }

    #[test]
    fn interaction_profile_aggregates_dimensions_and_resolves_preference() {
        let (conn, cid) = evidence_db();
        record_current_interaction(&conn, cid).unwrap();

        let p = interaction_profile(&conn, 2001).unwrap();
        assert_eq!(p.customer_id, 2001);
        assert_eq!(p.conversations_analyzed, 1);
        assert!(p.total_signals >= 1);
        assert!(p.last_signal_at.is_some());
        // The frustrated message + step-by-step request produced both a
        // frustration observation and an explicit response_preference.
        assert!(p
            .signal_breakdown
            .iter()
            .any(|d| d.dimension == "frustration" && !d.values.is_empty()));
        // A single observation is below the pattern threshold → no inferred
        // preference yet.
        assert!(
            p.response_preference.is_none()
                || p.response_preference.as_ref().is_some_and(
                    |r| r.observations >= crate::intelligence::MIN_OBSERVATIONS_FOR_PREFERENCE
                ),
            "inferred preferences need the min observation count"
        );

        // With a human override the preference resolves to it, source human.
        conn.execute(
            "INSERT INTO interaction_overrides (customer_id, field, value, reason)
             VALUES (2001, 'response_preference', 'concise', 'prefers short answers')",
            [],
        )
        .unwrap();
        let p = interaction_profile(&conn, 2001).unwrap();
        let pref = p.response_preference.expect("override wins");
        assert_eq!(pref.value, "concise");
        assert_eq!(pref.source, "human");
        assert_eq!(pref.reason.as_deref(), Some("prefers short answers"));

        // A customer with no analyzed conversations: zeroed profile, no 500.
        conn.execute(
            "INSERT INTO customers (id, remote_id) VALUES (2002, 2002)",
            [],
        )
        .unwrap();
        let p = interaction_profile(&conn, 2002).unwrap();
        assert_eq!(p.total_signals, 0);
        assert_eq!(p.conversations_analyzed, 0);
        assert!(p.signal_breakdown.is_empty());
        assert!(p.response_preference.is_none());
    }
}
