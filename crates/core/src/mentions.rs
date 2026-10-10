//! Mentions — the reference `mentionParser.ts` port + the legacy text scan.
//!
//! Two layers:
//! - [`scan_for_mentions`]: the port's original raw text scan
//!   (`@username` / `@team:name` tokens). Still used to store the
//!   `mentions_json` summary on side-thread messages.
//! - [`build_mention_directory`] + [`parse_mentions`]: the reference
//!   mentionParser.ts port. Resolves @tokens against the identities Help
//!   Scout itself knows about:
//!   - users: their Help Scout mention name (users.mention, e.g. "alex"),
//!     plus deterministic fallbacks — first name, "first last",
//!     "firstlast", "first.last" — because synced users may lack a mention
//!     name.
//!   - teams: exact team name (case-insensitive, spaces allowed after @).
//!
//! Honesty rules (ported verbatim):
//! - UNKNOWN tokens stay plain text (no guessed identity, no notification).
//! - Matching is exact (case-insensitive); we do NOT do prefix/substring
//!   matching, so "@al" never silently notifies Alex.
//! - Emails are not mentionable (agents are addressed by name here): the
//!   token class includes dots, so `alice@example.com` resolves as the
//!   single token "example.com" which matches no identity.
//!
//! ## Safety
//!
//! The `regex` crate uses a bounded NFA (no backtracking) — there is no
//! ReDoS surface. Patterns are static (compiled once), and message bodies
//! are bounded by [`MAX_BODY_BYTES`].

use std::collections::HashMap;
use std::sync::OnceLock;

use regex::Regex;
use rusqlite::{params, Connection};

use crate::error::Result;

/// The maximum message body size we'll scan, in bytes. Help Scout message
/// bodies are capped at 64KB; we apply 100KB as a defensive bound so a
/// malicious or buggy caller can't cause unbounded scanning.
pub const MAX_BODY_BYTES: usize = 100_000;

/// A parsed mention — either a user mention (`@username`) or a team
/// mention (`@team:teamname`). The raw-text scan result.
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
/// (alphanumeric + underscore). Returns `false` if `pos == 0` (start of
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

/// Scan a message body for raw mentions. Returns the parsed mentions in
/// order of appearance. Deduplicates: each unique mention string appears
/// only once.
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

// ---------------------------------------------------------------------------
// Reference mentionParser.ts (plan Phase 13: "Respect Help Scout identity")
// ---------------------------------------------------------------------------

/// The identities @tokens resolve against — the reference
/// `MentionDirectory`.
#[derive(Debug, Clone, Default)]
pub struct MentionDirectory {
    /// Exact mentionable names (lowercased) → user LOCAL id.
    pub user_by_name: HashMap<String, i64>,
    /// Lowercased team names → team LOCAL id.
    pub team_by_name: HashMap<String, i64>,
    /// Display names for users.
    pub display_by_user: HashMap<i64, String>,
}

/// Build the mention directory from the mirror — the reference
/// `buildMentionDirectory(db)`.
///
/// # Errors
///
/// Returns `Error::Sqlite` if the users/teams reads fail.
/// (id, first, last, mention, email) from the users mirror.
type UserRow = (
    i64,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

pub fn build_mention_directory(conn: &Connection) -> Result<MentionDirectory> {
    let mut dir = MentionDirectory::default();
    let users: Vec<UserRow> = {
        let mut stmt = conn.prepare(
            "SELECT id, first_name, last_name, mention, email FROM users
             WHERE deleted_at IS NULL",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    for (id, first, last, mention, email) in users {
        let display = [first.clone(), last.clone()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" ");
        let display = if !display.is_empty() {
            display
        } else {
            email.unwrap_or_else(|| format!("user #{id}"))
        };
        dir.display_by_user.insert(id, display);
        let mut candidates: Vec<String> = Vec::new();
        if let Some(m) = mention {
            candidates.push(m);
        }
        if let Some(f) = &first {
            candidates.push(f.clone());
        }
        let full = [first, last]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" ");
        if !full.is_empty() {
            candidates.push(full.clone());
            candidates.push(full.split_whitespace().collect::<String>());
            candidates.push(full.split_whitespace().collect::<Vec<_>>().join("."));
        }
        for c in candidates {
            let key = c.trim().to_lowercase();
            if !key.is_empty() {
                dir.user_by_name.entry(key).or_insert(id);
            }
        }
    }
    let teams: Vec<(i64, String)> = {
        let mut stmt = conn.prepare("SELECT id, name FROM teams")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    for (id, name) in teams {
        let key = name.trim().to_lowercase();
        if !key.is_empty() {
            dir.team_by_name.entry(key).or_insert(id);
        }
    }
    Ok(dir)
}

/// One resolved @mention — the reference `ParsedMention`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedMention {
    /// The matched token text (without the leading `@`).
    pub token: String,
    /// The mentioned user's LOCAL id (None for team mentions).
    pub user_local_id: Option<i64>,
    /// The mentioned team's LOCAL id (None for user mentions).
    pub team_local_id: Option<i64>,
    /// Display string for rendering.
    pub display: String,
}

/// Whether `c` is in the reference token class `[A-Za-z0-9._-]`.
fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-'
}

