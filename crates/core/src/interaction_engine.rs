//! Client Interaction Intelligence engine (AI-16 / AI-17 / AI-18 parity port
//! of `src/server/ai/interaction/{engine,safety}.ts` + migration 005 +
//! `interactionRepo.ts`).
//!
//! Deterministic core: baseline, change detection, outcomes, effort,
//! friction, and profile assembly all run WITHOUT any AI. The AI stages
//! (`crate::ai_pipeline::analyze_interaction`) only enrich signals and
//! recommendations; the engine degrades gracefully without LM Studio.
//!
//! Storage is DERIVED data (heuristic or ai) — it never overwrites Help
//! Scout source rows. Tables follow the reference migration 005 shapes:
//!
//! * `client_behavior_observations` — longitudinal observations, idempotent
//!   per `(conversation, dimension, source)`.
//! * `client_behavior_baselines` — recency-weighted typical values per
//!   customer + dimension, with a monotonically increasing profile version.
//! * `client_communication_preferences` — inferred + human-entered
//!   preferences (the overfit guard counts DISTINCT conversations).
//! * `client_human_overrides` — the human decision history (spec #22/#56).
//! * `client_support_outcomes` — per-conversation outcome facts.

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::interaction_current::{
    sanitize_signals, CurrentInteraction, InteractionSignal, MessageStats,
};

// ═══════════════════════════════════════════════════════════════════════════
// Safety layer (safety.ts parity — AI-18)
// ═══════════════════════════════════════════════════════════════════════════

/// A forbidden-claim pattern: model-authored free text that matches it is a
/// psychological claim / diagnosis / fixed trait label and is rejected
/// (safety.ts:16-29).
struct ForbiddenPattern {
    regex: regex::Regex,
    reason: &'static str,
}

macro_rules! fp {
    ($re:expr, $reason:expr) => {
        ForbiddenPattern {
            regex: regex::Regex::new(&format!("(?i){}", $re)).expect("static pattern"),
            reason: $reason,
        }
    };
}

/// FORBIDDEN_PATTERNS (safety.ts:16-29) — the system must NEVER produce
/// psychological claims, diagnoses, or fixed personality labels; only
/// observable, evidence-backed communication signals (spec #7, #55, #37).
fn forbidden_patterns() -> &'static [ForbiddenPattern] {
    static PATTERNS: std::sync::OnceLock<Vec<ForbiddenPattern>> = std::sync::OnceLock::new();
    PATTERNS.get_or_init(|| {
        vec![
            fp!(
                r"\b(narcissis|anxiet|depress|bipolar|adhd|autis|schizo|paranoid|delusional|mentally|insane|unstable|unhinged)\w*",
                "mental-health language"
            ),
            fp!(
                r"\b(personality (disorder|type|trait)|myers.?briggs|big five|temperament|character flaw)\b",
                "personality typing"
            ),
            fp!(
                r"\b(difficult (person|customer|human)|problem (person|customer)|is (a )?(narcissist|aggressive person)|emotionally (unstable|volatile))\b",
                "fixed person label"
            ),
            fp!(
                r"\b(race|ethnicity|religion|politic|sexual orientation|gay|lesbian|trans|muslim|christian|jew|hindu|atheist|white supremacy|nationalist)\b",
                "protected attribute inference"
            ),
            fp!(
                r"\b(diagnos\w+|symptom of|suffers? from|patholog\w+)\b",
                "clinical diagnosis language"
            ),
            fp!(
                r"\b(intelligen(t|ce) (level|of)|iq|cognitive ability|stupid|dumb|incompetent person)\b",
                "cognitive-ability judgment"
            ),
            fp!(
                r"\b(manipulative|toxic person|evil|malicious person|bad person|liar|dishonest person)\b",
                "moral character judgment"
            ),
            // Trait adjectives ascribed to the customer as a fixed
            // characteristic ("the customer is rude") — observable behavior
            // language must be used instead ("message contains X").
            fp!(
                r"\b((customer|client|user|person|he|she|they) (is|are|seems|acts|behaves) (a )?(rude|entitled|needy|demanding|lazy|clueless|hostile|abrasive|belligerent|passive.?aggressive|bully)|rude (person|customer|client)|entitled (person|customer|client)|needy (person|customer|client))\b",
                "fixed trait label"
            ),
            fp!(
                r"\b(passive.?aggressive|bullying|arrogant|condescending (person|customer)|vindictive|vengeful)\b",
                "character judgment"
            ),
        ]
    })
}

/// Result of the free-text forbidden-claim scan (safety.ts
/// `SanitizationResult`-for-text analog: `{ ok, warnings }`).
#[derive(Debug, Clone, PartialEq)]
pub struct TextScan {
    pub ok: bool,
    pub warnings: Vec<String>,
}

/// Scan free text produced by the AI for forbidden trait claims
/// (safety.ts `sanitizeInteractionText`). Every match appends the
/// warning `Removed {reason} — SupportOS only reports observable
/// support-communication behavior.`; `ok` is false when any matched.
pub fn sanitize_interaction_text(text: &str) -> TextScan {
    let mut warnings = Vec::new();
    for p in forbidden_patterns() {
        if p.regex.is_match(text) {
            warnings.push(format!(
                "Removed {} — SupportOS only reports observable support-communication behavior.",
                p.reason
            ));
        }
    }
    TextScan {
        ok: warnings.is_empty(),
        warnings,
    }
}

/// Full rejection gate for AI observation payloads (safety.ts
/// `assertInteractionTextSafe`): any forbidden claim invalidates the
/// free text. `None`/empty text is safe by definition.
pub fn assert_interaction_text_safe(text: Option<&str>) -> TextScan {
    match text {
        Some(t) if !t.is_empty() => sanitize_interaction_text(t),
        _ => TextScan {
            ok: true,
            warnings: Vec::new(),
        },
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Wire shapes (shared/types.ts parity)
// ═══════════════════════════════════════════════════════════════════════════

/// One dimension's typical value (reference `BehaviorBaseline.dimensions[]`).
#[derive(Debug, Clone, Serialize)]
pub struct BaselineDimension {
    pub dimension: String,
    pub typical_value: String,
    pub confidence: String,
    pub observation_count: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_observed: Option<String>,
}

/// The customer's historical pattern (reference `BehaviorBaseline`).
#[derive(Debug, Clone, Serialize)]
pub struct BehaviorBaseline {
    pub customer_local_id: i64,
    pub conversation_count: i64,
    pub observation_count: i64,
    pub dimensions: Vec<BaselineDimension>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_updated: Option<String>,
    pub profile_version: i64,
}

/// A current-vs-baseline deviation (reference `InteractionChange`).
#[derive(Debug, Clone, Serialize)]
pub struct InteractionChange {
    pub dimension: String,
    pub baseline_value: String,
    pub current_value: String,
    pub direction: String,
    pub magnitude: f64,
    pub significant: bool,
}

/// The recommended support approach (reference `SupportApproach`).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SupportApproach {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tone: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub length: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_with: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub then: Option<String>,
    pub avoid: Vec<String>,
    pub response_strategy: Vec<String>,
    pub de_escalation: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub escalation_recommendation: Option<String>,
    pub why: Vec<String>,
    pub source: String,
    pub confidence: String,
}

/// A computed conversation outcome (reference `computeOutcome` return).
#[derive(Debug, Clone, Serialize)]
pub struct OutcomeComputed {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort_score: Option<f64>,
    pub friction: String,
    pub follow_up_count: i64,
    pub clarification_count: i64,
    pub escalated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_after_first_response: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_style: Option<String>,
}

/// One historically effective response style (reference
/// `SupportOutcomeSummary.effective_approaches[]`).
#[derive(Debug, Clone, Serialize)]
pub struct EffectiveApproach {
    pub approach: String,
    pub worked_count: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub example_conversation_local_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub example_number: Option<i64>,
}

/// A conversation flagged for friction (reference `friction_flags[]`).
#[derive(Debug, Clone, Serialize)]
pub struct FrictionFlag {
    pub conversation_local_id: i64,
    pub number: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    pub friction: String,
}

/// Cross-conversation outcome roll-up (reference `SupportOutcomeSummary`).
#[derive(Debug, Clone, Serialize)]
pub struct SupportOutcomeSummary {
    pub customer_local_id: i64,
    pub total_conversations: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_response_resolution_rate: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub follow_up_rate: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clarification_rate: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub escalation_rate: Option<f64>,
    pub avg_effort_score: f64,
    pub effective_approaches: Vec<EffectiveApproach>,
    pub friction_flags: Vec<FrictionFlag>,
}

/// Repeat-issue detection result (reference `detectRepeatIssue`).
#[derive(Debug, Clone, Serialize)]
pub struct RelatedConversation {
    pub local_id: i64,
    pub number: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RepeatIssue {
    pub detected: bool,
    pub related_conversations: Vec<RelatedConversation>,
}

/// Card provenance (reference `InteractionCard['provenance']`).
#[derive(Debug, Clone, Serialize)]
pub struct CardProvenance {
    pub ai_generated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generated_at: Option<String>,
}

/// The ticket-scoped interaction card (reference `InteractionCard`).
#[derive(Debug, Clone, Serialize)]
pub struct InteractionCard {
    pub conversation_local_id: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub customer_local_id: Option<i64>,
    pub client_kind: String,
    pub current: CurrentInteraction,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline: Option<BehaviorBaseline>,
    pub changes: Vec<InteractionChange>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recommendation: Option<SupportApproach>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort_score: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub friction: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repeat_issue: Option<RepeatIssue>,
    pub provenance: CardProvenance,
}

/// A human override decision (reference `client_human_overrides` row,
/// served by `getActiveOverrides`).
#[derive(Debug, Clone, Serialize)]
pub struct OverrideRow {
    pub id: i64,
    pub field: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ai_value: Option<String>,
    pub human_value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub created_at: String,
    pub active: bool,
}

/// A communication preference (reference `CommunicationPreference`).
#[derive(Debug, Clone, Serialize)]
pub struct CommunicationPreference {
    pub preference: String,
    pub evidence_count: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_observed: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_observed: Option<String>,
    pub confidence: String,
    pub origin: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub human_override: Option<HumanOverride>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HumanOverride {
    pub value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub overridden_at: String,
}

/// One timeline month (reference `ClientInteractionProfile['timeline'][]`).
#[derive(Debug, Clone, Serialize)]
pub struct TimelineEntry {
    pub month: String,
    pub conversation_count: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub conversation_local_ids: Vec<i64>,
}

/// The support playbook (reference `ClientPlaybook`).
#[derive(Debug, Clone, Serialize)]
pub struct ClientPlaybook {
    pub best_opening: String,
    pub best_explanation_style: String,
    pub best_troubleshooting_style: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub likely_follow_up: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub historically_successful: Option<String>,
    pub avoid: Vec<String>,
}

/// The customer-scoped profile (reference `ClientInteractionProfile`).
#[derive(Debug, Clone, Serialize)]
pub struct ClientInteractionProfile {
    pub customer_local_id: i64,
    pub client_kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline: Option<BehaviorBaseline>,
    pub preferences: Vec<CommunicationPreference>,
    pub timeline: Vec<TimelineEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcomes: Option<SupportOutcomeSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub playbook: Option<ClientPlaybook>,
    pub overrides: Vec<OverrideRow>,
}

/// The stored Stage-2 recommendation wrapper (`recommendation_json`).
#[derive(Debug, Clone)]
pub struct StoredRecommendation {
    pub recommendation: SupportApproach,
    pub prompt_version: Option<String>,
    pub model: Option<String>,
}

/// The stored current-interaction snapshot (repo read side).
#[derive(Debug, Clone)]
pub struct StoredCurrent {
    pub signals: Vec<InteractionSignal>,
    pub message_stats: Option<MessageStats>,
    pub customer_goal: Option<String>,
    pub sources: String,
    pub generated_at: String,
    pub analysis_version: Option<String>,
    pub recommendation: Option<StoredRecommendation>,
}

// ═══════════════════════════════════════════════════════════════════════════
// Schema (migration 005 shapes — idempotent ensure)
// ═══════════════════════════════════════════════════════════════════════════

/// Ensure the interaction-intelligence tables exist (migration 005 shapes +
/// the `recommendation_json` column from migration 006). Idempotent — safe to
/// call at boot and before writes.
pub fn ensure_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS client_behavior_observations (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            customer_id INTEGER NOT NULL,
            conversation_id INTEGER,
            thread_local_id INTEGER,
            dimension TEXT NOT NULL,
            value TEXT NOT NULL,
            confidence TEXT NOT NULL DEFAULT 'low',
            evidence_excerpt TEXT,
            source TEXT NOT NULL DEFAULT 'heuristic',
            observed_at TEXT NOT NULL DEFAULT (datetime('now')),
            provenance TEXT NOT NULL DEFAULT 'heuristic'
        );
        CREATE INDEX IF NOT EXISTS idx_client_observations_customer
            ON client_behavior_observations(customer_id, dimension);
        CREATE INDEX IF NOT EXISTS idx_client_observations_conversation
            ON client_behavior_observations(conversation_id);
        CREATE UNIQUE INDEX IF NOT EXISTS idx_client_observations_unique
            ON client_behavior_observations(conversation_id, dimension, source);

        CREATE TABLE IF NOT EXISTS client_behavior_baselines (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            customer_id INTEGER NOT NULL,
            dimension TEXT NOT NULL,
            typical_value TEXT NOT NULL,
            confidence TEXT NOT NULL DEFAULT 'low',
            observation_count INTEGER NOT NULL DEFAULT 0,
            last_observed TEXT,
            profile_version INTEGER NOT NULL DEFAULT 1,
            updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            UNIQUE (customer_id, dimension)
        );

        CREATE TABLE IF NOT EXISTS client_communication_preferences (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            customer_id INTEGER NOT NULL,
            preference TEXT NOT NULL,
            evidence_count INTEGER NOT NULL DEFAULT 1,
            first_observed TEXT,
            last_observed TEXT,
            confidence TEXT NOT NULL DEFAULT 'low',
            origin TEXT NOT NULL DEFAULT 'ai_inferred',
            human_override_value TEXT,
            human_override_reason TEXT,
            overridden_at TEXT,
            provenance TEXT NOT NULL DEFAULT 'ai_generated',
            UNIQUE (customer_id, preference)
        );
        CREATE INDEX IF NOT EXISTS idx_client_preferences_customer
            ON client_communication_preferences(customer_id);

        CREATE TABLE IF NOT EXISTS client_human_overrides (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            customer_id INTEGER NOT NULL,
            field TEXT NOT NULL,
            ai_value TEXT,
            human_value TEXT NOT NULL,
            reason TEXT,
            created_by TEXT NOT NULL DEFAULT 'user',
            active INTEGER NOT NULL DEFAULT 1,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE INDEX IF NOT EXISTS idx_client_overrides_customer
            ON client_human_overrides(customer_id, active);

        CREATE TABLE IF NOT EXISTS client_support_outcomes (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            customer_id INTEGER NOT NULL,
            conversation_id INTEGER NOT NULL,
            resolved_after_first_response INTEGER,
            follow_up_count INTEGER NOT NULL DEFAULT 0,
            clarification_count INTEGER NOT NULL DEFAULT 0,
            escalated INTEGER NOT NULL DEFAULT 0,
            effort_score REAL,
            response_style TEXT,
            friction TEXT,
            computed_at TEXT NOT NULL DEFAULT (datetime('now')),
            UNIQUE (conversation_id)
        );
        CREATE INDEX IF NOT EXISTS idx_client_outcomes_customer
            ON client_support_outcomes(customer_id);",
    )?;
    // The Stage-2 recommendation column (migration 006 territory).
    let has_col: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('client_current_signals')
              WHERE name = 'recommendation_json'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    if has_col == 0 {
        conn.execute(
            "ALTER TABLE client_current_signals ADD COLUMN recommendation_json TEXT",
            [],
        )?;
    }
    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════
// Repository (interactionRepo.ts parity)
// ═══════════════════════════════════════════════════════════════════════════

/// One observation row (repo `getObservationsForCustomer` element; the
/// customer id rides along for the idempotent insert).
#[derive(Debug, Clone)]
pub struct ObservationRow {
    pub customer_id: i64,
    pub dimension: String,
    pub value: String,
    pub confidence: String,
    pub evidence_excerpt: Option<String>,
    pub conversation_id: Option<i64>,
    pub thread_local_id: Option<i64>,
    pub observed_at: String,
    pub source: String,
}

/// `saveCurrentInteraction` (repo:13-48) — ONE row per conversation,
/// upsert in place. The provenance label follows the reference's
/// sources→provenance mapping.
#[allow(clippy::too_many_arguments)] // interactionRepo.ts saveCurrentInteraction signature parity
pub fn save_current_interaction(
    conn: &Connection,
    conversation_id: i64,
    customer_id: Option<i64>,
    signals: &[InteractionSignal],
    message_stats: &MessageStats,
    customer_goal: Option<&str>,
    sources: &str,
    analysis_version: Option<&str>,
) -> Result<()> {
    crate::interaction_current::ensure_client_current_signals_table(conn)?;
    let provenance = if sources == "heuristic" {
        "heuristic"
    } else {
        "ai_generated"
    };
    conn.execute(
        "INSERT INTO client_current_signals
             (conversation_id, customer_id, signals_json, message_stats_json, customer_goal,
              sources, analysis_version, generated_at, provenance)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, datetime('now'), ?8)
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
            customer_id,
            serde_json::to_string(signals)?,
            serde_json::to_string(message_stats)?,
            customer_goal,
            sources,
            analysis_version,
            provenance,
        ],
    )?;
    Ok(())
}

/// `getLatestCurrentInteraction` (repo:50-73) — the stored snapshot with the
/// Stage-2 recommendation when one was persisted.
pub fn get_latest_current_interaction(
    conn: &Connection,
    conversation_id: i64,
) -> Result<Option<StoredCurrent>> {
    let row = conn
        .query_row(
            "SELECT signals_json, message_stats_json, customer_goal, sources, generated_at,
                    analysis_version, recommendation_json
               FROM client_current_signals WHERE conversation_id = ?1
              ORDER BY id DESC LIMIT 1",
            params![conversation_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, Option<String>>(6)?,
                ))
            },
        )
        .ok();
    let Some((signals_json, stats_json, goal, sources, generated_at, analysis_version, rec)) = row
    else {
        return Ok(None);
    };
    let recommendation = rec.and_then(|json| {
        let v: serde_json::Value = serde_json::from_str(&json).ok()?;
        Some(StoredRecommendation {
            recommendation: serde_json::from_value(v.get("recommendation")?.clone()).ok()?,
            prompt_version: v
                .get("prompt_version")
                .and_then(|p| p.as_str())
                .map(String::from),
            model: v.get("model").and_then(|m| m.as_str()).map(String::from),
        })
    });
    Ok(Some(StoredCurrent {
        signals: serde_json::from_str(&signals_json).unwrap_or_default(),
        message_stats: stats_json.and_then(|s| serde_json::from_str(&s).ok()),
        customer_goal: goal,
        sources,
        generated_at: generated_at.unwrap_or_default(),
        analysis_version,
        recommendation,
    }))
}

