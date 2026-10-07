//! Docs page — `/docs`.
//!
//! Reference-parity page (`pages/Docs.tsx`): aggregate stats, the collection
//! list, the article table, fed by the Docs API (`/api/docs/stats`,
//! `/api/docs/collections`, `/api/docs/articles`), and the article reader
//! (UI-27): `/docs?article=N` opens that article's reader — the deep link
//! the reference's search results use; a row click opens it too.
//!
//! Per KNOWN PITFALLS: every view has loading, empty, and error states.

use std::rc::Rc;

use leptos::*;
use leptos_router::{use_location, use_navigate, use_query_map};

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

    // ── Deep link (UI-27): /docs?article=N opens that article's reader ──
    // The reference reads `searchParams.get('article')` on mount and on
    // every URL change; search-result hits link here.
    let query_map = use_query_map();
    let location = use_location();
    let navigate = use_navigate();
    let reading = create_rw_signal(None::<i64>);
    create_effect(move |_| {
        let m = query_map.get();
        let article = crate::url_state::query_pos_int(&m, "article");
        // Number.isFinite guard parity: garbage means "no reader".
        if reading.get_untracked() != article {
            reading.set(article);
        }
    });
    let open_article: Rc<dyn Fn(i64)> = {
        let navigate = navigate.clone();
        let pathname = location.pathname;
        Rc::new(move |id: i64| {
            reading.set(Some(id));
            navigate(
                &format!(
                    "{}{}",
                    pathname.get_untracked(),
                    crate::url_state::query_string(&[("article", Some(id.to_string()))])
                ),
                leptos_router::NavigateOptions {
                    replace: true,
                    ..Default::default()
                },
            );
        })
    };
    let close_article: Rc<dyn Fn()> = {
        let navigate = navigate.clone();
        let pathname = location.pathname;
        Rc::new(move || {
            reading.set(None);
            navigate(
                &pathname.get_untracked(),
                leptos_router::NavigateOptions {
                    replace: true,
                    ..Default::default()
                },
            );
        })
    };
    // Copy handles for the Fn view closures (the close-handler pattern).
    let open_article_stored = StoredValue::new(open_article);
    let close_article_stored = StoredValue::new(close_article);

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
                                <th>"Collection"</th>
                                <th>"Status"</th>
                                <th>"Views"</th>
                                <th>"Updated"</th>
                            </tr>
                        </thead>
                        <tbody>
                            {move || {
                                articles.with(|arts| {
                                    arts.iter()
                                        .map(|a| {
                                            let id = a.get("id").and_then(|v| v.as_i64()).unwrap_or_default();
                                            let name = a
                                                .get("name")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("(untitled)")
                                                .to_string();
                                            let preview = a
                                                .get("preview")
                                                .and_then(|v| v.as_str())
                                                .map(|p| p.chars().take(110).collect::<String>())
                                                .unwrap_or_default();
                                            let collection = a
                                                .get("collection_name")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("—")
                                                .to_string();
                                            let category = a
                                                .get("category_name")
                                                .and_then(|v| v.as_str())
                                                .map(str::to_string);
                                            let status = a
                                                .get("status")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("—")
                                                .to_string();
                                            let views = a.get("views").and_then(|v| v.as_i64());
                                            let updated = a
                                                .get("remote_updated_at")
                                                .or_else(|| a.get("remote_created_at"))
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("")
                                                .get(..10)
                                                .unwrap_or("")
                                                .to_string();
                                            let open_row = open_article_stored.with_value(Rc::clone);
                                            view! {
                                                <tr class="spp-table__row-clickable" on:click=move |_| open_row(id)>
                                                    <td>
                                                        <strong>{name}</strong>
                                                        {if !preview.is_empty() {
                                                            view! {
                                                                <div class="spp-muted spp-text-xs">{format!("{preview}…")}</div>
                                                            }.into_view()
                                                        } else {
                                                            ().into_view()
                                                        }}
                                                    </td>
                                                    <td>
                                                        {collection}
                                                        {if let Some(category) = category {
                                                            view! {
                                                                <div class="spp-muted spp-text-xs">{category}</div>
                                                            }.into_view()
                                                        } else {
                                                            ().into_view()
                                                        }}
                                                    </td>
                                                    <td>
                                                        <span class=match status.as_str() {
                                                            "published" => "spp-badge spp-badge--ok",
                                                            "internal" => "spp-badge spp-badge--warn",
                                                            _ => "spp-badge",
                                                        }>
                                                            {status}
                                                        </span>
                                                    </td>
                                                    <td class="spp-table__cell-muted">
                                                        {if let Some(v) = views { v.to_string() } else { "—".to_string() }}
                                                    </td>
                                                    <td class="spp-table__cell-muted">{updated}</td>
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

            // The article reader (deep link / row click).
            <Show when=move || reading.get().is_some() fallback=|| ()>
                {move || {
                    let id = reading.get().unwrap_or_default();
                    let close = close_article_stored.with_value(Rc::clone);
                    view! { <ArticleReader id=id on_close=close /> }
                }}
            </Show>
        </div>
    }
}

/// The article reader modal (UI-27 deep link target + the row click).
/// Reference Docs.tsx: status/collection/category/views badges, the full
/// text, and the mirrored read-only note. A failed fetch (or a stale
/// ?article= link) shows an error — never an infinite spinner.
#[component]
fn ArticleReader(id: i64, on_close: Rc<dyn Fn()>) -> impl IntoView {
    let article = create_rw_signal(None::<serde_json::Value>);
    let loading = create_rw_signal(true);
    let error_msg = create_rw_signal(None::<String>);

    create_effect(move |_| {
        let article = article;
        let loading = loading;
        let error_msg = error_msg;
        loading.set(true);
        wasm_bindgen_futures::spawn_local(async move {
            let path = format!("/api/docs/articles/{id}");
            match crate::api::get_json::<serde_json::Value>(&path).await {
                Ok(v) => {
                    article.set(v.get("article").cloned());
                    error_msg.set(None);
                }
                Err(e) => error_msg.set(Some(e)),
            }
            loading.set(false);
        });
    });

    view! {
        <div class="spp-overlay" role="dialog" aria-modal="true" aria-label="Article reader">
            <div class="spp-modal spp-modal--reader">
                <div class="spp-modal__head">
                    <h3 class="spp-modal__title">
                        {move || {
                            article
                                .get()
                                .and_then(|a| a.get("name").and_then(|n| n.as_str()).map(str::to_string))
                                .unwrap_or_else(|| "Article".to_string())
                        }}
                    </h3>
                    <button
                        class="spp-button spp-button--ghost spp-button--tiny"
                        aria-label="Close reader"
                        on:click=move |_| on_close()
                    >
                        "\u{d7}"
                    </button>
                </div>
                <Show when=move || loading.get() fallback=|| ()>
                    <LoadingState />
                </Show>
                <Show when=move || error_msg.get().is_some() fallback=|| ()>
                    <div class="spp-state spp-state--error">
                        <span class="spp-state__icon" aria-hidden="true">"⚠"</span>
                        <p class="spp-state__body">"Could not open this article."</p>
                        <p class="spp-muted spp-text-xs">{move || error_msg.get().unwrap_or_default()}</p>
                    </div>
                </Show>
                <Show
                    when=move || !loading.get() && error_msg.get().is_none()
                    fallback=|| ()
                >
                    {move || {
                        let a = match article.get() {
                            Some(a) => a,
                            None => return view! { <div></div> }.into_view(),
                        };
                        let status = a.get("status").and_then(|v| v.as_str()).unwrap_or("—").to_string();
                        let collection = a
                            .get("collection_name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("—")
                            .to_string();
                        let category = a
                            .get("category_name")
                            .and_then(|v| v.as_str())
                            .map(str::to_string);
                        let views = a.get("views").and_then(|v| v.as_i64());
                        let text = a.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string();
                        view! {
                            <div class="spp-doc-reader__badges">
                                <span class=match status.as_str() {
                                    "published" => "spp-badge spp-badge--ok",
                                    "internal" => "spp-badge spp-badge--warn",
                                    _ => "spp-badge",
                                }>
                                    {status}
                                </span>
                                <span class="spp-badge">{collection}</span>
                                {if let Some(category) = category {
                                    view! { <span class="spp-badge">{category}</span> }.into_view()
                                } else {
                                    ().into_view()
                                }}
                                {if let Some(views) = views {
                                    view! {
                                        <span class="spp-badge">{format!("{views} views")}</span>
                                    }.into_view()
                                } else {
                                    ().into_view()
                                }}
                            </div>
                            {if text.is_empty() {
                                view! {
                                    <div class="spp-doc-reader__content spp-muted">
                                        "No text stored for this article."
                                    </div>
                                }.into_view()
                            } else {
                                view! { <div class="spp-doc-reader__content">{text}</div> }.into_view()
                            }}
                            <p class="spp-muted spp-text-xs">
                                "Mirrored read-only from Help Scout Docs; edits happen in Help Scout and arrive on the next sync."
                            </p>
                        }.into_view()
                    }}
                </Show>
            </div>
        </div>
    }
}

#[cfg(test)]
mod tests {
    // The Docs page's UI rendering is verified by the wasm test runner in
    // CI. The fetch paths are exercised end-to-end by the HTTP tests for
    // /api/docs/* in the core crate.
}
