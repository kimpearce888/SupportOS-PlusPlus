//! Customer memory panel (v2.2.0, plan Phase 36) — collapsed panel in the
//! conversation detail. Composed live from the local mirror; human entries
//! are the only persisted rows and can be added/removed here. Quarantined
//! entries (psychological/personality pattern matches) are shown separately,
//! never as usable memory.
//!
//! Reference: components/inbox/MemoryPanel.tsx.

use leptos::*;

/// One composed memory entry.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MemoryEntry {
    pub entry_id: Option<i64>,
    pub title: String,
    pub value: Option<String>,
    pub source: String,
    pub confidence: String,
    pub freshness: String,
    pub evidence: Vec<MemoryEvidence>,
    pub editable: bool,
    pub first_seen_at: Option<String>,
    pub last_seen_at: Option<String>,
}

/// One evidence link on an entry.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MemoryEvidence {
    pub description: String,
    pub conversation_number: Option<i64>,
}

/// One composed section of the profile.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MemorySection {
    pub section: String,
    pub label: String,
    pub entries: Vec<MemoryEntry>,
}

/// One quarantined row.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct QuarantinedEntry {
    pub entry_id: i64,
    pub key: String,
    pub reason: String,
}

/// The whole memory profile.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MemoryProfile {
    pub sections: Vec<MemorySection>,
    pub quarantined: Vec<QuarantinedEntry>,
    pub notes: Vec<String>,
}