/// `saveRecommendation` (repo:75-80) — persist the Stage-2 recommendation for
/// later GETs (and the draft prompt's strategy block).
pub fn save_recommendation(
    conn: &Connection,
    conversation_id: i64,
    recommendation: &SupportApproach,
    prompt_version: Option<&str>,
    model: Option<&str>,
) -> Result<()> {
    let wrapped = serde_json::json!({
        "recommendation": recommendation,
        "prompt_version": prompt_version,
        "model": model,
    });
    conn.execute(
        "UPDATE client_current_signals SET recommendation_json = ?1 WHERE conversation_id = ?2",
        params![wrapped.to_string(), conversation_id],
    )?;
    Ok(())
}

/// `insertObservations` (repo:84-104) — IDEMPOTENT per
/// `(conversation, dimension, source)`: a recompute updates the stored value
/// in place, never appends a duplicate row (that corrupted baselines and
/// defeated the 3-conversation preference threshold).
pub fn insert_observations(conn: &Connection, rows: &[ObservationRow]) -> Result<()> {
    for o in rows {
        conn.execute(
            "INSERT INTO client_behavior_observations
                 (customer_id, conversation_id, thread_local_id, dimension, value, confidence,
                  evidence_excerpt, source, observed_at, provenance)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT (conversation_id, dimension, source) DO UPDATE SET
               value = excluded.value,
               confidence = excluded.confidence,
               evidence_excerpt = excluded.evidence_excerpt,
               thread_local_id = excluded.thread_local_id,
               observed_at = excluded.observed_at,
               provenance = excluded.provenance",
            params![
                o.customer_id,
                o.conversation_id,
                o.thread_local_id,
                o.dimension,
                o.value,
                o.confidence,
                o.evidence_excerpt,
                o.source,
                o.observed_at,
                if o.source == "ai" {
                    "ai_generated"
                } else {
                    "heuristic"
                },
            ],
        )?;
    }
    Ok(())
}

/// `getObservationsForCustomer` (repo:121-131) — bounded to the 5000 most
/// recent rows so a pathological database cannot turn every profile build
/// into a full scan (v2.2.0 perf, plan Phase 41).
pub fn get_observations_for_customer(
    conn: &Connection,
    customer_id: i64,
) -> Result<Vec<ObservationRow>> {
    let mut stmt = conn.prepare(
        "SELECT dimension, value, confidence, evidence_excerpt, conversation_id,
                thread_local_id, observed_at, source
           FROM client_behavior_observations
          WHERE customer_id = ?1
          ORDER BY observed_at DESC
          LIMIT 5000",
    )?;
    let rows = stmt
        .query_map(params![customer_id], |r| {
            Ok(ObservationRow {
                dimension: r.get(0)?,
                value: r.get(1)?,
                confidence: r.get(2)?,
                evidence_excerpt: r.get(3)?,
                conversation_id: r.get(4)?,
                thread_local_id: r.get(5)?,
                observed_at: r.get(6)?,
                source: r.get(7)?,
                customer_id,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows)
}

/// `upsertBaseline` (repo:139-153).
#[allow(clippy::too_many_arguments)]
pub fn upsert_baseline(
    conn: &Connection,
    customer_id: i64,
    dimension: &str,
    typical_value: &str,
    confidence: &str,
    observation_count: i64,
    last_observed: Option<&str>,
    profile_version: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO client_behavior_baselines
             (customer_id, dimension, typical_value, confidence, observation_count,
              last_observed, profile_version, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, datetime('now'))
         ON CONFLICT (customer_id, dimension) DO UPDATE SET
           typical_value = excluded.typical_value,
           confidence = excluded.confidence,
           observation_count = excluded.observation_count,
           last_observed = excluded.last_observed,
           profile_version = excluded.profile_version,
           updated_at = datetime('now')",
        params![
            customer_id,
            dimension,
            typical_value,
            confidence,
            observation_count,
            last_observed,
            profile_version
        ],
    )?;
    Ok(())
}

/// One persisted baseline row (getBaseline's SQL projection).
type BaselineRow = (String, String, String, i64, Option<String>, i64, String);

/// `getBaseline` (repo:155-172) — the persisted baseline.
pub fn get_baseline(conn: &Connection, customer_id: i64) -> Result<Option<BehaviorBaseline>> {
    let mut stmt = conn.prepare(
        "SELECT dimension, typical_value, confidence, observation_count, last_observed,
                profile_version, updated_at
           FROM client_behavior_baselines WHERE customer_id = ?1",
    )?;
    let rows: Vec<BaselineRow> = stmt
        .query_map(params![customer_id], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
            ))
        })?
        .filter_map(|r| r.ok())
        .collect();
    if rows.is_empty() {
        return Ok(None);
    }
    let mut last_updated = rows[0].6.clone();
    let mut observation_count = 0i64;
    let mut profile_version = 0i64;
    let mut dimensions = Vec::with_capacity(rows.len());
    for (dimension, typical_value, confidence, count, last_observed, version, updated) in rows {
        observation_count += count;
        profile_version = profile_version.max(version);
        if updated > last_updated {
            last_updated = updated;
        }
        dimensions.push(BaselineDimension {
            dimension,
            typical_value,
            confidence,
            observation_count: count,
            last_observed,
        });
    }
    let conversation_count = conn.query_row(
        "SELECT COUNT(DISTINCT conversation_id) FROM client_behavior_observations
          WHERE customer_id = ?1 AND conversation_id IS NOT NULL",
        params![customer_id],
        |r| r.get(0),
    )?;
    Ok(Some(BehaviorBaseline {
        customer_local_id: customer_id,
        conversation_count,
        observation_count,
        dimensions,
        last_updated: Some(last_updated),
        profile_version,
    }))
}

