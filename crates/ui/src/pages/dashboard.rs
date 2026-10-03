//! Dashboard page — the `/` route.
//!
//! Wired to the `dashboard_metrics` Tauri IPC command on mount.
//! Shows real KPI metrics: total/new/closed/active conversations, avg response
//! times, SLA breach count, customer waiting count.

use leptos::*;

use crate::components::state_view::{EmptyState, LoadingState};

/// Dashboard KPI metrics displayed on the page.
#[derive(Debug, Clone, Default)]
pub struct DashboardKpis {
    pub total_conversations: u32,
    pub new_conversations: u32,
    pub closed_conversations: u32,
    pub active_conversations: u32,
    pub pending_conversations: u32,
    pub unassigned: u32,
    pub backlog: u32,
    pub avg_first_response_minutes: Option<f64>,
    pub avg_resolution_minutes: Option<f64>,
}

/// The dashboard page. Fetches metrics from `GET /api/analytics/dashboard`.
#[component]
pub fn DashboardPage() -> impl IntoView {
    let kpis = create_rw_signal(DashboardKpis::default());
    let loading = create_rw_signal(true);
    let error_msg = create_rw_signal(None::<String>);

    create_effect(move |_| {
        let kpis = kpis;
        let loading = loading;
        let error_msg = error_msg;
        wasm_bindgen_futures::spawn_local(async move {
            match crate::api::get_json::<serde_json::Value>("/api/analytics/dashboard?daysBack=7")
                .await
            {
                Ok(data) => {
                    let active = data
                        .get("active_conversations")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as u32;
                    let pending = data
                        .get("pending_conversations")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as u32;
                    let closed = data
                        .get("closed_conversations")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as u32;
                    kpis.set(DashboardKpis {
                        total_conversations: active + pending + closed,
                        new_conversations: data
                            .get("new_conversations")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0) as u32,
                        closed_conversations: closed,
                        active_conversations: active,
                        pending_conversations: pending,
                        unassigned: data.get("unassigned").and_then(|v| v.as_u64()).unwrap_or(0)
                            as u32,
                        backlog: data.get("backlog").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
                        avg_first_response_minutes: data
                            .get("first_response_time_avg_min")
                            .and_then(|v| v.as_f64()),
                        avg_resolution_minutes: data
                            .get("resolution_time_avg_min")
                            .and_then(|v| v.as_f64()),
                    });
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
        <div class="spp-page spp-page--dashboard">
            <h2 class="spp-page__title">"Dashboard"</h2>

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
                    when=move || kpis.with(|k| k.total_conversations > 0)
                    fallback=move || {
                        view! {
                            <EmptyState message="No conversations synced yet. Try demo mode or connect Help Scout." />
                        }
                    }
                >
                    <div class="spp-dashboard-grid">
                        <KpiCard label="Total conversations" value={move || kpis.get().total_conversations.to_string()} />
                        <KpiCard label="New (7d)" value={move || kpis.get().new_conversations.to_string()} />
                        <KpiCard label="Active" value={move || kpis.get().active_conversations.to_string()} />
                        <KpiCard label="Closed" value={move || kpis.get().closed_conversations.to_string()} />
                        <KpiCard label="Pending" value={move || kpis.get().pending_conversations.to_string()} />
                        <KpiCard label="Unassigned" value={move || kpis.get().unassigned.to_string()} />
                        <KpiCard label="Backlog" value={move || kpis.get().backlog.to_string()} />
                        <KpiCard
                            label="Avg first response (min)"
                            value={move || kpis.get().avg_first_response_minutes
                                .map(|m| format!("{m:.1}"))
                                .unwrap_or_else(|| "—".into())}
                        />
                        <KpiCard
                            label="Avg resolution (min)"
                            value={move || kpis.get().avg_resolution_minutes
                                .map(|m| format!("{m:.1}"))
                                .unwrap_or_else(|| "—".into())}
                        />
                    </div>
                </Show>
            </Show>
        </div>
    }
}

/// A single KPI card with a label and value.
#[component]
fn KpiCard(label: &'static str, value: impl Fn() -> String + 'static) -> impl IntoView {
    view! {
        <div class="spp-kpi-card">
            <span class="spp-kpi-card__label">{label}</span>
            <span class="spp-kpi-card__value">{value}</span>
        </div>
    }
}
