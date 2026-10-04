//! Local translation service (v2.1.0, plan Phase 30) — reference
//! `src/server/ai/translation.ts`.
//!
//! - Detection is DETERMINISTIC: script ranges first (Chinese/Japanese/
//!   Korean/Arabic/Devanagari/Thai/Greek/Hebrew/Cyrillic), then
//!   stopword-frequency scoring for Latin-script languages. Confidence and
//!   method are reported honestly; 'unknown' is a legitimate result.
//!   Detection never calls a model.
//! - Translation runs ONLY through the local LM Studio endpoint (the
//!   CopilotService raw-options chat pattern). No cloud translation.
//! - Results are cached by sha256(source|target|purpose|text): identical
//!   requests never re-run the model.
//! - The system prompt instructs the model to preserve technical terms,
//!   product names, code, URLs and email addresses verbatim.
//! - Nothing is ever sent automatically: the caller receives the
//!   translation for side-by-side review; sends still go through the human
//!   write path.

use std::collections::HashMap;

use rusqlite::{params, Connection};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::error::Result;

// ─── shared contract (shared/translation.ts) ───────────────────────────────

/// `SUPPORTED_LANGUAGES` — the 18 reference languages, in reference order.
pub const SUPPORTED_LANGUAGES: &[(&str, &str)] = &[
    ("en", "English"),
    ("es", "Spanish"),
    ("fr", "French"),
    ("de", "German"),
    ("it", "Italian"),
    ("pt", "Portuguese"),
    ("nl", "Dutch"),
    ("ru", "Russian"),
    ("zh", "Chinese"),
    ("ja", "Japanese"),
    ("ko", "Korean"),
    ("ar", "Arabic"),
    ("hi", "Hindi"),
    ("th", "Thai"),
    ("vi", "Vietnamese"),
    ("pl", "Polish"),
    ("tr", "Turkish"),
    ("sv", "Swedish"),
];

/// `LANGUAGE_NAMES[code] ?? code`.
pub fn language_name(code: &str) -> String {
    SUPPORTED_LANGUAGES
        .iter()
        .find(|(c, _)| *c == code)
        .map(|(_, name)| name.to_string())
        .unwrap_or_else(|| code.to_string())
}

fn is_supported(code: &str) -> bool {
    SUPPORTED_LANGUAGES.iter().any(|(c, _)| *c == code)
}

/// Ensure the translation cache exists (migration 015 DDL; idempotent —
/// the same boot-time guard pattern as the quality/pipeline services).
pub fn ensure_translation_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS translation_cache (
            cache_key        TEXT PRIMARY KEY,
            source_lang      TEXT NOT NULL,
            target_lang      TEXT NOT NULL,
            purpose          TEXT NOT NULL DEFAULT 'general'
                CHECK (purpose IN ('customer_inbound','agent_draft','general')),
            source_text      TEXT NOT NULL,
            translated_text  TEXT NOT NULL,
            model            TEXT,
            created_at       TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE INDEX IF NOT EXISTS idx_translation_cache_created
            ON translation_cache(created_at DESC);",
    )?;
    Ok(())
}

// ─── deterministic detection (translation.ts:27-142) ───────────────────────

struct ScriptRange {
    code: &'static str,
    ranges: &'static [(u32, u32)],
    confidence: &'static str,
}

/// Script ranges in reference order (Hangul before Han; Kana for Japanese).
static SCRIPT_RANGES: &[ScriptRange] = &[
    // Hangul before Han: Korean text is dominated by Hangul syllables.
    ScriptRange {
        code: "ko",
        ranges: &[(0xac00, 0xd7a3)],
        confidence: "medium",
    },
    // Kana (Hiragana + Katakana) - unique to Japanese.
    ScriptRange {
        code: "ja",
        ranges: &[(0x3040, 0x30ff)],
        confidence: "medium",
    },
    // Han script: shared; reported as Chinese with script-level confidence.
    ScriptRange {
        code: "zh",
        ranges: &[(0x4e00, 0x9fff), (0x3400, 0x4dbf)],
        confidence: "low",
    },
    ScriptRange {
        code: "ru",
        ranges: &[(0x0400, 0x04ff)],
        confidence: "high",
    },
    ScriptRange {
        code: "el",
        ranges: &[(0x0370, 0x03ff)],
        confidence: "high",
    },
    ScriptRange {
        code: "he",
        ranges: &[(0x0590, 0x05ff)],
        confidence: "high",
    },
    ScriptRange {
        code: "ar",
        ranges: &[(0x0600, 0x06ff)],
        confidence: "high",
    },
    ScriptRange {
        code: "hi",
        ranges: &[(0x0900, 0x097f)],
        confidence: "high",
    },
    ScriptRange {
        code: "th",
        ranges: &[(0x0e00, 0x0e7f)],
        confidence: "high",
    },
];