fn parse_entry(v: &serde_json::Value) -> MemoryEntry {
    MemoryEntry {
        entry_id: v.get("entry_id").and_then(|x| x.as_i64()),
        title: v
            .get("title")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string(),
        value: v.get("value").and_then(|x| x.as_str()).map(str::to_string),
        source: v
            .get("source")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string(),
        confidence: v
            .get("confidence")
            .and_then(|x| x.as_str())
            .unwrap_or("unknown")
            .to_string(),
        freshness: v
            .get("freshness")
            .and_then(|x| x.as_str())
            .unwrap_or("unknown")
            .to_string(),
        evidence: v
            .get("evidence")
            .and_then(|e| e.as_array())
            .map(|rows| {
                rows.iter()
                    .map(|e| MemoryEvidence {
                        description: e
                            .get("description")
                            .and_then(|x| x.as_str())
                            .unwrap_or_default()
                            .to_string(),
                        conversation_number: e.get("conversation_number").and_then(|x| x.as_i64()),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        editable: v.get("editable").and_then(|x| x.as_bool()).unwrap_or(false),
        first_seen_at: v
            .get("first_seen_at")
            .and_then(|x| x.as_str())
            .map(str::to_string),
        last_seen_at: v
            .get("last_seen_at")
            .and_then(|x| x.as_str())
            .map(str::to_string),
    }
}

/// Parse the /api/memory/:customerId response body.
#[must_use]
pub fn parse_memory_profile(v: &serde_json::Value) -> MemoryProfile {
    MemoryProfile {
        sections: v
            .get("sections")
            .and_then(|s| s.as_array())
            .map(|rows| {
                rows.iter()
                    .map(|s| MemorySection {
                        section: s
                            .get("section")
                            .and_then(|x| x.as_str())
                            .unwrap_or_default()
                            .to_string(),
                        label: s
                            .get("label")
                            .and_then(|x| x.as_str())
                            .unwrap_or_default()
                            .to_string(),
                        entries: s
                            .get("entries")
                            .and_then(|e| e.as_array())
                            .map(|rows| rows.iter().map(parse_entry).collect())
                            .unwrap_or_default(),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        quarantined: v
            .get("quarantined")
            .and_then(|q| q.as_array())
            .map(|rows| {
                rows.iter()
                    .map(|q| QuarantinedEntry {
                        entry_id: q.get("entry_id").and_then(|x| x.as_i64()).unwrap_or(0),
                        key: q
                            .get("key")
                            .and_then(|x| x.as_str())
                            .unwrap_or_default()
                            .to_string(),
                        reason: q
                            .get("reason")
                            .and_then(|x| x.as_str())
                            .unwrap_or_default()
                            .to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        notes: v
            .get("notes")
            .and_then(|n| n.as_array())
            .map(|rows| {
                rows.iter()
                    .filter_map(|n| n.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
    }
}

/// Total entry count across sections (the reference header count).
#[must_use]
pub fn total_entries(profile: &MemoryProfile) -> usize {
    profile.sections.iter().map(|s| s.entries.len()).sum()
}

/// The CSS class for an entry source (reference SOURCE_CLASS map).
#[must_use]
pub fn source_class(source: &str) -> &'static str {
    match source {
        "human_local" => "spp-badge--ok",
        "ai_derived" => "spp-badge--ai",
        _ => "",
    }
}

/// The CSS class for a freshness value (reference FRESHNESS_CLASS map).
#[must_use]
pub fn freshness_class(freshness: &str) -> &'static str {
    match freshness {
        "fresh" => "spp-badge--ok",
        "aging" => "spp-badge--warn",
        "stale" => "spp-badge--err",
        _ => "",
    }
}

/// The MemoryPanel component. Collapsed by default.
#[component]
pub fn MemoryPanel(customer_id: Option<i64>, conversation_id: i64) -> impl IntoView {
    let open = create_rw_signal(false);
    let profile = create_rw_signal(None::<MemoryProfile>);
    let loading = create_rw_signal(false);
    let new_key = create_rw_signal(String::new());
    let new_value = create_rw_signal(String::new());
    let new_kind = create_rw_signal("fact".to_string());
    let saving = create_rw_signal(false);
    let expanded_entry = create_rw_signal(None::<String>);

    let load = move || {
        let Some(cid) = customer_id else { return };
        loading.set(true);
        let profile = profile;
        let loading = loading;
        spawn_local(async move {
            match crate::api::get_json::<serde_json::Value>(&format!("/api/memory/{cid}")).await {
                Ok(v) => profile.set(Some(parse_memory_profile(&v))),
                Err(_) => profile.set(None),
            }
            loading.set(false);
        });
    };

    create_effect(move |_| {
        if open.get() && profile.get_untracked().is_none() && !loading.get_untracked() {
            load();
        }
    });

    let add_entry = move |_| {
        let Some(cid) = customer_id else { return };
        let key = new_key.get_untracked();
        if key.trim().is_empty() || saving.get_untracked() {
            return;
        }
        saving.set(true);
        let value = new_value.get_untracked();
        let kind = new_kind.get_untracked();
        let body = serde_json::json!({
            "key": key,
            "value": if value.is_empty() { serde_json::Value::Null } else { serde_json::json!(value) },
            "kind": kind,
            "conversation_id": conversation_id,
        });
        let new_key = new_key;
        let new_value = new_value;
        let saving = saving;
        let load = load;
        spawn_local(async move {
            if crate::api::post_json::<serde_json::Value>(
                &format!("/api/memory/{cid}/entries"),
                Some(&body),
            )
            .await
            .is_ok()
            {
                new_key.set(String::new());
                new_value.set(String::new());
                load();
            }
            saving.set(false);
        });
    };

    let remove_entry = move |entry_id: i64| {
        let Some(cid) = customer_id else { return };
        let load = load;
        spawn_local(async move {
            if crate::api::delete_json::<serde_json::Value>(&format!(
                "/api/memory/{cid}/entries/{entry_id}"
            ))
            .await
            .is_ok()
            {
                load();
            }
        });
    };

    view! {
        <section class="spp-card spp-memory-panel">
            <button
                class="spp-card__title spp-card__title--collapsible"
                on:click=move |_| open.update(|o| *o = !*o)
            >
                {move || {
                    let count = profile.get().map(|p| total_entries(&p))
                        .map(|n| format!(" ({n} entries)"))
                        .unwrap_or_default();
                    format!("Customer memory{count} {}", if open.get() { "▾" } else { "▸" })
                }}
            </button>
            <Show when=move || open.get() fallback=|| ()>
                {match customer_id {
                    None => view! {
                        <div class="spp-state spp-state--empty">
                            <p class="spp-state__title">"No customer on this conversation"</p>
                            <p class="spp-state__hint">"Memory composes per customer; this conversation has none linked."</p>
                        </div>
                    }.into_view(),
                    Some(_) => view! {
                        <p class="spp-muted spp-text-xs">
                            "Composed live from the local mirror — issue history, resolutions, preferences, patterns, campaigns and account facts. Human-written entries are the only stored rows."
                        </p>
                        <Show when=move || loading.get() fallback=|| ()>
                            <div class="spp-muted spp-text-xs">"Loading…"</div>
                        </Show>
                        <Show when=move || profile.get().is_some() fallback=|| ()>
                            {move || {
                                let p = profile.get().unwrap_or_default();
                                view! {
                                    {p.sections
                                        .iter()
                                        .filter(|s| !s.entries.is_empty() || s.section == "human_entries")
                                        .map(|s| {
                                            let section_key = s.section.clone();
                                            view! {
                                                <div class="spp-memory-section">
                                                    <h4 class="spp-card__title">
                                                        {s.label.clone()}
                                                        <span class="spp-muted spp-text-xs">{format!(" ({})", s.entries.len())}</span>
                                                    </h4>
                                                    {if s.entries.is_empty() {
                                                        view! { <div class="spp-muted spp-text-xs">"Nothing on record (honest unknown)."</div> }.into_view()
                                                    } else {
                                                        s.entries
                                                            .iter()
                                                            .enumerate()
                                                            .map(|(i, e)| {
                                                                let entry_key = format!("{section_key}-{i}");
                                                                let is_expanded = expanded_entry.get() == Some(entry_key.clone());
                                                                let entry_id = e.entry_id;
                                                                let editable = e.editable && entry_id.is_some();
                                                                let delete_entry_id = entry_id;
                                                                view! {
                                                                    <div class="spp-memory-entry">
                                                                        <div class="spp-memory-entry__row">
                                                                            <div class="spp-memory-entry__main">
                                                                                <strong>{e.title.clone()}</strong>
                                                                                {if let Some(value) = &e.value {
                                                                                    let shown: String = if is_expanded {
                                                                                        value.chars().take(2000).collect()
                                                                                    } else {
                                                                                        value.chars().take(160).collect()
                                                                                    };
                                                                                    let ellipsis = if value.chars().count() > 160 && !is_expanded { "…" } else { "" };
                                                                                    view! {
                                                                                        <div class="spp-muted spp-text-xs spp-pre-wrap">
                                                                                            {format!("{shown}{ellipsis}")}
                                                                                        </div>
                                                                                    }.into_view()
                                                                                } else {
                                                                                    ().into_view()
                                                                                }}
                                                                            </div>
                                                                            <div class="spp-memory-entry__badges">
                                                                                <span class=format!("spp-badge {}", source_class(&e.source))>
                                                                                    {e.source.replace('_', " ")}
                                                                                </span>
                                                                                <span class="spp-badge">{format!("conf: {}", e.confidence)}</span>
                                                                                <span class=format!("spp-badge {}", freshness_class(&e.freshness))>
                                                                                    {e.freshness.clone()}
                                                                                </span>
                                                                            </div>
                                                                        </div>
                                                                        <div class="spp-memory-entry__meta">
                                                                            {if e.evidence.is_empty() {
                                                                                view! { <span class="spp-muted spp-text-xs">"no evidence links"</span> }.into_view()
                                                                            } else {
                                                                                let key_for_toggle = entry_key.clone();
                                                                                view! {
                                                                                    <button
                                                                                        class="spp-button spp-button--ghost spp-button--small"
                                                                                        on:click=move |_| {
                                                                                            expanded_entry.set(if is_expanded { None } else { Some(key_for_toggle.clone()) });
                                                                                        }
                                                                                    >
                                                                                        {if is_expanded { "Hide evidence".to_string() } else { format!("Evidence ({})", e.evidence.len()) }}
                                                                                    </button>
                                                                                }.into_view()
                                                                            }}
                                                                            <span class="spp-muted spp-text-xs">
                                                                                {match (&e.last_seen_at, &e.first_seen_at) {
                                                                                    (Some(seen), _) => format!("last seen {}", &seen[..seen.len().min(10)]),
                                                                                    (None, Some(first)) => format!("since {}", &first[..first.len().min(10)]),
                                                                                    (None, None) => "no timestamp".to_string(),
                                                                                }}
                                                                            </span>
                                                                            {if editable {
                                                                                view! {
                                                                                    <button
                                                                                        class="spp-button spp-button--ghost spp-button--small"
                                                                                        on:click=move |_| remove_entry(delete_entry_id.unwrap_or(0))
                                                                                    >
                                                                                        "Delete"
                                                                                    </button>
                                                                                }.into_view()
                                                                            } else {
                                                                                ().into_view()
                                                                            }}
                                                                        </div>
                                                                        {if is_expanded && !e.evidence.is_empty() {
                                                                            view! {
                                                                                <div class="spp-memory-entry__evidence">
                                                                                    {e.evidence
                                                                                        .iter()
                                                                                        .map(|ev| {
                                                                                            view! {
                                                                                                <div class="spp-mono spp-text-xs">
                                                                                                    {match ev.conversation_number {
                                                                                                        Some(n) => format!("{} (#{n})", ev.description),
                                                                                                        None => ev.description.clone(),
                                                                                                    }}
                                                                                                </div>
                                                                                            }
                                                                                        })
                                                                                        .collect::<Vec<_>>()}
                                                                                </div>
                                                                            }.into_view()
                                                                        } else {
                                                                            ().into_view()
                                                                        }}
                                                                    </div>
                                                                }
                                                            })
                                                            .collect::<Vec<_>>()
                                                            .into_view()
                                                    }}
                                                </div>
                                            }
                                        })
                                        .collect::<Vec<_>>()}
                                    {if !p.quarantined.is_empty() {
                                        view! {
                                            <div class="spp-alert spp-alert--error spp-text-xs">
                                                <strong>{format!("Quarantined entries ({}) — never used as memory:", p.quarantined.len())}</strong>
                                                {p.quarantined
                                                    .iter()
                                                    .map(|q| {
                                                        let entry_id = q.entry_id;
                                                        view! {
                                                            <div class="spp-memory-quarantined__row">
                                                                <span>{format!("\u{201c}{}\u{201d} — {}", q.key, q.reason)}</span>
                                                                <button
                                                                    class="spp-button spp-button--ghost spp-button--small"
                                                                    title="Purge this quarantined entry"
                                                                    on:click=move |_| remove_entry(entry_id)
                                                                >
                                                                    "Purge"
                                                                </button>
                                                            </div>
                                                        }
                                                    })
                                                    .collect::<Vec<_>>()}
                                            </div>
                                        }.into_view()
                                    } else {
                                        ().into_view()
                                    }}
                                    <div class="spp-memory-add">
                                        <h4 class="spp-card__title">"Add a human memory entry"</h4>
                                        <div class="spp-memory-add__row">
                                            <input
                                                class="spp-input"
                                                type="text"
                                                maxlength=120
                                                placeholder="Key (e.g. Escalation contact)"
                                                aria-label="Memory key"
                                                prop:value=move || new_key.get()
                                                on:input=move |ev| new_key.set(event_target_value(&ev))
                                            />
                                            <select
                                                class="spp-input"
                                                aria-label="Memory kind"
                                                prop:value=move || new_kind.get()
                                                on:change=move |ev| new_kind.set(event_target_value(&ev))
                                            >
                                                <option value="fact">"Fact"</option>
                                                <option value="account">"Account"</option>
                                                <option value="preference">"Preference"</option>
                                                <option value="issue_history">"Issue history"</option>
                                                <option value="context">"Context"</option>
                                            </select>
                                        </div>
                                        <textarea
                                            class="spp-input"
                                            rows=2
                                            maxlength=2000
                                            placeholder="Value (observable facts only — psychological/personality judgments are refused by policy)"
                                            aria-label="Memory value"
                                            prop:value=move || new_value.get()
                                            on:input=move |ev| new_value.set(event_target_value(&ev))
                                        ></textarea>
                                        <button
                                            class="spp-button spp-button--primary spp-button--small"
                                            disabled=move || saving.get() || new_key.get().trim().is_empty()
                                            on:click=add_entry
                                        >
                                            {move || if saving.get() { "Saving…" } else { "Save entry" }.to_string()}
                                        </button>
                                    </div>
                                    <div class="spp-alert spp-alert--info spp-text-xs">
                                        {p.notes.iter().map(|n| view! { <div>{n.clone()}</div> }).collect::<Vec<_>>()}
                                    </div>
                                }
                            }}
                        </Show>
                    }.into_view(),
                }}
            </Show>
        </section>
    }
}

// (The delete closures capture only Copy signals, so they clone freely.)

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_memory_profile_sections_and_quarantine() {
        let v = serde_json::json!({
            "sections": [
                { "section": "issue_history", "label": "Issue history", "entries": [
                    { "entry_id": null, "title": "2 billing disputes", "value": null, "source": "helpscout_mirror", "confidence": "high", "freshness": "fresh", "evidence": [ { "description": "Conv #1021", "conversation_number": 1021 } ], "editable": false, "first_seen_at": "2026-09-01T10:00:00Z", "last_seen_at": "2026-09-20T10:00:00Z" }
                ] },
                { "section": "human_entries", "label": "Human entries", "entries": [
                    { "entry_id": 5, "title": "Escalation contact", "value": "Prefers email", "source": "human_local", "confidence": "human", "freshness": "unknown", "evidence": [], "editable": true, "first_seen_at": "2026-09-02T10:00:00Z", "last_seen_at": null }
                ] }
            ],
            "quarantined": [ { "entry_id": 9, "key": "seems anxious", "reason": "personality judgment (policy)" } ],
            "notes": ["Composed from 14 conversations."]
        });
        let p = parse_memory_profile(&v);
        assert_eq!(total_entries(&p), 2);
        assert_eq!(p.quarantined.len(), 1);
        assert_eq!(p.quarantined[0].key, "seems anxious");
        assert_eq!(p.sections[1].entries[0].entry_id, Some(5));
        assert!(p.sections[1].entries[0].editable);
        assert_eq!(source_class("human_local"), "spp-badge--ok");
        assert_eq!(freshness_class("stale"), "spp-badge--err");
    }

    #[test]
    fn parse_memory_profile_empty_body_is_all_defaults() {
        let p = parse_memory_profile(&serde_json::json!({}));
        assert!(p.sections.is_empty());
        assert!(p.quarantined.is_empty());
        assert_eq!(total_entries(&p), 0);
    }
}