/// Parse @mentions in a body — the reference `parseMentions`. Team mentions
/// are resolved greedily: for every @-position we first try the single token
/// against users, then extend the match across spaces (team names may
/// contain spaces), trying progressively shorter prefixes.
///
/// # Errors
///
/// Returns `Error::Other` if `body.len()` exceeds `MAX_BODY_BYTES`.
pub fn parse_mentions(body: &str, directory: &MentionDirectory) -> Result<Vec<ParsedMention>> {
    if body.len() > MAX_BODY_BYTES {
        return Err(crate::error::Error::Other(
            format!(
                "mention parse refused: body {} bytes exceeds max {} bytes",
                body.len(),
                MAX_BODY_BYTES
            )
            .into(),
        ));
    }
    let mut mentions: Vec<ParsedMention> = Vec::new();
    let mut seen_users: std::collections::HashSet<i64> = std::collections::HashSet::new();
    let mut seen_teams: std::collections::HashSet<i64> = std::collections::HashSet::new();
    let team_max_len = directory
        .team_by_name
        .keys()
        .map(|k| k.len())
        .max()
        .unwrap_or(0);

    let mut search_from = 0usize;
    while let Some(found) = body[search_from..].find('@') {
        let at = search_from + found;
        let after_at = at + 1;
        let token: String = body[after_at..]
            .chars()
            .take_while(|c| is_token_char(*c))
            .collect();
        if token.is_empty() {
            search_from = after_at;
            continue;
        }
        let lower = token.to_lowercase();
        // 1) single-token user match (exact, case-insensitive).
        if let Some(uid) = directory.user_by_name.get(&lower) {
            if seen_users.insert(*uid) {
                mentions.push(ParsedMention {
                    token: token.clone(),
                    user_local_id: Some(*uid),
                    team_local_id: None,
                    display: directory
                        .display_by_user
                        .get(uid)
                        .cloned()
                        .unwrap_or_else(|| token.clone()),
                });
            }
            search_from = after_at + token.len();
            continue;
        }
        // 2) team match, possibly spanning spaces.
        if !directory.team_by_name.is_empty() {
            let window_len = 60.min(team_max_len.max(lower.len()));
            let window_end = (after_at + window_len).min(body.len());
            let window = &body[after_at..window_end];
            let mut matched: Option<(i64, usize, String)> = None;
            let mut len = window.len().min(team_max_len + 12);
            while len >= lower.len() {
                // Keep the slice on a char boundary (team names may sit next
                // to multibyte text).
                while len > 0 && !window.is_char_boundary(len) {
                    len -= 1;
                }
                if len < lower.len() {
                    break;
                }
                let raw = &window[..len];
                let candidate = raw.trim().to_lowercase();
                let candidate = candidate.split_whitespace().collect::<Vec<_>>().join(" ");
                if !candidate.is_empty() {
                    if let Some(tid) = directory.team_by_name.get(&candidate) {
                        if !seen_teams.contains(tid) {
                            // Require the consumed span to end at a word
                            // boundary (next char is not a token char).
                            let next_char = window[len..].chars().next();
                            let boundary_ok = next_char.is_none_or(|c| !is_token_char(c));
                            if boundary_ok {
                                matched = Some((*tid, len, raw.trim().to_string()));
                                break;
                            }
                        }
                    }
                }
                len -= 1;
            }
            if let Some((tid, matched_len, raw)) = matched {
                seen_teams.insert(tid);
                mentions.push(ParsedMention {
                    token: raw.clone(),
                    user_local_id: None,
                    team_local_id: Some(tid),
                    display: raw,
                });
                search_from = after_at + matched_len;
                continue;
            }
        }
        // 3) unknown token: stays plain text, no mention row, no notification.
        search_from = after_at + token.len();
    }
    Ok(mentions)
}

