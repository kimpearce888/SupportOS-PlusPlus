//! Mentions — text scan + emit Mentioned/TeamMentioned notification (M4-T08).
//!
//! Per spec M4: "Team operations: Operations Center, workload and capacity,
//! Notification Center, mentions, side threads, automation."
//!
//! ## What this module does
//!
//! `scan_for_mentions(body)` parses a message body for two patterns:
//! - `@username` → emits a `NotificationType::Mentioned` notification
//!   targeted at the mentioned user.
//! - `@team:teamname` → emits a `NotificationType::TeamMentioned` notification
//!   targeted at the team's members.
//!
//! ## Safety
//!
//! The `regex` crate uses a bounded NFA (no backtracking) — there is no
//! ReDoS surface. The patterns are static (compiled once via `LazyLock`),
//! and the message body is bounded by the caller (Help Scout message bodies
//! are capped at 64KB; we apply our own 100KB cap as a defensive bound).
//!
//! ## Fail-safe
//!
//! Mentions of nonexistent users or teams produce no notification (fail-safe).
//! The caller resolves mention strings to user/team IDs via the existing
//! `users` and `teams` SQLite tables (the `mention` column on `users` and
//! the `name` column on `teams` are the lookup keys).

use std::sync::OnceLock;

use regex::Regex;
use rusqlite::{params, Connection};

use crate::catalog::NotificationType;
use crate::error::Result;
use crate::notifications::record_notification;

/// The maximum message body size we'll scan, in bytes. Help Scout message
/// bodies are capped at 64KB; we apply 100KB as a defensive bound so a
/// malicious or buggy caller can't cause unbounded scanning.
pub const MAX_BODY_BYTES: usize = 100_000;

/// A parsed mention — either a user mention (`@username`) or a team
/// mention (`@team:teamname`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mention {
    /// A user mention. `username` is the text after `@` (without the `@`).
    User {
        /// The mentioned username (case-sensitive lookup against `users.mention`).
        username: String,
    },
    /// A team mention. `teamname` is the text after `@team:` (without the prefix).
    Team {
        /// The mentioned team name (case-insensitive lookup against `teams.name`).
        teamname: String,
    },
}

impl Mention {
    /// The display string for the mention (e.g. `@alice` or `@team:engineering`).
    #[must_use]
    pub fn display(&self) -> String {
        match self {
            Self::User { username } => format!("@{username}"),
            Self::Team { teamname } => format!("@team:{teamname}"),
        }
    }
}

/// The user-mention regex. Matches `@` followed by 1–64 word characters
/// (alphanumeric + underscore). The preceding-character check (to skip
/// `email@example.com`-style matches) is done in code after the match —
/// the `regex` crate doesn't support look-behind, so we filter post-match.
///
/// Per the regex crate's bounded NFA: no backtracking, no ReDoS surface.
fn user_mention_regex() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"@(\w{1,64})").expect("static regex compiles"))
}

/// The team-mention regex. Matches `@team:name` where `name` is 1–64
/// word characters or hyphens (team names can contain hyphens).
fn team_mention_regex() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"@team:([\w-]{1,64})").expect("static regex compiles"))
}

/// Returns `true` if the character at `body[pos - 1]` is a word character
/// (alphanumeric or underscore). Returns `false` if `pos == 0` (start of
/// string). Used to filter out email-style `@` matches.
fn is_preceded_by_word_char(body: &str, pos: usize) -> bool {
    if pos == 0 {
        return false;
    }
    // Walk back to find the start of the previous UTF-8 character.
    let bytes = body.as_bytes();
    let mut i = pos - 1;
    // Continuation bytes start with 0b10xxxxxx; walk back to the lead byte.
    while i > 0 && (bytes[i] & 0xC0) == 0x80 {
        i -= 1;
    }
    let prev = body[i..pos].chars().next();
    match prev {
        Some(c) => c.is_alphanumeric() || c == '_',
        None => false,
    }
}