/// `upsertPreference` (repo:176-188) — evidence_count keeps the max and a
/// human_entered origin is never demoted.
#[allow(clippy::too_many_arguments)] // interactionRepo.ts upsertPreference signature parity
pub fn upsert_preference(
    conn: &Connection,
    customer_id: i64,
    preference: &str,
    evidence_count: i64,
    first_observed: Option<&str>,
    last_observed: Option<&str>,
    confidence: &str,
    origin: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO client_communication_preferences
             (customer_id, preference, evidence_count, first_observed, last_observed,
              confidence, origin, provenance)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT (customer_id, preference) DO UPDATE SET
           evidence_count = MAX(client_communication_preferences.evidence_count, excluded.evidence_count),
           last_observed = excluded.last_observed,
           confidence = excluded.confidence,
           origin = CASE WHEN client_communication_preferences.origin = 'human_entered'
                        THEN 'human_entered' ELSE excluded.origin END",
        params![
            customer_id,
            preference,
            evidence_count,
            first_observed,
            last_observed,
            confidence,
            origin,
            if origin == "human_entered" {
                "human"
            } else {
                "ai_generated"
            }
        ],
    )?;
    Ok(())
}

/// `getPreferences` (repo:190-203).
pub fn get_preferences(
    conn: &Connection,
    customer_id: i64,
) -> Result<Vec<CommunicationPreference>> {
    let mut stmt = conn.prepare(
        "SELECT preference, evidence_count, first_observed, last_observed, confidence, origin,
                human_override_value, human_override_reason, overridden_at
           FROM client_communication_preferences WHERE customer_id = ?
          ORDER BY evidence_count DESC",
    )?;
    let rows = stmt
        .query_map(params![customer_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, Option<String>>(7)?,
                r.get::<_, Option<String>>(8)?,
            ))
        })?
        .filter_map(|r| r.ok())
        .collect::<Vec<_>>();
    Ok(rows
        .into_iter()
        .map(
            |(
                preference,
                evidence_count,
                first_observed,
                last_observed,
                confidence,
                origin,
                hov,
                hor,
                hoa,
            )| {
                CommunicationPreference {
                    preference,
                    evidence_count,
                    first_observed,
                    last_observed,
                    confidence: confidence.unwrap_or_else(|| "unknown".into()),
                    origin: if origin == "human_entered" {
                        "human_entered".into()
                    } else {
                        "ai_inferred".into()
                    },
                    human_override: hov.map(|value| HumanOverride {
                        value,
                        reason: hor,
                        overridden_at: hoa.unwrap_or_default(),
                    }),
                }
            },
        )
        .collect())
}

/// `setHumanOverride` (repo:205-237) — deactivate previous overrides, record
/// the decision, drop phantom materialized rows, and materialize the value.
pub fn set_human_override(
    conn: &Connection,
    customer_id: i64,
    value: &str,
    ai_value: Option<&str>,
    reason: Option<&str>,
) -> Result<()> {
    conn.execute(
        "UPDATE client_human_overrides SET active = 0
          WHERE customer_id = ?1 AND field = 'response_preference' AND active = 1",
        params![customer_id],
    )?;
    conn.execute(
        "INSERT INTO client_human_overrides
             (customer_id, field, ai_value, human_value, reason, active, created_at)
         VALUES (?1, 'response_preference', ?2, ?3, ?4, 1, datetime('now'))",
        params![customer_id, ai_value, value, reason],
    )?;
    conn.execute(
        "DELETE FROM client_communication_preferences
          WHERE customer_id = ?1 AND origin = 'human_entered' AND evidence_count = 0",
        params![customer_id],
    )?;
    conn.execute(
        "INSERT INTO client_communication_preferences
             (customer_id, preference, evidence_count, first_observed, last_observed, confidence,
              origin, human_override_value, human_override_reason, overridden_at, provenance)
         VALUES (?1, ?2, 0, NULL, datetime('now'), 'high', 'human_entered', ?3, ?4, datetime('now'), 'human')
         ON CONFLICT (customer_id, preference) DO UPDATE SET
           last_observed = datetime('now'),
           confidence = 'high',
           origin = 'human_entered',
           human_override_value = excluded.human_override_value,
           human_override_reason = excluded.human_override_reason,
           overridden_at = datetime('now')",
        params![customer_id, value, value, reason],
    )?;
    Ok(())
}

/// `clearHumanOverride` (repo:239-253) — fully restore AI semantics.
pub fn clear_human_override(conn: &Connection, customer_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE client_human_overrides SET active = 0
          WHERE customer_id = ?1 AND field = 'response_preference' AND active = 1",
        params![customer_id],
    )?;
    conn.execute(
        "DELETE FROM client_communication_preferences
          WHERE customer_id = ?1 AND evidence_count = 0 AND origin = 'human_entered'",
        params![customer_id],
    )?;
    conn.execute(
        "UPDATE client_communication_preferences SET
            human_override_value = NULL, human_override_reason = NULL, overridden_at = NULL,
            origin = 'ai_inferred',
            confidence = CASE WHEN evidence_count >= 3 THEN confidence ELSE 'low' END
          WHERE customer_id = ?1",
        params![customer_id],
    )?;
    Ok(())
}

/// `getActiveOverrides` (repo:255-260).
pub fn get_active_overrides(conn: &Connection, customer_id: i64) -> Result<Vec<OverrideRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, field, ai_value, human_value, reason, created_at, active
           FROM client_human_overrides
          WHERE customer_id = ?1 AND active = 1
          ORDER BY created_at DESC",
    )?;
    let rows = stmt
        .query_map(params![customer_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, i64>(6)?,
            ))
        })?
        .filter_map(|r| r.ok())
        .collect::<Vec<_>>();
    Ok(rows
        .into_iter()
        .map(
            |(id, field, ai_value, human_value, reason, created_at, active)| OverrideRow {
                id,
                field,
                ai_value,
                human_value,
                reason,
                created_at,
                active: active == 1,
            },
        )
        .collect())
}

/// `upsertOutcome` (repo:264-280).
#[allow(clippy::too_many_arguments)] // interactionRepo.ts upsertOutcome signature parity
pub fn upsert_outcome(
    conn: &Connection,
    customer_id: i64,
    conversation_id: i64,
    resolved_after_first_response: Option<bool>,
    follow_up_count: i64,
    clarification_count: i64,
    escalated: bool,
    effort_score: Option<f64>,
    response_style: Option<&str>,
    friction: Option<&str>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO client_support_outcomes
             (customer_id, conversation_id, resolved_after_first_response, follow_up_count,
              clarification_count, escalated, effort_score, response_style, friction, computed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, datetime('now'))
         ON CONFLICT (conversation_id) DO UPDATE SET
           resolved_after_first_response = excluded.resolved_after_first_response,
           follow_up_count = excluded.follow_up_count,
           clarification_count = excluded.clarification_count,
           escalated = excluded.escalated,
           effort_score = excluded.effort_score,
           response_style = excluded.response_style,
           friction = excluded.friction,
           computed_at = datetime('now')",
        params![
            customer_id,
            conversation_id,
            resolved_after_first_response.map(i64::from),
            follow_up_count,
            clarification_count,
            i64::from(escalated),
            effort_score,
            response_style,
            friction,
        ],
    )?;
    Ok(())
}

/// One stored outcome row (`getOutcomesForCustomer` element).
#[derive(Debug, Clone)]
pub struct OutcomeRow {
    pub conversation_id: i64,
    pub resolved_after_first_response: Option<i64>,
    pub follow_up_count: i64,
    pub clarification_count: i64,
    pub escalated: i64,
    pub effort_score: Option<f64>,
    pub response_style: Option<String>,
    pub friction: Option<String>,
}

