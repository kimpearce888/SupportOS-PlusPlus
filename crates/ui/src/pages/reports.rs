//! Reports page — the tabbed report center.
//!
//! Reference pages/Reports.tsx: every number is labeled (local calculation ·
//! Help Scout · AI-derived). This port lands the M5 tab set (plan phases 28,
//! 29, 33) — Overview, Response effectiveness, Friction, and the 21×14
//! custom Report builder — on the reference tab shell; the remaining
//! reference tabs (SLA, questions, intelligence, Help Scout, definitions,
//! releases) stay on the backlog.
//!
//! Per KNOWN PITFALLS: "every view has loading, empty and error states."

use leptos::*;

use crate::components::state_view::{EmptyState, LoadingState};
use crate::components::{BuilderTab, EffectivenessTab, FrictionTab};

/// The Reports page's tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportsTab {
    Overview,
    Effectiveness,
    Friction,
    Builder,
}

/// The Reports page.
#[component]
pub fn ReportsPage() -> impl IntoView {
    let tab = create_rw_signal(ReportsTab::Overview);

    view! {
        <div class="spp-page spp-page--reports">
            <h2 class="spp-page__title">"Reports"</h2>
            <p class="spp-page__intro">
                "Every number is labeled: Help Scout (native API) · local calculation · AI-derived"
            </p>

            <div class="spp-tabs">
                <button
                    class="spp-tab"
                    class:is-active=move || tab.get() == ReportsTab::Overview
                    on:click=move |_| tab.set(ReportsTab::Overview)
                >
                    "Overview"
                </button>
                <button
                    class="spp-tab"
                    class:is-active=move || tab.get() == ReportsTab::Effectiveness
                    on:click=move |_| tab.set(ReportsTab::Effectiveness)
                >
                    "Response effectiveness"
                </button>
                <button
                    class="spp-tab"
                    class:is-active=move || tab.get() == ReportsTab::Friction
                    on:click=move |_| tab.set(ReportsTab::Friction)
                >
                    "Friction"
                </button>
                <button
                    class="spp-tab"
                    class:is-active=move || tab.get() == ReportsTab::Builder
                    on:click=move |_| tab.set(ReportsTab::Builder)
                >
                    "Report builder"
                </button>
            </div>

            <Show when=move || tab.get() == ReportsTab::Overview fallback=|| ()>
                <OverviewTab />
            </Show>
            <Show when=move || tab.get() == ReportsTab::Effectiveness fallback=|| ()>
                <EffectivenessTab />
            </Show>
            <Show when=move || tab.get() == ReportsTab::Friction fallback=|| ()>
                <FrictionTab />
            </Show>
            <Show when=move || tab.get() == ReportsTab::Builder fallback=|| ()>
                <BuilderTab />
            </Show>
        </div>
    }
}

/// One overview KPI row.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OverviewFacts {
    pub new_conversations: i64,
    pub replies_sent: i64,
    pub closed_conversations: i64,
    pub ratings_great: i64,
    pub ratings_okay: i64,
    pub ratings_not_good: i64,
}

/// Parse the GET /api/analytics/dashboard?daysBack=N response's overview facts.
#[must_use]
pub fn parse_overview(v: &serde_json::Value) -> OverviewFacts {
    OverviewFacts {
        new_conversations: v
            .get("new_conversations")
            .and_then(|x| x.as_i64())
            .unwrap_or(0),
        replies_sent: v.get("replies_sent").and_then(|x| x.as_i64()).unwrap_or(0),
        closed_conversations: v
            .get("closed_conversations")
            .and_then(|x| x.as_i64())
            .unwrap_or(0),
        ratings_great: v
            .pointer("/ratings/great")
            .and_then(|x| x.as_i64())
            .unwrap_or(0),
        ratings_okay: v
            .pointer("/ratings/okay")
            .and_then(|x| x.as_i64())
            .unwrap_or(0),
        ratings_not_good: v
            .pointer("/ratings/not-good")
            .and_then(|x| x.as_i64())
            .unwrap_or(0),
    }
}

