//! Knowledge page — `/knowledge` (the knowledge library).
//!
//! Reference pages/Knowledge.tsx: four tabs — Documents, Sources, Freshness,
//! Gaps. Documents lists the knowledge store with visibility, version and
//! chunk counts; Sources lists the source registries; Freshness shows the
//! stale/fresh split; Gaps mounts the gap-engine tab (v2.1.0, plan Phase 26)
//! with human-only candidate decisions.
//!
//! Per KNOWN PITFALLS: every view has loading, empty, and error states.

use leptos::*;

use crate::components::state_view::{EmptyState, LoadingState};
use crate::components::GapsTab;

/// The knowledge page's tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KnowledgeTab {
    Documents,
    Sources,
    Freshness,
    Gaps,
}

/// The Knowledge page — `/knowledge`.
#[component]
pub fn KnowledgePage() -> impl IntoView {
    let tab = create_rw_signal(KnowledgeTab::Documents);

    view! {
        <div class="spp-page spp-page--knowledge">
            <h2 class="spp-page__title">"Knowledge"</h2>
            <p class="spp-page__intro">
                "Local knowledge base — indexed by FTS and (optionally) vector search. Visibility separates customer-safe from internal-only."
            </p>

            <div class="spp-tabs">
                <button
                    class="spp-tab"
                    class:is-active=move || tab.get() == KnowledgeTab::Documents
                    on:click=move |_| tab.set(KnowledgeTab::Documents)
                >
                    "Documents"
                </button>
                <button
                    class="spp-tab"
                    class:is-active=move || tab.get() == KnowledgeTab::Sources
                    on:click=move |_| tab.set(KnowledgeTab::Sources)
                >
                    "Sources"
                </button>
                <button
                    class="spp-tab"
                    class:is-active=move || tab.get() == KnowledgeTab::Freshness
                    on:click=move |_| tab.set(KnowledgeTab::Freshness)
                >
                    "Freshness"
                </button>
                <button
                    class="spp-tab"
                    class:is-active=move || tab.get() == KnowledgeTab::Gaps
                    on:click=move |_| tab.set(KnowledgeTab::Gaps)
                >
                    "Gaps"
                </button>
            </div>

            <Show
                when=move || tab.get() == KnowledgeTab::Documents
                fallback=|| ()
            >
                <DocumentsTab />
            </Show>
            <Show
                when=move || tab.get() == KnowledgeTab::Sources
                fallback=|| ()
            >
                <SourcesTab />
            </Show>
            <Show
                when=move || tab.get() == KnowledgeTab::Freshness
                fallback=|| ()
            >
                <FreshnessTab />
            </Show>
            <Show
                when=move || tab.get() == KnowledgeTab::Gaps
                fallback=|| ()
            >
                <GapsTab />
            </Show>
        </div>
    }
}

/// The Documents tab — the knowledge document list.
#[component]
fn DocumentsTab() -> impl IntoView {
    let documents = create_rw_signal(Vec::<serde_json::Value>::new());
    let loading = create_rw_signal(true);
    let error_msg = create_rw_signal(None::<String>);

    create_effect(move |_| {
        let documents = documents;
        let loading = loading;
        let error_msg = error_msg;
        wasm_bindgen_futures::spawn_local(async move {
            match crate::api::get_json::<serde_json::Value>("/api/knowledge/documents?limit=50")
                .await
            {
                Ok(v) => {
                    let items = v
                        .get("documents")
                        .and_then(|d| d.as_array())
                        .cloned()
                        .unwrap_or_default();
                    documents.set(items);
                    loading.set(false);
                }
                Err(e) => {
                    error_msg.set(Some(e));
                    loading.set(false);
                }
            }
        });
    });

    view! {
        <Show when=move || loading.get() fallback=|| ()>
            <LoadingState />
        </Show>
        <Show when=move || error_msg.get().is_some() fallback=|| ()>
            <div class="spp-state spp-state--error">
                <span class="spp-state__icon" aria-hidden="true">"⚠"</span>
                <p class="spp-state__body">{move || error_msg.get().unwrap_or_default()}</p>
            </div>
        </Show>
        <Show
            when=move || !loading.get() && error_msg.get().is_none()
            fallback=|| ()
        >
            <Show
                when=move || !documents.with(|d| d.is_empty())
                fallback=|| {
                    view! {
                        <EmptyState message="No knowledge documents yet. Import docs or finish a sync that harvests them." />
                    }
                }
            >
                <table class="spp-table">
                    <thead>
                        <tr>
                            <th>"Title"</th>
                            <th>"Source"</th>
                            <th>"Freshness"</th>
                            <th>"Last reviewed"</th>
                        </tr>
                    </thead>
                    <tbody>
                        {move || {
                            documents.with(|docs| {
                                docs.iter()
                                    .map(|doc| {
                                        let title = doc
                                            .get("title")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("(untitled)")
                                            .to_string();
                                        let source = doc
                                            .get("source")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("—")
                                            .to_string();
                                        let freshness = doc
                                            .get("freshness_status")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("unknown")
                                            .to_string();
                                        let reviewed = doc
                                            .get("last_reviewed_at")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("never")
                                            .to_string();
                                        view! {
                                            <tr>
                                                <td>{title}</td>
                                                <td class="spp-table__cell-muted">{source}</td>
                                                <td>
                                                    <span class="spp-badge">{freshness}</span>
                                                </td>
                                                <td class="spp-table__cell-muted">{reviewed}</td>
                                            </tr>
                                        }
                                    })
                                    .collect::<Vec<_>>()
                            })
                        }}
                    </tbody>
                </table>
            </Show>
        </Show>
    }
}

