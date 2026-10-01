//! Backup page — export + restore.
//!
//! Per spec M10: "backup/restore." Calls `backup_export` IPC.
//! Restore is not yet wired (needs a file picker + the import_settings
//! function which requires a password).

use leptos::*;

use crate::components::state_view::{EmptyState, LoadingState};

/// The Backup page.
#[component]
pub fn BackupPage() -> impl IntoView {
    let backup = create_rw_signal(None::<serde_json::Value>);
    let loading = create_rw_signal(true);
    let error_msg = create_rw_signal(None::<String>);
    let action_msg = create_rw_signal(None::<String>);

    let do_export = move || {
        loading.set(true);
        error_msg.set(None);
        wasm_bindgen_futures::spawn_local(async move {
            let args = serde_json::json!({});
            match crate::ipc::invoke::<serde_json::Value>("backup_export", &args).await {
                Ok(data) => {
                    backup.set(Some(data));
                    loading.set(false);
                    action_msg.set(Some("Backup exported successfully.".to_string()));
                }
                Err(e) => {
                    error_msg.set(Some(e));
                    loading.set(false);
                }
            }
        });
    };

    // Auto-load on mount.
    create_effect(move |_| {
        do_export();
    });

    let download_json = move || {
        if let Some(ref b) = backup.get() {
            let json_str = serde_json::to_string_pretty(b).unwrap_or_default();
            // Create a download link via a data URL.
            if let Some(window) = web_sys::window() {
                if let Some(document) = window.document() {
                    let data_url = format!(
                        "data:application/json;charset=utf-8,{}",
                        js_sys::encode_uri_component(&json_str)
                            .as_string()
                            .unwrap_or_default()
                    );
                    let a = document.create_element("a").ok();
                    if let Some(a) = a {
                        let _ = a.set_attribute("href", &data_url);
                        let _ = a.set_attribute("download", "supportos-plusplus-backup.json");
                        // Cast Element to HtmlElement to call click().
                        if let Ok(html_el) =
                            wasm_bindgen::JsCast::dyn_into::<web_sys::HtmlElement>(a)
                        {
                            html_el.click();
                        }
                    }
                }
            }
        }
    };

    view! {
        <div class="spp-page spp-page--backup">
            <h2 class="spp-page__title">"Backup & Restore"</h2>

            <p class="spp-page__intro">
                "Export all table data as a JSON backup. Restore is not yet available from the UI (requires the import_settings function + a password)."
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

            <Show when=move || action_msg.get().is_some() fallback=|| ()>
                <div class="spp-state spp-state--success">
                    {move || action_msg.get().unwrap_or_default()}
                </div>
            </Show>

            <Show
                when=move || !loading.get() && error_msg.get().is_none()
                fallback=|| ()
            >
                <section class="spp-backup__section">
                    <h3>"Export"</h3>
                    <div class="spp-backup__actions">
                        <button class="spp-button" on:click=move |_| do_export()>
                            "Refresh backup"
                        </button>
                        <button class="spp-button" on:click=move |_| download_json()>
                            "Download JSON"
                        </button>
                    </div>

                    {move || {
                        let b = match backup.get() {
                            Some(b) => b,
                            None => return view! {
                                <EmptyState message="No backup data available." />
                            }.into_view(),
                        };
                        let version = b.get("version").and_then(|v| v.as_u64()).unwrap_or(0);
                        let schema_version = b.get("schema_version").and_then(|v| v.as_u64()).unwrap_or(0);
                        let tables = b.get("tables").and_then(|v| v.as_array()).cloned().unwrap_or_default();
                        view! {
                            <div class="spp-backup__summary">
                                <div class="spp-backup__field">
                                    <label>"Format version"</label>
                                    <span>{version.to_string()}</span>
                                </div>
                                <div class="spp-backup__field">
                                    <label>"Schema version"</label>
                                    <span>{schema_version.to_string()}</span>
                                </div>
                                <div class="spp-backup__field">
                                    <label>"Tables"</label>
                                    <span>{tables.len().to_string()}</span>
                                </div>
                            </div>

                            <h4>"Tables"</h4>
                            <ul class="spp-backup__tables">
                                {tables.iter().map(|t| {
                                    let name = t.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                    let rows = t.get("rows").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
                                    view! {
                                        <li class="spp-backup__table">
                                            <span class="spp-backup__table-name">{name}</span>
                                            <span class="spp-backup__table-rows">{rows.to_string()} {" rows"}</span>
                                        </li>
                                    }
                                }).collect::<Vec<_>>()}
                            </ul>
                        }.into_view()
                    }}
                </section>

                <section class="spp-backup__section">
                    <h3>"Restore"</h3>
                    <EmptyState message="Restore from a .json backup file is not yet available from the UI. It requires the import_settings function (which needs a password for encrypted settings). Use the CLI or database directly for now." />
                </section>
            </Show>
        </div>
    }
}

#[cfg(test)]
mod tests {
    // The Backup page's UI rendering is verified by the wasm test runner in CI.
}
