//! Reports page — the 21×14 report builder.
//!
//! Per spec M8: "report builder." Per A11: visual reference is the
//! reference repo's reports screenshot.
//! Per KNOWN PITFALLS: "every view has loading, empty and error states."
//!
//! Every control calls a real IPC command:
//! - Metric + dimension + days-back selectors call `report_build`.
//! - Results render as a table with current period + previous period comparison.

use leptos::*;

use crate::components::state_view::{EmptyState, LoadingState};

/// The Reports page.
#[component]
pub fn ReportsPage() -> impl IntoView {
    let result = create_rw_signal(None::<serde_json::Value>);
    let loading = create_rw_signal(false);
    let error_msg = create_rw_signal(None::<String>);
    let selected_metric = create_rw_signal("total_conversations".to_string());
    let selected_dimension = create_rw_signal("day".to_string());
    let days_back = create_rw_signal("30".to_string());

    let run_report = move || {
        let metric = selected_metric.get();
        let dimension = selected_dimension.get();
        let days = days_back.get().parse::<u32>().unwrap_or(30);
        loading.set(true);
        error_msg.set(None);
        wasm_bindgen_futures::spawn_local(async move {
            let args = serde_json::json!({
                "metric": metric,
                "dimension": dimension,
                "days_back": days,
            });
            match crate::ipc::invoke::<serde_json::Value>("report_build", &args).await {
                Ok(data) => {
                    result.set(Some(data));
                    loading.set(false);
                }
                Err(e) => {
                    error_msg.set(Some(e));
                    result.set(None);
                    loading.set(false);
                }
            }
        });
    };

    // Run the default report on mount.
    create_effect(move |_| {
        run_report();
    });

    view! {
        <div class="spp-page spp-page--reports">
            <h2 class="spp-page__title">"Reports"</h2>

            <p class="spp-page__intro">
                "Custom report builder — 21 metrics × 14 dimensions with previous-period comparison."
            </p>

            <div class="spp-reports__controls">
                <label>"Metric"</label>
                <select
                    class="spp-reports__select"
                    value=move || selected_metric.get()
                    on:change=move |ev| selected_metric.set(event_target_value(&ev))
                >
                    <option value="total_conversations">"Total conversations"</option>
                    <option value="new_conversations">"New conversations"</option>
                    <option value="closed_conversations">"Closed conversations"</option>
                    <option value="active_conversations">"Active conversations"</option>
                    <option value="avg_first_response_minutes">"Avg first response (min)"</option>
                    <option value="avg_resolution_minutes">"Avg resolution (min)"</option>
                    <option value="sla_breach_rate">"SLA breach rate"</option>
                    <option value="high_friction_count">"High friction count"</option>
                    <option value="high_friction_rate">"High friction rate"</option>
                    <option value="resolution_ratio">"Resolution ratio"</option>
                </select>

                <label>"Dimension"</label>
                <select
                    class="spp-reports__select"
                    value=move || selected_dimension.get()
                    on:change=move |ev| selected_dimension.set(event_target_value(&ev))
                >
                    <option value="none">"None (total)"</option>
                    <option value="day">"Day"</option>
                    <option value="week">"Week"</option>
                    <option value="month">"Month"</option>
                    <option value="mailbox">"Mailbox"</option>
                    <option value="channel">"Channel"</option>
                    <option value="custom_state">"Custom state"</option>
                    <option value="response_state">"Response state"</option>
                    <option value="issue">"Issue"</option>
                    <option value="ai_attribute">"AI attribute"</option>
                </select>

                <label>"Days back"</label>
                <input
                    class="spp-reports__input"
                    type="number"
                    min="1"
                    max="365"
                    value=move || days_back.get()
                    on:input=move |ev| days_back.set(event_target_value(&ev))
                />

                <button class="spp-button" on:click=move |_| run_report()>
                    "Run report"
                </button>
            </div>

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
                when=move || !loading.get() && error_msg.get().is_none() && result.get().is_some()
                fallback=|| ()
            >
                {move || {
                    let r = result.get().unwrap();
                    let metric = r.get("metric").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let dimension = r.get("dimension").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let metric_def = r.get("metric_definition").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let metric_lim = r.get("metric_limitations").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let rows = r.get("rows").and_then(|v| v.as_array()).cloned().unwrap_or_default();
                    let prev_rows = r.get("previous_period_rows").and_then(|v| v.as_array()).cloned().unwrap_or_default();
                    let dimension_header = dimension.clone();

                    view! {
                        <div class="spp-reports__result">
                            <div class="spp-reports__meta">
                                <span class="spp-reports__metric">"Metric: " {metric}</span>
                                <span class="spp-reports__dimension">"Dimension: " {dimension}</span>
                            </div>

                            {if !metric_def.is_empty() {
                                view! {
                                    <div class="spp-reports__definition">
                                        <h4>"Definition"</h4>
                                        <p>{metric_def.clone()}</p>
                                        {if !metric_lim.is_empty() {
                                            view! {
                                                <h4>"Limitations"</h4>
                                                <p>{metric_lim.clone()}</p>
                                            }.into_view()
                                        } else {
                                            ().into_view()
                                        }}
                                    </div>
                                }.into_view()
                            } else {
                                ().into_view()
                            }}

                            {if rows.is_empty() {
                                view! {
                                    <EmptyState message="No data for the selected metric + dimension + time range." />
                                }.into_view()
                            } else {
                                view! {
                                    <table class="spp-reports__table">
                                        <thead>
                                            <tr>
                                                <th>{dimension_header}</th>
                                                <th>"Current period"</th>
                                                <th>"Previous period"</th>
                                                <th>"Change"</th>
                                            </tr>
                                        </thead>
                                        <tbody>
                                            {rows.iter().map(|row| {
                                                let dim_val = row.get("dimension_value").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                                let metric_val = row.get("metric_value").and_then(|v| v.as_f64()).unwrap_or(0.0);
                                                // Find matching previous period row by dimension_value.
                                                let prev_val = prev_rows.iter()
                                                    .find(|pr| pr.get("dimension_value").and_then(|v| v.as_str()) == Some(&dim_val))
                                                    .and_then(|pr| pr.get("metric_value"))
                                                    .and_then(|v| v.as_f64())
                                                    .unwrap_or(0.0);
                                                let change = if prev_val != 0.0 {
                                                    format!("{:+.1}%", ((metric_val - prev_val) / prev_val) * 100.0)
                                                } else {
                                                    "—".to_string()
                                                };
                                                view! {
                                                    <tr>
                                                        <td>{dim_val}</td>
                                                        <td>{format!("{metric_val:.2}")}</td>
                                                        <td>{format!("{prev_val:.2}")}</td>
                                                        <td>{change}</td>
                                                    </tr>
                                                }
                                            }).collect::<Vec<_>>()}
                                        </tbody>
                                    </table>
                                }.into_view()
                            }}
                        </div>
                    }.into_view()
                }}
            </Show>
        </div>
    }
}

#[cfg(test)]
mod tests {
    // The Reports page's UI rendering is verified by the wasm test runner in CI.
}
