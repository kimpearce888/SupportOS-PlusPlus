//! Custom objects page — list custom object types + their fields.
//!
//! Per spec M10: "custom objects." Calls `custom_object_types_list` +
//! `custom_object_fields_list` IPC commands.

use leptos::*;

use crate::components::state_view::{EmptyState, LoadingState};

/// The Custom Objects page.
#[component]
pub fn CustomObjectsPage() -> impl IntoView {
    let types = create_rw_signal(Vec::<serde_json::Value>::new());
    let selected_type_id = create_rw_signal(None::<i64>);
    let fields = create_rw_signal(Vec::<serde_json::Value>::new());
    let loading = create_rw_signal(true);
    let error_msg = create_rw_signal(None::<String>);

    create_effect(move |_| {
        let types = types;
        let loading = loading;
        let error_msg = error_msg;
        wasm_bindgen_futures::spawn_local(async move {
            match crate::api::get_json::<serde_json::Value>("/api/custom-objects/types").await {
                Ok(data) => {
                    let items = data
                        .get("types")
                        .and_then(|v| v.as_array())
                        .cloned()
                        .unwrap_or_default();
                    types.set(items);
                    loading.set(false);
                }
                Err(e) => {
                    error_msg.set(Some(e));
                    loading.set(false);
                }
            }
        });
    });

    create_effect(move |_| {
        let fields = fields;
        let error_msg = error_msg;
        if let Some(tid) = selected_type_id.get() {
            wasm_bindgen_futures::spawn_local(async move {
                let path = format!("/api/custom-objects/types/{tid}");
                match crate::api::get_json::<serde_json::Value>(&path).await {
                    Ok(data) => {
                        // Reference shape: { type: { fields: [...] } } — the
                        // field rows carry key/label/fieldType/required.
                        let items = data
                            .get("type")
                            .and_then(|t| t.get("fields"))
                            .and_then(|v| v.as_array())
                            .cloned()
                            .unwrap_or_default();
                        fields.set(items);
                    }
                    Err(e) => {
                        error_msg.set(Some(e));
                    }
                }
            });
        } else {
            fields.set(Vec::new());
        }
    });

    view! {
        <div class="spp-page spp-page--custom-objects">
            <h2 class="spp-page__title">"Custom Objects"</h2>

            <p class="spp-page__intro">
                "User-defined object types with custom fields. Each type can have text, number, date, or select fields."
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
                <div class="spp-custom-objects__layout">
                    <aside class="spp-custom-objects__types">
                        <h3>"Types"</h3>
                        <Show
                            when=move || !types.with(|t| t.is_empty())
                            fallback=|| {
                                view! {
                                    <EmptyState message="No custom object types defined." />
                                }
                            }
                        >
                            <ul class="spp-custom-objects__type-list">
                                {move || types.with(|items| {
                                    items.iter().map(|t| {
                                        let id = t.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
                                        let name = t.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                        let slug = t.get("slug").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                        let is_selected = move || selected_type_id.get() == Some(id);
                                        view! {
                                            <li
                                                class="spp-custom-objects__type"
                                                class:is-selected=is_selected
                                                on:click=move |_| {
                                                    selected_type_id.set(Some(id));
                                                }
                                            >
                                                <span class="spp-custom-objects__type-name">{name}</span>
                                                <span class="spp-custom-objects__type-slug">{slug}</span>
                                            </li>
                                        }
                                    }).collect::<Vec<_>>()
                                })}
                            </ul>
                        </Show>
                    </aside>

                    <main class="spp-custom-objects__fields">
                        <Show
                            when=move || selected_type_id.get().is_some()
                            fallback=|| {
                                view! {
                                    <EmptyState message="Select a type to view its fields." />
                                }
                            }
                        >
                            <Show
                                when=move || !fields.with(|f| f.is_empty())
                                fallback=|| {
                                    view! {
                                        <EmptyState message="No fields defined for this type." />
                                    }
                                }
                            >
                                <table class="spp-custom-objects__fields-table">
                                    <thead>
                                        <tr>
                                            <th>"Key"</th>
                                            <th>"Type"</th>
                                            <th>"Required"</th>
                                        </tr>
                                    </thead>
                                    <tbody>
                                        {move || fields.with(|items| {
                                            items.iter().map(|f| {
                                                let key = f.get("key").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                                let label = f.get("label").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                                let field_type = f.get("fieldType").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                                let required = f.get("required").and_then(|v| v.as_bool()).unwrap_or(false);
                                                view! {
                                                    <tr>
                                                        <td>
                                                            <span class="spp-custom-objects__field-key">{key}</span>
                                                            <span class="spp-custom-objects__field-label">{label}</span>
                                                        </td>
                                                        <td><span class="spp-badge">{field_type}</span></td>
                                                        <td>{if required { "✅" } else { "—" }}</td>
                                                    </tr>
                                                }
                                            }).collect::<Vec<_>>()
                                        })}
                                    </tbody>
                                </table>
                            </Show>
                        </Show>
                    </main>
                </div>
            </Show>
        </div>
    }
}

#[cfg(test)]
mod tests {
    // The Custom Objects page's UI rendering is verified by the wasm test runner in CI.
}