/// Top function words per Latin-script language (deterministic scoring).
static STOPWORDS: &[(&str, &[&str])] = &[
    ("en", &["the", "and", "is", "are", "was", "you", "for", "with", "that", "this", "have", "not", "but", "can", "how", "what", "when", "why", "please", "thank", "we", "our", "your", "it", "my", "do", "does", "did", "has", "had", "will", "would", "could", "should", "from", "about"]),
    ("es", &["el", "la", "los", "las", "de", "que", "y", "en", "un", "una", "por", "con", "para", "no", "se", "lo", "su", "más", "está", "estoy", "hola", "gracias", "cómo", "qué", "cuando", "dónde", "puedo", "necesito", "favor"]),
    ("fr", &["le", "la", "les", "de", "des", "et", "en", "un", "une", "du", "que", "qui", "pour", "avec", "dans", "pas", "est", "je", "vous", "nous", "merci", "bonjour", "comment", "pourquoi", "peux", "avez", "être"]),
    ("de", &["der", "die", "das", "und", "ist", "nicht", "mit", "für", "ein", "eine", "auf", "von", "ich", "sie", "wir", "und", "auch", "als", "wie", "danke", "bitte", "hallo", "können", "haben", "sehr", "warum"]),
    ("it", &["il", "lo", "la", "le", "di", "che", "e", "in", "un", "una", "per", "con", "non", "sono", "ho", "mi", "si", "come", "grazie", "ciao", "perché", "quando", "posso", "molto", "anche", "dove"]),
    ("pt", &["o", "a", "os", "as", "de", "que", "e", "em", "um", "uma", "para", "com", "não", "por", "do", "da", "estou", "você", "obrigado", "olá", "como", "por", "quando", "posso", "muito", "também"]),
    ("nl", &["de", "het", "een", "en", "van", "is", "dat", "niet", "met", "voor", "ik", "wij", "jullie", "heb", "hebben", "kan", "niet", "dank", "hallo", "hoe", "waarom", "want", "ook", "maar", "nog", "wel"]),
    ("pl", &["nie", "jest", "się", "na", "że", "do", "mam", "jak", "ale", "czy", "dziękuję", "cześć", "dlaczego", "kiedy", "można", "bardzo", "proszę", "jeśli", "tego", "dla", "od", "przy", "bez"]),
    ("tr", &["bir", "ve", "bu", "için", "ile", "değil", "mi", "my", "nasıl", "teşekkür", "merhaba", "neden", "ne", "zaman", "olabilir", "çok", "ama", "gerekli", "lütfen", "var", "yok", "olarak"]),
    ("sv", &["och", "att", "det", "en", "som", "är", "för", "med", "inte", "har", "den", "jag", "vi", "ni", "tack", "hej", "hur", "varför", "när", "kan", "mycket", "också", "men", "om"]),
    ("vi", &["của", "và", "là", "có", "không", "được", "cho", "với", "này", "tôi", "bạn", "chúng", "cảm ơn", "xin", "làm", "thế nào", "tại sao", "khi", "có thể", "rất", "nhưng"]),
];

/// `Number(x.toFixed(n))` — round to n decimals, back to a JSON number.
fn round_to(value: f64, decimals: u32) -> f64 {
    let factor = 10f64.powi(decimals as i32);
    (value * factor).round() / factor
}

/// The reference's letter test `[a-z0-9\u00c0-\uffff]/i`.
fn is_letterish(c: char) -> bool {
    c.is_ascii_alphanumeric() || (u32::from(c) >= 0xc0 && u32::from(c) <= 0xffff)
}