/// `getOutcomesForCustomer` (repo:282-290) — joined with live conversations
/// only (deleted rows drop out of the summary).
pub fn get_outcomes_for_customer(conn: &Connection, customer_id: i64) -> Result<Vec<OutcomeRow>> {
    let mut stmt = conn.prepare(
        "SELECT o.conversation_id, o.resolved_after_first_response, o.follow_up_count,
                o.clarification_count, o.escalated, o.effort_score, o.response_style, o.friction
           FROM client_support_outcomes o
           JOIN conversations c ON c.id = o.conversation_id
          WHERE o.customer_id = ?1 AND c.deleted_at IS NULL
          ORDER BY c.created_at DESC",
    )?;
    let rows = stmt
        .query_map(params![customer_id], |r| {
            Ok(OutcomeRow {
                conversation_id: r.get(0)?,
                resolved_after_first_response: r.get(1)?,
                follow_up_count: r.get(2)?,
                clarification_count: r.get(3)?,
                escalated: r.get(4)?,
                effort_score: r.get(5)?,
                response_style: r.get(6)?,
                friction: r.get(7)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows)
}

/// One customer-conversation history row (`getCustomerConversations`).
#[derive(Debug, Clone)]
pub struct CustomerConversation {
    pub id: i64,
    pub number: i64,
    pub subject: Option<String>,
    pub status: String,
    pub remote_created_at: Option<String>,
    pub thread_count: i64,
}

/// `getCustomerConversations` (repo:294-302) — the customer's live
/// conversations, excluding the given one, newest first.
pub fn get_customer_conversations(
    conn: &Connection,
    customer_id: i64,
    exclude_conversation_id: Option<i64>,
) -> Result<Vec<CustomerConversation>> {
    let mut stmt = conn.prepare(
        "SELECT c.id, c.number, c.subject, c.status, c.created_at,
           (SELECT COUNT(*) FROM conversation_threads t
             WHERE t.conversation_id = c.id AND t.deleted_at IS NULL) AS thread_count
           FROM conversations c
          WHERE c.customer_id = ?1 AND c.deleted_at IS NULL AND c.id != ?2
          ORDER BY c.created_at DESC",
    )?;
    let rows = stmt
        .query_map(
            params![customer_id, exclude_conversation_id.unwrap_or(-1)],
            |r| {
                Ok(CustomerConversation {
                    id: r.get(0)?,
                    number: r.get(1)?,
                    subject: r.get(2)?,
                    status: r.get(3)?,
                    remote_created_at: r.get(4)?,
                    thread_count: r.get(5)?,
                })
            },
        )?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows)
}

// ═══════════════════════════════════════════════════════════════════════════
// Engine (engine.ts parity)
// ═══════════════════════════════════════════════════════════════════════════

/// Conversation facts the engine needs (engine.ts `conversationInfo`).
struct ConversationInfo {
    customer_local_id: Option<i64>,
    subject: Option<String>,
    #[allow(dead_code)] // parity field (engine.ts conversationInfo)
    number: i64,
    status: String,
}

fn conversation_info(conn: &Connection, conversation_id: i64) -> Option<ConversationInfo> {
    conn.query_row(
        "SELECT customer_id, subject, number, status
           FROM conversations WHERE id = ?1 AND deleted_at IS NULL",
        params![conversation_id],
        |r| {
            Ok(ConversationInfo {
                customer_local_id: r.get(0)?,
                subject: r.get(1)?,
                number: r.get(2)?,
                status: r.get(3)?,
            })
        },
    )
    .ok()
}

/// Customer id for a conversation (used by the evidence route).
pub fn conversation_customer(conn: &Connection, conversation_id: i64) -> Option<i64> {
    conversation_info(conn, conversation_id)?.customer_local_id
}

/// `isClosingAcknowledgment` (engine.ts:664-669): a pure closing message —
/// short, no question, no continued problem statement — is excluded from
/// follow-up counting (a perfectly resolved ticket must not look unresolved).
pub fn is_closing_acknowledgment(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() || t.chars().count() > 400 || t.contains('?') {
        return false;
    }
    let problem = regex::Regex::new(
        r"(?i)still|again|but\b|however|issue|problem|not work|broken|error|fail|doesn'?t|didn'?t|can'?t|cannot",
    )
    .expect("static");
    if problem.is_match(t) {
        return false;
    }
    let closing = regex::Regex::new(
        r"(?i)^(thanks|thank you|thankyou|thx|appreciate|that (is|was|'s|sounds|seems) (exactly |just |very )?(what i|great|perfect|helpful|awesome|amazing|clear)|perfect|great|works|working|resolved|closing|closed|all set|confirmed|done|sorted)",
    )
    .expect("static");
    closing.is_match(t)
}

/// `ratio(n, d)` (engine.ts:671-673).
fn ratio(n: i64, d: i64) -> Option<f64> {
    (d > 0).then(|| ((n as f64 / d as f64) * 100.0).round() / 100.0)
}

/// `humanizeStyle` (engine.ts:675-677): `step_by_step` → `Step By Step`.
fn humanize_style(style: &str) -> String {
    style
        .split('_')
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// `dimensionRank` (engine.ts:691-696): the value's position in the ranked
/// vocabulary (None when the dimension is nominal or the value unknown).
fn dimension_rank(dimension: &str, value: &str) -> Option<usize> {
    crate::interaction_current::dimension_vocabulary(dimension)?
        .iter()
        .position(|v| *v == value)
}

/// `rankSpan` (engine.ts:698-701).
fn rank_span(dimension: &str) -> usize {
    crate::interaction_current::dimension_vocabulary(dimension)
        .map(|v| v.len().saturating_sub(1))
        .filter(|s| *s >= 1)
        .unwrap_or(1)
}

/// `dominantValue` (engine.ts:712-726): recency-weighted mode of the values.
fn dominant_value(observations: &[&ObservationRow]) -> Option<String> {
    if observations.is_empty() {
        return None;
    }
    let now = chrono::Utc::now().timestamp_millis() as f64;
    let mut scores: std::collections::HashMap<&str, f64> = std::collections::HashMap::new();
    for o in observations {
        let age_days = parse_age_days(&o.observed_at, now);
        let weight = 0.5_f64.powf(age_days / crate::intelligence::RECENCY_HALF_LIFE_DAYS);
        *scores.entry(o.value.as_str()).or_insert(0.0) += weight;
    }
    scores
        .into_iter()
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(v, _)| v.to_string())
}

/// `Date.parse(observed_at.replace(' ', 'T') + 'Z')` age in days; a missing /
/// unparseable stamp counts as ancient (999 days) exactly like the reference.
fn parse_age_days(observed_at: &str, now_ms: f64) -> f64 {
    let iso = format!("{}Z", observed_at.replace(' ', "T"));
    chrono::DateTime::parse_from_rfc3339(&iso)
        .map(|t| ((now_ms - t.timestamp_millis() as f64) / 86_400_000.0).max(0.0))
        .unwrap_or(999.0)
}

/// `mergeSignals` (engine.ts:748-752): AI enrichment overrides the heuristic
/// signal per dimension.
pub fn merge_signals(
    base: &[InteractionSignal],
    enrichment: &[InteractionSignal],
) -> Vec<InteractionSignal> {
    let mut by_dim: std::collections::BTreeMap<String, InteractionSignal> = base
        .iter()
        .map(|s| (s.dimension.clone(), s.clone()))
        .collect();
    for e in enrichment {
        by_dim.insert(e.dimension.clone(), e.clone());
    }
    by_dim.into_values().collect()
}

/// `nextProfileVersion` (engine.ts:728-731).
fn next_profile_version(conn: &Connection, customer_id: i64) -> i64 {
    conn.query_row(
        "SELECT MAX(profile_version) FROM client_behavior_baselines WHERE customer_id = ?1",
        params![customer_id],
        |r| r.get::<_, Option<i64>>(0),
    )
    .ok()
    .flatten()
    .unwrap_or(0)
        + 1
}

// ---------------- observation recording (spec #29, #30) ----------------

/// Record the snapshot's signals as longitudinal observations for the
/// customer (engine.ts `recordCurrentInteraction` observation path).
/// Observations are dated by the CONVERSATION's date so recency weighting
/// reflects when the behavior happened; preferences need an explicit request
/// or repetition (only high-confidence response_preference passes).
pub fn record_observations_for(conn: &Connection, current: &CurrentInteraction) -> Result<()> {
    let Some(customer_id) = current.customer_local_id else {
        return Ok(());
    };
    let conv_date: Option<String> = conn
        .query_row(
            "SELECT created_at FROM conversations WHERE id = ?1",
            params![current.conversation_local_id],
            |r| r.get(0),
        )
        .ok();
    let observed_at = conv_date
        .filter(|d| !d.is_empty())
        .map(|d| d.replace('T', " ").chars().take(19).collect::<String>())
        .unwrap_or_else(|| chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string());
    let rows: Vec<ObservationRow> = current
        .signals
        .iter()
        .filter(|s| s.dimension != "response_preference" || s.confidence == "high")
        .map(|s| ObservationRow {
            customer_id,
            conversation_id: Some(current.conversation_local_id),
            thread_local_id: s.evidence.as_ref().and_then(|e| e.thread_local_id),
            dimension: s.dimension.clone(),
            value: s.value.clone(),
            confidence: s.confidence.clone(),
            evidence_excerpt: s.evidence.as_ref().map(|e| e.excerpt.clone()),
            source: "heuristic".into(),
            observed_at: observed_at.clone(),
        })
        .collect();
    if !rows.is_empty() {
        insert_observations(conn, &rows)?;
    }
    Ok(())
}

/// Observations from COMPLETED (closed) conversations form the baseline:
/// today's still-open ticket is "current", not "normal" (spec #5, #24).
fn closed_conversation_ids(conn: &Connection, customer_id: i64) -> std::collections::HashSet<i64> {
    get_customer_conversations(conn, customer_id, None)
        .map(|rows| {
            rows.into_iter()
                .filter(|c| c.status == "closed")
                .map(|c| c.id)
                .collect()
        })
        .unwrap_or_default()
}

/// `rebuildBaseline` (engine.ts:163-201): recency-weighted dominant values
/// from the CLOSED-conversation observations, persisted with a fresh profile
/// version. The preference threshold demotes thin evidence to `low`.
pub fn rebuild_baseline(conn: &Connection, customer_id: i64) -> Result<Option<BehaviorBaseline>> {
    let closed = closed_conversation_ids(conn, customer_id);
    let observations: Vec<ObservationRow> = get_observations_for_customer(conn, customer_id)?
        .into_iter()
        .filter(|o| o.conversation_id.is_some_and(|id| closed.contains(&id)))
        .collect();
    if observations.is_empty() {
        return Ok(None);
    }
    let conv_rows = get_customer_conversations(conn, customer_id, None).unwrap_or_default();
    let conversation_count = conv_rows.len() as i64;
    let mut last_observed_by_dim: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    for o in &observations {
        let entry = last_observed_by_dim.entry(o.dimension.clone()).or_default();
        if o.observed_at > *entry {
            *entry = o.observed_at.clone();
        }
    }
    let profile_version = next_profile_version(conn, customer_id);
    let mut dimensions = Vec::new();
    let mut dims: Vec<&str> = observations
        .iter()
        .map(|o| o.dimension.as_str())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    dims.sort();
    for dim in dims {
        let dim_obs: Vec<&ObservationRow> =
            observations.iter().filter(|o| o.dimension == dim).collect();
        let Some(best) = dominant_value(&dim_obs) else {
            continue;
        };
        let count = dim_obs.len() as i64;
        // Highest confidence observed on the dimension.
        let mut confidence = "low".to_string();
        for o in &dim_obs {
            if rank_confidence(&o.confidence) > rank_confidence(&confidence) {
                confidence = o.confidence.clone();
            }
        }
        let effective = if count >= crate::intelligence::MIN_OBSERVATIONS_FOR_PREFERENCE as i64 {
            confidence.clone()
        } else {
            "low".to_string()
        };
        upsert_baseline(
            conn,
            customer_id,
            dim,
            &best,
            &effective,
            count,
            last_observed_by_dim.get(dim).map(String::as_str),
            profile_version,
        )?;
        dimensions.push(BaselineDimension {
            dimension: dim.to_string(),
            typical_value: best,
            confidence: effective,
            observation_count: count,
            last_observed: last_observed_by_dim.get(dim).cloned(),
        });
    }
    Ok(Some(BehaviorBaseline {
        customer_local_id: customer_id,
        conversation_count,
        observation_count: observations.len() as i64,
        dimensions,
        last_updated: Some(chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string()),
        profile_version,
    }))
}

fn rank_confidence(c: &str) -> u8 {
    match c {
        "high" => 3,
        "medium" => 2,
        "low" => 1,
        _ => 0,
    }
}

/// `comparisonBaseline` (engine.ts:208-236): the customer's typical values
/// computed from every conversation EXCEPT the current one. First-time
/// clients get None (no invented history, spec #3).
pub fn comparison_baseline(
    conn: &Connection,
    customer_id: i64,
    exclude_conversation_id: i64,
) -> Result<Option<BehaviorBaseline>> {
    let closed = closed_conversation_ids(conn, customer_id);
    let observations: Vec<ObservationRow> = get_observations_for_customer(conn, customer_id)?
        .into_iter()
        .filter(|o| {
            o.conversation_id.is_some_and(|id| closed.contains(&id))
                && o.conversation_id != Some(exclude_conversation_id)
        })
        .collect();
    if observations.is_empty() {
        return Ok(None);
    }
    let conv_rows = get_customer_conversations(conn, customer_id, None).unwrap_or_default();
    let mut dims: Vec<&str> = observations
        .iter()
        .map(|o| o.dimension.as_str())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    dims.sort();
    let mut dimensions = Vec::new();
    for dim in dims {
        let dim_obs: Vec<&ObservationRow> =
            observations.iter().filter(|o| o.dimension == dim).collect();
        let Some(best) = dominant_value(&dim_obs) else {
            continue;
        };
        let count = dim_obs.len() as i64;
        let last_observed = dim_obs
            .iter()
            .map(|o| o.observed_at.as_str())
            .max()
            .map(String::from);
        dimensions.push(BaselineDimension {
            dimension: dim.to_string(),
            typical_value: best,
            confidence: if count >= crate::intelligence::MIN_OBSERVATIONS_FOR_PREFERENCE as i64 {
                "medium".into()
            } else {
                "low".into()
            },
            observation_count: count,
            last_observed,
        });
    }
    Ok(Some(BehaviorBaseline {
        customer_local_id: customer_id,
        conversation_count: (conv_rows.len() as i64).saturating_sub(1).max(0),
        observation_count: observations.len() as i64,
        dimensions,
        last_updated: Some(chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string()),
        profile_version: 1,
    }))
}

// ---------------- change detection (spec #5, #19) ----------------

/// Dimensions whose values form a meaningful ORDER (low -> high). Only these
/// get increase/decrease directions and magnitude math; nominal dimensions
/// only report "changed".
const ORDINAL_DIMENSIONS: [&str; 5] = [
    "urgency",
    "frustration",
    "detail",
    "directness",
    "technical_language",
];

/// `computeChanges` (engine.ts:246-282).
pub fn compute_changes(
    current: &CurrentInteraction,
    baseline: Option<&BehaviorBaseline>,
) -> Vec<InteractionChange> {
    let Some(baseline) = baseline else {
        return Vec::new();
    };
    let mut changes = Vec::new();
    let current_by_dim: std::collections::HashMap<&str, &str> = current
        .signals
        .iter()
        .map(|s| (s.dimension.as_str(), s.value.as_str()))
        .collect();
    for b in &baseline.dimensions {
        let Some(cur) = current_by_dim.get(b.dimension.as_str()) else {
            continue;
        };
        if !ORDINAL_DIMENSIONS.contains(&b.dimension.as_str()) {
            if *cur == b.typical_value {
                continue;
            }
            changes.push(InteractionChange {
                dimension: b.dimension.clone(),
                baseline_value: b.typical_value.clone(),
                current_value: (*cur).to_string(),
                direction: "changed".into(),
                magnitude: 0.0,
                significant: false,
            });
            continue;
        }
        let (Some(b_rank), Some(c_rank)) = (
            dimension_rank(&b.dimension, &b.typical_value),
            dimension_rank(&b.dimension, cur),
        ) else {
            continue;
        };
        if b_rank == c_rank {
            continue;
        }
        let span = rank_span(&b.dimension);
        let raw_delta = (c_rank as f64 - b_rank as f64) / span as f64;
        let magnitude = (raw_delta.abs()).min(1.0);
        changes.push(InteractionChange {
            dimension: b.dimension.clone(),
            baseline_value: b.typical_value.clone(),
            current_value: (*cur).to_string(),
            direction: if raw_delta > 0.08 {
                "increase".into()
            } else if raw_delta < -0.08 {
                "decrease".into()
            } else {
                "same".into()
            },
            magnitude: (magnitude * 100.0).round() / 100.0,
            significant: magnitude >= crate::intelligence::CHANGE_SIGNIFICANCE_THRESHOLD,
        });
    }
    changes.sort_by(|a, b| b.magnitude.total_cmp(&a.magnitude));
    changes
}

// ---------------- outcomes, effort, friction (spec #16, #17, #52, #53) ----

/// `computeOutcome` (engine.ts:286-340): follow-ups (closing
/// acknowledgments excluded), clarifications, escalation markers, effort
/// score (0..10), friction band and the response-style classifier. The
/// result is persisted via `upsert_outcome`.
pub fn compute_outcome(conn: &Connection, conversation_id: i64) -> Result<Option<OutcomeComputed>> {
    let Some(conv) = conversation_info(conn, conversation_id) else {
        return Ok(None);
    };
    let Some(customer_id) = conv.customer_local_id else {
        return Ok(None);
    };
    let mut stmt = conn.prepare(
        "SELECT type, body_html, body_text, created_at
           FROM conversation_threads
          WHERE conversation_id = ?1 AND deleted_at IS NULL AND state = 'published'
          ORDER BY remote_created_at ASC",
    )?;
    let rows: Vec<(String, Option<String>, Option<String>)> = stmt
        .query_map(params![conversation_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })?
        .filter_map(|r| r.ok())
        .collect();
    let text = |html: &Option<String>, body: &Option<String>| -> String {
        let raw = html
            .as_deref()
            .filter(|h| !h.is_empty())
            .or(body.as_deref())
            .unwrap_or("");
        crate::demo::html_to_text(raw).to_lowercase()
    };
    let customer_msgs: Vec<String> = rows
        .iter()
        .filter(|(t, _, _)| t == "customer")
        .map(|(_t, h, b)| text(h, b))
        .collect();
    let reply_msgs: Vec<String> = rows
        .iter()
        .filter(|(t, _, _)| t == "reply")
        .map(|(_t, h, b)| text(h, b))
        .collect();
    let note_msgs: Vec<String> = rows
        .iter()
        .filter(|(t, _, _)| t == "note")
        .map(|(_t, h, b)| text(h, b))
        .collect();

    // Follow-ups: customer messages after the first support reply that ask
    // for MORE help (a pure closing acknowledgment is not customer effort).
    let mut follow_up_count: i64 = 0;
    let mut saw_reply = false;
    for (t, h, b) in &rows {
        if t == "reply" {
            saw_reply = true;
        } else if t == "customer" && saw_reply {
            let plain = {
                let raw = h
                    .as_deref()
                    .filter(|x| !x.is_empty())
                    .or(b.as_deref())
                    .unwrap_or("");
                crate::demo::html_to_text(raw)
            };
            if !is_closing_acknowledgment(&plain) {
                follow_up_count += 1;
            }
        }
    }
    // Clarifications: asks for clarification or repeats issue phrasing.
    let clarification_re = regex::Regex::new(
        r"still|again|re-?send|clarif|you didn'?t|that didn'?t|not what i|same issue|as i (said|mentioned|wrote)",
    )
    .expect("static");
    let clarification_count = customer_msgs
        .iter()
        .skip(1)
        .filter(|t| clarification_re.is_match(t))
        .count() as i64;
    // Escalation markers in notes or replies.
    let note_re = regex::Regex::new(r"(?i)escalat|urgent|priority|vip").expect("static");
    let reply_re = regex::Regex::new(r"(?i)escalat").expect("static");
    let escalated = note_msgs.iter().any(|t| note_re.is_match(t))
        || reply_msgs.iter().any(|t| reply_re.is_match(t));
    let resolved_after_first: Option<bool> =
        (!reply_msgs.is_empty()).then(|| follow_up_count == 0 && conv.status == "closed");
    // Effort score (spec #52): support friction, 0 (low) .. 10 (high).
    let effort = (customer_msgs.len() as f64) * 1.2
        + (follow_up_count as f64) * 1.5
        + (clarification_count as f64) * 2.0
        + i64::from(escalated) as f64 * 2.0;
    let effort_score =
        (!customer_msgs.is_empty()).then(|| (effort.min(10.0) * 10.0).round() / 10.0);
    let friction = match effort_score {
        Some(s) if s >= 6.0 => "high",
        Some(s) if s >= 3.5 => "moderate",
        _ => "none",
    }
    .to_string();
    // Response style classifier (spec #16).
    let joined = reply_msgs.concat();
    let response_style: Option<String> = (!reply_msgs.is_empty()).then(|| {
        let avg = joined.len() as f64 / reply_msgs.len() as f64;
        let step_re =
            regex::Regex::new(r"(?i)\b(step|first|then|next|finally)\b|1\.").expect("static");
        if avg > 700.0 {
            "detailed_explanation".to_string()
        } else if step_re.is_match(&joined) {
            "step_by_step".to_string()
        } else if avg < 200.0 {
            "short_answer".to_string()
        } else {
            "direct_answer_with_explanation".to_string()
        }
    });

    let computed = OutcomeComputed {
        effort_score,
        friction: friction.clone(),
        follow_up_count,
        clarification_count,
        escalated,
        resolved_after_first_response: resolved_after_first,
        response_style: response_style.clone(),
    };
    upsert_outcome(
        conn,
        customer_id,
        conversation_id,
        resolved_after_first,
        follow_up_count,
        clarification_count,
        escalated,
        effort_score,
        response_style.as_deref(),
        Some(&friction),
    )?;
    Ok(Some(computed))
}

/// `outcomeSummary` (engine.ts:342-387): rates + historically effective
/// approaches + friction flags.
pub fn outcome_summary(
    conn: &Connection,
    customer_id: i64,
) -> Result<Option<SupportOutcomeSummary>> {
    let outcomes = get_outcomes_for_customer(conn, customer_id)?;
    if outcomes.is_empty() {
        return Ok(None);
    }
    let total = outcomes.len() as i64;
    let resolved = outcomes
        .iter()
        .filter(|o| o.resolved_after_first_response == Some(1))
        .count() as i64;
    let with_follow_ups = outcomes.iter().filter(|o| o.follow_up_count > 0).count() as i64;
    let with_clarifications = outcomes
        .iter()
        .filter(|o| o.clarification_count > 0)
        .count() as i64;
    let escalated = outcomes.iter().filter(|o| o.escalated == 1).count() as i64;
    let avg_effort = outcomes
        .iter()
        .map(|o| o.effort_score.unwrap_or(0.0))
        .sum::<f64>()
        / total as f64;

    let conv_rows = get_customer_conversations(conn, customer_id, None).unwrap_or_default();
    let conv_numbers: std::collections::HashMap<i64, i64> =
        conv_rows.iter().map(|c| (c.id, c.number)).collect();
    let conv_subjects: std::collections::HashMap<i64, Option<String>> = conv_rows
        .iter()
        .map(|c| (c.id, c.subject.clone()))
        .collect();

    // Historically effective approaches (spec #18, #44).
    let mut style_groups: std::collections::HashMap<String, (i64, i64, Option<i64>)> =
        std::collections::HashMap::new();
    for o in &outcomes {
        let Some(style) = o.response_style.as_deref() else {
            continue;
        };
        let g = style_groups
            .entry(style.to_string())
            .or_insert((0, 0, None));
        g.1 += 1;
        if o.resolved_after_first_response == Some(1) {
            g.0 += 1;
            if g.2.is_none() {
                g.2 = Some(o.conversation_id);
            }
        }
    }
    let mut effective_approaches: Vec<EffectiveApproach> = style_groups
        .into_iter()
        .map(|(approach, (worked, _, example))| EffectiveApproach {
            approach: humanize_style(&approach),
            worked_count: worked,
            example_conversation_local_id: example,
            example_number: example.and_then(|id| conv_numbers.get(&id).copied()),
        })
        .collect();
    effective_approaches.sort_by_key(|a| std::cmp::Reverse(a.worked_count));
    let friction_flags: Vec<FrictionFlag> = outcomes
        .iter()
        .filter(|o| {
            o.friction.as_deref() == Some("high") || o.friction.as_deref() == Some("moderate")
        })
        .map(|o| FrictionFlag {
            conversation_local_id: o.conversation_id,
            number: conv_numbers.get(&o.conversation_id).copied().unwrap_or(0),
            subject: conv_subjects.get(&o.conversation_id).cloned().flatten(),
            friction: o.friction.clone().unwrap_or_else(|| "moderate".into()),
        })
        .collect();

    Ok(Some(SupportOutcomeSummary {
        customer_local_id: customer_id,
        total_conversations: total,
        first_response_resolution_rate: ratio(resolved, total),
        follow_up_rate: ratio(with_follow_ups, total),
        clarification_rate: ratio(with_clarifications, total),
        escalation_rate: ratio(escalated, total),
        avg_effort_score: (avg_effort * 10.0).round() / 10.0,
        effective_approaches,
        friction_flags,
    }))
}

// ---------------- recommendation (deterministic fallback, spec #13) --------

/// `heuristicRecommendation` (engine.ts:391-444): the deterministic support
/// approach every conversation gets, AI or not.
pub fn heuristic_recommendation(
    current: &CurrentInteraction,
    changes: &[InteractionChange],
    baseline: Option<&BehaviorBaseline>,
    overrides: &[OverrideRow],
) -> Option<SupportApproach> {
    let sig: std::collections::HashMap<&str, &str> = current
        .signals
        .iter()
        .map(|s| (s.dimension.as_str(), s.value.as_str()))
        .collect();
    // Only VALID preference values participate: a malformed/legacy override
    // value must not silently steer the recommendation or leak into prompts.
    let pref_override = overrides.iter().find(|o| {
        o.field == "response_preference"
            && crate::interaction_current::dimension_vocabulary("response_preference")
                .is_some_and(|vocab| vocab.contains(&o.human_value.as_str()))
    });
    let explicit_pref = current.signals.iter().find(|s| {
        s.dimension == "response_preference" && s.source == "heuristic" && s.confidence == "high"
    });
    let urgency = sig.get("urgency").copied().unwrap_or("none");
    let frustration = sig.get("frustration").copied().unwrap_or("none");
    let detail = sig.get("detail").copied().unwrap_or("moderate");

    let mut avoid: Vec<String> = Vec::new();
    if frustration == "strong" || frustration == "moderate" {
        avoid.push("repeating troubleshooting steps the customer already described".into());
    }
    if urgency == "high" {
        avoid.push("generic responses that do not address the reported impact".into());
    }
    if detail == "low" || detail == "very_low" {
        avoid.push("long background explanations before the answer".into());
    }
    if current.message_stats.question_count > 1 {
        avoid.push("answering only one of several questions".into());
    }

    let mut strategy: Vec<String> = Vec::new();
    strategy.push("Acknowledge the specific issue the customer reported".into());
    if frustration != "none" {
        strategy.push("Recognize the reported impact before troubleshooting".into());
    }
    strategy.push("Answer the primary question directly".into());
    if crate::interaction_current::needs_de_escalation(&current.signals) {
        strategy.push("Explain what is being checked and what happens next".into());
    }
    strategy.push("State the concrete next action".into());
    if urgency == "high" {
        strategy.push("Set expectations using only supported timeframes".into());
    }

    let length = if pref_override.is_some_and(|o| o.human_value == "concise")
        || explicit_pref.is_some_and(|s| s.value == "concise")
        || detail == "low"
        || detail == "very_low"
    {
        "concise"
    } else if detail == "very_high"
        || explicit_pref.is_some_and(|s| s.value == "detailed")
        || pref_override.is_some_and(|o| o.human_value == "detailed")
    {
        "detailed"
    } else {
        "moderate"
    };

    let mut why: Vec<String> = Vec::new();
    if let Some(b) = baseline {
        why.push(format!(
            "{} observations across {} conversations inform this approach",
            b.observation_count, b.conversation_count
        ));
    }
    let significant: Vec<&InteractionChange> = changes.iter().filter(|c| c.significant).collect();
    if !significant.is_empty() {
        why.push(format!(
            "today's interaction differs from the customer's norm: {}",
            significant
                .iter()
                .map(|c| format!("{} {}", c.dimension.replace('_', " "), c.direction))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if explicit_pref.is_some() {
        why.push(
            "the customer explicitly requested this response style in the current message".into(),
        );
    }
    if pref_override.is_some() {
        why.push(
            "a support rep manually set a preference that takes precedence over AI inference"
                .into(),
        );
    }
    if why.is_empty() {
        why.push("based on observable signals in the current message".into());
    }

    Some(SupportApproach {
        tone: Some(
            if frustration != "none" || urgency == "high" {
                "Calm and direct"
            } else {
                "Direct and friendly"
            }
            .into(),
        ),
        length: Some(length.into()),
        start_with: Some(
            if frustration != "none" {
                "Acknowledge the specific problem and its impact, then answer the primary question."
            } else {
                "Answer the primary question directly."
            }
            .into(),
        ),
        then: Some("Explain the action being taken and what happens next.".into()),
        avoid,
        response_strategy: strategy,
        de_escalation: crate::interaction_current::needs_de_escalation(&current.signals),
        escalation_recommendation: (sig.get("expectation") == Some(&"escalation")).then(|| {
            "The customer is asking for escalation — review the linked previous cases before replying.".to_string()
        }),
        why,
        source: if pref_override.is_some() {
            "ai+human-override".into()
        } else {
            "heuristic".into()
        },
        confidence: baseline
            .filter(|b| {
                b.observation_count
                    >= crate::intelligence::MIN_OBSERVATIONS_FOR_PREFERENCE as i64
            })
            .map(|_| "medium".into())
            .unwrap_or_else(|| "low".into()),
    })
}

// ---------------- repeat-issue detection (spec #50) ----------------

/// STOPWORDS (engine.ts:656).
const STOPWORDS: [&str; 16] = [
    "this", "that", "with", "from", "have", "been", "after", "before", "about", "would", "could",
    "their", "there", "issue", "problem", "support",
];

fn subject_tokens(subject: &str) -> Vec<String> {
    subject
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.chars().count() > 3 && !STOPWORDS.contains(t))
        .map(String::from)
        .collect()
}

fn conversation_tags(conn: &Connection, conversation_id: i64) -> Vec<String> {
    conn.prepare(
        "SELECT t.name FROM conversation_tags ct
           JOIN tags t ON t.id = ct.tag_id
          WHERE ct.conversation_id = ?1",
    )
    .map(|mut stmt| {
        stmt.query_map(params![conversation_id], |r| r.get::<_, String>(0))
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
    })
    .unwrap_or_default()
}

/// `detectRepeatIssue` (engine.ts:448-476): subject-token + tag overlap
/// against the customer's history; `detected` needs >= 2 related tickets.
pub fn detect_repeat_issue(conn: &Connection, conversation_id: i64) -> Result<Option<RepeatIssue>> {
    let Some(conv) = conversation_info(conn, conversation_id) else {
        return Ok(None);
    };
    let Some(customer_id) = conv.customer_local_id else {
        return Ok(None);
    };
    let current_subject_tokens = subject_tokens(conv.subject.as_deref().unwrap_or(""));
    let current_tags: Vec<String> = conversation_tags(conn, conversation_id)
        .into_iter()
        .map(|t| t.to_lowercase())
        .collect();
    let history = get_customer_conversations(conn, customer_id, Some(conversation_id))?;
    let mut related: Vec<(&CustomerConversation, f64)> = history
        .iter()
        .map(|h| {
            let tokens = subject_tokens(h.subject.as_deref().unwrap_or(""));
            let token_overlap = tokens
                .iter()
                .filter(|t| current_subject_tokens.contains(t))
                .count() as f64
                / 1f64.max(current_subject_tokens.len().min(tokens.len().max(1)) as f64);
            let history_tags = conversation_tags(conn, h.id)
                .into_iter()
                .map(|t| t.to_lowercase())
                .collect::<Vec<_>>();
            let tag_overlap = current_tags
                .iter()
                .filter(|t| history_tags.contains(t))
                .count() as f64
                / 1f64.max(current_tags.len().min(history_tags.len()) as f64);
            let score = token_overlap.max(tag_overlap * 0.6);
            (h, score)
        })
        .filter(|(_, score)| *score >= 0.4)
        .collect::<Vec<_>>();
    related.sort_by(|a, b| b.1.total_cmp(&a.1));
    let related: Vec<RelatedConversation> = related
        .into_iter()
        .take(5)
        .map(|(h, _)| RelatedConversation {
            local_id: h.id,
            number: h.number,
            subject: h.subject.clone(),
        })
        .collect();
    Ok(Some(RepeatIssue {
        detected: related.len() >= 2,
        related_conversations: related,
    }))
}

// ---------------- lazy history backfill ----------------

/// `ensureHistoryBackfill` (engine.ts:490-521): materialize observations for
/// any of the customer's conversations that lack them. Guarded by two cheap
/// COUNT queries so the heavy path only runs when coverage is genuinely
/// incomplete; the baseline is rebuilt ONCE after the loop (backfill was
/// O(n^2) on large histories).
fn ensure_history_backfill(conn: &Connection, customer_id: Option<i64>) -> Result<()> {
    let Some(customer_id) = customer_id else {
        return Ok(());
    };
    let conv_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM conversations WHERE customer_id = ?1 AND deleted_at IS NULL",
        params![customer_id],
        |r| r.get(0),
    )?;
    if conv_count == 0 {
        return Ok(());
    }
    let covered_count: i64 = conn.query_row(
        "SELECT COUNT(DISTINCT conversation_id) FROM client_behavior_observations
          WHERE customer_id = ?1 AND conversation_id IS NOT NULL",
        params![customer_id],
        |r| r.get(0),
    )?;
    if covered_count >= conv_count {
        return Ok(());
    }
    let convs = get_customer_conversations(conn, customer_id, None)?;
    if convs.is_empty() {
        return Ok(());
    }
    let covered: std::collections::HashSet<i64> = {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT conversation_id FROM client_behavior_observations
              WHERE customer_id = ?1 AND conversation_id IS NOT NULL",
        )?;
        let rows: std::collections::HashSet<i64> = stmt
            .query_map(params![customer_id], |r| r.get::<_, i64>(0))?
            .filter_map(|r| r.ok())
            .collect();
        rows
    };
    let mut added = false;
    for c in &convs {
        if covered.contains(&c.id) {
            continue;
        }
        // rebuildBaseline=false: one rebuild after the loop (the reference's
        // O(n^2) fix). Broken conversations are skipped, never fatal.
        if crate::interaction_current::record_current_interaction_opts(conn, c.id, false).is_err() {
            continue;
        }
        let _ = compute_outcome(conn, c.id);
        added = true;
    }
    if added {
        rebuild_baseline(conn, customer_id)?;
    }
    Ok(())
}

// ---------------- full card assembly (spec #26, #62) ----------------

/// `buildCard` (engine.ts:523-583): current + baseline + changes +
/// recommendation + outcome + repeat-issue + provenance. Recommendation
/// precedence: explicit AI enrichment (refresh response) > STORED Stage-2
/// recommendation > deterministic heuristic.
/// The refresh-time AI enrichment slice: (signals, recommendation,
/// prompt_version, model) — `buildCard`'s optional inputs.
pub type AiEnrichment<'a> = (
    &'a [InteractionSignal],
    Option<&'a SupportApproach>,
    Option<&'a str>,
    Option<&'a str>,
);

pub fn build_card(
    conn: &Connection,
    conversation_id: i64,
    ai_enrichment: Option<AiEnrichment<'_>>,
) -> Result<Option<InteractionCard>> {
    let Some(conv) = conversation_info(conn, conversation_id) else {
        return Ok(None);
    };
    ensure_history_backfill(conn, conv.customer_local_id)?;
    let stored = get_latest_current_interaction(conn, conversation_id)?;
    // Current interaction: stored snapshot when present, else freshly
    // computed (optionally merged with AI enrichment signals).
    let mut current_interaction = match (&stored, ai_enrichment) {
        (Some(s), _) if !s.signals.is_empty() => {
            let mut c =
                crate::interaction_current::compute_current_interaction(conn, conversation_id)?
                    .unwrap_or_else(|| CurrentInteraction {
                        conversation_local_id: conversation_id,
                        customer_local_id: conv.customer_local_id,
                        is_returning_client: false,
                        signals: Vec::new(),
                        customer_goal: None,
                        message_stats: MessageStats::default(),
                        sources: String::new(),
                        generated_at: None,
                    });
            c.signals = sanitize_signals(s.signals.clone()).signals;
            c.message_stats = s.message_stats.unwrap_or_default();
            c.customer_goal = s.customer_goal.clone().or(c.customer_goal);
            c.sources = match s.sources.as_str() {
                "ai" => "ai".to_string(),
                "heuristic+ai" => "heuristic+ai".to_string(),
                _ => "heuristic".to_string(),
            };
            c.generated_at = Some(s.generated_at.clone());
            c
        }
        _ => {
            let Some(computed) =
                crate::interaction_current::compute_current_interaction(conn, conversation_id)?
            else {
                return Ok(None);
            };
            let mut c = computed;
            if let Some((ai_signals, _, _, _)) = ai_enrichment {
                if !ai_signals.is_empty() {
                    let merged = merge_signals(&c.signals, ai_signals);
                    c.signals = sanitize_signals(merged).signals;
                    c.sources = "heuristic+ai".to_string();
                }
            }
            c
        }
    };
    current_interaction.conversation_local_id = conversation_id;
    current_interaction.customer_local_id = conv.customer_local_id;
    current_interaction.is_returning_client = conv
        .customer_local_id
        .map(|cid| {
            get_customer_conversations(conn, cid, Some(conversation_id))
                .map(|rows| !rows.is_empty())
                .unwrap_or(false)
        })
        .unwrap_or(false);
    let customer_id = conv.customer_local_id;
    let baseline = customer_id
        .and_then(|cid| comparison_baseline(conn, cid, conversation_id).ok())
        .flatten();
    let changes = compute_changes(&current_interaction, baseline.as_ref());
    let overrides: Vec<OverrideRow> = customer_id
        .map(|cid| get_active_overrides(conn, cid).unwrap_or_default())
        .unwrap_or_default();
    let stored_rec = stored.as_ref().and_then(|s| s.recommendation.clone());
    let recommendation: Option<SupportApproach> = ai_enrichment
        .and_then(|(_, rec, _, _)| rec.cloned())
        .or(stored_rec.as_ref().map(|r| r.recommendation.clone()))
        .or_else(|| {
            heuristic_recommendation(
                &current_interaction,
                &changes,
                baseline.as_ref(),
                &overrides,
            )
        });
    let outcome = compute_outcome(conn, conversation_id)?;
    let repeat_issue = detect_repeat_issue(conn, conversation_id)?;
    Ok(Some(InteractionCard {
        conversation_local_id: conversation_id,
        customer_local_id: conv.customer_local_id,
        client_kind: if current_interaction.is_returning_client {
            "returning".into()
        } else {
            "first_time".into()
        },
        current: current_interaction,
        baseline,
        changes,
        recommendation,
        effort_score: outcome.as_ref().and_then(|o| o.effort_score),
        friction: outcome.as_ref().map(|o| o.friction.clone()),
        repeat_issue,
        provenance: CardProvenance {
            ai_generated: ai_enrichment.is_some() || stored_rec.is_some(),
            prompt_version: ai_enrichment
                .and_then(|(_, _, pv, _)| pv.map(String::from))
                .or(stored_rec.as_ref().and_then(|r| r.prompt_version.clone())),
            model: ai_enrichment
                .and_then(|(_, _, _, m)| m.map(String::from))
                .or(stored_rec.as_ref().and_then(|r| r.model.clone())),
            generated_at: stored.as_ref().map(|s| s.generated_at.clone()),
        },
    }))
}

// ---------------- customer profile assembly (spec #25) ----------------

/// `buildTimeline` (engine.ts:754-774): the customer's conversations grouped
/// by month, last 12 months.
fn build_timeline(conv_rows: &[CustomerConversation]) -> Vec<TimelineEntry> {
    let mut sorted: Vec<&CustomerConversation> = conv_rows.iter().collect();
    sorted.sort_by(|a, b| a.remote_created_at.cmp(&b.remote_created_at));
    let mut by_month: std::collections::BTreeMap<String, (i64, Vec<i64>, Vec<String>)> =
        std::collections::BTreeMap::new();
    for c in sorted {
        let month = c
            .remote_created_at
            .as_deref()
            .unwrap_or("")
            .chars()
            .take(7)
            .collect::<String>();
        let g = by_month.entry(month).or_insert((0, Vec::new(), Vec::new()));
        g.0 += 1;
        g.1.push(c.id);
        g.2.push(c.subject.clone().unwrap_or_else(|| "(no subject)".into()));
    }
    by_month
        .into_iter()
        .rev()
        .take(12)
        .map(|(month, (count, ids, subjects))| TimelineEntry {
            month,
            conversation_count: count,
            summary: Some(if count == 1 {
                subjects.first().cloned().unwrap_or_default()
            } else {
                let head: Vec<&str> = subjects.iter().take(2).map(String::as_str).collect();
                format!(
                    "{} tickets: {}{}",
                    count,
                    head.join(" · "),
                    if count > 2 { " …" } else { "" }
                )
            }),
            conversation_local_ids: ids,
        })
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

/// `buildProfile` (engine.ts:587-632): baseline + preferences (with the
/// overfit guard: the threshold counts DISTINCT CONVERSATIONS) + timeline +
/// outcomes + playbook + overrides. Unknown customer → None (404).
pub fn build_profile(
    conn: &Connection,
    customer_id: i64,
) -> Result<Option<ClientInteractionProfile>> {
    let exists: i64 = conn.query_row(
        "SELECT COUNT(*) FROM customers WHERE id = ?1",
        params![customer_id],
        |r| r.get(0),
    )?;
    if exists == 0 {
        return Ok(None);
    }
    ensure_history_backfill(conn, Some(customer_id))?;
    let conv_rows = get_customer_conversations(conn, customer_id, None)?;
    let baseline = get_baseline(conn, customer_id)?;
    // Preferences with overfit guard (spec #39, #40).
    let observations = get_observations_for_customer(conn, customer_id)?;
    let pref_observations: Vec<&ObservationRow> = observations
        .iter()
        .filter(|o| o.dimension == "response_preference")
        .collect();
    let mut pref_conv_sets: std::collections::HashMap<String, std::collections::HashSet<i64>> =
        std::collections::HashMap::new();
    for o in &pref_observations {
        if let Some(conv_id) = o.conversation_id {
            pref_conv_sets
                .entry(o.value.clone())
                .or_default()
                .insert(conv_id);
        }
    }
    let mut preferences = get_preferences(conn, customer_id)?;
    let min_pref: i64 = crate::intelligence::MIN_OBSERVATIONS_FOR_PREFERENCE as i64;
    for (value, convs) in &pref_conv_sets {
        if convs.len() as i64 >= min_pref
            && !preferences
                .iter()
                .any(|p| p.preference == *value && p.origin == "human_entered")
        {
            let rows: Vec<&ObservationRow> = pref_observations
                .iter()
                .filter(|o| o.value == *value && !o.observed_at.is_empty())
                .copied()
                .collect();
            let mut dates: Vec<&str> = rows.iter().map(|o| o.observed_at.as_str()).collect();
            dates.sort();
            let confidence = if convs.len() >= 5 { "high" } else { "medium" };
            let last = dates.last().map(|d| d.to_string());
            upsert_preference(
                conn,
                customer_id,
                value,
                convs.len() as i64,
                dates.first().copied(),
                last.as_deref(),
                confidence,
                "ai_inferred",
            )?;
        }
    }
    preferences = get_preferences(conn, customer_id)?;
    let preferences: Vec<CommunicationPreference> = preferences
        .into_iter()
        .filter(|p| {
            p.origin == "human_entered"
                || p.evidence_count >= crate::intelligence::MIN_OBSERVATIONS_FOR_PREFERENCE as i64
        })
        .collect();

    let timeline = build_timeline(&conv_rows);
    let outcomes = outcome_summary(conn, customer_id)?;
    let playbook = build_playbook(baseline.as_ref(), outcomes.as_ref());
    let overrides = get_active_overrides(conn, customer_id)?;
    Ok(Some(ClientInteractionProfile {
        customer_local_id: customer_id,
        client_kind: if conv_rows.is_empty() {
            "first_time".into()
        } else {
            "returning".into()
        },
        baseline,
        preferences,
        timeline,
        outcomes,
        playbook,
        overrides,
    }))
}

/// `buildPlaybook` (engine.ts:634-651): the rep-facing cheat sheet derived from
/// the baseline + what historically worked.
fn build_playbook(
    baseline: Option<&BehaviorBaseline>,
    outcomes: Option<&SupportOutcomeSummary>,
) -> Option<ClientPlaybook> {
    if baseline.is_none() && outcomes.is_none() {
        return None;
    }
    let dims: std::collections::HashMap<&str, &str> = baseline
        .map(|b| {
            b.dimensions
                .iter()
                .map(|d| (d.dimension.as_str(), d.typical_value.as_str()))
                .collect()
        })
        .unwrap_or_default();
    let effective: Vec<&EffectiveApproach> = outcomes
        .map(|o| {
            o.effective_approaches
                .iter()
                .filter(|a| a.worked_count > 0)
                .collect()
        })
        .unwrap_or_default();
    let mut avoid: Vec<String> = Vec::new();
    let friction_high = outcomes
        .map(|o| o.friction_flags.iter().any(|f| f.friction == "high"))
        .unwrap_or(false);
    if dims.get("detail") == Some(&"very_high") || dims.get("detail") == Some(&"high") {
        avoid.push("one-line answers with no context".into());
    }
    if dims.get("detail") == Some(&"low") || dims.get("detail") == Some(&"very_low") {
        avoid.push("long background explanations".into());
    }
    if friction_high {
        avoid.push("asking for information the customer already provided".into());
    }
    Some(ClientPlaybook {
        best_opening: if dims.get("directness") == Some(&"direct")
            || dims.get("directness") == Some(&"highly_direct")
        {
            "Acknowledge the issue directly and answer the primary question first.".into()
        } else {
            "Friendly greeting, then the answer with brief context.".into()
        },
        best_explanation_style: if dims.get("technical_language") == Some(&"technical")
            || dims.get("technical_language") == Some(&"highly_technical")
        {
            "Technical but concise.".into()
        } else if dims.get("detail") == Some(&"low") || dims.get("detail") == Some(&"very_low") {
            "Short paragraphs, plain language.".into()
        } else {
            "Moderate detail with concrete examples.".into()
        },
        best_troubleshooting_style: if effective.iter().any(|e| e.approach.contains("step")) {
            "Numbered steps.".into()
        } else {
            "Direct fix with a short explanation.".into()
        },
        likely_follow_up: outcomes
            .and_then(|o| o.follow_up_rate)
            .filter(|r| *r > 0.4)
            .map(|_| {
                "Often asks follow-up questions — consider covering likely next questions proactively."
                    .to_string()
            }),
        historically_successful: (!effective.is_empty()).then(|| {
            effective
                .iter()
                .take(3)
                .map(|e| format!("{} (worked in {} cases)", e.approach, e.worked_count))
                .collect::<Vec<_>>()
                .join("; ")
        }),
        avoid,
    })
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests (parity with tests/unit+integration/interaction.test.ts)
// ═══════════════════════════════════════════════════════════════════════════
#[cfg(test)]
mod tests {
    use super::*;
    use crate::interaction_current::record_current_interaction;
    use rusqlite::Connection;
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

    fn seed_customer(conn: &Connection, id: i64, remote: i64, name: &str) {
        let (first, last) = name.split_once(' ').unwrap_or((name, ""));
        conn.execute(
            "INSERT INTO customers (id, remote_id, first_name, last_name) VALUES (?1, ?2, ?3, ?4)",
            params![id, remote, first, last],
        )
        .unwrap();
    }

    #[allow(clippy::too_many_arguments)]
    fn seed_conversation(
        conn: &Connection,
        id: i64,
        remote: i64,
        customer: i64,
        subject: &str,
        status: &str,
    ) {
        conn.execute(
            "INSERT INTO conversations (id, remote_id, number, subject, status, mailbox_id, customer_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, datetime('now', '-10 days'))",
            params![id, remote, 7000 + id, subject, status, customer],
        )
        .unwrap();
    }

    fn seed_thread(conn: &Connection, conv: i64, kind: &str, body: &str) {
        conn.execute(
            "INSERT INTO conversation_threads (conversation_id, type, state, body_text, from_type, created_at)
             VALUES (?1, ?2, 'published', ?3, ?4, datetime('now', '-9 days'))",
            params![conv, kind, body, if kind == "customer" { "customer" } else { "user" }],
        )
        .unwrap();
    }

    // ---- AI-18: the forbidden-claim text scan (safety.ts) -------------------

    #[test]
    fn forbidden_patterns_reject_psychological_claims() {
        // mental-health language
        assert!(
            !sanitize_interaction_text("The customer seems to have anxiety about deadlines").ok
        );
        // personality typing
        assert!(!sanitize_interaction_text("This is a big five extravert personality type").ok);
        // fixed person label
        assert!(!sanitize_interaction_text("a difficult customer who is always demanding").ok);
        // protected attribute inference
        assert!(!sanitize_interaction_text("mentioned their religion in passing").ok);
        // clinical diagnosis language
        assert!(!sanitize_interaction_text("clearly suffers from a pathology").ok);
        // cognitive-ability judgment
        assert!(!sanitize_interaction_text("not the smartest user, borderline IQ").ok);
        // moral character judgment
        assert!(!sanitize_interaction_text("the user is a manipulative liar").ok);
        // fixed trait label ("the customer is rude")
        assert!(!sanitize_interaction_text("the customer is rude in every message").ok);
        // character judgment
        assert!(!sanitize_interaction_text("passive-aggressive and bullying tone").ok);
    }

    #[test]
    fn observable_behavior_text_passes_the_scan() {
        assert!(
            sanitize_interaction_text(
                "Customer writes detailed, highly technical messages and asks multiple questions."
            )
            .ok
        );
        assert!(
            sanitize_interaction_text(
                "Tone is frustrated; urgency cues present; expects immediate resolution."
            )
            .ok
        );
        // Each rejection carries the observable-behavior warning.
        let scan = sanitize_interaction_text("the customer is rude");
        assert!(!scan.ok);
        assert_eq!(scan.warnings.len(), 1);
        assert!(scan.warnings[0].contains("observable support-communication behavior"));
    }

    #[test]
    fn assert_safe_handles_empty_and_none() {
        assert!(assert_interaction_text_safe(None).ok);
        assert!(assert_interaction_text_safe(Some("")).ok);
        assert!(!assert_interaction_text_safe(Some("narcissistic demands")).ok);
    }

    // ---- outcome engine (spec #16, #17, #52, #53) ---------------------------

    #[test]
    fn closing_acknowledgment_is_not_customer_effort() {
        assert!(is_closing_acknowledgment("Thanks, that worked perfectly!"));
        assert!(is_closing_acknowledgment(
            "Perfect, all set — closing from my side"
        ));
        assert!(!is_closing_acknowledgment(
            "Thanks, but it still doesn't work"
        ));
        assert!(!is_closing_acknowledgment(
            "Can you re-send the instructions?"
        ));
        assert!(!is_closing_acknowledgment("However the issue came back"));
    }

    #[test]
    fn compute_outcome_resolved_after_first_response() {
        let conn = fresh_db();
        seed_customer(&conn, 500, 99001, "Ada Lovelace");
        seed_conversation(&conn, 700, 99700, 500, "API returns 401", "closed");
        seed_thread(
            &conn,
            700,
            "customer",
            "The API returns 401 since yesterday",
        );
        seed_thread(&conn, 700, "reply", "Rotate the token with the new secret");
        seed_thread(&conn, 700, "customer", "Thanks, that worked!");
        let out = compute_outcome(&conn, 700).unwrap().expect("outcome");
        assert_eq!(out.follow_up_count, 0, "closing ack is not a follow-up");
        assert_eq!(out.resolved_after_first_response, Some(true));
        assert!(!out.escalated);
        assert_eq!(out.friction, "none");
        assert!(
            out.effort_score.is_some_and(|s| s < 3.5),
            "{:?}",
            out.effort_score
        );
        // persisted
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM client_support_outcomes WHERE conversation_id = 700",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn compute_outcome_counts_clarifications_escalation_and_friction() {
        let conn = fresh_db();
        seed_customer(&conn, 501, 99002, "Grace Hopper");
        seed_conversation(&conn, 701, 99701, 501, "Import fails", "active");
        seed_thread(&conn, 701, "customer", "Import fails with error 500");
        seed_thread(&conn, 701, "reply", "Could you share the log file?");
        seed_thread(
            &conn,
            701,
            "customer",
            "Still broken, same issue as before, you didn't fix it",
        );
        seed_thread(&conn, 701, "note", "Escalating to tier 2, urgent");
        let out = compute_outcome(&conn, 701).unwrap().expect("outcome");
        assert_eq!(out.follow_up_count, 1);
        assert_eq!(out.clarification_count, 1);
        assert!(out.escalated);
        // 2 customer * 1.2 + 1 follow-up * 1.5 + 1 clarif * 2 + escalated * 2 = 7.9
        assert!(
            (out.effort_score.unwrap() - 7.9).abs() < 1e-6,
            "{:?}",
            out.effort_score
        );
        assert_eq!(out.friction, "high");
        assert_eq!(out.resolved_after_first_response, Some(false));
    }

    #[test]
    fn outcome_summary_reports_rates_and_effective_approaches() {
        let conn = fresh_db();
        seed_customer(&conn, 502, 99003, "Alan Turing");
        for (id, subject) in [
            (710, "Login issue"),
            (711, "Login broken again"),
            (712, "SSO login"),
        ] {
            seed_conversation(&conn, id, 99_000 + id, 502, subject, "closed");
            seed_thread(&conn, id, "customer", "Cannot log in");
            seed_thread(
                &conn,
                id,
                "reply",
                "First reset the password, then clear cookies.",
            );
            seed_thread(&conn, id, "customer", "Thanks, that worked!");
        }
        for id in [710, 711, 712] {
            compute_outcome(&conn, id).unwrap();
        }
        let s = outcome_summary(&conn, 502).unwrap().expect("summary");
        assert_eq!(s.total_conversations, 3);
        assert_eq!(s.first_response_resolution_rate, Some(1.0));
        assert_eq!(s.follow_up_rate, Some(0.0));
        assert!(!s.effective_approaches.is_empty());
        assert_eq!(s.effective_approaches[0].worked_count, 3);
        assert!(s.effective_approaches[0].approach.contains("Step"));
    }

    // ---- change detection (spec #5, #19) ------------------------------------

    fn current_with(signals: &[(&str, &str)]) -> CurrentInteraction {
        CurrentInteraction {
            conversation_local_id: 1,
            customer_local_id: Some(500),
            is_returning_client: true,
            signals: signals
                .iter()
                .map(|(d, v)| InteractionSignal {
                    dimension: (*d).to_string(),
                    value: (*v).to_string(),
                    confidence: "high".into(),
                    evidence: None,
                    source: "heuristic".into(),
                })
                .collect(),
            customer_goal: None,
            message_stats: MessageStats::default(),
            sources: "heuristic".into(),
            generated_at: None,
        }
    }

    #[test]
    fn compute_changes_ordinal_vs_nominal_dimensions() {
        let baseline = BehaviorBaseline {
            customer_local_id: 500,
            conversation_count: 5,
            observation_count: 20,
            dimensions: vec![
                BaselineDimension {
                    dimension: "detail".into(),
                    typical_value: "high".into(),
                    confidence: "medium".into(),
                    observation_count: 10,
                    last_observed: None,
                },
                BaselineDimension {
                    dimension: "tone".into(),
                    typical_value: "friendly".into(),
                    confidence: "medium".into(),
                    observation_count: 10,
                    last_observed: None,
                },
            ],
            last_updated: None,
            profile_version: 1,
        };
        let current = current_with(&[("detail", "low"), ("tone", "frustrated")]);
        let changes = compute_changes(&current, Some(&baseline));
        let detail = changes.iter().find(|c| c.dimension == "detail").unwrap();
        assert_eq!(detail.direction, "decrease");
        assert!(
            (detail.magnitude - 0.5).abs() < 1e-9,
            "{}",
            detail.magnitude
        );
        assert!(detail.significant, "0.5 >= 0.34");
        let tone = changes.iter().find(|c| c.dimension == "tone").unwrap();
        assert_eq!(tone.direction, "changed");
        assert_eq!(tone.magnitude, 0.0);
        assert!(!tone.significant);
        // no baseline → no changes
        assert!(compute_changes(&current, None).is_empty());
    }

    // ---- baseline (spec #5, #10, #23, #40) ----------------------------------

    #[test]
    fn rebuild_baseline_uses_closed_conversations_only() {
        let conn = fresh_db();
        seed_customer(&conn, 503, 99004, "Edsger Dijkstra");
        // Two CLOSED conversations with observations.
        for (id, subject) in [(720, "Bug A"), (721, "Bug A again")] {
            seed_conversation(&conn, id, 99_000 + id, 503, subject, "closed");
            seed_thread(
                &conn,
                id,
                "customer",
                "Still broken, this is very annoying, ASAP please",
            );
            record_current_interaction(&conn, id).unwrap();
        }
        // One OPEN conversation (today's ticket — current, not normal).
        seed_conversation(&conn, 722, 99022, 503, "Bug A third time", "active");
        seed_thread(&conn, 722, "customer", "Still not fixed");
        record_current_interaction(&conn, 722).unwrap();
        rebuild_baseline(&conn, 503).unwrap();
        let baseline = get_baseline(&conn, 503).unwrap().expect("baseline");
        // Frustration observed on all three conversations but the baseline
        // counts only the closed two.
        let frustration = baseline
            .dimensions
            .iter()
            .find(|d| d.dimension == "frustration")
            .expect("frustration baseline");
        assert_eq!(frustration.observation_count, 2, "closed-only membership");
        assert!(!frustration.typical_value.is_empty());
    }

    // ---- card + recommendation (spec #26, #13, #62) -------------------------

    #[test]
    fn build_card_first_time_client_gets_no_invented_history() {
        let conn = fresh_db();
        seed_customer(&conn, 504, 99005, "Barbara Liskov");
        seed_conversation(&conn, 730, 99730, 504, "First ever question", "active");
        seed_thread(
            &conn,
            730,
            "customer",
            "Could you explain how the export works please?",
        );
        let card = build_card(&conn, 730, None).unwrap().expect("card");
        assert_eq!(card.client_kind, "first_time");
        assert!(card.baseline.is_none());
        assert!(card.changes.is_empty());
        let rec = card.recommendation.expect("heuristic recommendation");
        assert_eq!(rec.source, "heuristic");
        assert_eq!(rec.confidence, "low");
        assert!(!rec.response_strategy.is_empty());
        assert!(!card.provenance.ai_generated);
    }

    #[test]
    fn build_card_serves_stored_stage2_recommendation() {
        let conn = fresh_db();
        seed_customer(&conn, 505, 99006, "Donald Knuth");
        seed_conversation(&conn, 731, 99731, 505, "Licensing question", "active");
        seed_thread(&conn, 731, "customer", "How many seats are included?");
        record_current_interaction(&conn, 731).unwrap();
        let stored = SupportApproach {
            tone: Some("Calm and direct".into()),
            length: Some("concise".into()),
            start_with: Some("Answer the primary question directly.".into()),
            then: Some("Explain the next step.".into()),
            avoid: vec!["long background explanations".into()],
            response_strategy: vec!["State the seat count".into()],
            de_escalation: false,
            escalation_recommendation: None,
            why: vec!["based on observable signals".into()],
            source: "ai".into(),
            confidence: "medium".into(),
        };
        save_recommendation(
            &conn,
            731,
            &stored,
            Some("interaction_recommendation_v1"),
            Some("qwen2.5"),
        )
        .unwrap();
        let card = build_card(&conn, 731, None).unwrap().expect("card");
        let rec = card.recommendation.expect("recommendation");
        assert_eq!(rec.source, "ai");
        assert_eq!(rec.length.as_deref(), Some("concise"));
        assert!(card.provenance.ai_generated);
        assert_eq!(
            card.provenance.prompt_version.as_deref(),
            Some("interaction_recommendation_v1")
        );
        assert_eq!(card.provenance.model.as_deref(), Some("qwen2.5"));
    }

    #[test]
    fn heuristic_recommendation_respects_human_override() {
        let conn = fresh_db();
        seed_customer(&conn, 506, 99007, "Frances Allen");
        seed_conversation(&conn, 732, 99732, 506, "Data export", "active");
        seed_thread(
            &conn,
            732,
            "customer",
            "Please walk me through step by step how to export",
        );
        record_current_interaction(&conn, 732).unwrap();
        set_human_override(
            &conn,
            506,
            "concise",
            Some("detailed"),
            Some("prefers short answers"),
        )
        .unwrap();
        let card = build_card(&conn, 732, None).unwrap().expect("card");
        let rec = card.recommendation.expect("recommendation");
        assert_eq!(rec.length.as_deref(), Some("concise"));
        assert_eq!(rec.source, "ai+human-override");
        // Revert restores AI semantics (spec #22).
        clear_human_override(&conn, 506).unwrap();
        let prefs = get_preferences(&conn, 506).unwrap();
        assert!(!prefs.iter().any(|p| p.origin == "human_entered"));
        let card_after = build_card(&conn, 732, None).unwrap().expect("card");
        assert_ne!(
            card_after.recommendation.unwrap().source,
            "ai+human-override"
        );
    }

    // ---- profile (spec #25, #39, #40) ---------------------------------------

    #[test]
    fn build_profile_preferences_need_repetition() {
        let conn = fresh_db();
        seed_customer(&conn, 507, 99008, "John Backus");
        // One conversation with an explicit step_by_step request — below the
        // 3-distinct-conversation pattern threshold.
        seed_conversation(&conn, 740, 99740, 507, "Setup help", "closed");
        seed_thread(
            &conn,
            740,
            "customer",
            "Please walk me through step by step",
        );
        // Materialize history the way workers.onAfterInitialSync does
        // (MAIN's integration test setup: record + computeOutcome per conv).
        record_current_interaction(&conn, 740).unwrap();
        compute_outcome(&conn, 740).unwrap();
        let profile = build_profile(&conn, 507).unwrap().expect("profile");
        assert!(profile
            .preferences
            .iter()
            .all(|p| p.origin == "human_entered"
                || p.evidence_count
                    >= crate::intelligence::MIN_OBSERVATIONS_FOR_PREFERENCE as i64));
        assert!(profile
            .preferences
            .iter()
            .all(|p| p.preference != "step_by_step"));
        // timeline + outcomes + playbook present on the MAIN shape
        assert!(!profile.timeline.is_empty());
        assert!(profile.outcomes.is_some());
    }

    #[test]
    fn build_profile_unknown_customer_is_none() {
        let conn = fresh_db();
        assert!(build_profile(&conn, 424242).unwrap().is_none());
    }

    // ---- repeat issue (spec #50) ----------------------------------------------

    #[test]
    fn detect_repeat_issue_by_subject_token_overlap() {
        let conn = fresh_db();
        seed_customer(&conn, 508, 99009, "Katherine Johnson");
        for (id, subject) in [
            (750, "Slack integration stopped posting updates"),
            (751, "Slack integration stopped posting again"),
            (752, "Slack integration broken third time"),
        ] {
            seed_conversation(&conn, id, 99_000 + id, 508, subject, "closed");
            seed_thread(&conn, id, "customer", "the slack channel receives nothing");
            record_current_interaction(&conn, id).unwrap();
        }
        seed_conversation(
            &conn,
            753,
            99053,
            508,
            "Slack integration stopped posting today",
            "active",
        );
        seed_thread(&conn, 753, "customer", "again nothing arrives in slack");
        let r = detect_repeat_issue(&conn, 753).unwrap().expect("repeat");
        assert!(
            r.detected,
            "needs >= 2 related: {:?}",
            r.related_conversations.len()
        );
        assert!(r.related_conversations.len() >= 2);
        assert!(r.related_conversations.len() <= 5);
    }

    // ---- observation idempotency (spec #29/#30) -------------------------------

    #[test]
    fn repeated_refreshes_do_not_duplicate_observations() {
        let conn = fresh_db();
        seed_customer(&conn, 509, 99010, "Margaret Hamilton");
        seed_conversation(&conn, 760, 99760, 509, "Question", "active");
        seed_thread(&conn, 760, "customer", "Still not working, very annoying");
        record_current_interaction(&conn, 760).unwrap();
        record_current_interaction(&conn, 760).unwrap();
        record_current_interaction(&conn, 760).unwrap();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM client_behavior_observations WHERE conversation_id = 760",
                [],
                |r| r.get(0),
            )
            .unwrap();
        // One row per (conversation, dimension, source) — never duplicates.
        let distinct: i64 = conn
            .query_row(
                "SELECT COUNT(DISTINCT dimension) FROM client_behavior_observations WHERE conversation_id = 760",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            n, distinct,
            "idempotent per (conversation, dimension, source)"
        );
    }
}
