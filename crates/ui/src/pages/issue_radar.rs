//! Issue Radar page — the `/issues` route (UI-27 `?tab=` deep link).
//!
//! Reference pages/Issues.tsx: five tabs — Issue Radar, Clusters, Known
//! Issues, Doc Gaps, Answer Reuse — each fed by its existing API. The
//! `?tab=` query param is a deep-link target (search results link to
//! `/issues?tab=known`): it seeds the tab on mount and re-syncs on every
//! URL change (browser back / a new link into the page).
//!
//! The tab depth here is the list-level view (the counts, titles and
//! conversation links); the full per-tab feature depth (drill-downs,
//! editors, filters) is tracked separately as UI-07.
//!
//! Per KNOWN PITFALLS: every view has loading, empty, and error states.

use leptos::*;
use leptos_router::use_query_map;

use crate::components::state_view::{EmptyState, LoadingState};

/// The five tabs (reference Issues.tsx tab order).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssuesTab {
    Radar,
    Clusters,
    Known,
    Gaps,
    Reuse,
}

impl IssuesTab {
    /// The wire/tab-key name (the `?tab=` vocabulary).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Radar => "radar",
            Self::Clusters => "clusters",
            Self::Known => "known",
            Self::Gaps => "gaps",
            Self::Reuse => "reuse",
        }
    }

    /// Parse the `?tab=` param (reference: unknown values fall back to
    /// 'radar').
    #[must_use]
    pub fn from_param(raw: Option<&str>) -> Self {
        match raw {
            Some("clusters") => Self::Clusters,
            Some("known") => Self::Known,
            Some("gaps") => Self::Gaps,
            Some("reuse") => Self::Reuse,
            _ => Self::Radar,
        }
    }
}

/// The Issue Radar page.
#[component]
pub fn IssueRadarPage() -> impl IntoView {
    // ── Deep link (UI-27): /issues?tab=known opens that tab directly ────
    // Used by search results (the search engine's known-issue hits link to
    // /issues?tab=known) — previously a dead link.
    let query_map = use_query_map();
    let tab = create_rw_signal(IssuesTab::Radar);
    create_effect(move |_| {
        let m = query_map.get();
        let next = IssuesTab::from_param(crate::url_state::query_str(&m, "tab").as_deref());
        if tab.get_untracked() != next {
            tab.set(next);
        }
    });

    view! {
        <div class="spp-page spp-page--issue-radar">
            <h2 class="spp-page__title">"Issues"</h2>

            <p class="spp-page__intro">
                "Issue intelligence from your local data — every alert links to underlying tickets; correlations are never claimed as causes."
            </p>

            <div class="spp-tabs">
                <button
                    class="spp-tab"
                    class:is-active=move || tab.get() == IssuesTab::Radar
                    on:click=move |_| tab.set(IssuesTab::Radar)
                >
                    "Issue Radar"
                </button>
                <button
                    class="spp-tab"
                    class:is-active=move || tab.get() == IssuesTab::Clusters
                    on:click=move |_| tab.set(IssuesTab::Clusters)
                >
                    "Clusters"
                </button>
                <button
                    class="spp-tab"
                    class:is-active=move || tab.get() == IssuesTab::Known
                    on:click=move |_| tab.set(IssuesTab::Known)
                >
                    "Known Issues"
                </button>
                <button
                    class="spp-tab"
                    class:is-active=move || tab.get() == IssuesTab::Gaps
                    on:click=move |_| tab.set(IssuesTab::Gaps)
                >
                    "Doc Gaps"
                </button>
                <button
                    class="spp-tab"
                    class:is-active=move || tab.get() == IssuesTab::Reuse
                    on:click=move |_| tab.set(IssuesTab::Reuse)
                >
                    "Answer Reuse"
                </button>
            </div>

            <Show when=move || tab.get() == IssuesTab::Radar fallback=|| ()>
                <RadarTab />
            </Show>
            <Show when=move || tab.get() == IssuesTab::Clusters fallback=|| ()>
                <ClustersTab />
            </Show>
            <Show when=move || tab.get() == IssuesTab::Known fallback=|| ()>
                <KnownTab />
            </Show>
            <Show when=move || tab.get() == IssuesTab::Gaps fallback=|| ()>
                <GapsTabLite />
            </Show>
            <Show when=move || tab.get() == IssuesTab::Reuse fallback=|| ()>
                <ReuseTab />
            </Show>
        </div>
    }
}