/// Strip URLs, emails, code fences and long hex runs so technical noise
/// does not skew script/stopword statistics (translation.ts:66-70).
fn clean_for_detection(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let lower = text.to_lowercase();
    let chars: Vec<char> = text.chars().collect();
    let lower_chars: Vec<char> = lower.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        // URLs: http(s)://... up to whitespace.
        if lower_chars[i] == 'h'
            && lower_chars[i..].starts_with(&['h', 't', 't', 'p'])
        {
            let rest: String = lower_chars[i..].iter().collect();
            if rest.starts_with("http://") || rest.starts_with("https://") {
                // consume until whitespace
                while i < chars.len() && !chars[i].is_whitespace() {
                    i += 1;
                }
                out.push(' ');
                continue;
            }
        }
        // Code fences: ``` ... ```
        if chars[i] == '`' && chars[i..].starts_with(&['`', '`', '`']) {
            if let Some(close) = find_fence_close(&chars, i + 3) {
                i = close + 3;
                out.push(' ');
                continue;
            }
        }
        // Emails: word@word.tld — approximate `[\w.+-]+@[\w-]+\.[\w.]+`.
        if chars[i] == '@' {
            if let Some((start, end)) = email_span(&chars, i) {
                let _ = start;
                i = end;
                out.push(' ');
                continue;
            }
        }
        // Long hex runs: 8+ hex chars bounded by non-word.
        if chars[i].is_ascii_hexdigit() {
            let mut j = i;
            while j < chars.len() && chars[j].is_ascii_hexdigit() {
                j += 1;
            }
            if j - i >= 8 {
                // treat like the reference \b boundary: surrounded by non-word
                let before_ok = i == 0 || !is_word_char(chars[i - 1]);
                let after_ok = j >= chars.len() || !is_word_char(chars[j]);
                if before_ok && after_ok {
                    i = j;
                    out.push(' ');
                    continue;
                }
            }
            out.push(chars[i]);
            i += 1;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn find_fence_close(chars: &[char], from: usize) -> Option<usize> {
    let mut i = from;
    while i + 2 < chars.len() {
        if chars[i] == '`' && chars[i + 1] == '`' && chars[i + 2] == '`' {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Detect `local@domain.tld` spans around an `@` (approximation of the
/// reference regex; technical noise only needs to be REMOVED, not located
/// perfectly).
fn email_span(chars: &[char], at: usize) -> Option<(usize, usize)> {
    // walk back over [\w.+-]
    let mut start = at;
    while start > 0 {
        let c = chars[start - 1];
        if is_word_char(c) || c == '.' || c == '+' || c == '-' {
            start -= 1;
        } else {
            break;
        }
    }
    if start == at {
        return None; // no local part
    }
    // walk forward over [\w-] then require a dot + [\w.]
    let mut i = at + 1;
    while i < chars.len() && (is_word_char(chars[i]) || chars[i] == '-') {
        i += 1;
    }
    if i == at + 1 || i >= chars.len() || chars[i] != '.' {
        return None;
    }
    let dot = i;
    let _ = dot;
    i += 1;
    let mut tld_chars = 0;
    while i < chars.len() && (is_word_char(chars[i]) || chars[i] == '.') {
        i += 1;
        tld_chars += 1;
    }
    if tld_chars == 0 {
        return None;
    }
    Some((start, i))
}

/// `detectLanguage(text)` — deterministic, never calls a model.
pub fn detect_language(text: &str) -> Value {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return json!({
            "text": trimmed,
            "code": Value::Null,
            "name": Value::Null,
            "confidence": "unknown",
            "method": "empty",
            "alternatives": [],
            "note": "No text to analyze."
        });
    }
    let cleaned = clean_for_detection(trimmed);
    let chars: Vec<char> = cleaned.chars().collect();
    let letter_count = chars.iter().filter(|c| is_letterish(**c)).count();
    if letter_count < 3 {
        return json!({
            "text": trimmed,
            "code": Value::Null,
            "name": Value::Null,
            "confidence": "unknown",
            "method": "empty",
            "alternatives": [],
            "note": "Not enough recognizable characters to detect a language."
        });
    }

    // 1) Script ranges (weighted by share of script letters).
    let mut script_scores: Vec<(&str, f64, &str)> = Vec::new();
    for s in SCRIPT_RANGES {
        let mut hits = 0usize;
        for ch in &chars {
            let cp = u32::from(*ch);
            if s.ranges.iter().any(|(lo, hi)| cp >= *lo && cp <= *hi) {
                hits += 1;
            }
        }
        if hits > 0 {
            script_scores.push((s.code, hits as f64 / letter_count as f64, s.confidence));
        }
    }
    script_scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    if let Some(&(code, share, confidence)) = script_scores.first() {
        if share >= 0.15 {
            let is_ambiguous_han = code == "zh";
            let confidence = if is_ambiguous_han {
                "low"
            } else if share >= 0.4 {
                confidence
            } else {
                "medium"
            };
            let alternatives: Vec<Value> = script_scores[1..3.min(script_scores.len())]
                .iter()
                .map(|&(c, s, _)| {
                    json!({ "code": c, "name": language_name(c), "score": round_to(s, 2) })
                })
                .collect();
            let note = if is_ambiguous_han {
                "Han script detected (Chinese/Japanese share it); reported as Chinese with low confidence - confirm before relying on it.".to_string()
            } else {
                format!(
                    "Detected by Unicode script range analysis ({}% of letters).",
                    (share * 100.0).round() as i64
                )
            };
            return json!({
                "text": trimmed,
                "code": code,
                "name": language_name(code),
                "confidence": confidence,
                "method": "script",
                "alternatives": alternatives,
                "note": note
            });
        }
    }

    // 2) Latin-script stopword scoring.
    let words = stopwords_from(&cleaned);
    let mut scored: Vec<(&str, String, f64, usize)> = STOPWORDS
        .iter()
        .map(|&(code, list)| {
            let hits = words
                .iter()
                .filter(|w| list.iter().any(|s| s == w))
                .count();
            let score = if words.is_empty() {
                0.0
            } else {
                hits as f64 / (words.len().max(4)) as f64
            };
            (code, language_name(code), score, hits)
        })
        .collect();
    scored.sort_by(|a, b| {
        b.2.partial_cmp(&a.2)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.3.cmp(&a.3))
    });
    let best = scored.first();
    let second = scored.get(1);
    let Some(&(best_code, ref best_name, best_score, best_hits)) = best else {
        return unknown_stopwords(trimmed);
    };
    if best_hits == 0 {
        return unknown_stopwords(trimmed);
    }
    let separation = best_score - second.map(|s| s.2).unwrap_or(0.0);
    let confidence = if best_score >= 0.08 && separation >= 0.03 {
        "high"
    } else if best_score >= 0.05 {
        "medium"
    } else {
        "low"
    };
    let alternatives: Vec<Value> = scored[1..3.min(scored.len())]
        .iter()
        .map(|&(c, ref name, s, _)| json!({ "code": c, "name": name, "score": round_to(s, 3) }))
        .collect();
    let note = if confidence == "high" {
        format!("Detected by function-word frequency ({best_hits} matching words).")
    } else if confidence == "medium" {
        format!(
            "Detected by function-word frequency with limited separation from {}.",
            second.map(|s| s.1.clone()).unwrap_or_else(|| "other candidates".into())
        )
    } else {
        "Weak function-word signal; treat the language as a low-confidence guess.".to_string()
    };
    json!({
        "text": trimmed,
        "code": best_code,
        "name": best_name,
        "confidence": confidence,
        "method": "stopwords",
        "alternatives": alternatives,
        "note": note
    })
}

fn unknown_stopwords(trimmed: &str) -> Value {
    json!({
        "text": trimmed,
        "code": Value::Null,
        "name": Value::Null,
        "confidence": "unknown",
        "method": "stopwords",
        "alternatives": [],
        "note": "No language-specific function words matched; the language is honestly unknown."
    })
}

/// Tokenize for stopword scoring — keep only reference word characters
/// (`[a-zà-öø-ÿā-ž0-9\s]` after lowercasing), split on runs of space.
fn stopwords_from(cleaned: &str) -> Vec<String> {
    let lowered = cleaned.to_lowercase();
    let mut words = Vec::new();
    let mut current = String::new();
    for c in lowered.chars() {
        let keep = c.is_ascii_lowercase()
            || c.is_ascii_digit()
            || c.is_whitespace()
            || matches!(c, 'à'..='ö' | 'ø'..='ÿ' | '\u{0101}'..='\u{017e}');
        if keep {
            if c.is_whitespace() {
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
            } else {
                current.push(c);
            }
        } else if !current.is_empty() {
            words.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

// ─── service (translation.ts:152-292) ──────────────────────────────────────

/// `detect(texts)` — up to 50 texts.
pub fn detect(texts: &[String]) -> Vec<Value> {
    texts.iter().take(50).map(|t| detect_language(t)).collect()
}

/// Detect the languages of a conversation's customer messages + aggregate.
/// `None` = conversation not found.
pub fn conversation_languages(conn: &Connection, conversation_id: i64) -> Result<Option<Value>> {
    let exists: Option<i64> = conn
        .query_row(
            "SELECT id FROM conversations WHERE id = ?1 AND deleted_at IS NULL",
            params![conversation_id],
            |r| r.get(0),
        )
        .ok();
    if exists.is_none() {
        return Ok(None);
    }
    let Ok(mut stmt) = conn.prepare(
        "SELECT id, html_stripped FROM (
             SELECT id, COALESCE(body_html, body, '') AS html_stripped,
                    COALESCE(remote_created_at, created_at) AS at
             FROM conversation_threads
             WHERE conversation_id = ?1 AND thread_type = 'customer'
               AND deleted_at IS NULL AND state = 'published'
         ) ORDER BY at ASC LIMIT 100",
    ) else {
        return Ok(Some(conversation_summary(conversation_id, Vec::new())));
    };
    let per_message: Vec<(i64, Value)> = stmt
        .query_map(params![conversation_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
            ))
        })
        .map(|rows| {
            rows.filter_map(|r| r.ok())
                .map(|(id, html)| {
                    let text = crate::demo::html_to_text(&html);
                    (id, detect_language(&text))
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(Some(conversation_summary(conversation_id, per_message)))
}

fn conversation_summary(conversation_id: i64, per_message: Vec<(i64, Value)>) -> Value {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for (_, detection) in &per_message {
        if let (Some(code), Some(conf)) = (
            detection.get("code").and_then(|v| v.as_str()),
            detection.get("confidence").and_then(|v| v.as_str()),
        ) {
            if conf != "unknown" {
                *counts.entry(code.to_string()).or_default() += 1;
            }
        }
    }
    let mut ranked: Vec<(String, usize)> = counts.into_iter().collect();
    ranked.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    let primary = ranked.first();
    let primary_detection: Option<&Value> = per_message
        .iter()
        .find(|(_, d)| {
            d.get("code").and_then(|v| v.as_str()) == primary.map(|(c, _)| c.as_str())
        })
        .map(|(_, d)| d);
    let mut notes = vec![
        "Detection is deterministic (script ranges + function words) and runs entirely locally.".to_string(),
    ];
    if primary.is_none() {
        notes.push("No customer message had a detectable language.".to_string());
    }
    if !per_message.is_empty() && ranked.len() > 1 {
        let listing = ranked
            .iter()
            .map(|(c, n)| format!("{} x{}", language_name(c), n))
            .collect::<Vec<_>>()
            .join(", ");
        notes.push(format!(
            "Multiple languages detected ({listing}); the primary is the most frequent."
        ));
    }
    let (p_code, p_name, p_conf, p_method) = if let Some((code, _)) = primary {
        let detection = primary_detection;
        (
            json!(code),
            json!(language_name(code)),
            detection
                .and_then(|d| d.get("confidence"))
                .cloned()
                .unwrap_or(json!("unknown")),
            detection
                .and_then(|d| d.get("method"))
                .cloned()
                .unwrap_or(json!("empty")),
        )
    } else {
        (
            Value::Null,
            Value::Null,
            json!("unknown"),
            json!("empty"),
        )
    };
    json!({
        "conversation_id": conversation_id,
        "customer_messages": per_message.len(),
        "analyzed_messages": per_message.len(),
        "primary_language": {
            "code": p_code,
            "name": p_name,
            "confidence": p_conf,
            "method": p_method
        },
        "per_message": per_message
            .iter()
            .map(|(id, d)| json!({ "thread_id": id, "detection": d }))
            .collect::<Vec<_>>(),
        "notes": notes
    })
}

// ─── translate ─────────────────────────────────────────────────────────────

/// Translation failures, pre-classified for the route's status mapping
/// (routes/translation.ts:78-90).
#[derive(Debug)]
pub enum TranslateError {
    /// Client-shaped problems the route reports as 422 ValidationError.
    Client(String),
    /// AI disabled / LM Studio down — the route reports an honest 503.
    Service(String),
    /// Everything else — the route reports an honest 502.
    BadGateway(String),
}

/// `PURPOSE_PROMPTS` (translation.ts:146-150).
fn purpose_prompt(purpose: &str) -> &'static str {
    match purpose {
        "customer_inbound" => {
            "Translate the CUSTOMER support message into the target language for an agent to review."
        }
        "agent_draft" => "Translate the AGENT draft reply into the target language the customer asked to be served in.",
        _ => "Translate the text into the target language.",
    }
}

/// The raw-options LM Studio chat the translation uses (the reference's
/// injectable `TranslateChatFn`).
pub type ChatFn<'a> = &'a dyn Fn(
    Vec<crate::ai_provider::ChatMessage>,
    f64,
    u32,
) -> std::pin::Pin<
    Box<dyn std::future::Future<Output = std::result::Result<(Option<String>, String), String>> + 'a>,
>;

/// `translate(input)` — LM Studio only, cached, never sent anywhere.
/// `chat` yields `(content, model)`.
pub async fn translate(
    conn: &Connection,
    chat: ChatFn<'_>,
    text: &str,
    from: Option<&str>,
    to: &str,
    purpose: Option<&str>,
) -> std::result::Result<Value, TranslateError> {
    let text = text.trim();
    let purpose = purpose.unwrap_or("general");
    if !is_supported(to) {
        return Err(TranslateError::Client(format!(
            "Unsupported target language '{to}'. Supported: {}.",
            SUPPORTED_LANGUAGES
                .iter()
                .map(|(c, _)| *c)
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    if text.is_empty() {
        return Err(TranslateError::BadGateway(
            "Nothing to translate: empty text.".into(),
        ));
    }
    if text.chars().count() > 8000 {
        return Err(TranslateError::BadGateway(
            "Text too long for a single local translation (8000 char cap).".into(),
        ));
    }

    // Deterministic detection when 'from' is omitted or 'auto'.
    let mut detected: Option<Value> = None;
    let mut source_lang = from.unwrap_or("").trim().to_lowercase();
    if source_lang.is_empty() || source_lang == "auto" {
        let detection = detect_language(text);
        let code = detection.get("code").and_then(|v| v.as_str()).map(str::to_string);
        source_lang = code.clone().unwrap_or_else(|| "en".into());
        if code.is_none() {
            // Honest: we cannot detect - refuse rather than guess a source.
            return Err(TranslateError::Client(
                "Source language could not be detected; please specify it explicitly.".into(),
            ));
        }
        detected = Some(detection);
    }
    if source_lang == to {
        return Err(TranslateError::Client(format!(
            "Source and target language are both '{to}' - nothing to translate."
        )));
    }

    // Cache key: sha256(source|target|purpose|text).
    let mut hasher = Sha256::new();
    hasher.update(format!("{source_lang}|{to}|{purpose}|{text}"));
    let cache_key = format!("{:x}", hasher.finalize());
    let cached: Option<(String, String, String, Option<String>, String)> = conn
        .query_row(
            "SELECT source_lang, target_lang, translated_text, model, purpose
             FROM translation_cache WHERE cache_key = ?1",
            params![cache_key],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                ))
            },
        )
        .ok();
    if let Some((src, tgt, translated, model, purpose)) = cached {
        return Ok(json!({
            "source_lang": src,
            "source_lang_name": language_name(&src),
            "target_lang": tgt,
            "target_lang_name": language_name(&tgt),
            "purpose": purpose,
            "source_text": text,
            "translated_text": translated,
            "model": model,
            "cached": true,
            "detected": detected,
            "note": "Served from the local translation cache - the model was not re-run. Review original and translation side by side; nothing is sent automatically."
        }));
    }

    let system = [
        format!("You are a precise translation engine. {}", purpose_prompt(purpose)),
        format!("Translate from {} to {}.", language_name(&source_lang), language_name(to)),
        "Rules:".to_string(),
        "- Preserve technical terms, product names, feature names, error messages, code, URLs and email addresses VERBATIM (do not translate them).".to_string(),
        "- Keep the original meaning and tone; do not add, remove or answer any content.".to_string(),
        "- Keep line breaks and list structure.".to_string(),
        "- Output ONLY the translated text, no explanations, no quotes.".to_string(),
    ]
    .join("\n");
    let messages = vec![
        crate::ai_provider::ChatMessage {
            role: "system".into(),
            content: system,
        },
        crate::ai_provider::ChatMessage {
            role: "user".into(),
            content: text.to_string(),
        },
    ];
    let (content, model) = chat(messages, 0.1, 2048)
        .await
        .map_err(TranslateError::Service)?;
    let translated = content.unwrap_or_default().trim().to_string();
    if translated.is_empty() {
        return Err(TranslateError::BadGateway(
            "The local model returned an empty translation.".into(),
        ));
    }

    conn.execute(
        "INSERT INTO translation_cache (cache_key, source_lang, target_lang, purpose, source_text, translated_text, model, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, datetime('now'))
         ON CONFLICT (cache_key) DO UPDATE SET translated_text = excluded.translated_text, model = excluded.model",
        params![cache_key, source_lang, to, purpose, text, translated, model],
    )
    .map_err(|e| TranslateError::BadGateway(e.to_string()))?;

    Ok(json!({
        "source_lang": source_lang,
        "source_lang_name": language_name(&source_lang),
        "target_lang": to,
        "target_lang_name": language_name(to),
        "purpose": purpose,
        "source_text": text,
        "translated_text": translated,
        "model": model,
        "cached": false,
        "detected": detected,
        "note": "Translated by the locally configured model (LM Studio) - no cloud service. Review original and translation side by side; nothing is sent automatically."
    }))
}

/// Agent's preferred drafting language (settings key, default 'en').
/// The reference JSON-parses the stored value; the port's settings writer
/// stores raw strings, so both shapes are accepted.
pub fn agent_language(conn: &Connection) -> Value {
    let raw = crate::settings::get_string(conn, "agent_language")
        .ok()
        .flatten()
        .unwrap_or_default();
    let mut code = "en".to_string();
    if !raw.is_empty() {
        let parsed = serde_json::from_str::<Value>(&raw)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_else(|| raw.clone());
        if is_supported(&parsed) {
            code = parsed;
        }
    }
    json!({ "code": code, "name": language_name(&code) })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_db() -> Connection {
        let f = tempfile::NamedTempFile::new()
            .unwrap()
            .into_temp_path()
            .keep()
            .unwrap();
        let mut conn = crate::db::open(&f).unwrap();
        crate::db::ensure_migrations_table(&conn).unwrap();
        crate::migrations::run_all(&mut conn).unwrap();
        conn
    }

    #[test]
    fn detects_scripts_before_stopwords() {
        let d = detect_language("이 계정은 어제부터 로그인이 안 됩니다");
        assert_eq!(d["code"], json!("ko"));
        assert_eq!(d["method"], json!("script"));

        let d = detect_language("こんにちは、ログインできないのですが");
        assert_eq!(d["code"], json!("ja"));

        // Han is ambiguous → low confidence + honest note.
        let d = detect_language("我的账户无法登录");
        assert_eq!(d["code"], json!("zh"));
        assert_eq!(d["confidence"], json!("low"));
        assert!(d["note"].as_str().unwrap().contains("Han script"));

        let d = detect_language("Вход в аккаунт невозможен");
        assert_eq!(d["code"], json!("ru"));
        assert_eq!(d["confidence"], json!("high"));

        // Latin-script stopword scoring.
        let d = detect_language("Hola, no puedo entrar en mi cuenta de usuario desde ayer");
        assert_eq!(d["code"], json!("es"));
        assert_eq!(d["method"], json!("stopwords"));

        let d = detect_language("Hello, my account is broken and I cannot log in");
        assert_eq!(d["code"], json!("en"));
    }

    #[test]
    fn detection_is_honest_about_unknowns() {
        let d = detect_language("");
        assert_eq!(d["confidence"], json!("unknown"));
        assert_eq!(d["method"], json!("empty"));

        // Technical noise only → no detectable language.
        let d = detect_language("zzz qqxxx vvv");
        assert_eq!(d["code"], Value::Null);
        assert_eq!(d["method"], json!("stopwords"));
        assert!(d["note"].as_str().unwrap().contains("honestly unknown"));

        // URLs and emails are stripped before scoring.
        let d = detect_language("Contact https://example.com or mail support@example.com");
        let words_only = detect_language("Contact or mail");
        assert_eq!(d["code"], words_only["code"]);
    }

    #[test]
    fn detect_caps_at_50_texts() {
        let texts: Vec<String> = (0..80).map(|i| format!("hello world {i}")).collect();
        assert_eq!(detect(&texts).len(), 50);
    }

    #[tokio::test]
    #[allow(clippy::let_and_return)] // the block scopes the closure borrow
    async fn translates_caches_and_refuses_like_the_reference() {
        let conn = fresh_db();
        ensure_translation_schema(&conn).unwrap();
        let chat = |_messages: Vec<crate::ai_provider::ChatMessage>,
                    _temperature: f64,
                    _max_tokens: u32| {
            Box::pin(async {
                Ok::<(Option<String>, String), String>((
                    Some("Bonjour world".to_string()),
                    "fake-translate".to_string(),
                ))
            }) as std::pin::Pin<
                Box<
                    dyn std::future::Future<
                            Output = std::result::Result<(Option<String>, String), String>,
                        > + '_,
                >,
            >
        };
        let r = translate(
            &conn,
            &chat,
            "Hello, my account is broken",
            Some("en"),
            "fr",
            Some("customer_inbound"),
        )
        .await
        .unwrap();
        assert_eq!(r["translated_text"], json!("Bonjour world"));
        assert_eq!(r["cached"], json!(false));
        assert_eq!(r["model"], json!("fake-translate"));
        assert_eq!(r["purpose"], json!("customer_inbound"));

        // Second identical call is served from the cache.
        let r2 = translate(
            &conn,
            &chat,
            "Hello, my account is broken",
            Some("en"),
            "fr",
            Some("customer_inbound"),
        )
        .await
        .unwrap();
        assert_eq!(r2["cached"], json!(true));
        assert_eq!(r2["translated_text"], json!("Bonjour world"));

        // Unknown target language is a client error.
        let err = translate(&conn, &chat, "x", Some("en"), "xx", None)
            .await
            .unwrap_err();
        assert!(
            matches!(err, TranslateError::Client(ref m) if m.contains("Unsupported target language")),
            "{err:?}"
        );

        // Same source and target is a client error.
        let err = translate(&conn, &chat, "Hello", Some("en"), "en", None)
            .await
            .unwrap_err();
        assert!(
            matches!(err, TranslateError::Client(ref m) if m.contains("nothing to translate")),
            "{err:?}"
        );

        // Undetectable source without explicit `from` is a client error.
        let err = translate(&conn, &chat, "zzz qqxxx vvv", None, "en", None)
            .await
            .unwrap_err();
        assert!(
            matches!(err, TranslateError::Client(ref m) if m.contains("could not be detected")),
            "{err:?}"
        );

        // Deterministic detection picks es and translates.
        let r = translate(
            &conn,
            // Same shape as the route adapter: an inline closure whose
            // boxed future borrows nothing.
            &{
                let chat = |_messages: Vec<crate::ai_provider::ChatMessage>,
                            _temperature: f64,
                            _max_tokens: u32| {
                    Box::pin(async {
                        Ok::<(Option<String>, String), String>((
                            Some("Hello world".to_string()),
                            "fake-translate".to_string(),
                        ))
                    }) as std::pin::Pin<
                        Box<
                            dyn std::future::Future<
                                    Output = std::result::Result<
                                        (Option<String>, String),
                                        String,
                                    >,
                                > + '_,
                        >,
                    >
                };
                chat
            },
            "Hola, no puedo entrar en mi cuenta de usuario desde ayer",
            None,
            "en",
            None,
        )
        .await
        .unwrap();
        assert_eq!(r["source_lang"], json!("es"));
        assert!(r["detected"]["code"] == json!("es"));
    }

    #[test]
    fn agent_language_defaults_and_reads_both_storage_shapes() {
        let conn = fresh_db();
        assert_eq!(agent_language(&conn), json!({"code": "en", "name": "English"}));

        crate::settings::set_string(&conn, "agent_language", "fr").unwrap();
        assert_eq!(
            agent_language(&conn),
            json!({"code": "fr", "name": "French"})
        );

        // Reference JSON shape ('"de"') is accepted too.
        crate::settings::set_string(&conn, "agent_language", "\"de\"").unwrap();
        assert_eq!(
            agent_language(&conn),
            json!({"code": "de", "name": "German"})
        );

        // Unsupported codes fall back to English.
        crate::settings::set_string(&conn, "agent_language", "zz").unwrap();
        assert_eq!(
            agent_language(&conn),
            json!({"code": "en", "name": "English"})
        );
    }
}