/// Scan a message body for mentions. Returns the parsed mentions in order
/// of appearance. Deduplicates: each unique mention string appears only once.
///
/// # Errors
///
/// Returns `Error::Other` if `body.len()` exceeds `MAX_BODY_BYTES`.
pub fn scan_for_mentions(body: &str) -> Result<Vec<Mention>> {
    if body.len() > MAX_BODY_BYTES {
        return Err(crate::error::Error::Other(
            format!(
                "mention scan refused: body {} bytes exceeds max {} bytes",
                body.len(),
                MAX_BODY_BYTES
            )
            .into(),
        ));
    }

    // Fast path: if there's no `@` in the body, no scan is needed.
    if !body.contains('@') {
        return Ok(Vec::new());
    }

    let mut mentions: Vec<Mention> = Vec::new();
    let mut seen_usernames: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut seen_teamnames: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Team mentions first (they're more specific — `@team:foo` shouldn't also
    // match the user-mention regex on `@team`).
    for cap in team_mention_regex().captures_iter(body) {
        if let Some(m) = cap.get(0) {
            // Filter out email-style matches: if the char before `@` is a
            // word character, this is `email@example.com`, not a mention.
            if is_preceded_by_word_char(body, m.start()) {
                continue;
            }
            if let Some(name_match) = cap.get(1) {
                let teamname = name_match.as_str().to_string();
                if seen_teamnames.insert(teamname.clone()) {
                    mentions.push(Mention::Team { teamname });
                }
            }
        }
    }

    // User mentions — but skip any whose username is `team` (already captured
    // by the team regex above).
    for cap in user_mention_regex().captures_iter(body) {
        if let Some(m) = cap.get(0) {
            if is_preceded_by_word_char(body, m.start()) {
                continue;
            }
            if let Some(name_match) = cap.get(1) {
                let username = name_match.as_str().to_string();
                if username == "team" {
                    continue;
                }
                if seen_usernames.insert(username.clone()) {
                    mentions.push(Mention::User { username });
                }
            }
        }
    }

    Ok(mentions)
}

/// The result of emitting notifications for a scanned message body.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MentionEmissionResult {
    /// The number of `Mentioned` notifications emitted (may be less than the
    /// number of user mentions if some usernames didn't resolve to a real user).
    pub user_notifications_emitted: u32,
    /// The number of `TeamMentioned` notifications emitted (one per team member,
    /// so a 5-member team mention emits 5 notifications).
    pub team_notifications_emitted: u32,
    /// The number of mentions that didn't resolve to a real user/team
    /// (fail-safe: no notification emitted).
    pub unresolved_mentions: u32,
}

