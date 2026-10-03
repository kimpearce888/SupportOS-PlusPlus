//! Docs page — `/docs`.
//!
//! Minimal reference-parity page (`pages/Docs.tsx`): aggregate stats, the
//! collection list, and the article table, fed by the Docs API
//! (`/api/docs/stats`, `/api/docs/collections`, `/api/docs/articles`).
//!
//! Per KNOWN PITFALLS: every view has loading, empty, and error states.

use leptos::*;

use crate::components::state_view::{EmptyState, LoadingState};

/// The Docs page — `/docs`.
#[component]
pub fn DocsPage() -> impl IntoView {
    let stats = create_rw_signal(None::<serde_json::Value>);
    let collections = create_rw_signal(Vec::<serde_json::Value>::new());
    let articles = create_rw_signal(Vec::<serde_json::Value>::new());
    let loading = create_rw_signal(true);
    let error_msg = create_rw_signal(None::<String>);

    create_effect(move |_| {
        let stats = stats;
        let collections = collections;
        let articles = articles;
        let loading = loading;
        let error_msg = error_msg;
        wasm_bindgen_futures::spawn_local(async move {
            let stats_result = crate::api::get_json::<serde_json::Value>("/api/docs/stats").await;
            let collections_result =
                crate::api::get_json::<serde_json::Value>("/api/docs/collections").await;
            let articles_result =
                crate::api::get_json::<serde_json::Value>("/api/docs/articles?limit=50").await;
            match (stats_result, collections_result, articles_result) {
                (Ok(s), Ok(c), Ok(a)) => {
                    stats.set(Some(s));
                    collections.set(
                        c.get("collections")
                            .and_then(|v| v.as_array())
                            .cloned()
                            .unwrap_or_default(),
                    );
                    articles.set(
                        a.get("articles")
                            .and_then(|v| v.as_array())
                            .cloned()
                            .unwrap_or_default(),
                    );
                    loading.set(false);
                }
                (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => {
                    error_msg.set(Some(e));
                    loading.set(false);
                }
            }
        });
    });

    view! {
        <div class="spp-page spp-page--docs">
            <h2 class="spp-page__title">"Docs"</h2>
            <p class="spp-page__intro">
                "The documents knowledge base: collections, articles, and freshness across every synced source."
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
                <div class="spp-docs__stats">
                    {move || {
                        let s = stats.get().unwrap_or_default();
                        let total = s.get("total_articles").and_then(|v| v.as_i64()).unwrap_or(0);
                        let fresh = s.get("fresh").and_then(|v| v.as_i64()).unwrap_or(0);
                        let stale = s.get("stale").and_then(|v| v.as_i64()).unwrap_or(0);
                        let sources = s.get("sources").and_then(|v| v.as_i64()).unwrap_or(0);
                        view! {
                            <div class="spp-docs__stat">
                                <span class="spp-docs__stat-value">{total.to_string()}</span>
                                <span class="spp-docs__stat-label">"articles"</span>
                            </div>
                            <div class="spp-docs__stat">
                                <span class="spp-docs__stat-value">{sources.to_string()}</span>
                                <span class="spp-docs__stat-label">"sources"</span>
                            </div>
                            <div class="spp-docs__stat">
                                <span class="spp-docs__stat-value">{fresh.to_string()}</span>
                                <span class="spp-docs__stat-label">"fresh"</span>
                            </div>
                            <div class="spp-docs__stat">
                                <span class="spp-docs__stat-value">{stale.to_string()}</span>
                                <span class="spp-docs__stat-label">"stale"</span>
                            </div>
                        }
                    }}
                </div>

                <Show
                    when=move || !collections.with(|c| c.is_empty())
                    fallback=|| ()
                >
                    <section class="spp-docs__collections">
                        <h3>"Collections"</h3>
                        <ul class="spp-docs__collection-list">
                            {move || {
                                collections.with(|cs| {
                                    cs.iter()
                                        .map(|c| {
                                            let name = c
                                                .get("name")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("(unnamed)")
                                                .to_string();
                                            let count = c
                                                .get("article_count")
                                                .and_then(|v| v.as_i64())
                                                .unwrap_or(0);
                                            view! {
                                                <li class="spp-docs__collection">
                                                    <span>{name}</span>
                                                    <span class="spp-badge">{count.to_string()}</span>
                                                </li>
                                            }
                                        })
                                        .collect::<Vec<_>>()
                                })
                            }}
                        </ul>
                    </section>
                </Show>

                <Show
                    when=move || !articles.with(|a| a.is_empty())
                    fallback=|| {
                        view! {
                            <EmptyState message="No doc articles yet. They appear once knowledge docs are imported or synced." />
                        }
                    }
                >
                    <table class="spp-table">
                        <thead>
                            <tr>
                                <th>"Article"</th>
                                <th>"Source"</th>
                                <th>"Freshness"</th>
                                <th>"Last reviewed"</th>
                            </tr>
                        </thead>
                        <tbody>
                            {move || {
                                articles.with(|arts| {
                                    arts.iter()
                                        .map(|a| {
                                            let title = a
                                                .get("title")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("(untitled)")
                                                .to_string();
                                            let source = a
                                                .get("source")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("—")
                                                .to_string();
                                            let freshness = a
                                                .get("freshness_status")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("unknown")
                                                .to_string();
                                            let reviewed = a
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
    // The Docs page's UI rendering is verified by the wasm test runner in
    // CI. The fetch paths are exercised end-to-end by the HTTP tests for
    // /api/docs/* in the core crate.
}