/// Shared list-load plumbing for the simple tabs: GET `path`, surface
/// `rows` (a closure picking the array out of the response envelope).
fn load_list(
    path: String,
    pick: impl Fn(&serde_json::Value) -> Vec<serde_json::Value> + 'static,
) -> (
    RwSignal<Vec<serde_json::Value>>,
    RwSignal<bool>,
    RwSignal<Option<String>>,
) {
    let rows = create_rw_signal(Vec::<serde_json::Value>::new());
    let loading = create_rw_signal(true);
    let error_msg = create_rw_signal(None::<String>);
    wasm_bindgen_futures::spawn_local(async move {
        match crate::api::get_json::<serde_json::Value>(&path).await {
            Ok(v) => rows.set(pick(&v)),
            Err(e) => error_msg.set(Some(e)),
        }
        loading.set(false);
    });
    (rows, loading, error_msg)
}

/// The loading/empty/error scaffolding every tab shares.
#[component]
fn ListStates(
    loading: RwSignal<bool>,
    error_msg: RwSignal<Option<String>>,
    empty: &'static str,
) -> impl IntoView {
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
            <EmptyState message=empty />
        </Show>
    }
}

/// ── Radar tab: the counts + the alert list (the pre-UI-27 page). ────────
#[component]
fn RadarTab() -> impl IntoView {
    let snapshot = create_rw_signal(serde_json::json!({}));
    let alerts = create_rw_signal(Vec::<serde_json::Value>::new());
    let loading = create_rw_signal(true);
    let error_msg = create_rw_signal(None::<String>);

    create_effect(move |_| {
        let snapshot = snapshot;
        let alerts = alerts;
        let loading = loading;
        let error_msg = error_msg;
        // Cross-page invalidation: 'dashboard' bumps refresh the radar card
        // in the reference via the same query root; the radar list itself
        // refetches with it.
        let _ = crate::queries::version("dashboard").get();
        wasm_bindgen_futures::spawn_local(async move {
            match crate::api::get_json::<serde_json::Value>("/api/reports/issue-radar").await {
                Ok(data) => {
                    let snap = data
                        .get("alerts")
                        .and_then(|v| v.as_array())
                        .and_then(|arr| arr.first())
                        .cloned()
                        .unwrap_or(data.clone());
                    alerts.set(
                        data.get("alerts")
                            .and_then(|v| v.as_array())
                            .cloned()
                            .unwrap_or_default(),
                    );
                    snapshot.set(snap);
                }
                Err(e) => error_msg.set(Some(e)),
            }
            loading.set(false);
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
            {move || {
                let s = snapshot.get();
                let active_known_issues = s.get("active_known_issues").and_then(|v| v.as_u64()).unwrap_or(0);
                let active_clusters = s.get("active_clusters").and_then(|v| v.as_u64()).unwrap_or(0);
                let active_incidents = s.get("active_incidents").and_then(|v| v.as_u64()).unwrap_or(0);

                view! {
                    <div class="spp-radar-grid">
                        <div class="spp-radar-card">
                            <span class="spp-radar-card__label">"Active known issues"</span>
                            <span class="spp-radar-card__value">{active_known_issues.to_string()}</span>
                            <span class="spp-radar-card__hint">"Documented issues currently affecting customers"</span>
                        </div>
                        <div class="spp-radar-card">
                            <span class="spp-radar-card__label">"Active clusters"</span>
                            <span class="spp-radar-card__value">{active_clusters.to_string()}</span>
                            <span class="spp-radar-card__hint">"Groups of similar issues identified by clustering"</span>
                        </div>
                        <div class="spp-radar-card spp-radar-card--incident">
                            <span class="spp-radar-card__label">"Active incidents"</span>
                            <span class="spp-radar-card__value">{active_incidents.to_string()}</span>
                            <span class="spp-radar-card__hint">"Unresolved incidents (status != resolved)"</span>
                        </div>
                    </div>

                    <Show
                        when=move || !alerts.with(|a| a.is_empty())
                        fallback=move || {
                            view! {
                                <EmptyState message="No active issues, clusters, or incidents. The radar is clear." />
                            }
                        }
                    >
                        <div class="spp-card">
                            <h3 class="spp-card__title">"Alerts"</h3>
                            <ul class="spp-issues__alert-list">
                                {move || alerts.get().iter().map(|a| {
                                    let title = a.get("title").and_then(|v| v.as_str()).unwrap_or("(alert)").to_string();
                                    let detail = a.get("detail").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                    let severity = a.get("severity").and_then(|v| v.as_str()).unwrap_or("info").to_string();
                                    let conv_ids = a.get("conversation_ids")
                                        .and_then(|v| v.as_array())
                                        .map(|arr| arr.iter().filter_map(|c| c.as_i64()).collect::<Vec<_>>())
                                        .unwrap_or_default();
                                    view! {
                                        <li class=match severity.as_str() {
                                            "critical" => "spp-issues__alert spp-issues__alert--critical",
                                            "warning" => "spp-issues__alert spp-issues__alert--warning",
                                            _ => "spp-issues__alert spp-issues__alert--info",
                                        }>
                                            <strong>{title}</strong>
                                            <p class="spp-muted spp-text-xs">{detail}</p>
                                            <div class="spp-issues__alert-convs">
                                                {conv_ids.iter().take(6).map(|cid| {
                                                    view! {
                                                        <a class="spp-badge" href={format!("/inbox/conversation/{cid}")}>
                                                            {format!("#{cid}")}
                                                        </a>
                                                    }
                                                }).collect::<Vec<_>>()}
                                            </div>
                                        </li>
                                    }
                                }).collect::<Vec<_>>()}
                            </ul>
                        </div>
                    </Show>
                }.into_view()
            }}
        </Show>
    }
}

/// ── Clusters tab: the cluster list from /api/issues/clusters. ─────────
#[component]
fn ClustersTab() -> impl IntoView {
    let (clusters, loading, error_msg) = load_list("/api/issues/clusters".to_string(), |v| {
        v.get("clusters")
            .and_then(|c| c.as_array())
            .cloned()
            .unwrap_or_default()
    });

    view! {
        <ListStates loading=loading error_msg=error_msg empty="No issue clusters yet. Clusters appear after AI analyses group similar tickets." />
        <Show
            when=move || !loading.get() && error_msg.get().is_none() && !clusters.with(|c| c.is_empty())
            fallback=|| ()
        >
            <div class="spp-card">
                <table class="spp-table">
                    <thead>
                        <tr>
                            <th>"Cluster"</th>
                            <th>"Category"</th>
                            <th>"Conversations"</th>
                            <th>"Customers"</th>
                            <th>"Trend"</th>
                        </tr>
                    </thead>
                    <tbody>
                        {move || clusters.get().iter().map(|c| {
                            let title = c.get("title").and_then(|v| v.as_str()).unwrap_or("(untitled)").to_string();
                            let summary = c.get("summary").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            let category = c.get("category").and_then(|v| v.as_str()).unwrap_or("—").to_string();
                            let count = c.get("conversation_count").and_then(|v| v.as_i64()).unwrap_or(0);
                            let customers = c.get("customer_count").and_then(|v| v.as_i64()).unwrap_or(0);
                            let trend = c.get("trend").and_then(|v| v.as_str()).unwrap_or("flat").to_string();
                            view! {
                                <tr>
                                    <td>
                                        <strong>{title}</strong>
                                        {if !summary.is_empty() {
                                            view! { <div class="spp-muted spp-text-xs">{summary}</div> }.into_view()
                                        } else {
                                            ().into_view()
                                        }}
                                    </td>
                                    <td class="spp-table__cell-muted">{category}</td>
                                    <td><span class="spp-badge">{count.to_string()}</span></td>
                                    <td class="spp-table__cell-muted">{customers.to_string()}</td>
                                    <td><span class="spp-badge">{trend}</span></td>
                                </tr>
                            }
                        }).collect::<Vec<_>>()}
                    </tbody>
                </table>
            </div>
        </Show>
    }
}

/// ── Known Issues tab: the list from /api/issues/known. ─────────────────
#[component]
fn KnownTab() -> impl IntoView {
    let (known, loading, error_msg) = load_list("/api/issues/known".to_string(), |v| {
        v.get("known_issues")
            .and_then(|k| k.as_array())
            .cloned()
            .unwrap_or_default()
    });

    view! {
        <ListStates loading=loading error_msg=error_msg empty="No known issues documented yet." />
        <Show
            when=move || !loading.get() && error_msg.get().is_none() && !known.with(|k| k.is_empty())
            fallback=|| ()
        >
            <div class="spp-card">
                <table class="spp-table">
                    <thead>
                        <tr>
                            <th>"Issue"</th>
                            <th>"Status"</th>
                            <th>"Conversations"</th>
                            <th>"Provenance"</th>
                        </tr>
                    </thead>
                    <tbody>
                        {move || known.get().iter().map(|k| {
                            let title = k.get("title").and_then(|v| v.as_str()).unwrap_or("(untitled)").to_string();
                            let symptoms = k.get("symptoms").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            let status = k.get("status").and_then(|v| v.as_str()).unwrap_or("open").to_string();
                            let count = k.get("conversation_count").and_then(|v| v.as_i64()).unwrap_or(0);
                            let provenance = k.get("provenance").and_then(|v| v.as_str()).unwrap_or("—").to_string();
                            view! {
                                <tr>
                                    <td>
                                        <strong>{title}</strong>
                                        {if !symptoms.is_empty() {
                                            view! { <div class="spp-muted spp-text-xs">{symptoms}</div> }.into_view()
                                        } else {
                                            ().into_view()
                                        }}
                                    </td>
                                    <td>
                                        <span class=match status.as_str() {
                                            "resolved" | "mitigated" => "spp-badge spp-badge--ok",
                                            "investigating" => "spp-badge spp-badge--warn",
                                            _ => "spp-badge",
                                        }>
                                            {status}
                                        </span>
                                    </td>
                                    <td><span class="spp-badge">{count.to_string()}</span></td>
                                    <td class="spp-table__cell-muted">{provenance}</td>
                                </tr>
                            }
                        }).collect::<Vec<_>>()}
                    </tbody>
                </table>
            </div>
        </Show>
    }
}

/// ── Doc Gaps tab (list-level): the open gap candidates from
/// /api/knowledge/gaps. The full decide/draft workflow stays with the
/// Knowledge page's gap tab. ────────────────────────────────────────────
#[component]
fn GapsTabLite() -> impl IntoView {
    let rows = create_rw_signal(Vec::<serde_json::Value>::new());
    let loading = create_rw_signal(true);
    let error_msg = create_rw_signal(None::<String>);

    wasm_bindgen_futures::spawn_local(async move {
        match crate::api::get_json::<serde_json::Value>("/api/knowledge/gaps").await {
            Ok(v) => {
                // Flatten kinds[].candidates[] into one list, newest open first.
                let mut flat: Vec<serde_json::Value> = v
                    .get("kinds")
                    .and_then(|k| k.as_array())
                    .map(|kinds| {
                        kinds
                            .iter()
                            .filter_map(|kind| {
                                kind.get("candidates").and_then(|c| c.as_array()).cloned()
                            })
                            .flatten()
                            .collect()
                    })
                    .unwrap_or_default();
                flat.sort_by_key(|c| {
                    std::cmp::Reverse(
                        c.get("occurrence_count")
                            .and_then(|o| o.as_i64())
                            .unwrap_or(0),
                    )
                });
                rows.set(flat);
            }
            Err(e) => error_msg.set(Some(e)),
        }
        loading.set(false);
    });

    view! {
        <ListStates loading=loading error_msg=error_msg empty="No knowledge gaps detected. Gaps appear when recurring questions have no matching document." />
        <Show
            when=move || !loading.get() && error_msg.get().is_none() && !rows.with(|r| r.is_empty())
            fallback=|| ()
        >
            <div class="spp-card">
                <table class="spp-table">
                    <thead>
                        <tr>
                            <th>"Gap"</th>
                            <th>"Kind"</th>
                            <th>"Occurrences"</th>
                            <th>"Status"</th>
                        </tr>
                    </thead>
                    <tbody>
                        {move || rows.get().iter().map(|g| {
                            let query_text = g.get("query_text").and_then(|v| v.as_str()).unwrap_or("(no text)").to_string();
                            let kind = g.get("kind").and_then(|v| v.as_str()).unwrap_or("—").to_string();
                            let occurrences = g.get("occurrence_count").and_then(|v| v.as_i64()).unwrap_or(0);
                            let status = g.get("status").and_then(|v| v.as_str()).unwrap_or("open").to_string();
                            view! {
                                <tr>
                                    <td>{query_text}</td>
                                    <td class="spp-table__cell-muted">{kind}</td>
                                    <td><span class="spp-badge">{occurrences.to_string()}</span></td>
                                    <td>
                                        <span class=match status.as_str() {
                                            "approved" => "spp-badge spp-badge--ok",
                                            "rejected" => "spp-badge spp-badge--warn",
                                            _ => "spp-badge",
                                        }>
                                            {status}
                                        </span>
                                    </td>
                                </tr>
                            }
                        }).collect::<Vec<_>>()}
                    </tbody>
                </table>
            </div>
        </Show>
    }
}

/// ── Answer Reuse tab: /api/reports/answer-reuse. ────────────────────────
#[component]
fn ReuseTab() -> impl IntoView {
    let (rows, loading, error_msg) =
        load_list("/api/reports/answer-reuse?days=90".to_string(), |v| {
            v.get("candidates")
                .and_then(|c| c.as_array())
                .cloned()
                .unwrap_or_default()
        });

    view! {
        <ListStates loading=loading error_msg=error_msg empty="No repeated questions detected in the last 90 days." />
        <Show
            when=move || !loading.get() && error_msg.get().is_none() && !rows.with(|r| r.is_empty())
            fallback=|| ()
        >
            <div class="spp-card">
                <table class="spp-table">
                    <thead>
                        <tr>
                            <th>"Question"</th>
                            <th>"Conversations"</th>
                            <th>"Resolution"</th>
                        </tr>
                    </thead>
                    <tbody>
                        {move || rows.get().iter().map(|r| {
                            let question = r.get("question").and_then(|v| v.as_str()).unwrap_or("(no question)").to_string();
                            let count = r.get("conversation_count").and_then(|v| v.as_i64()).unwrap_or(0);
                            let resolution = r.get("common_resolution").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            let resolution: String = resolution.chars().take(160).collect();
                            view! {
                                <tr>
                                    <td>{question}</td>
                                    <td><span class="spp-badge">{count.to_string()}</span></td>
                                    <td class="spp-table__cell-muted">{resolution}</td>
                                </tr>
                            }
                        }).collect::<Vec<_>>()}
                    </tbody>
                </table>
            </div>
        </Show>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_param_vocabulary_matches_reference() {
        // The five reference tabs, in order.
        assert_eq!(IssuesTab::Radar.as_str(), "radar");
        assert_eq!(IssuesTab::Clusters.as_str(), "clusters");
        assert_eq!(IssuesTab::Known.as_str(), "known");
        assert_eq!(IssuesTab::Gaps.as_str(), "gaps");
        assert_eq!(IssuesTab::Reuse.as_str(), "reuse");
    }

    #[test]
    fn tab_param_parses_and_falls_back_to_radar() {
        // Deep links: /issues?tab=known etc. Unknown/absent → radar.
        assert_eq!(IssuesTab::from_param(Some("known")), IssuesTab::Known);
        assert_eq!(IssuesTab::from_param(Some("clusters")), IssuesTab::Clusters);
        assert_eq!(IssuesTab::from_param(Some("gaps")), IssuesTab::Gaps);
        assert_eq!(IssuesTab::from_param(Some("reuse")), IssuesTab::Reuse);
        assert_eq!(IssuesTab::from_param(Some("radar")), IssuesTab::Radar);
        // Garbage and missing fall back like the reference.
        assert_eq!(IssuesTab::from_param(Some("nonsense")), IssuesTab::Radar);
        assert_eq!(IssuesTab::from_param(None), IssuesTab::Radar);
    }
}
