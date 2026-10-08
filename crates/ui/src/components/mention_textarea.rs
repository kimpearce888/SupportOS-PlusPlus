//! MentionTextarea — a plain-text textarea with @autocomplete.
//!
//! Reference: components/inbox/MentionTextarea.tsx (v1.8.0, plan Phase 13).
//! Typing '@' opens a suggestion popup (max 8 entries) filtered
//! case-insensitively; ArrowUp/Down navigate, Enter/Tab complete, Escape
//! closes. The control stays plain text — the value is only ever what the
//! user typed plus a completed token.

use leptos::*;

use super::mention::{use_mention_directory, MentionDirectory};

/// The caret query: text after the last '@' that starts a token.
#[must_use]
pub fn caret_query(value: &str, caret: usize) -> Option<(String, usize)> {
    let upto = &value[..caret.min(value.len())];
    // find the last '@' that begins the token: preceded by start or whitespace
    let mut idx = None;
    for (i, ch) in upto.char_indices().rev() {
        if ch == '@' {
            let before = upto[..i].chars().next_back();
            if before.is_none() || before.is_some_and(char::is_whitespace) {
                idx = Some(i);
            }
            break;
        }
        if ch.is_whitespace() {
            break; // a space before any '@' ends the search window
        }
        if !is_token_char(ch) {
            break; // token chars only
        }
    }
    let at = idx?;
    let text = upto[at + 1..].to_string();
    Some((text, at))
}

fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-'
}

/// Filter directory entries for the current query (max 8, reference order).
#[must_use]
pub fn filter_matches(dir: &MentionDirectory, query: &str) -> Vec<(String, String, bool)> {
    let q = query.to_lowercase();
    if q.chars().any(|c| !is_token_char(c)) {
        return Vec::new();
    }
    dir.entries()
        .into_iter()
        .filter(|(token, display, _)| {
            token.to_lowercase().starts_with(&q) || display.to_lowercase().contains(&q)
        })
        .take(8)
        .collect()
}

/// Apply a completion at `start`, mirroring the reference insertion rule:
/// `@token` plus a space only when the text after the caret needs separation.
#[must_use]
pub fn apply_completion(value: &str, start: usize, token: &str, caret: usize) -> String {
    let caret = caret.min(value.len());
    let after = &value[caret..];
    let sep = if after.is_empty() || after.starts_with(' ') {
        ""
    } else {
        " "
    };
    format!("{}@{}{}{}", &value[..start], token, sep, after)
}

