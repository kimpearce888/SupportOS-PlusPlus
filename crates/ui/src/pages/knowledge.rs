//! Knowledge page — `/knowledge` (the knowledge library).
//!
//! Minimal reference-parity page (`pages/Knowledge.tsx`): lists knowledge
//! documents from `GET /api/knowledge/documents` with their source,
//! freshness status, and last review date. The port's unified knowledge
//! store is `knowledge_doc_freshness`; imports, the doc reader, and the
//! freshness tab land with the knowledge-domain work.
//!
//! Per KNOWN PITFALLS: every view has loading, empty, and error states.

use leptos::*;

use crate::components::state_view::{EmptyState, LoadingState};

/// The Knowledge page — `/knowledge`.
#[component]
pub fn KnowledgePage() -> impl IntoView {
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
        <div class="spp-page spp-page--knowledge">
            <h2 class="spp-page__title">"Knowledge"</h2>
            <p class="spp-page__intro">
                "The knowledge library used for answer assist and issue clustering. Each document tracks freshness so stale answers surface."
            </p>

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
        </div>
    }
}

#[cfg(test)]
mod tests {
    // The Knowledge page's UI rendering is verified by the wasm test runner
    // in CI. The fetch path is exercised end-to-end by the HTTP tests for
    // /api/knowledge/documents in the core crate.
}