/// The Overview tab — local volume KVs from the dashboard analytics.
#[component]
fn OverviewTab() -> impl IntoView {
    let facts = create_rw_signal(None::<OverviewFacts>);
    let loading = create_rw_signal(true);
    let error_msg = create_rw_signal(None::<String>);

    create_effect(move |_| {
        let facts = facts;
        let loading = loading;
        let error_msg = error_msg;
        wasm_bindgen_futures::spawn_local(async move {
            match crate::api::get_json::<serde_json::Value>("/api/analytics/dashboard?daysBack=30")
                .await
            {
                Ok(v) => {
                    facts.set(Some(parse_overview(&v)));
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
        <div class="spp-grid-2">
            <div class="spp-card">
                <h3 class="spp-card__title">"Volume (local)"</h3>
                <Show when=move || loading.get() fallback=|| ()>
                    <LoadingState />
                </Show>
                <Show when=move || error_msg.get().is_some() fallback=|| ()>
                    <div class="spp-state spp-state--error">
                        <p class="spp-state__body">"This report failed to load."</p>
                        <p class="spp-state__detail">{move || error_msg.get().unwrap_or_default()}</p>
                    </div>
                </Show>
                {move || {
                    let f = facts.clone().get();
                    if loading.get() {
                        return ().into_view();
                    }
                    let Some(f) = f else {
                        return ().into_view();
                    };
                    view! {
                        <div class="spp-flex spp-flex--col spp-gap-4">
                            <div class="spp-flex spp-flex--between">
                                <span class="spp-muted spp-text-sm">"New conversations"</span>
                                <span class="spp-text-sm">{f.new_conversations.to_string()}</span>
                            </div>
                            <div class="spp-flex spp-flex--between">
                                <span class="spp-muted spp-text-sm">"Replies sent"</span>
                                <span class="spp-text-sm">{f.replies_sent.to_string()}</span>
                            </div>
                            <div class="spp-flex spp-flex--between">
                                <span class="spp-muted spp-text-sm">"Closed in range"</span>
                                <span class="spp-text-sm">{f.closed_conversations.to_string()}</span>
                            </div>
                            <div class="spp-flex spp-flex--between">
                                <span class="spp-muted spp-text-sm">"Ratings"</span>
                                <span class="spp-text-sm">
                                    {format!(
                                        "{} great / {} okay / {} not-good",
                                        f.ratings_great, f.ratings_okay, f.ratings_not_good
                                    )}
                                </span>
                            </div>
                        </div>
                        <p class="spp-muted spp-text-xs spp-mt-8">
                            "All numbers are local calculations over the mirror (last 30 days)."
                        </p>
                    }.into_view()
                }}
            </div>
            <div class="spp-card">
                <h3 class="spp-card__title">"Where to look next"</h3>
                <p class="spp-muted spp-text-sm">
                    "The M5 quality tabs — response effectiveness, conversation friction and the custom report builder — surface observed associations with sample sizes and evidence links. Every metric ships its definition and limitations next to the numbers."
                </p>
                <EmptyState message="Response effectiveness and friction data appear once conversations are analyzed; the builder runs on demand." />
            </div>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_overview_facts() {
        let v = serde_json::json!({
            "new_conversations": 12,
            "replies_sent": 40,
            "closed_conversations": 9,
            "ratings": { "great": 3, "okay": 1, "not-good": 2 }
        });
        let f = parse_overview(&v);
        assert_eq!(f.new_conversations, 12);
        assert_eq!(f.replies_sent, 40);
        assert_eq!(f.closed_conversations, 9);
        assert_eq!(f.ratings_great, 3);
        assert_eq!(f.ratings_not_good, 2);
        let empty = parse_overview(&serde_json::json!({}));
        assert_eq!(empty.new_conversations, 0);
        assert_eq!(empty.ratings_okay, 0);
    }
}