/// Emit notifications for all mentions in `body`. The `conversation_id` and
/// `author_user_id` are recorded in the notification payload for context.
///
/// Per spec: AI is advisory. Mentions are NOT AI — they're a real human
/// action, so they always emit if the mention resolves to a real user/team.
///
/// # Errors
///
/// Returns `Error::Sqlite` if any query/insert fails, or `Error::Other` if
/// the body exceeds `MAX_BODY_BYTES`.
pub fn emit_mention_notifications(
    conn: &Connection,
    body: &str,
    conversation_id: i64,
    author_user_id: Option<i64>,
) -> Result<MentionEmissionResult> {
    let mentions = scan_for_mentions(body)?;
    let mut result = MentionEmissionResult::default();

    for mention in &mentions {
        match mention {
            Mention::User { username } => {
                // Resolve the username to a user via `users.mention` (case-sensitive).
                let target_user_id: Option<i64> = conn
                    .query_row(
                        "SELECT remote_id FROM users WHERE mention = ?1",
                        params![username],
                        |r| r.get(0),
                    )
                    .ok();
                match target_user_id {
                    Some(uid) => {
                        // Don't notify the author about their own mention.
                        if Some(uid) == author_user_id {
                            continue;
                        }
                        let payload = serde_json::json!({
                            "mention": mention.display(),
                            "username": username,
                            "conversation_id": conversation_id,
                            "author_user_id": author_user_id,
                        })
                        .to_string();
                        // Reference dedup key: `n:mention:{scope}:{user}` —
                        // the same mention never notifies twice.
                        let dedup =
                            format!("n:mention:{conversation_id}:{}:{uid}", mention.display());
                        let created = record_notification(
                            conn,
                            &NotificationType::Mentioned,
                            Some(uid),
                            Some(conversation_id),
                            Some(&payload),
                            &dedup,
                        )?;
                        if created.is_some() {
                            result.user_notifications_emitted += 1;
                        }
                    }
                    None => {
                        result.unresolved_mentions += 1;
                    }
                }
            }
            Mention::Team { teamname } => {
                // Resolve the team name to a team via `teams.name` (case-insensitive).
                let team_remote_id: Option<i64> = conn
                    .query_row(
                        "SELECT remote_id FROM teams WHERE LOWER(name) = LOWER(?1)",
                        params![teamname],
                        |r| r.get(0),
                    )
                    .ok();
                match team_remote_id {
                    Some(tid) => {
                        // Per spec, team mentions notify all team members.
                        // The teams table doesn't persist membership — that
                        // comes from Help Scout sync as
                        // HsTeam.member_user_ids. For M4-T08 we emit a single
                        // broadcast notification (target_user_id = NULL) tagged
                        // with the team_remote_id in the payload; M4-T10 (the
                        // Tauri shell wiring) will fan it out to actual members
                        // once team membership is loaded.
                        let payload = serde_json::json!({
                            "mention": mention.display(),
                            "teamname": teamname,
                            "team_remote_id": tid,
                            "conversation_id": conversation_id,
                            "author_user_id": author_user_id,
                        })
                        .to_string();
                        let dedup = format!(
                            "n:mention:{conversation_id}:{}:team:{tid}",
                            mention.display()
                        );
                        let created = record_notification(
                            conn,
                            &NotificationType::TeamMentioned,
                            None, // broadcast — M4-T10 wires per-member fanout.
                            Some(conversation_id),
                            Some(&payload),
                            &dedup,
                        )?;
                        if created.is_some() {
                            result.team_notifications_emitted += 1;
                        }
                    }
                    None => {
                        result.unresolved_mentions += 1;
                    }
                }
            }
        }
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notifications::count_unread_for_user;
    use rusqlite::params;
    use tempfile::NamedTempFile;

    fn fresh_db() -> Connection {
        let f = NamedTempFile::new()
            .unwrap()
            .into_temp_path()
            .keep()
            .unwrap();
        let mut conn = crate::db::open(&f).unwrap();
        // The canonical boot chain — mention emission writes notifications,
        // which only have their dedup_key column + unique index under the
        // full schema.
        crate::bootstrap::apply_all(&mut conn).unwrap();
        conn
    }

    fn insert_user(conn: &Connection, remote_id: i64, mention: &str) {
        conn.execute(
            "INSERT INTO users (remote_id, first_name, last_name, mention, user_type)
             VALUES (?1, 'Test', 'User', ?2, 'user')",
            params![remote_id, mention],
        )
        .unwrap();
    }

    fn insert_team(conn: &Connection, remote_id: i64, name: &str) {
        conn.execute(
            "INSERT INTO teams (remote_id, name) VALUES (?1, ?2)",
            params![remote_id, name],
        )
        .unwrap();
    }

    fn insert_conversation(conn: &Connection, remote_id: i64) {
        conn.execute(
            "INSERT INTO conversations (remote_id, number, status, mailbox_id, customer_id)
             VALUES (?1, ?1, 'active', 101, 2001)",
            params![remote_id],
        )
        .unwrap();
    }

    // ---- scan_for_mentions --------------------------------------------------

    #[test]
    fn scan_returns_empty_when_no_at_sign() {
        let mentions = scan_for_mentions("Hello world, no mentions here.").unwrap();
        assert!(mentions.is_empty());
    }

    #[test]
    fn scan_returns_empty_when_only_at_sign() {
        // A bare @ with no username doesn't match (regex requires \w{1,64}).
        let mentions = scan_for_mentions("Email me @").unwrap();
        assert!(mentions.is_empty());
    }

    #[test]
    fn scan_finds_single_user_mention() {
        let mentions = scan_for_mentions("Hey @alice, can you look at this?").unwrap();
        assert_eq!(mentions.len(), 1);
        assert_eq!(
            mentions[0],
            Mention::User {
                username: "alice".into()
            }
        );
    }

    #[test]
    fn scan_finds_multiple_user_mentions() {
        let mentions = scan_for_mentions("@alice and @bob, please review. @charlie too.").unwrap();
        assert_eq!(mentions.len(), 3);
        let usernames: Vec<&str> = mentions
            .iter()
            .map(|m| match m {
                Mention::User { username } => username.as_str(),
                _ => "",
            })
            .collect();
        assert!(usernames.contains(&"alice"));
        assert!(usernames.contains(&"bob"));
        assert!(usernames.contains(&"charlie"));
    }

    #[test]
    fn scan_finds_team_mention() {
        let mentions = scan_for_mentions("Hey @team:engineering, this needs review.").unwrap();
        assert_eq!(mentions.len(), 1);
        assert_eq!(
            mentions[0],
            Mention::Team {
                teamname: "engineering".into()
            }
        );
    }

    #[test]
    fn scan_finds_both_user_and_team_mentions() {
        let mentions =
            scan_for_mentions("@alice please ask @team:engineering about this.").unwrap();
        assert_eq!(mentions.len(), 2);
        // The team mention comes first (we scan team regex first).
        assert!(mentions
            .iter()
            .any(|m| matches!(m, Mention::Team { teamname } if teamname == "engineering")));
        assert!(mentions
            .iter()
            .any(|m| matches!(m, Mention::User { username } if username == "alice")));
    }

    #[test]
    fn scan_does_not_match_at_in_email() {
        // The negative lookbehind `(?<!\w)` prevents matching `@` in emails.
        let mentions = scan_for_mentions("Contact me at alice@example.com").unwrap();
        // The regex shouldn't match `@example` because `@` is preceded by `e`.
        assert!(
            mentions.is_empty(),
            "email-style @ should not match: {mentions:?}"
        );
    }

    #[test]
    fn scan_deduplicates_repeated_mentions() {
        let mentions = scan_for_mentions("@alice @alice @alice").unwrap();
        assert_eq!(mentions.len(), 1, "duplicate mentions are deduplicated");
    }

    #[test]
    fn scan_deduplicates_user_and_team_separately() {
        let mentions =
            scan_for_mentions("@alice @team:engineering @alice @team:engineering").unwrap();
        assert_eq!(mentions.len(), 2, "1 user + 1 team after dedup");
    }

    #[test]
    fn scan_skips_team_as_username() {
        // `@team:engineering` should ONLY be a team mention, not also a
        // user mention of `team`.
        let mentions = scan_for_mentions("@team:engineering").unwrap();
        assert_eq!(mentions.len(), 1);
        assert!(matches!(mentions[0], Mention::Team { .. }));
    }

    #[test]
    fn scan_truncates_long_usernames_at_64_chars() {
        // 70 chars — regex only matches the first 64.
        let long_name = "a".repeat(70);
        let body = format!("@{long_name}");
        let mentions = scan_for_mentions(&body).unwrap();
        assert_eq!(mentions.len(), 1);
        if let Mention::User { username } = &mentions[0] {
            assert_eq!(username.len(), 64, "username truncated to 64 chars");
        } else {
            panic!("expected User mention");
        }
    }

    #[test]
    fn scan_rejects_body_exceeding_max_bytes() {
        let big = "a".repeat(MAX_BODY_BYTES + 1);
        let result = scan_for_mentions(&big);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("exceeds max"));
    }

    #[test]
    fn scan_handles_unicode_in_body() {
        // Body contains unicode; mentions are still ASCII-word-matched.
        let mentions = scan_for_mentions("Здравей @alice, как си?").unwrap();
        assert_eq!(mentions.len(), 1);
        assert_eq!(
            mentions[0],
            Mention::User {
                username: "alice".into()
            }
        );
    }

    #[test]
    fn scan_handles_mention_at_start_and_end_of_body() {
        let mentions = scan_for_mentions("@alice").unwrap();
        assert_eq!(mentions.len(), 1);
        let mentions = scan_for_mentions("Hello @alice").unwrap();
        assert_eq!(mentions.len(), 1);
    }

    #[test]
    fn scan_mention_display_round_trips() {
        let m = Mention::User {
            username: "alice".into(),
        };
        assert_eq!(m.display(), "@alice");
        let m = Mention::Team {
            teamname: "engineering".into(),
        };
        assert_eq!(m.display(), "@team:engineering");
    }

    // ---- emit_mention_notifications -----------------------------------------

    #[test]
    fn emit_user_mention_creates_notification_for_resolved_user() {
        let conn = fresh_db();
        insert_user(&conn, 42, "alice");
        insert_conversation(&conn, 1001);

        let result =
            emit_mention_notifications(&conn, "Hey @alice, please review.", 1001, Some(99))
                .unwrap();
        assert_eq!(result.user_notifications_emitted, 1);
        assert_eq!(result.team_notifications_emitted, 0);
        assert_eq!(result.unresolved_mentions, 0);

        // The Mentioned notification should be visible to user 42.
        let count = count_unread_for_user(&conn, 42).unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn emit_user_mention_for_nonexistent_user_is_fail_safe() {
        let conn = fresh_db();
        insert_conversation(&conn, 1001);

        let result =
            emit_mention_notifications(&conn, "Hey @nobody, please review.", 1001, None).unwrap();
        assert_eq!(result.user_notifications_emitted, 0);
        assert_eq!(result.unresolved_mentions, 1);

        // No notification recorded for any user.
        let count = count_unread_for_user(&conn, 42).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn emit_team_mention_creates_broadcast_notification() {
        let conn = fresh_db();
        insert_team(&conn, 7, "engineering");
        insert_conversation(&conn, 1001);

        let result = emit_mention_notifications(
            &conn,
            "Hey @team:engineering, please review.",
            1001,
            Some(99),
        )
        .unwrap();
        assert_eq!(result.team_notifications_emitted, 1);
        assert_eq!(result.user_notifications_emitted, 0);

        // TeamMentioned is broadcast (target_user_id = NULL) — visible to all users.
        let count = count_unread_for_user(&conn, 42).unwrap();
        assert_eq!(count, 1, "broadcast notification visible to user 42");
        let count = count_unread_for_user(&conn, 99).unwrap();
        assert_eq!(count, 1, "broadcast notification visible to user 99 too");
    }

    #[test]
    fn emit_team_mention_for_nonexistent_team_is_fail_safe() {
        let conn = fresh_db();
        insert_conversation(&conn, 1001);

        let result =
            emit_mention_notifications(&conn, "Hey @team:nonexistent, please review.", 1001, None)
                .unwrap();
        assert_eq!(result.team_notifications_emitted, 0);
        assert_eq!(result.unresolved_mentions, 1);
    }

    #[test]
    fn emit_does_not_notify_author_about_their_own_mention() {
        let conn = fresh_db();
        insert_user(&conn, 42, "alice");
        insert_conversation(&conn, 1001);

        // Alice mentions herself — no self-notification.
        let result = emit_mention_notifications(&conn, "Hey @alice", 1001, Some(42)).unwrap();
        assert_eq!(
            result.user_notifications_emitted, 0,
            "self-mention is suppressed"
        );
    }

    #[test]
    fn emit_team_mention_match_is_case_insensitive() {
        let conn = fresh_db();
        insert_team(&conn, 7, "Engineering"); // capital E
        insert_conversation(&conn, 1001);

        let result = emit_mention_notifications(
            &conn,
            "Hey @team:engineering", // lowercase
            1001,
            None,
        )
        .unwrap();
        assert_eq!(
            result.team_notifications_emitted, 1,
            "team name match is case-insensitive"
        );
    }

    #[test]
    fn emit_multiple_user_mentions_emit_one_per_resolved_user() {
        let conn = fresh_db();
        insert_user(&conn, 42, "alice");
        insert_user(&conn, 43, "bob");
        insert_conversation(&conn, 1001);

        let result =
            emit_mention_notifications(&conn, "@alice and @bob, please review.", 1001, None)
                .unwrap();
        assert_eq!(result.user_notifications_emitted, 2);
        assert_eq!(count_unread_for_user(&conn, 42).unwrap(), 1);
        assert_eq!(count_unread_for_user(&conn, 43).unwrap(), 1);
    }

    #[test]
    fn emit_mixed_user_team_and_unresolved_mentions() {
        let conn = fresh_db();
        insert_user(&conn, 42, "alice");
        insert_team(&conn, 7, "engineering");
        insert_conversation(&conn, 1001);

        let result = emit_mention_notifications(
            &conn,
            "@alice please ask @team:engineering about @nobody",
            1001,
            None,
        )
        .unwrap();
        assert_eq!(result.user_notifications_emitted, 1, "alice resolves");
        assert_eq!(result.team_notifications_emitted, 1, "engineering resolves");
        assert_eq!(result.unresolved_mentions, 1, "nobody doesn't resolve");
    }

    // ---- Performance guard --------------------------------------------------

    #[test]
    fn scan_1000_messages_completes_under_max_query_ms() {
        use std::time::Instant;
        // 1,000 messages of varying complexity.
        let bodies: Vec<String> = (0..1000)
            .map(|i| {
                if i % 3 == 0 {
                    format!("Hey @user{i}, please review this conversation.")
                } else if i % 3 == 1 {
                    format!("@team:engineering please look at ticket #{i}")
                } else {
                    format!("No mentions here, just message #{i}.")
                }
            })
            .collect();

        // Warm up the OnceLock-cached regexes so the timing loop measures
        // steady-state per-message scan cost, not first-use compilation.
        let _ = scan_for_mentions("warmup @alice @team:engineering").unwrap();

        let start = Instant::now();
        for body in &bodies {
            let _ = scan_for_mentions(body).unwrap();
        }
        let elapsed = start.elapsed().as_millis();
        // Per project standard: MAX_QUERY_MS = 500 (perf_guards.rs).
        // 1,000 message scans should be well under that budget, even under
        // parallel-test load. The 500ms threshold matches the documented
        // project standard for "bounded query" performance.
        assert!(
            elapsed < 500,
            "1,000-message scan took {elapsed}ms (must be < 500ms per MAX_QUERY_MS)"
        );
    }
}
