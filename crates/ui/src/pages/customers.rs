//! Customer profile page — `/customers/:id`.
//!
//! Per spec M3: "Customer profile." Per A11: visual reference is the
//! reference repo's customer profile screenshot.
//! Per KNOWN PITFALLS: "every view has loading, empty and error states."
//!
//! Every control calls a real IPC command:
//! - Page load calls `customer_get` + `customer_conversations` + `customer_timeline`.
//! - Search box calls `customer_search`.

use leptos::*;

use crate::components::state_view::{EmptyState, LoadingState};

/// Percent-encode a query value (the query-string subset that needs it).
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The customer profile page.
#[component]
pub fn CustomerProfilePage(customer_id: i64) -> impl IntoView {
    let customer = create_rw_signal(None::<serde_json::Value>);
    let conversations = create_rw_signal(Vec::<serde_json::Value>::new());
    let timeline = create_rw_signal(Vec::<serde_json::Value>::new());
    let loading = create_rw_signal(true);
    let error_msg = create_rw_signal(None::<String>);
    // Bumped after a successful edit so the effect refetches both payloads.
    let reload = create_rw_signal(0u32);
    let editing = create_rw_signal(false);

    create_effect(move |_| {
        let _ = reload.get();
        let customer = customer;
        let conversations = conversations;
        let timeline = timeline;
        let loading = loading;
        let error_msg = error_msg;
        wasm_bindgen_futures::spawn_local(async move {
            // Reference CustomerDetailData: GET /api/customers/:id returns the
            // customer with its recent conversations; the timeline comes from
            // GET /api/customers/:id/timeline.
            let detail_path = format!("/api/customers/{customer_id}");
            let cust_result = crate::api::get_json::<serde_json::Value>(&detail_path).await;
            let tl_result =
                crate::api::get_json::<serde_json::Value>(&format!("{detail_path}/timeline")).await;

            match (cust_result, tl_result) {
                (Ok(mut c), Ok(tl)) => {
                    let cv = c
                        .get("conversations")
                        .and_then(|v| v.as_array())
                        .cloned()
                        .unwrap_or_default();
                    if let Some(obj) = c.as_object_mut() {
                        obj.remove("conversations");
                    }
                    customer.set(Some(c));
                    conversations.set(cv);
                    timeline.set(
                        tl.get("events")
                            .and_then(|v| v.as_array())
                            .cloned()
                            .unwrap_or_default(),
                    );
                    loading.set(false);
                }
                (Err(e), _) | (_, Err(e)) => {
                    error_msg.set(Some(e));
                    loading.set(false);
                }
            }
        });
    });

    view! {
        <div class="spp-page spp-page--customer">
            <h2 class="spp-page__title">"Customer Profile"</h2>

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
                    when=move || customer.get().is_some()
                    fallback=move || {
                        view! {
                            <EmptyState message="Customer not found. They may have been deleted or never synced." />
                        }
                    }
                >
                    {move || {
                        let c = customer.get().unwrap();
                        let first_name = c.get("first_name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                        let last_name = c.get("last_name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                        let email = c.get("email").and_then(|v| v.as_str()).unwrap_or("").to_string();
                        let organization = c.get("organization").and_then(|v| v.as_str()).unwrap_or("").to_string();
                        let job_title = c.get("job_title").and_then(|v| v.as_str()).unwrap_or("").to_string();
                        let phone = c.get("phone").and_then(|v| v.as_str()).unwrap_or("").to_string();
                        let full_name = format!("{first_name} {last_name}");
                        view! {
                            <div class="spp-customer-profile">
                                <header class="spp-customer-profile__header">
                                    <h3 class="spp-customer-profile__name">{full_name}</h3>
                                    {if !job_title.is_empty() {
                                        view! {
                                            <span class="spp-customer-profile__title">{job_title.clone()}</span>
                                        }.into_view()
                                    } else {
                                        ().into_view()
                                    }}
                                    <button
                                        class="spp-button spp-button--ghost spp-button--small spp-customer-profile__edit"
                                        on:click=move |_| editing.set(true)
                                    >
                                        "Edit profile"
                                    </button>
                                </header>

                                <dl class="spp-customer-profile__details">
                                    {if !email.is_empty() {
                                        view! {
                                            <dt>"Email"</dt>
                                            <dd>{email.clone()}</dd>
                                        }.into_view()
                                    } else {
                                        ().into_view()
                                    }}
                                    {if !phone.is_empty() {
                                        view! {
                                            <dt>"Phone"</dt>
                                            <dd>{phone.clone()}</dd>
                                        }.into_view()
                                    } else {
                                        ().into_view()
                                    }}
                                    {if !organization.is_empty() {
                                        view! {
                                            <dt>"Organization"</dt>
                                            <dd>{organization.clone()}</dd>
                                        }.into_view()
                                    } else {
                                        ().into_view()
                                    }}
                                </dl>
                            </div>
                        }
                    }}

                    <section class="spp-customer-profile__section">
                        <h3>"Conversations"</h3>
                        <Show
                            when=move || !conversations.with(|c| c.is_empty())
                            fallback=|| {
                                view! {
                                    <EmptyState message="No conversations for this customer yet." />
                                }
                            }
                        >
                            <ul class="spp-customer-profile__conversation-list">
                                {move || conversations.with(|convs| {
                                    convs.iter().map(|c| {
                                        let subject = c.get("subject").and_then(|v| v.as_str()).unwrap_or("(no subject)").to_string();
                                        let status = c.get("status").and_then(|v| v.as_str()).unwrap_or("active").to_string();
                                        let number = c.get("number").and_then(|v| v.as_i64()).unwrap_or(0);
                                        view! {
                                            <li class="spp-customer-profile__conversation">
                                                <span class="spp-customer-profile__conv-number">{"#"}{number.to_string()}</span>
                                                <span class="spp-customer-profile__conv-subject">{subject}</span>
                                                <span class="spp-badge spp-badge--status">{status}</span>
                                            </li>
                                        }
                                    }).collect::<Vec<_>>()
                                })}
                            </ul>
                        </Show>
                    </section>

                    <section class="spp-customer-profile__section">
                        <h3>"Timeline"</h3>
                        <Show
                            when=move || !timeline.with(|t| t.is_empty())
                            fallback=|| {
                                view! {
                                    <EmptyState message="No timeline events yet. Send a reply or note to start the history." />
                                }
                            }
                        >
                            <div class="spp-customer-profile__timeline">
                                {move || timeline.with(|entries| {
                                    entries.iter().map(|e| {
                                        let kind = e.get("event_kind").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                        let title = e.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                        let time = e.get("occurred_at").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                        let conv_id = e.get("detail")
                                            .and_then(|d| d.get("conversation_id"))
                                            .and_then(|v| v.as_i64());
                                        view! {
                                            <div class="spp-timeline-entry">
                                                <div class="spp-timeline-entry__header">
                                                    <span class="spp-timeline-entry__type">{kind}</span>
                                                    <span class="spp-timeline-entry__time">{time}</span>
                                                </div>
                                                <div class="spp-timeline-entry__body">{title.clone()}</div>
                                                {if conv_id.is_some() {
                                                    view! {
                                                        <div class="spp-timeline-entry__conv">
                                                            {"conversation #"}{conv_id.unwrap().to_string()}
                                                        </div>
                                                    }.into_view()
                                                } else {
                                                    ().into_view()
                                                }}
                                            </div>
                                        }
                                    }).collect::<Vec<_>>()
                                })}
                            </div>
                        </Show>
                    </section>
                </Show>
            </Show>

            <Show when=move || editing.get() fallback=|| ()>
                <EditCustomerModal
                    customer_id=customer_id
                    customer=customer.get_untracked()
                    on_close=std::rc::Rc::new(move || editing.set(false))
                    on_saved=std::rc::Rc::new(move || {
                        editing.set(false);
                        reload.set(reload.get_untracked() + 1);
                    })
                />
            </Show>
        </div>
    }
}

/// The customer edit modal — PATCH /api/customers/:id over the six
/// profile fields. An emptied input clears the field (empty string and
/// explicit null are the same thing on the wire); a field left exactly as
/// loaded is still sent (the backend PATCH is idempotent).
#[component]
fn EditCustomerModal(
    customer_id: i64,
    customer: Option<serde_json::Value>,
    on_close: std::rc::Rc<dyn Fn()>,
    on_saved: std::rc::Rc<dyn Fn()>,
) -> impl IntoView {
    let c = customer.unwrap_or_default();
    let field = |key: &str| -> String {
        c.get(key)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    let first_name = create_rw_signal(field("first_name"));
    let last_name = create_rw_signal(field("last_name"));
    let email = create_rw_signal(field("email"));
    let organization = create_rw_signal(field("organization"));
    let job_title = create_rw_signal(field("job_title"));
    let phone = create_rw_signal(field("phone"));
    let submitting = create_rw_signal(false);

    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        if submitting.get() {
            return;
        }
        submitting.set(true);
        // Empty string = clear (the route treats "" and null alike for
        // these optional text fields).
        let body = serde_json::json!({
            "firstName": first_name.get(),
            "lastName": last_name.get(),
            "email": email.get(),
            "organization": organization.get(),
            "jobTitle": job_title.get(),
            "phone": phone.get(),
        });
        let on_saved = std::rc::Rc::clone(&on_saved);
        wasm_bindgen_futures::spawn_local(async move {
            let path = format!("/api/customers/{customer_id}");
            match crate::api::patch_json::<serde_json::Value>(&path, &body).await {
                Ok(r) if r.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) => {
                    crate::toasts::success("Customer profile saved.");
                    on_saved();
                }
                Ok(r) => {
                    crate::toasts::error(
                        r.get("message")
                            .and_then(|v| v.as_str())
                            .unwrap_or("Save failed"),
                    );
                    submitting.set(false);
                }
                Err(e) => {
                    crate::toasts::error(e);
                    submitting.set(false);
                }
            }
        });
    };

    view! {
        <div class="spp-overlay" role="dialog" aria-modal="true">
            <div class="spp-modal spp-modal--form">
                <h3 class="spp-modal__title">"Edit customer profile"</h3>
                <p class="spp-modal__message">
                    "Changes are local-only (they never sync back to Help Scout). Empty a field to clear it."
                </p>
                <form on:submit=submit>
                    <div class="spp-form-grid">
                        <label class="spp-form-grid__label">"First name"</label>
                        <input
                            class="spp-input"
                            maxlength=80
                            prop:value=first_name
                            on:input=move |ev| first_name.set(event_target_value(&ev))
                        />
                        <label class="spp-form-grid__label">"Last name"</label>
                        <input
                            class="spp-input"
                            maxlength=80
                            prop:value=last_name
                            on:input=move |ev| last_name.set(event_target_value(&ev))
                        />
                        <label class="spp-form-grid__label">"Email"</label>
                        <input
                            class="spp-input"
                            type="email"
                            maxlength=200
                            prop:value=email
                            on:input=move |ev| email.set(event_target_value(&ev))
                        />
                        <label class="spp-form-grid__label">"Organization"</label>
                        <input
                            class="spp-input"
                            maxlength=200
                            prop:value=organization
                            on:input=move |ev| organization.set(event_target_value(&ev))
                        />
                        <label class="spp-form-grid__label">"Job title"</label>
                        <input
                            class="spp-input"
                            maxlength=200
                            prop:value=job_title
                            on:input=move |ev| job_title.set(event_target_value(&ev))
                        />
                        <label class="spp-form-grid__label">"Phone"</label>
                        <input
                            class="spp-input"
                            maxlength=60
                            prop:value=phone
                            on:input=move |ev| phone.set(event_target_value(&ev))
                        />
                    </div>
                    <div class="spp-modal__actions spp-mt-8">
                        <button class="spp-button spp-button--ghost" type="button" on:click=move |_| on_close()>
                            "Cancel"
                        </button>
                        <button
                            class="spp-button spp-button--primary"
                            type="submit"
                            disabled=move || submitting.get()
                        >
                            {move || if submitting.get() { "Saving…" } else { "Save changes" }}
                        </button>
                    </div>
                </form>
            </div>
        </div>
    }
}