/// The members of a team (LOCAL user ids) — the reference
/// `sideThreadService.teamMembers`.
///
/// # Errors
///
/// Returns `Error::Sqlite` if the query fails (no team_members table on
/// very old schemas → empty list).
pub fn team_members(conn: &Connection, team_local_id: i64) -> Result<Vec<i64>> {
    let mut stmt = match conn.prepare("SELECT user_id FROM team_members WHERE team_id = ?1") {
        Ok(stmt) => stmt,
        Err(_) => return Ok(Vec::new()),
    };
    let rows = stmt
        .query_map(params![team_local_id], |r| r.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
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
        // The canonical boot chain — the directory reads users/teams and the
        // team fan-out reads team_members (db_breadth's 001 shape).
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
        // M047 FKs: mailbox_local_id -> mailboxes(id), customer_local_id ->
        // customers(id); seed the parents (idempotent for repeat calls).
        conn.execute(
            "INSERT OR IGNORE INTO mailboxes (id, remote_id, name) VALUES (101, 101, 'Support')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO customers (id, remote_id, first_name, last_name) VALUES (2001, 2001, 'Ada', 'Lovelace')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO conversations (remote_id, number, status, mailbox_local_id, customer_local_id)
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

    // ---- build_mention_directory --------------------------------------------

    #[test]
    fn directory_maps_mention_names_first_names_and_full_names() {
        let conn = fresh_db();
        conn.execute(
            "INSERT INTO users (remote_id, first_name, last_name, mention, user_type)
             VALUES (1001, 'Alex', 'Rivera', 'alex', 'user')",
            [],
        )
        .unwrap();
        let dir = build_mention_directory(&conn).unwrap();
        let id = dir.user_by_name["alex"];
        assert_eq!(dir.user_by_name["alex"], id);
        assert_eq!(dir.user_by_name["alex"], id);
        assert_eq!(dir.user_by_name["alex rivera"], id);
        assert_eq!(dir.user_by_name["alexrivera"], id);
        assert_eq!(dir.user_by_name["alex.rivera"], id);
        assert_eq!(dir.display_by_user[&id], "Alex Rivera");
    }

    #[test]
    fn directory_maps_team_names_case_insensitively() {
        let conn = fresh_db();
        insert_team(&conn, 501, "Tier 1");
        insert_team(&conn, 502, "Escalations");
        let dir = build_mention_directory(&conn).unwrap();
        assert!(dir.team_by_name.contains_key("tier 1"));
        assert!(dir.team_by_name.contains_key("escalations"));
    }

    #[test]
    fn directory_first_identity_wins_on_colliding_names() {
        let conn = fresh_db();
        // Two users named Alex; the first synced user owns the name.
        conn.execute(
            "INSERT INTO users (remote_id, first_name, mention, user_type)
             VALUES (1001, 'Alex', 'alex', 'user')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO users (remote_id, first_name, mention, user_type)
             VALUES (1002, 'Alex', 'alex', 'user')",
            [],
        )
        .unwrap();
        let dir = build_mention_directory(&conn).unwrap();
        assert_eq!(dir.user_by_name["alex"], 1, "first claimant wins");
    }

    // ---- parse_mentions -----------------------------------------------------

    #[test]
    fn parse_resolves_user_mention_to_local_id() {
        let conn = fresh_db();
        insert_user(&conn, 1001, "alex");
        let dir = build_mention_directory(&conn).unwrap();
        let mentions = parse_mentions("Hey @alex, please review.", &dir).unwrap();
        assert_eq!(mentions.len(), 1);
        assert_eq!(mentions[0].user_local_id, Some(1));
        assert_eq!(mentions[0].team_local_id, None);
        assert_eq!(mentions[0].token, "alex");
    }

    #[test]
    fn parse_is_exact_no_prefix_matching() {
        let conn = fresh_db();
        insert_user(&conn, 1001, "alex");
        let dir = build_mention_directory(&conn).unwrap();
        let mentions = parse_mentions("Hey @al, please review.", &dir).unwrap();
        assert!(mentions.is_empty(), "@al never silently notifies Alex");
    }

    #[test]
    fn parse_unknown_tokens_stay_plain_text() {
        let conn = fresh_db();
        insert_user(&conn, 1001, "alex");
        let dir = build_mention_directory(&conn).unwrap();
        let mentions = parse_mentions("Hey @nobody and @alex", &dir).unwrap();
        assert_eq!(mentions.len(), 1);
        assert_eq!(mentions[0].user_local_id, Some(1));
    }

    #[test]
    fn parse_team_mention_spans_spaces() {
        let conn = fresh_db();
        insert_team(&conn, 501, "Tier 1");
        let dir = build_mention_directory(&conn).unwrap();
        let mentions = parse_mentions("Loop in @Tier 1 please", &dir).unwrap();
        assert_eq!(mentions.len(), 1);
        assert_eq!(mentions[0].team_local_id, Some(1));
        assert_eq!(mentions[0].token, "Tier 1");
    }

    #[test]
    fn parse_team_mention_boundary_matches_reference_quirk() {
        let conn = fresh_db();
        insert_team(&conn, 501, "Tier 1");
        let dir = build_mention_directory(&conn).unwrap();
        // REFERENCE QUIRK (reported, not "fixed"): mentionParser.ts slices
        // the team window at max(teamNameLength, tokenLength), so the
        // boundary check `window[len] ?? ''` can never see the character
        // AFTER a full-length team name — "@Tier 1x" matches "Tier 1" in
        // the reference, and the port reproduces that exactly.
        let mentions = parse_mentions("Loop in @Tier 1x please", &dir).unwrap();
        assert_eq!(mentions.len(), 1, "the reference matches here too");
        assert_eq!(mentions[0].team_local_id, Some(1));
        assert_eq!(mentions[0].token, "Tier 1");
    }

    #[test]
    fn parse_deduplicates_users_and_teams() {
        let conn = fresh_db();
        insert_user(&conn, 1001, "alex");
        insert_team(&conn, 501, "Tier 1");
        let dir = build_mention_directory(&conn).unwrap();
        let mentions = parse_mentions("@alex @alex @Tier 1 @Tier 1", &dir).unwrap();
        assert_eq!(mentions.len(), 2);
    }

    #[test]
    fn parse_email_is_not_a_mention() {
        let conn = fresh_db();
        insert_user(&conn, 1001, "alex");
        let dir = build_mention_directory(&conn).unwrap();
        let mentions = parse_mentions("Contact alex@example.com and @alex", &dir).unwrap();
        assert_eq!(mentions.len(), 1, "only the real @alex resolves");
    }

    #[test]
    fn parse_rejects_body_exceeding_max_bytes() {
        let dir = MentionDirectory::default();
        let big = "a".repeat(MAX_BODY_BYTES + 1);
        assert!(parse_mentions(&big, &dir).is_err());
    }

    // ---- team_members -------------------------------------------------------

    #[test]
    fn team_members_lists_local_user_ids() {
        let conn = fresh_db();
        insert_user(&conn, 1001, "alex");
        insert_user(&conn, 1002, "priya");
        insert_team(&conn, 501, "Tier 1");
        conn.execute(
            "INSERT INTO team_members (team_id, user_id) VALUES (1, 1), (1, 2)",
            [],
        )
        .unwrap();
        assert_eq!(team_members(&conn, 1).unwrap(), vec![1, 2]);
        assert!(team_members(&conn, 999).unwrap().is_empty());
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
        assert!(
            elapsed < 500,
            "1,000-message scan took {elapsed}ms (must be < 500ms per MAX_QUERY_MS)"
        );
    }

    // ---- shared helpers used by both layers ---------------------------------

    #[test]
    fn insert_conversation_helper_works() {
        let conn = fresh_db();
        insert_conversation(&conn, 1001);
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM conversations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }
}