/// The MentionTextarea component.
#[component]
pub fn MentionTextarea(
    #[prop(into)] value: RwSignal<String>,
    #[prop(default = "Type @ to mention a teammate or team".to_string(), into)] placeholder: String,
    #[prop(default = 3)] rows: u8,
    #[prop(default = false)] submit_on_enter: bool,
    #[prop(default = 3)] submit_key: u8,
    /// The submit callback (UI-02): fired on Ctrl/Cmd+Enter when
    /// `submit_on_enter` is set and the mention popup is closed.
    #[prop(default = 13)]
    submit_key_code: u32,
    #[prop(optional)] on_submit: Option<std::rc::Rc<dyn Fn()>>,
) -> impl IntoView {
    let _ = submit_key;
    let directory = use_mention_directory();
    let popup_open = create_rw_signal(false);
    let active = create_rw_signal(0usize);
    let caret = create_rw_signal(0usize);

    let matches = create_memo(move |_| {
        let value_now = value.get();
        let caret_now = caret.get();
        caret_query(&value_now, caret_now)
            .map(|(q, _)| filter_matches(&directory.get(), &q))
            .unwrap_or_default()
    });

    let complete = move |token: String| {
        let value_now = value.get();
        let caret_now = caret.get();
        if let Some((_, start)) = caret_query(&value_now, caret_now) {
            value.set(apply_completion(&value_now, start, &token, caret_now));
        }
        popup_open.set(false);
    };

    view! {
        <div class="spp-mention-wrap">
            <textarea
                class="spp-mention-input"
                rows=rows
                placeholder=placeholder
                prop:value=move || value.get()
                on:input=move |ev| {
                    let el: web_sys::HtmlTextAreaElement = event_target(&ev);
                    caret.set(el.selection_start().unwrap_or(None).unwrap_or(0) as usize);
                    value.set(event_target_value(&ev));
                    popup_open.set(true);
                }
                on:keyup=move |ev| {
                    // keep the caret fresh for arrow-key navigation
                    let el: web_sys::HtmlTextAreaElement = event_target(&ev);
                    caret.set(el.selection_start().unwrap_or(None).unwrap_or(0) as usize);
                }
                on:blur=move |_| {
                    set_timeout(move || popup_open.set(false), std::time::Duration::from_millis(150));
                }
                on:keydown=move |ev| {
                    let open = popup_open.get();
                    let n = matches.get().len();
                    if open && n > 0 {
                        match ev.key().as_str() {
                            "ArrowDown" => {
                                ev.prevent_default();
                                active.update(|a| *a = (*a + 1) % n);
                            }
                            "ArrowUp" => {
                                ev.prevent_default();
                                active.update(|a| *a = (*a + n - 1) % n);
                            }
                            "Enter" | "Tab" => {
                                ev.prevent_default();
                                let list = matches.get();
                                let i = active.get().min(list.len().saturating_sub(1));
                                if let Some((token, _, _)) = list.get(i).cloned() {
                                    complete(token);
                                }
                            }
                            "Escape" => {
                                popup_open.set(false);
                            }
                            _ => {}
                        }
                    } else if submit_on_enter
                        && ev.key_code() == submit_key_code
                        && (ev.ctrl_key() || ev.meta_key())
                    {
                        // Ctrl/Cmd+Enter submits (UI-02): the composer's
                        // send path, exactly like the reference's keyboard
                        // shortcut. Plain Enter keeps inserting newlines.
                        ev.prevent_default();
                        if let Some(on_submit) = on_submit.as_ref() {
                            on_submit();
                        }
                    }
                }
            ></textarea>
            <Show when=move || popup_open.get() && !matches.get().is_empty() fallback=|| ()>
                <div class="spp-mention-popup" role="listbox">
                    <div class="spp-mention-popup__head">"@ mentions"</div>
                    {move || {
                        matches.get()
                            .into_iter()
                            .enumerate()
                            .map(|(i, (token, display, _))| {
                                let token_for_click = token.clone();
                                view! {
                                    <button
                                        type="button"
                                        class="spp-mention-option"
                                        class:is-active=move || active.get() == i
                                        role="option"
                                        aria-selected=move || active.get() == i
                                        on:mousedown=move |ev| {
                                            ev.prevent_default();
                                            complete(token_for_click.clone());
                                        }
                                        on:mouseenter=move |_| active.set(i)
                                    >
                                        {display.clone()}
                                    </button>
                                }
                            })
                            .collect::<Vec<_>>()
                    }}
                </div>
            </Show>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::mention::{DirectoryTeam, DirectoryUser};

    fn dir() -> MentionDirectory {
        MentionDirectory {
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
                name: "Engineering".into(),
            }],
        }
    }

    #[test]
    fn caret_query_finds_token_after_at() {
        assert_eq!(caret_query("ping @al", 8), Some(("al".to_string(), 5)));
        assert_eq!(caret_query("@", 1), Some(("".to_string(), 0)));
        // '@' not at token start -> no query
        assert_eq!(caret_query("email me a@b", 13), None);
        // window closed by whitespace after the caret
        assert_eq!(caret_query("hi @alice ok", 12), None);
    }

    #[test]
    fn filter_matches_prefix_and_display_contains() {
        let d = dir();
        let m = filter_matches(&d, "al");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].0, "alice");
        // display containment: "zhang" only appears in the display name
        let m2 = filter_matches(&d, "zhang");
        assert_eq!(m2.len(), 1);
        assert_eq!(m2[0].0, "alice");
        // invalid query chars (e.g. a space) close the popup
        assert!(filter_matches(&d, "a b").is_empty());
    }

    #[test]
    fn apply_completion_inserts_token_with_separation_rules() {
        // after-caret text starts with a space -> no extra separator
        assert_eq!(apply_completion("hi @al ok", 3, "alice", 6), "hi @alice ok");
        // after-caret text does not start with a space -> add one
        assert_eq!(apply_completion("hi @alx", 3, "alice", 6), "hi @alice x");
        // caret at end -> no separator
        assert_eq!(apply_completion("hi @al", 3, "alice", 6), "hi @alice");
        // caret mid-token: everything after the caret is preserved
        assert_eq!(apply_completion("hi @alx", 3, "alice", 5), "hi @alice lx");
    }
}