/// One source row for the Sources tab.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct KnowledgeSource {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub visibility: String,
    pub document_count: i64,
}

/// Parse the GET /api/knowledge/sources response.
#[must_use]
pub fn parse_sources(v: &serde_json::Value) -> Vec<KnowledgeSource> {
    v.get("sources")
        .and_then(|s| s.as_array())
        .map(|rows| {
            rows.iter()
                .map(|s| KnowledgeSource {
                    id: s.get("id").and_then(|x| x.as_i64()).unwrap_or(0),
                    name: s
                        .get("name")
                        .and_then(|x| x.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    kind: s
                        .get("kind")
                        .and_then(|x| x.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    visibility: s
                        .get("visibility")
                        .and_then(|x| x.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    document_count: s
                        .get("document_count")
                        .and_then(|x| x.as_i64())
                        .unwrap_or(0),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The Sources tab — the knowledge source registries.
#[component]
fn SourcesTab() -> impl IntoView {
    let sources = create_rw_signal(Vec::<KnowledgeSource>::new());
    let loading = create_rw_signal(true);
    let error_msg = create_rw_signal(None::<String>);

    create_effect(move |_| {
        let sources = sources;
        let loading = loading;
        let error_msg = error_msg;
        wasm_bindgen_futures::spawn_local(async move {
            match crate::api::get_json::<serde_json::Value>("/api/knowledge/sources").await {
                Ok(v) => {
                    sources.set(parse_sources(&v));
                    loading.set(false);
                }
                Err(e) => {
                    error_msg.set(Some(e));
                    loading.set(false);
                }
            }
        });
    });

    view! {
        <div class="spp-card">
            <h3 class="spp-card__title">"Knowledge sources"</h3>
            <Show when=move || loading.get() fallback=|| ()>
                <LoadingState />
            </Show>
            <Show when=move || error_msg.get().is_some() fallback=|| ()>
                <div class="spp-state spp-state--error">
                    <p class="spp-state__body">{move || error_msg.get().unwrap_or_default()}</p>
                </div>
            </Show>
            <Show
                when=move || !loading.get() && error_msg.get().is_none()
                fallback=|| ()
            >
                <Show
                    when=move || !sources.with(|s| s.is_empty())
                    fallback=|| view! { <EmptyState message="No sources yet." /> }
                >
                    <div class="spp-flex spp-flex--col spp-gap-4">
                        {sources.get()
                            .iter()
                            .map(|s| {
                                let badge = if s.visibility == "customer_safe" {
                                    "spp-badge spp-badge--ok"
                                } else {
                                    "spp-badge"
                                };
                                let label = if s.visibility == "customer_safe" {
                                    "customer-safe"
                                } else {
                                    "internal-only"
                                };
                                view! {
                                    <div class="spp-flex spp-flex--between">
                                        <strong class="spp-text-sm">{s.name.clone()}</strong>
                                        <span class="spp-text-xs spp-muted">
                                            {format!("{} docs · {}", s.document_count, s.kind)}
                                            <span class=badge>{label}</span>
                                        </span>
                                    </div>
                                }
                            })
                            .collect::<Vec<_>>()}
                    </div>
                </Show>
            </Show>
        </div>
    }
}

/// The freshness overview for the Freshness tab.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FreshnessOverview {
    pub stale: i64,
    pub fresh: i64,
    pub documents: Vec<FreshnessDocument>,
}

/// One freshness document row.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FreshnessDocument {
    pub id: i64,
    pub title: String,
    pub freshness_status: String,
    pub last_reviewed_at: Option<String>,
}

/// Parse the GET /api/knowledge/freshness response.
#[must_use]
pub fn parse_freshness(v: &serde_json::Value) -> FreshnessOverview {
    FreshnessOverview {
        stale: v.get("stale").and_then(|x| x.as_i64()).unwrap_or(0),
        fresh: v.get("fresh").and_then(|x| x.as_i64()).unwrap_or(0),
        documents: v
            .get("documents")
            .and_then(|d| d.as_array())
            .map(|rows| {
                rows.iter()
                    .map(|d| FreshnessDocument {
                        id: d.get("id").and_then(|x| x.as_i64()).unwrap_or(0),
                        title: d
                            .get("title")
                            .and_then(|x| x.as_str())
                            .unwrap_or_default()
                            .to_string(),
                        freshness_status: d
                            .get("freshness_status")
                            .and_then(|x| x.as_str())
                            .unwrap_or_default()
                            .to_string(),
                        last_reviewed_at: d
                            .get("last_reviewed_at")
                            .and_then(|x| x.as_str())
                            .map(str::to_string),
                    })
                    .collect()
            })
            .unwrap_or_default(),
    }
}

/// The Freshness tab — stale vs fresh knowledge documents.
#[component]
fn FreshnessTab() -> impl IntoView {
    let overview = create_rw_signal(None::<FreshnessOverview>);
    let loading = create_rw_signal(true);
    let error_msg = create_rw_signal(None::<String>);

    create_effect(move |_| {
        let overview = overview;
        let loading = loading;
        let error_msg = error_msg;
        wasm_bindgen_futures::spawn_local(async move {
            match crate::api::get_json::<serde_json::Value>("/api/knowledge/freshness").await {
                Ok(v) => {
                    overview.set(Some(parse_freshness(&v)));
                    loading.set(false);
                }
                Err(e) => {
                    error_msg.set(Some(e));
                    loading.set(false);
                }
            }
        });
    });

    view! {
        <div class="spp-card">
            <h3 class="spp-card__title">"Knowledge freshness"</h3>
            <Show when=move || loading.get() fallback=|| ()>
                <LoadingState />
            </Show>
            <Show when=move || error_msg.get().is_some() fallback=|| ()>
                <div class="spp-state spp-state--error">
                    <p class="spp-state__body">{move || error_msg.get().unwrap_or_default()}</p>
                </div>
            </Show>
            {move || {
                let o = overview.clone().get();
                if loading.get() {
                    return ().into_view();
                }
                let Some(o) = o else {
                    return ().into_view();
                };
                view! {
                    <div class="spp-flex spp-gap-8 spp-mb-12">
                        <span class="spp-badge spp-badge--warn">{format!("{} stale", o.stale)}</span>
                        <span class="spp-badge spp-badge--ok">{format!("{} fresh", o.fresh)}</span>
                    </div>
                    {if o.documents.is_empty() {
                        view! { <EmptyState message="No documents to review yet." /> }.into_view()
                    } else {
                        o.documents
                            .iter()
                            .map(|d| {
                                let badge = if d.freshness_status == "stale" {
                                    "spp-badge spp-badge--warn"
                                } else {
                                    "spp-badge spp-badge--ok"
                                };
                                view! {
                                    <div class="spp-flex spp-flex--between" title=format!("last reviewed {}", d.last_reviewed_at.clone().unwrap_or_else(|| "never".into()))>
                                        <span class="spp-text-sm">{d.title.clone()}</span>
                                        <span class=badge>{d.freshness_status.clone()}</span>
                                    </div>
                                }
                            })
                            .collect::<Vec<_>>()
                            .into_view()
                    }}
                }.into_view()
            }}
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_sources_shapes() {
        let v = serde_json::json!({
            "sources": [
                { "id": 1, "name": "Product docs", "kind": "api", "visibility": "customer_safe", "document_count": 12 },
                { "id": 2, "name": "Internal runbook", "kind": "import", "visibility": "internal_only", "document_count": 0 }
            ]
        });
        let s = parse_sources(&v);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].name, "Product docs");
        assert_eq!(s[0].document_count, 12);
        assert_eq!(s[1].visibility, "internal_only");
        assert!(parse_sources(&serde_json::json!({})).is_empty());
    }

    #[test]
    fn parse_freshness_overview() {
        let v = serde_json::json!({
            "stale": 2,
            "fresh": 5,
            "documents": [
                { "id": 1, "title": "Reset password", "freshness_status": "stale", "last_reviewed_at": "2026-01-01" },
                { "id": 2, "title": "Exports", "freshness_status": "fresh", "last_reviewed_at": null }
            ]
        });
        let o = parse_freshness(&v);
        assert_eq!(o.stale, 2);
        assert_eq!(o.fresh, 5);
        assert_eq!(o.documents.len(), 2);
        assert_eq!(o.documents[0].freshness_status, "stale");
        assert!(o.documents[1].last_reviewed_at.is_none());
    }
}