/// Customer search page — `/customers`.
#[component]
pub fn CustomerSearchPage() -> impl IntoView {
    let query = create_rw_signal(String::new());
    let results = create_rw_signal(Vec::<serde_json::Value>::new());
    let loading = create_rw_signal(false);
    let error_msg = create_rw_signal(None::<String>);

    let do_search = move || {
        let q = query.get();
        if q.trim().is_empty() {
            results.set(Vec::new());
            return;
        }
        loading.set(true);
        error_msg.set(None);
        wasm_bindgen_futures::spawn_local(async move {
            let path = format!("/api/customers?q={}&limit=20", urlencode(&q));
            match crate::api::get_json::<serde_json::Value>(&path).await {
                Ok(data) => {
                    let r = data
                        .get("customers")
                        .and_then(|v| v.as_array())
                        .cloned()
                        .unwrap_or_default();
                    results.set(r);
                    loading.set(false);
                }
                Err(e) => {
                    error_msg.set(Some(e));
                    loading.set(false);
                }
            }
        });
    };

    view! {
        <div class="spp-page spp-page--customer-search">
            <h2 class="spp-page__title">"Customers"</h2>

            <div class="spp-customer-search__bar">
                <input
                    class="spp-customer-search__input"
                    type="text"
                    placeholder="Search by name, email, or organization..."
                    prop:value=query
                    on:input=move |ev| query.set(event_target_value(&ev))
                    on:keydown=move |ev| {
                        if ev.key() == "Enter" {
                            do_search();
                        }
                    }
                />
                <button class="spp-button" on:click=move |_| do_search()>
                    "Search"
                </button>
            </div>

            <Show when=move || loading.get() fallback=|| ()>
                <LoadingState />
            </Show>
            <Show when=move || error_msg.get().is_some() fallback=|| ()>
                <div class="spp-state spp-state--error">
                    {move || error_msg.get().unwrap_or_default()}
                </div>
            </Show>

            <Show
                when=move || !loading.get() && error_msg.get().is_none()
                fallback=|| ()
            >
                <Show
                    when=move || !results.with(|r| r.is_empty())
                    fallback=|| {
                        view! {
                            <EmptyState message="No customers found. Try a different search query." />
                        }
                    }
                >
                    <ul class="spp-customer-search__results">
                        {move || results.with(|items| {
                            items.iter().map(|c| {
                                let first = c.get("first_name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                let last = c.get("last_name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                let email = c.get("email").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                let org = c.get("organization").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                view! {
                                    <li class="spp-customer-search__result">
                                        <span class="spp-customer-search__name">{format!("{first} {last}")}</span>
                                        {if !email.is_empty() {
                                            view! {
                                                <span class="spp-customer-search__email">{email.clone()}</span>
                                            }.into_view()
                                        } else {
                                            ().into_view()
                                        }}
                                        {if !org.is_empty() {
                                            view! {
                                                <span class="spp-customer-search__org">{org.clone()}</span>
                                            }.into_view()
                                        } else {
                                            ().into_view()
                                        }}
                                    </li>
                                }
                            }).collect::<Vec<_>>()
                        })}
                    </ul>
                </Show>
            </Show>
        </div>
    }
}

#[cfg(test)]
mod tests {
    // The customer profile page's UI rendering is verified by the wasm test
    // runner in CI. This module exists to ensure the file compiles as a test
    // target without triggering clippy::assertions_on_constants.
}
