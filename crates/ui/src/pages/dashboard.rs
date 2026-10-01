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
    pub customer_waiting: u32,
    pub avg_first_response_minutes: Option<f64>,
    pub avg_resolution_minutes: Option<f64>,
    pub sla_breach_count: u32,
}

/// The dashboard page. Fetches metrics from `dashboard_metrics` IPC on mount.
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
            let args = serde_json::json!({ "mailbox_id": null, "days_back": 7 });
            match crate::ipc::invoke::<serde_json::Value>("dashboard_metrics", &args).await {
                Ok(data) => {
                    kpis.set(DashboardKpis {
                        total_conversations: data
                            .get("total_conversations")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0) as u32,
                        new_conversations: data
                            .get("new_conversations")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0) as u32,
                        closed_conversations: data
                            .get("closed_conversations")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0) as u32,
                        active_conversations: data
                            .get("active_conversations")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0) as u32,
                        customer_waiting: data
                            .get("customer_waiting")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0) as u32,
                        avg_first_response_minutes: data
                            .get("avg_first_response_minutes")
                            .and_then(|v| v.as_f64()),
                        avg_resolution_minutes: data
                            .get("avg_resolution_minutes")
                            .and_then(|v| v.as_f64()),
                        sla_breach_count: data
                            .get("sla_breach_count")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0) as u32,
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
                        <KpiCard label="Customer waiting" value={move || kpis.get().customer_waiting.to_string()} />
                        <KpiCard label="SLA breaches" value={move || kpis.get().sla_breach_count.to_string()} />
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
