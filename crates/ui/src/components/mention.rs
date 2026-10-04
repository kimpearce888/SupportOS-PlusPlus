//! Mention directory — the Help Scout identity mirror behind @autocomplete.
//!
//! Reference: `useMentionDirectory` (api/hooks.ts) → GET /api/mention-directory
//! `{ users: [{ user_local_id, display_name, mention }], teams: [{ team_local_id, name }] }`.
//! Only mirrored identities are offered — the UI never invents mention targets.

use leptos::*;

/// A directory user row.
#[derive(Clone, Debug, PartialEq)]
pub struct DirectoryUser {
    pub user_local_id: i64,
    pub display_name: String,
    pub mention: Option<String>,
}

/// A directory team row.
#[derive(Clone, Debug, PartialEq)]
pub struct DirectoryTeam {
    pub team_local_id: i64,
    pub name: String,
}

/// The full directory (users + teams).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MentionDirectory {
    pub users: Vec<DirectoryUser>,
    pub teams: Vec<DirectoryTeam>,
}

impl MentionDirectory {
    /// The mentionable tokens, in reference order: users first (the explicit
    /// `@mention` when set, otherwise the sanitized first name), then teams.
    pub fn entries(&self) -> Vec<(String, String, bool)> {
        let mut out = Vec::new();
        for u in &self.users {
            let token = match &u.mention {
                Some(m) => m.clone(),
                None => u
                    .display_name
                    .split(' ')
                    .next()
                    .unwrap_or(&u.display_name)
                    .chars()
                    .filter(|c| c.is_ascii_alphanumeric() || *c == '.' || *c == '_' || *c == '-')
                    .collect(),
            };
            let display = match &u.mention {
                Some(m) => format!("{} (@{})", u.display_name, m),
                None => u.display_name.clone(),
            };
            out.push((token, display, false));
        }
        for t in &self.teams {
            out.push((t.name.clone(), format!("{} (team)", t.name), true));
        }
        out
    }
}

/// Parse the /api/mention-directory response body.
#[must_use]
pub fn parse_mention_directory(v: &serde_json::Value) -> MentionDirectory {
    let users = v
        .get("users")
        .and_then(|u| u.as_array())
        .map(|rows| {
            rows.iter()
                .map(|r| DirectoryUser {
                    user_local_id: r.get("user_local_id").and_then(|x| x.as_i64()).unwrap_or(0),
                    display_name: r
                        .get("display_name")
                        .and_then(|x| x.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    mention: r
                        .get("mention")
                        .and_then(|x| x.as_str())
                        .map(str::to_string),
                })
                .collect()
        })
        .unwrap_or_default();
    let teams = v
        .get("teams")
        .and_then(|t| t.as_array())
        .map(|rows| {
            rows.iter()
                .map(|r| DirectoryTeam {
                    team_local_id: r.get("team_local_id").and_then(|x| x.as_i64()).unwrap_or(0),
                    name: r
                        .get("name")
                        .and_then(|x| x.as_str())
                        .unwrap_or_default()
                        .to_string(),
                })
                .collect()
        })
        .unwrap_or_default();
    MentionDirectory { users, teams }
}

/// Fetch the directory once and hand every reader the same signal.
pub fn provide_mention_directory() -> RwSignal<MentionDirectory> {
    let dir = create_rw_signal(MentionDirectory::default());
    provide_context(dir);
    spawn_local(async move {
        match crate::api::get_json::<serde_json::Value>("/api/mention-directory").await {
            Ok(v) => dir.set(parse_mention_directory(&v)),
            Err(_) => {}
        }
    });
    dir
}

/// Read the directory from context (or fetch it if not provided).
pub fn use_mention_directory() -> RwSignal<MentionDirectory> {
    use_context::<RwSignal<MentionDirectory>>().unwrap_or_else(provide_mention_directory)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_mention_directory_extracts_users_and_teams() {
        let v = serde_json::json!({
            "users": [
                { "user_local_id": 1, "display_name": "Alice Zhang", "mention": "alice" },
                { "user_local_id": 2, "display_name": "Bob O'Hara", "mention": null }
            ],
            "teams": [ { "team_local_id": 5, "name": "Engineering" } ]
        });
        let d = parse_mention_directory(&v);
        assert_eq!(d.users.len(), 2);
        assert_eq!(d.users[0].mention.as_deref(), Some("alice"));
        assert_eq!(d.teams.len(), 1);
        assert_eq!(d.teams[0].name, "Engineering");
    }

    #[test]
    fn entries_prefer_explicit_mentions_and_sanitize_fallbacks() {
        let d = MentionDirectory {
            users: vec![
                DirectoryUser {
                    user_local_id: 1,
                    display_name: "Alice Zhang".into(),
                    mention: Some("alice".into()),
                },
                DirectoryUser {
                    user_local_id: 2,
                    display_name: "Bob O'Hara".into(),
                    mention: None,
                },
            ],
            teams: vec![DirectoryTeam {
                team_local_id: 5,
                name: "Billing".into(),
            }],
        };
        let entries = d.entries();
        assert_eq!(entries[0].0, "alice");
        assert_eq!(entries[0].1, "Alice Zhang (@alice)");
        // fallback token = sanitized first name (apostrophe stripped)
        assert_eq!(entries[1].0, "Bob");
        assert_eq!(entries[1].1, "Bob O'Hara");
        assert_eq!(entries[2].0, "Billing");
        assert!(entries[2].2, "third entry is the team");
    }

    #[test]
    fn parse_mention_directory_handles_missing_fields() {
        let d = parse_mention_directory(&serde_json::json!({}));
        assert!(d.users.is_empty());
        assert!(d.teams.is_empty());
    }
}
