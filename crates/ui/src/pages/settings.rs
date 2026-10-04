//! Settings page — the `/settings` route.
//!
//! Full port of the reference `SettingsPage.tsx`: eight tabs (Synchronization
//! & AI, Help Scout, LM Studio, Qdrant, Business hours, Backups & export,
//! Encrypted sync, Capability matrix) with every mutation wired to the same
//! HTTP API the reference calls. All write outcomes surface as toasts
//! (reference `pushToast` semantics, v1.6.0 audit fixes: network failures are
//! never a silent no-op).
//!
//! The two form tabs re-sync their input state when the loaded settings
//! arrive — the v1.6.0 audit fix for the "default capture" data-loss bug
//! (a form initialized from an undefined query silently saved DEFAULTS over
//! the real settings). Per KNOWN PITFALLS: loading, empty, and error states
//! everywhere.

use leptos::*;
use std::rc::Rc;
use wasm_bindgen::JsCast;

use crate::components::state_view::{EmptyState, ErrorState, LoadingState};
use crate::toasts;

/// The reference tab ids (`general | helpscout | lmstudio | qdrant | hours |
/// backups | encsync | capability`), in the reference's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsTab {
    General,
    HelpScout,
    LmStudio,
    Qdrant,
    Hours,
    Backups,
    EncSync,
    Capability,
}

impl SettingsTab {
    /// The visible tab label (reference tab-name mapping).
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::General => "Synchronization & AI",
            Self::HelpScout => "Help Scout",
            Self::LmStudio => "LM Studio",
            Self::Qdrant => "Qdrant",
            Self::Hours => "Business hours",
            Self::Backups => "Backups & export",
            Self::EncSync => "Encrypted sync",
            Self::Capability => "Capability matrix",
        }
    }

    /// Every tab, in reference order.
    pub const ALL: [Self; 8] = [
        Self::General,
        Self::HelpScout,
        Self::LmStudio,
        Self::Qdrant,
        Self::Hours,
        Self::Backups,
        Self::EncSync,
        Self::Capability,
    ];
}

/// The Settings page.
#[component]
pub fn SettingsPage() -> impl IntoView {
    let tab = create_rw_signal(SettingsTab::General);

    view! {
        <div class="spp-page spp-page--settings">
            <h2 class="spp-page__title">"Settings"</h2>
            <p class="spp-page__subtitle">
                "Secrets are never displayed after storage. Safe defaults: automatic reply sending OFF, automation writes OFF."
            </p>

            <div class="spp-tabs">
                {SettingsTab::ALL
                    .iter()
                    .map(|t| {
                        let t = *t;
                        view! {
                            <button
                                class="spp-tab"
                                class:is-active=move || tab.get() == t
                                on:click=move |_| tab.set(t)
                            >
                                {t.label()}
                            </button>
                        }
                            .into_view()
                    })
                    .collect::<Vec<_>>()}
            </div>

            <Show when=move || tab.get() == SettingsTab::General fallback=|| ().into_view()>
                <GeneralSettingsTab />
            </Show>
            <Show when=move || tab.get() == SettingsTab::HelpScout fallback=|| ().into_view()>
                <HelpScoutSettingsTab />
            </Show>
            <Show when=move || tab.get() == SettingsTab::LmStudio fallback=|| ().into_view()>
                <LmStudioSettingsTab />
            </Show>
            <Show when=move || tab.get() == SettingsTab::Qdrant fallback=|| ().into_view()>
                <QdrantSettingsTab />
            </Show>
            <Show when=move || tab.get() == SettingsTab::Hours fallback=|| ().into_view()>
                <BusinessHoursSettingsTab />
            </Show>
            <Show when=move || tab.get() == SettingsTab::Backups fallback=|| ().into_view()>
                <BackupsSettingsTab />
            </Show>
            <Show when=move || tab.get() == SettingsTab::EncSync fallback=|| ().into_view()>
                <EncryptedSyncSettingsTab />
            </Show>
            <Show when=move || tab.get() == SettingsTab::Capability fallback=|| ().into_view()>
                <CapabilitySettingsTab />
            </Show>
        </div>
    }
}

// ---------------------------------------------------------------------------
// General (Synchronization & AI)
// ---------------------------------------------------------------------------

/// One row of the flat `GET /api/settings` payload the general tab edits.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GeneralSettings {
    pub sync_interval_minutes: i64,
    pub api_concurrency: i64,
    pub attachment_auto_download: bool,
    pub retention_days: Option<i64>,
    pub backup_interval_hours: Option<i64>,
    pub ai_enabled: bool,
    pub automatic_analysis_enabled: bool,
    pub automatic_note_enabled: bool,
    pub automatic_draft_enabled: bool,
    pub automation_enabled: bool,
    pub automation_write_actions_enabled: bool,
    pub redaction_enabled: bool,
    pub agent_language: String,
}

/// Parse the flat settings object (the reference reads the same keys; the
/// backend answers typed JSON — bools for flags, number-or-null for the two
/// nullable fields).
#[must_use]
pub fn parse_general_settings(v: &serde_json::Value) -> GeneralSettings {
    let flag = |k: &str, default: bool| v.get(k).and_then(|x| x.as_bool()).unwrap_or(default);
    let opt_i64 = |k: &str| v.get(k).and_then(|x| x.as_i64());
    GeneralSettings {
        sync_interval_minutes: opt_i64("sync_interval_minutes").unwrap_or(5),
        api_concurrency: opt_i64("api_concurrency").unwrap_or(2),
        attachment_auto_download: flag("attachment_auto_download", true),
        retention_days: opt_i64("retention_days"),
        backup_interval_hours: opt_i64("backup_interval_hours"),
        ai_enabled: flag("ai_enabled", true),
        automatic_analysis_enabled: flag("automatic_analysis_enabled", true),
        automatic_note_enabled: flag("automatic_note_enabled", false),
        automatic_draft_enabled: flag("automatic_draft_enabled", false),
        automation_enabled: flag("automation_enabled", false),
        automation_write_actions_enabled: flag("automation_write_actions_enabled", false),
        redaction_enabled: flag("redaction_enabled", true),
        agent_language: v
            .get("agent_language")
            .and_then(|x| x.as_str())
            .unwrap_or("en")
            .to_string(),
    }
}

/// Blur-commit semantics for a general-settings number field
/// (reference onBlur): blank -> `Null` when the key is nullable and
/// currently set, otherwise no write; a changed valid integer -> the number;
/// invalid input -> no write (the reference requires `Number.isFinite`).
/// Unchanged values skip the write (the sync-interval field's `n !==`
/// check, applied uniformly — the reference's retention field re-saved
/// unchanged values on every blur, spamming toasts).
#[must_use]
pub fn number_patch(raw: &str, nullable: bool, current: Option<i64>) -> Option<serde_json::Value> {
    let t = raw.trim();
    if t.is_empty() {
        if nullable && current.is_some() {
            return Some(serde_json::Value::Null);
        }
        return None;
    }
    match t.parse::<i64>() {
        Ok(n) if Some(n) != current => Some(serde_json::Value::from(n)),
        Ok(_) => None,
        Err(_) => None,
    }
}

/// The general (Synchronization & AI) tab — every control saves immediately
/// on change/blur through `PATCH /api/settings`, exactly like the reference
/// (`save.mutate({key: value})` + toast + refetch).
#[component]
fn GeneralSettingsTab() -> impl IntoView {
    let settings = create_rw_signal(None::<GeneralSettings>);
    let loading = create_rw_signal(true);
    let error_msg = create_rw_signal(None::<String>);

    let load: Rc<dyn Fn()> = Rc::new(move || {
        let settings = settings;
        let loading = loading;
        let error_msg = error_msg;
        wasm_bindgen_futures::spawn_local(async move {
            match crate::api::get_json::<serde_json::Value>("/api/settings").await {
                Ok(v) => {
                    settings.set(Some(parse_general_settings(&v)));
                    error_msg.set(None);
                    loading.set(false);
                }
                Err(e) => {
                    error_msg.set(Some(e));
                    loading.set(false);
                }
            }
        });
    });
    load();

    // The shared save (reference `save` mutation: PATCH + toast + refetch).
    let save: Rc<dyn Fn(serde_json::Value)> = Rc::new({
        let load = Rc::clone(&load);
        move |patch: serde_json::Value| {
            let load = Rc::clone(&load);
            wasm_bindgen_futures::spawn_local(async move {
                match crate::api::patch_json::<serde_json::Value>("/api/settings", &patch).await {
                    Ok(r) => {
                        let ok = r.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
                        let message = r
                            .get("message")
                            .and_then(|v| v.as_str())
                            .unwrap_or("Settings could not be saved.");
                        if ok {
                            toasts::success(message);
                        } else {
                            toasts::error(message);
                        }
                        load();
                    }
                    // v1.6.0 audit fix: surface network failures, never a
                    // silent no-op.
                    Err(e) => toasts::error(e),
                }
            });
        }
    });

    view! {
        <Show when=move || loading.get() fallback=|| ().into_view()>
            <LoadingState />
        </Show>
        <Show when=move || error_msg.get().is_some() fallback=|| ().into_view()>
            <ErrorState message="Could not load settings" retry=None />
        </Show>
        <Show
            when=move || !loading.get() && error_msg.get().is_none()
            fallback=|| ().into_view()
        >
            <GeneralSettingsForm settings=settings save=Rc::clone(&save) />
        </Show>
    }
}

/// The loaded general-settings form. A separate component because `Show`
/// children are `Fn` closures that cannot move handler captures out of
/// their environment; the component body is the FnOnce context that owns
/// its `save` prop (the RulesList call-site-clone pattern).
#[component]
fn GeneralSettingsForm(
    settings: RwSignal<Option<GeneralSettings>>,
    save: Rc<dyn Fn(serde_json::Value)>,
) -> impl IntoView {
    // Immediate-save boolean toggle (reference `Toggle` rows).
    let toggle: Rc<dyn Fn(&'static str, bool)> = Rc::new({
        let save = Rc::clone(&save);
        move |key, value| {
            save(serde_json::json!({ key: value }));
        }
    });

    // Blur-committed number fields (named handlers so each owns its own
    // save clone — the call-site-clone pattern).
    let blur_sync_interval = {
        let save = Rc::clone(&save);
        move |ev: ev::FocusEvent| {
            let current = settings.get().map(|s| s.sync_interval_minutes);
            if let Some(v) = number_patch(&event_target_value(&ev), false, current) {
                save(serde_json::json!({ "sync_interval_minutes": v }));
            }
        }
    };
    let blur_api_conc = {
        let save = Rc::clone(&save);
        move |ev: ev::FocusEvent| {
            let current = settings.get().map(|s| s.api_concurrency);
            if let Some(v) = number_patch(&event_target_value(&ev), false, current) {
                save(serde_json::json!({ "api_concurrency": v }));
            }
        }
    };
    let blur_retention = {
        let save = Rc::clone(&save);
        move |ev: ev::FocusEvent| {
            let current = settings.get().and_then(|s| s.retention_days);
            if let Some(v) = number_patch(&event_target_value(&ev), true, current) {
                save(serde_json::json!({ "retention_days": v }));
            }
        }
    };
    let blur_backup_interval = {
        let save = Rc::clone(&save);
        move |ev: ev::FocusEvent| {
            let current = settings.get().and_then(|s| s.backup_interval_hours);
            if let Some(v) = number_patch(&event_target_value(&ev), true, current) {
                save(serde_json::json!({ "backup_interval_hours": v }));
            }
        }
    };
    let on_language_change = {
        let save = Rc::clone(&save);
        move |ev: ev::Event| {
            let lang = event_target_value(&ev);
            save(serde_json::json!({ "agent_language": lang }));
        }
    };

    view! {
            <div class="spp-grid-2">
                <section class="spp-card spp-settings__card">
                    <h3 class="spp-settings__card-title">"Synchronization"</h3>
                    <div class="spp-settings__field">
                        <label class="spp-settings__label" for="sync-int">"Sync interval (minutes)"</label>
                        <input
                            id="sync-int"
                            class="spp-input"
                            type="number"
                            min="1"
                            max="1440"
                            prop:value=move || settings.get().unwrap_or_default().sync_interval_minutes.to_string()
                            on:blur=blur_sync_interval
                        />
                    </div>
                    <div class="spp-settings__field">
                        <label class="spp-settings__label" for="api-conc">"API concurrency"</label>
                        <input
                            id="api-conc"
                            class="spp-input"
                            type="number"
                            min="1"
                            max="10"
                            prop:value=move || settings.get().unwrap_or_default().api_concurrency.to_string()
                            on:blur=blur_api_conc
                        />
                    </div>
                    {toggle_row(
                        "Attachment auto-download",
                        "Download attachments in the background after sync",
                        move || settings.get().unwrap_or_default().attachment_auto_download,
                        { let t = Rc::clone(&toggle); move |v| t("attachment_auto_download", v) },
                    )}
                    <div class="spp-settings__field">
                        <label class="spp-settings__label" for="retention">"Retention days (blank = keep forever)"</label>
                        <input
                            id="retention"
                            class="spp-input"
                            type="number"
                            prop:value=move || settings.get().unwrap_or_default().retention_days.map(|n| n.to_string()).unwrap_or_default()
                            on:blur=blur_retention
                        />
                    </div>
                    <div class="spp-settings__field">
                        <label class="spp-settings__label" for="backup-int">"Automatic backup interval (hours, blank = off)"</label>
                        <input
                            id="backup-int"
                            class="spp-input"
                            type="number"
                            prop:value=move || settings.get().unwrap_or_default().backup_interval_hours.map(|n| n.to_string()).unwrap_or_default()
                            on:blur=blur_backup_interval
                        />
                    </div>
                </section>
                <section class="spp-card spp-settings__card">
                    <h3 class="spp-settings__card-title">"AI behaviour (safe defaults)"</h3>
                    {toggle_row(
                        "AI enabled",
                        "The app remains fully usable when off",
                        move || settings.get().unwrap_or_default().ai_enabled,
                        { let t = Rc::clone(&toggle); move |v| t("ai_enabled", v) },
                    )}
                    {toggle_row(
                        "Automatic ticket analysis",
                        "Analyze new/changed tickets in the background",
                        move || settings.get().unwrap_or_default().automatic_analysis_enabled,
                        { let t = Rc::clone(&toggle); move |v| t("automatic_analysis_enabled", v) },
                    )}
                    {toggle_row(
                        "Automatic AI note creation",
                        "Create internal Help Scout notes with the AI analysis",
                        move || settings.get().unwrap_or_default().automatic_note_enabled,
                        { let t = Rc::clone(&toggle); move |v| t("automatic_note_enabled", v) },
                    )}
                    {toggle_row(
                        "Automatic AI draft creation",
                        "Generate customer-safe draft suggestions",
                        move || settings.get().unwrap_or_default().automatic_draft_enabled,
                        { let t = Rc::clone(&toggle); move |v| t("automatic_draft_enabled", v) },
                    )}
                    {toggle_row(
                        "Automation engine",
                        "",
                        move || settings.get().unwrap_or_default().automation_enabled,
                        { let t = Rc::clone(&toggle); move |v| t("automation_enabled", v) },
                    )}
                    {toggle_row(
                        "Automation write actions",
                        "Without this, all non-read automation actions await approval",
                        move || settings.get().unwrap_or_default().automation_write_actions_enabled,
                        { let t = Rc::clone(&toggle); move |v| t("automation_write_actions_enabled", v) },
                    )}
                    <div class="spp-state spp-state--error spp-settings__locked">
                        <strong>"Automatic reply sending: permanently OFF."</strong>
                        " AI never sends customer replies in v1 - drafts always require human review and an explicit send action."
                    </div>
                    {toggle_row(
                        "Redaction layer",
                        "Mask payment data, tokens, API keys before prompting the model",
                        move || settings.get().unwrap_or_default().redaction_enabled,
                        { let t = Rc::clone(&toggle); move |v| t("redaction_enabled", v) },
                    )}
                    <div class="spp-settings__field">
                        <label class="spp-settings__label" for="agent-language">"Your drafting language (translation feature)"</label>
                        <select
                            id="agent-language"
                            class="spp-input"
                            prop:value=move || settings.get().unwrap_or_default().agent_language
                            on:change=on_language_change
                        >
                            {SUPPORTED_LANGUAGES
                                .iter()
                                .map(|(code, name)| {
                                    view! {
                                        <option value=*code>{*name}</option>
                                    }
                                })
                                .collect::<Vec<_>>()}
                        </select>
                        <p class="spp-settings__hint">
                            "Customer messages can be translated into this language for your review, and your drafts translated into the customer's language - always side by side, never sent automatically, local model only."
                        </p>
                    </div>
                </section>
            </div>
    }
}

/// The 18 reference languages (`shared/translation.ts` SUPPORTED_LANGUAGES).
pub const SUPPORTED_LANGUAGES: &[(&str, &str)] = &[
    ("en", "English"),
    ("es", "Spanish"),
    ("fr", "French"),
    ("de", "German"),
    ("it", "Italian"),
    ("pt", "Portuguese"),
    ("nl", "Dutch"),
    ("ru", "Russian"),
    ("zh", "Chinese"),
    ("ja", "Japanese"),
    ("ko", "Korean"),
    ("ar", "Arabic"),
    ("hi", "Hindi"),
    ("th", "Thai"),
    ("vi", "Vietnamese"),
    ("pl", "Polish"),
    ("tr", "Turkish"),
    ("sv", "Swedish"),
];

/// One label+hint+checkbox row (reference `Toggle`). The value getter re-reads
/// the settings signal so a reload re-renders the checked state.
fn toggle_row<F>(
    label: &'static str,
    hint: &'static str,
    value: impl Fn() -> bool + 'static,
    on_change: F,
) -> impl IntoView
where
    F: Fn(bool) + 'static,
{
    let on_change = Rc::new(on_change);
    view! {
        <div class="spp-settings__toggle">
            <div>
                <div class="spp-settings__toggle-label">{label}</div>
                {if hint.is_empty() {
                    ().into_view()
                } else {
                    view! { <div class="spp-muted spp-text-xs">{hint}</div> }.into_view()
                }}
            </div>
            <label class="spp-settings__switch">
                <input
                    type="checkbox"
                    prop:checked=value
                    on:change=move |ev| on_change(event_target_checked(&ev))
                    aria-label=label
                />
            </label>
        </div>
    }
}

// ---------------------------------------------------------------------------
// Help Scout (OAuth)
// ---------------------------------------------------------------------------

/// The `GET /api/oauth/status` payload.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OauthStatus {
    pub configured: bool,
    pub authenticated: bool,
    pub demo_mode: bool,
    pub expires_at: Option<String>,
    pub me_name: Option<String>,
    pub me_email: Option<String>,
}

/// Parse the oauth status payload (me is `{name, email} | null`).
#[must_use]
pub fn parse_oauth_status(v: &serde_json::Value) -> OauthStatus {
    let s = |k: &str| {
        v.get(k)
            .and_then(|x| x.as_str())
            .map(str::to_string)
            .filter(|x| !x.is_empty())
    };
    OauthStatus {
        configured: v
            .get("configured")
            .and_then(|x| x.as_bool())
            .unwrap_or(false),
        authenticated: v
            .get("authenticated")
            .and_then(|x| x.as_bool())
            .unwrap_or(false),
        demo_mode: v
            .get("demo_mode")
            .and_then(|x| x.as_bool())
            .unwrap_or(false),
        expires_at: s("expires_at"),
        me_name: v
            .pointer("/me/name")
            .and_then(|x| x.as_str())
            .map(str::to_string),
        me_email: v
            .pointer("/me/email")
            .and_then(|x| x.as_str())
            .map(str::to_string),
    }
}

/// "Name (email)" or the em dash, like the reference KV row.
#[must_use]
pub fn format_oauth_me(o: &OauthStatus) -> String {
    match &o.me_name {
        Some(name) => match &o.me_email {
            Some(email) => format!("{name} ({email})"),
            None => format!("{name} (—)"),
        },
        None => "—".to_string(),
    }
}

/// The Help Scout tab — OAuth status display + authorize / client-credentials
/// / disconnect (reference `HelpScoutSettings`).
#[component]
fn HelpScoutSettingsTab() -> impl IntoView {
    let oauth = create_rw_signal(None::<OauthStatus>);
    let loading = create_rw_signal(true);
    let error_msg = create_rw_signal(None::<String>);

    let load: Rc<dyn Fn()> = Rc::new(move || {
        let oauth = oauth;
        let loading = loading;
        let error_msg = error_msg;
        wasm_bindgen_futures::spawn_local(async move {
            match crate::api::get_json::<serde_json::Value>("/api/oauth/status").await {
                Ok(v) => {
                    oauth.set(Some(parse_oauth_status(&v)));
                    error_msg.set(None);
                    loading.set(false);
                }
                Err(e) => {
                    error_msg.set(Some(e));
                    loading.set(false);
                }
            }
        });
    });
    load();

    view! {
        <Show when=move || loading.get() fallback=|| ().into_view()>
            <LoadingState />
        </Show>
        <Show when=move || error_msg.get().is_some() fallback=|| ().into_view()>
            <ErrorState message="Could not load the Help Scout connection state." retry=None />
        </Show>
        <Show
            when=move || oauth.get().is_some() && error_msg.get().is_none()
            fallback=|| ().into_view()
        >
            <HelpScoutConnection oauth=oauth load=Rc::clone(&load) />
        </Show>
    }
}

/// The loaded Help Scout connection card (FnOnce context owning its
/// handlers — `Show` children are `Fn` and cannot move captures). The
/// status rows re-render reactively from the `oauth` signal; the buttons
/// are static so they can own the mutation handlers.
#[component]
fn HelpScoutConnection(oauth: RwSignal<Option<OauthStatus>>, load: Rc<dyn Fn()>) -> impl IntoView {
    // Authorize via browser (reference: GET /api/oauth/authorize-url, then
    // window.open(url, '_blank', 'noopener'); demo mode shows an info toast).
    let authorize = {
        let load = Rc::clone(&load);
        move |_| {
            let load = Rc::clone(&load);
            wasm_bindgen_futures::spawn_local(async move {
                match crate::api::get_json::<serde_json::Value>("/api/oauth/authorize-url").await {
                    Ok(r) => {
                        if let Some(url) = r.get("url").and_then(|v| v.as_str()) {
                            if let Some(w) = web_sys::window() {
                                let _ = w.open_with_url_and_target_and_features(
                                    url, "_blank", "noopener",
                                );
                            }
                        } else {
                            let message = r
                                .get("message")
                                .and_then(|v| v.as_str())
                                .unwrap_or("Demo mode active.");
                            toasts::info(message);
                        }
                        load();
                    }
                    Err(e) => toasts::error(e),
                }
            });
        }
    };

    // Client Credentials (reference: POST /api/oauth/client-credentials).
    let client_creds = {
        let load = Rc::clone(&load);
        move |_| {
            let load = Rc::clone(&load);
            wasm_bindgen_futures::spawn_local(async move {
                match crate::api::post_json::<serde_json::Value>(
                    "/api/oauth/client-credentials",
                    None,
                )
                .await
                {
                    Ok(r) => {
                        let ok = r.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
                        let message = r
                            .get("message")
                            .and_then(|v| v.as_str())
                            .unwrap_or("Request failed.");
                        if ok {
                            toasts::success(message);
                        } else {
                            toasts::error(message);
                        }
                        load();
                    }
                    // v1.6.0 audit fix: surface network failures instead of a
                    // silent no-op.
                    Err(e) => toasts::error(e),
                }
            });
        }
    };

    // Disconnect (reference: POST /api/oauth/disconnect + success toast).
    let disconnect = {
        let load = Rc::clone(&load);
        move |_| {
            let load = Rc::clone(&load);
            wasm_bindgen_futures::spawn_local(async move {
                match crate::api::post_json::<serde_json::Value>("/api/oauth/disconnect", None)
                    .await
                {
                    Ok(r) => {
                        let message = r
                            .get("message")
                            .and_then(|v| v.as_str())
                            .unwrap_or("Disconnected.");
                        toasts::success(message);
                        load();
                    }
                    Err(e) => toasts::error(e),
                }
            });
        }
    };

    view! {
        <section class="spp-card spp-settings__card">
            <h3 class="spp-settings__card-title">"Help Scout connection"</h3>
            {move || {
                    let o = oauth.get().unwrap_or_default();
                    view! {
                        <Show when=move || o.demo_mode fallback=|| ().into_view()>
                            <div class="spp-state spp-state--warn">
                                "Demo mode is active (simulated Help Scout account). Switch off LOCAL_DEMO_MODE in .env and restart to connect a real account."
                            </div>
                        </Show>
                        <div class="spp-settings__kv">
                            <span class="spp-settings__label">"App credentials"</span>
                            <span>{
                                if o.configured {
                                    "configured via .env"
                                } else {
                                    "not configured (set HELPSCOUT_CLIENT_ID/SECRET in .env)"
                                }
                            }</span>
                        </div>
                        <div class="spp-settings__kv">
                            <span class="spp-settings__label">"Connected as"</span>
                            <span>{format_oauth_me(&o)}</span>
                        </div>
                        <div class="spp-settings__kv">
                            <span class="spp-settings__label">"Token expires"</span>
                            <span>{o.expires_at.clone().unwrap_or_else(|| "—".to_string())}</span>
                        </div>
                    }.into_view()
                }}
                <div class="spp-flex spp-flex--wrap spp-mt-4">
                    <button class="spp-button spp-button--primary" type="button" on:click=authorize>
                        "Authorize via browser (OAuth code flow)"
                    </button>
                    <button class="spp-button" type="button" on:click=client_creds>
                        "Connect with Client Credentials"
                    </button>
                    <button class="spp-button spp-button--danger" type="button" on:click=disconnect>
                        "Disconnect"
                    </button>
                </div>
                <p class="spp-settings__hint">
                    "Tokens are stored server-side only and never exposed to the browser. The callback URL for your Help Scout app is /oauth/callback (configure HELPSCOUT_REDIRECT_URI). Client Credentials is the simplest flow for a personal internal integration."
                </p>
        </section>
    }
}

// ---------------------------------------------------------------------------
// LM Studio
// ---------------------------------------------------------------------------

/// The `GET /api/settings/lmstudio` payload (form state).
#[derive(Debug, Clone, PartialEq)]
pub struct LmStudioForm {
    pub base_url: String,
    pub chat_model: String,
    pub embedding_model: String,
    pub timeout_ms: i64,
    pub concurrency: i64,
}

impl Default for LmStudioForm {
    fn default() -> Self {
        Self {
            base_url: "http://127.0.0.1:1234".to_string(),
            chat_model: String::new(),
            embedding_model: String::new(),
            timeout_ms: 120_000,
            concurrency: 2,
        }
    }
}

/// Parse the lmstudio settings payload.
#[must_use]
pub fn parse_lmstudio(v: &serde_json::Value) -> LmStudioForm {
    LmStudioForm {
        base_url: v
            .get("base_url")
            .and_then(|x| x.as_str())
            .unwrap_or("http://127.0.0.1:1234")
            .to_string(),
        chat_model: v
            .get("chat_model")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string(),
        embedding_model: v
            .get("embedding_model")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string(),
        timeout_ms: v
            .get("timeout_ms")
            .and_then(|x| x.as_i64())
            .unwrap_or(120_000),
        concurrency: v.get("concurrency").and_then(|x| x.as_i64()).unwrap_or(2),
    }
}

/// The LM Studio PATCH body (blank models -> null, like the reference form).
#[must_use]
pub fn lmstudio_patch_body(f: &LmStudioForm) -> serde_json::Value {
    serde_json::json!({
        "base_url": f.base_url,
        "chat_model": if f.chat_model.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(f.chat_model.clone()) },
        "embedding_model": if f.embedding_model.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(f.embedding_model.clone()) },
        "timeout_ms": f.timeout_ms,
        "concurrency": f.concurrency,
    })
}

/// The LM Studio tab — editable form + test connection with model discovery
/// (reference `LmStudioSettings`, including the v1.6.0 default-capture fix).
/// Like the reference, Test probes the STORED settings (save first to test
/// what you see).
#[component]
fn LmStudioSettingsTab() -> impl IntoView {
    let form = create_rw_signal(LmStudioForm::default());
    let loaded = create_rw_signal(false);
    let testing = create_rw_signal(false);
    let models = create_rw_signal(Vec::<String>::new());

    // v1.6.0 audit fix: re-sync the form as soon as the loaded settings
    // arrive (before the user edits) — the form must never save the
    // defaults over real settings.
    wasm_bindgen_futures::spawn_local(async move {
        match crate::api::get_json::<serde_json::Value>("/api/settings/lmstudio").await {
            Ok(v) => {
                if !loaded.get() {
                    form.set(parse_lmstudio(&v));
                    loaded.set(true);
                }
            }
            Err(e) => toasts::error(e),
        }
    });

    let save = move |_| {
        let body = lmstudio_patch_body(&form.get());
        wasm_bindgen_futures::spawn_local(async move {
            match crate::api::patch_json::<serde_json::Value>("/api/settings/lmstudio", &body).await
            {
                Ok(r) => {
                    let message = r
                        .get("message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("LM Studio settings saved.");
                    toasts::success(message);
                }
                Err(e) => toasts::error(e),
            }
        });
    };

    let test = move |_| {
        testing.set(true);
        wasm_bindgen_futures::spawn_local(async move {
            match crate::api::post_json::<serde_json::Value>("/api/settings/lmstudio/test", None)
                .await
            {
                Ok(r) => {
                    let connected = r
                        .get("connected")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let message = r
                        .get("message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Probe finished.");
                    models.set(
                        r.get("models")
                            .and_then(|v| v.as_array())
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|m| m.as_str().map(str::to_string))
                                    .collect()
                            })
                            .unwrap_or_default(),
                    );
                    if connected {
                        toasts::success(message);
                    } else {
                        toasts::error(message);
                    }
                }
                Err(e) => toasts::error(e),
            }
            testing.set(false);
        });
    };

    view! {
        <section class="spp-card spp-settings__card">
            <h3 class="spp-settings__card-title">"LM Studio (local AI gateway)"</h3>
            <div class="spp-settings__field">
                <label class="spp-settings__label" for="lm-url">"Base URL"</label>
                <input
                    id="lm-url"
                    class="spp-input spp-mono"
                    type="text"
                    prop:value=move || form.get().base_url
                    on:input=move |ev| form.update(|f| f.base_url = event_target_value(&ev))
                    placeholder="http://127.0.0.1:1234"
                />
            </div>
            <div class="spp-settings__field">
                <label class="spp-settings__label" for="lm-chat">"Chat model (blank = first loaded)"</label>
                <input
                    id="lm-chat"
                    class="spp-input spp-mono"
                    type="text"
                    list="lm-models"
                    prop:value=move || form.get().chat_model
                    on:input=move |ev| form.update(|f| f.chat_model = event_target_value(&ev))
                    placeholder="e.g. qwen2.5-7b-instruct"
                />
            </div>
            <div class="spp-settings__field">
                <label class="spp-settings__label" for="lm-embed">"Embedding model (enables vector features)"</label>
                <input
                    id="lm-embed"
                    class="spp-input spp-mono"
                    type="text"
                    list="lm-models"
                    prop:value=move || form.get().embedding_model
                    on:input=move |ev| form.update(|f| f.embedding_model = event_target_value(&ev))
                    placeholder="e.g. nomic-embed-text-v1.5"
                />
            </div>
            <datalist id="lm-models">
                {move || {
                    models
                        .get()
                        .into_iter()
                        .map(|m| {
                            view! { <option value=m.clone() /> }.into_view()
                        })
                        .collect::<Vec<_>>()
                }}
            </datalist>
            <div class="spp-grid-2">
                <div class="spp-settings__field">
                    <label class="spp-settings__label" for="lm-timeout">"Timeout (ms)"</label>
                    <input
                        id="lm-timeout"
                        class="spp-input"
                        type="number"
                        prop:value=move || form.get().timeout_ms.to_string()
                        on:input=move |ev| form.update(|f| f.timeout_ms = event_target_value(&ev).parse().unwrap_or(f.timeout_ms))
                    />
                </div>
                <div class="spp-settings__field">
                    <label class="spp-settings__label" for="lm-conc">"Concurrency"</label>
                    <input
                        id="lm-conc"
                        class="spp-input"
                        type="number"
                        min="1"
                        max="8"
                        prop:value=move || form.get().concurrency.to_string()
                        on:input=move |ev| form.update(|f| f.concurrency = event_target_value(&ev).parse().unwrap_or(f.concurrency))
                    />
                </div>
            </div>
            <div class="spp-flex">
                <button class="spp-button" type="button" on:click=test>
                    {move || if testing.get() { "Testing…" } else { "Test connection + discover models" }}
                </button>
                <button class="spp-button spp-button--primary" type="button" on:click=save>
                    "Save"
                </button>
            </div>
            {move || {
                let discovered = models.get();
                if discovered.is_empty() {
                    ().into_view()
                } else {
                    view! {
                        <div class="spp-mt-4">
                            <strong class="spp-text-sm">"Discovered models:"</strong>
                            <div class="spp-mt-4">
                                {discovered
                                    .into_iter()
                                    .map(|m| {
                                        let for_chip = m.clone();
                                        view! {
                                            <button
                                                class="spp-badge spp-settings__model-chip"
                                                type="button"
                                                on:click=move |_| form.update(|f| f.chat_model = for_chip.clone())
                                            >
                                                {m.clone()}
                                            </button>
                                        }
                                        .into_view()
                                    })
                                    .collect::<Vec<_>>()}
                            </div>
                        </div>
                    }.into_view()
                }
            }}
            <p class="spp-settings__hint">
                "In LM Studio: load a model, then Developer → Start Server. Requests go to your local machine only - nothing is sent to any cloud AI provider."
            </p>
        </section>
    }
}

// ---------------------------------------------------------------------------
// Qdrant
// ---------------------------------------------------------------------------

/// The `GET /api/settings/qdrant` payload.
#[derive(Debug, Clone, PartialEq)]
pub struct QdrantForm {
    pub url: String,
    pub enabled: bool,
}

impl Default for QdrantForm {
    fn default() -> Self {
        Self {
            url: "http://127.0.0.1:6333".to_string(),
            enabled: true,
        }
    }
}

/// Parse the qdrant settings payload.
#[must_use]
pub fn parse_qdrant(v: &serde_json::Value) -> QdrantForm {
    QdrantForm {
        url: v
            .get("url")
            .and_then(|x| x.as_str())
            .unwrap_or("http://127.0.0.1:6333")
            .to_string(),
        enabled: v.get("enabled").and_then(|x| x.as_bool()).unwrap_or(true),
    }
}

/// The Qdrant tab — url + enabled, test + save (reference `QdrantSettings`
/// with the same v1.6.0 re-sync fix; the test result also renders inline).
#[component]
fn QdrantSettingsTab() -> impl IntoView {
    let form = create_rw_signal(QdrantForm::default());
    let loaded = create_rw_signal(false);
    let result = create_rw_signal(None::<String>);

    wasm_bindgen_futures::spawn_local(async move {
        match crate::api::get_json::<serde_json::Value>("/api/settings/qdrant").await {
            Ok(v) => {
                if !loaded.get() {
                    form.set(parse_qdrant(&v));
                    loaded.set(true);
                }
            }
            Err(e) => toasts::error(e),
        }
    });

    let save = move |_| {
        let f = form.get();
        wasm_bindgen_futures::spawn_local(async move {
            let body = serde_json::json!({ "url": f.url, "enabled": f.enabled });
            match crate::api::patch_json::<serde_json::Value>("/api/settings/qdrant", &body).await {
                Ok(r) => {
                    let message = r
                        .get("message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Qdrant settings saved.");
                    toasts::success(message);
                }
                Err(e) => toasts::error(e),
            }
        });
    };

    let test = move |_| {
        wasm_bindgen_futures::spawn_local(async move {
            match crate::api::post_json::<serde_json::Value>("/api/settings/qdrant/test", None)
                .await
            {
                Ok(r) => {
                    let connected = r
                        .get("connected")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let message = r
                        .get("message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Probe finished.");
                    result.set(Some(message.to_string()));
                    if connected {
                        toasts::success(message);
                    } else {
                        toasts::warning(message);
                    }
                }
                Err(e) => toasts::error(e),
            }
        });
    };

    view! {
        <section class="spp-card spp-settings__card">
            <h3 class="spp-settings__card-title">"Qdrant (local vector store, optional)"</h3>
            <div class="spp-settings__field">
                <label class="spp-settings__label" for="q-url">"URL"</label>
                <input
                    id="q-url"
                    class="spp-input spp-mono"
                    type="text"
                    prop:value=move || form.get().url
                    on:input=move |ev| form.update(|f| f.url = event_target_value(&ev))
                    placeholder="http://127.0.0.1:6333"
                />
            </div>
            {toggle_row(
                "Enabled",
                "When unavailable, keyword search (FTS5) remains fully functional",
                move || form.get().enabled,
                move |v| form.update(|f| f.enabled = v),
            )}
            <div class="spp-flex spp-mt-4">
                <button class="spp-button" type="button" on:click=test>
                    "Test connection"
                </button>
                <button class="spp-button spp-button--primary" type="button" on:click=save>
                    "Save"
                </button>
            </div>
            {move || {
                match result.get() {
                    Some(r) => view! {
                        <div class="spp-state spp-state--info spp-mt-4">{r}</div>
                    }.into_view(),
                    None => ().into_view(),
                }
            }}
            <p class="spp-settings__hint">
                "Run Qdrant locally (docker run -p 6333:6333 qdrant/qdrant). Embedding-model changes are detected and never silently mixed with old vectors."
            </p>
        </section>
    }
}

// ---------------------------------------------------------------------------
// Business hours
// ---------------------------------------------------------------------------

const DAY_LABELS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

const TIMEZONE_SUGGESTIONS: [&str; 11] = [
    "UTC",
    "America/New_York",
    "America/Chicago",
    "America/Los_Angeles",
    "America/Santiago",
    "Europe/London",
    "Europe/Berlin",
    "Europe/Stockholm",
    "Asia/Kolkata",
    "Asia/Tokyo",
    "Australia/Sydney",
];

/// One mailbox row of `GET /api/settings/business-hours`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BusinessHoursRow {
    pub mailbox_id: i64,
    pub name: String,
    pub configured: bool,
    pub timezone: Option<String>,
    pub days: Option<Vec<i64>>,
    pub start_minute: Option<i64>,
    pub end_minute: Option<i64>,
    pub first_response_target_min: Option<i64>,
    pub resolution_target_min: Option<i64>,
}

/// Parse the business-hours payload `{mailboxes: [...]}`.
#[must_use]
pub fn parse_business_hours(v: &serde_json::Value) -> Vec<BusinessHoursRow> {
    v.get("mailboxes")
        .and_then(|x| x.as_array())
        .map(|arr| arr.iter().filter_map(parse_bh_row).collect())
        .unwrap_or_default()
}

fn parse_bh_row(r: &serde_json::Value) -> Option<BusinessHoursRow> {
    Some(BusinessHoursRow {
        mailbox_id: r.get("mailbox_id")?.as_i64()?,
        name: r.get("name")?.as_str()?.to_string(),
        configured: r
            .get("configured")
            .and_then(|x| x.as_bool())
            .unwrap_or(false),
        timezone: r
            .get("timezone")
            .and_then(|x| x.as_str())
            .map(str::to_string),
        days: r
            .get("days")
            .and_then(|x| x.as_array())
            .map(|arr| arr.iter().filter_map(|d| d.as_i64()).collect::<Vec<_>>()),
        start_minute: r.get("start_minute").and_then(|x| x.as_i64()),
        end_minute: r.get("end_minute").and_then(|x| x.as_i64()),
        first_response_target_min: r.get("first_response_target_min").and_then(|x| x.as_i64()),
        resolution_target_min: r.get("resolution_target_min").and_then(|x| x.as_i64()),
    })
}

/// Minutes-of-day -> "HH:MM" (reference `minutesToHHMM`; null -> "").
#[must_use]
pub fn minutes_to_hhmm(m: Option<i64>) -> String {
    match m {
        None => String::new(),
        Some(m) => format!("{:02}:{:02}", m / 60, m % 60),
    }
}

/// "H:MM"/"HH:MM" -> minutes-of-day; invalid -> None
/// (reference `hhmmToMinutes`).
#[must_use]
pub fn hhmm_to_minutes(v: &str) -> Option<i64> {
    let (h, min) = v.split_once(':')?;
    let h: i64 = h.parse().ok()?;
    let min: i64 = min.parse().ok()?;
    if h > 24 || min > 59 || h < 0 || min < 0 {
        return None;
    }
    Some(h * 60 + min)
}

/// The schedule summary "Mon Tue Wed · 09:00–17:00 · Europe/Berlin"
/// (reference row rendering; unconfigured -> None for the wall-clock badge).
#[must_use]
pub fn bh_schedule_label(row: &BusinessHoursRow) -> Option<String> {
    if !row.configured {
        return None;
    }
    let days = row.days.as_ref()?;
    let days: Vec<&str> = days
        .iter()
        .filter_map(|d| {
            usize::try_from(*d)
                .ok()
                .and_then(|i| DAY_LABELS.get(i).copied())
        })
        .collect();
    Some(format!(
        "{} · {}–{} · {}",
        days.join(" "),
        minutes_to_hhmm(row.start_minute),
        minutes_to_hhmm(row.end_minute),
        row.timezone.clone().unwrap_or_else(|| "UTC".to_string())
    ))
}

/// The Business hours tab — per-mailbox schedule + SLA targets
/// (reference `BusinessHoursSettings` + `BusinessHoursEditor`).
#[component]
fn BusinessHoursSettingsTab() -> impl IntoView {
    let rows = create_rw_signal(Vec::<BusinessHoursRow>::new());
    let loading = create_rw_signal(true);
    let error_msg = create_rw_signal(None::<String>);
    let editing = create_rw_signal(None::<BusinessHoursRow>);

    let load: Rc<dyn Fn()> = Rc::new(move || {
        let rows = rows;
        let loading = loading;
        let error_msg = error_msg;
        wasm_bindgen_futures::spawn_local(async move {
            match crate::api::get_json::<serde_json::Value>("/api/settings/business-hours").await {
                Ok(v) => {
                    rows.set(parse_business_hours(&v));
                    error_msg.set(None);
                    loading.set(false);
                }
                Err(e) => {
                    error_msg.set(Some(e));
                    loading.set(false);
                }
            }
        });
    });
    load();

    view! {
        <section class="spp-card spp-settings__card">
            <h3 class="spp-settings__card-title">"Business hours & SLA targets (per mailbox)"</h3>
            <p class="spp-settings__hint">
                "SLA reports measure first-response and resolution times in business minutes - nights, weekends and non-configured weekdays contribute zero. Without a schedule the mailbox is measured in wall-clock minutes and labeled as such in Reports → SLA."
            </p>
            <Show when=move || loading.get() fallback=|| ().into_view()>
                <LoadingState />
            </Show>
            <Show when=move || error_msg.get().is_some() fallback=|| ().into_view()>
                <ErrorState message="Could not load business hours." retry=None />
            </Show>
            <Show
                when=move || !loading.get() && error_msg.get().is_none()
                fallback=|| ().into_view()
            >
                <BusinessHoursContent rows=rows editing=editing load=Rc::clone(&load) />
            </Show>
        </section>
    }
}

/// The loaded business-hours table + editor (FnOnce context owning the
/// clear/close/cancel handlers — `Show` children are `Fn` and cannot move
/// captures).
#[component]
fn BusinessHoursContent(
    rows: RwSignal<Vec<BusinessHoursRow>>,
    editing: RwSignal<Option<BusinessHoursRow>>,
    load: Rc<dyn Fn()>,
) -> impl IntoView {
    // Clear (reference: DELETE /:mailboxId + toast + refetch).
    let clear_hours: Rc<dyn Fn(i64)> = Rc::new({
        let load = Rc::clone(&load);
        move |mailbox_id: i64| {
            let load = Rc::clone(&load);
            wasm_bindgen_futures::spawn_local(async move {
                let path = format!("/api/settings/business-hours/{mailbox_id}");
                match crate::api::delete_json::<serde_json::Value>(&path).await {
                    Ok(r) => {
                        let message = r
                            .get("message")
                            .and_then(|v| v.as_str())
                            .unwrap_or("Business hours cleared.");
                        toasts::success(message);
                        load();
                    }
                    Err(e) => toasts::error(e),
                }
            });
        }
    });

    // Save success closes the editor (reference `setEditing(null)`).
    let close_editor: Rc<dyn Fn()> = Rc::new({
        let load = Rc::clone(&load);
        move || {
            editing.set(None);
            load();
        }
    });
    let cancel_editor: Rc<dyn Fn()> = Rc::new(move || editing.set(None));

    view! {
        <Show
            when=move || !rows.get().is_empty()
            fallback=|| {
                view! {
                    <EmptyState message="No mailboxes yet - run an initial sync first." />
                }
                .into_view()
            }
        >
            <BusinessHoursTable rows=rows editing=editing clear_hours=Rc::clone(&clear_hours) />
        </Show>
        {move || {
            match editing.get() {
                Some(row) => view! {
                    <BusinessHoursEditor
                        row=row
                        on_done=Rc::clone(&close_editor)
                        on_cancel=Rc::clone(&cancel_editor)
                    />
                }.into_view(),
                None => ().into_view(),
            }
        }}
    }
}

/// The per-mailbox schedule table. A separate component because the rows
/// reactive closure must OWN its `clear_hours` — it sits inside the
/// empty-check `Show` whose children are `Fn` closures and cannot give it
/// up. The component body is the FnOnce context that owns the prop.
#[component]
fn BusinessHoursTable(
    rows: RwSignal<Vec<BusinessHoursRow>>,
    editing: RwSignal<Option<BusinessHoursRow>>,
    clear_hours: Rc<dyn Fn(i64)>,
) -> impl IntoView {
    view! {
        <table class="spp-table">
            <thead>
                <tr>
                    <th>"Mailbox"</th>
                    <th>"Schedule"</th>
                    <th>"First-response target"</th>
                    <th></th>
                </tr>
            </thead>
            <tbody>
                {move || {
                    rows.get()
                        .into_iter()
                        .map(|m| {
                            view! {
                                <BusinessHoursRowView
                                    row=m
                                    editing=editing
                                    clear_hours=Rc::clone(&clear_hours)
                                />
                            }
                        })
                        .collect::<Vec<_>>()
                }}
            </tbody>
        </table>
    }
}

/// One mailbox schedule row (the RuleCard pattern: a row component's
/// FnOnce body owns its `clear_hours` clone; inline handlers inside
/// reactive row views can only capture Copy values).
#[component]
fn BusinessHoursRowView(
    row: BusinessHoursRow,
    editing: RwSignal<Option<BusinessHoursRow>>,
    clear_hours: Rc<dyn Fn(i64)>,
) -> impl IntoView {
    let schedule = bh_schedule_label(&row);
    let mailbox_id = row.mailbox_id;
    let configured = row.configured;
    let fr_target = row.first_response_target_min;
    let name = row.name.clone();
    view! {
        <tr>
            <td><strong>{name}</strong></td>
            <td class="spp-text-sm">
                {match schedule {
                    Some(s) => view! { <span>{s}</span> }.into_view(),
                    None => view! {
                        <span class="spp-badge">"wall-clock (not configured)"</span>
                    }.into_view(),
                }}
            </td>
            <td class="spp-text-sm">
                {match fr_target {
                    Some(n) => format!("{n} business min"),
                    None => "—".to_string(),
                }}
            </td>
            <td>
                <div class="spp-flex">
                    <button
                        class="spp-button spp-button--tiny"
                        type="button"
                        on:click=move |_| editing.set(Some(row.clone()))
                    >
                        "Edit"
                    </button>
                    {if configured {
                        // Plain conditional (the reference ternary): the row's
                        // `configured` flag is static data, and a Show would
                        // wrap its children in an Fn closure that cannot own
                        // the Clear handler.
                        view! {
                            <button
                                class="spp-button spp-button--ghost spp-button--tiny"
                                type="button"
                                on:click=move |_| clear_hours(mailbox_id)
                            >
                                "Clear"
                            </button>
                        }.into_view()
                    } else {
                        ().into_view()
                    }}
                </div>
            </td>
        </tr>
    }
}

/// The inline per-mailbox editor (reference `BusinessHoursEditor`).
#[component]
fn BusinessHoursEditor(
    row: BusinessHoursRow,
    on_done: Rc<dyn Fn()>,
    on_cancel: Rc<dyn Fn()>,
) -> impl IntoView {
    let name = row.name.clone();
    let mailbox_id = row.mailbox_id;
    let timezone = create_rw_signal(row.timezone.clone().unwrap_or_else(|| "UTC".to_string()));
    let days = create_rw_signal(row.days.clone().unwrap_or_else(|| vec![1, 2, 3, 4, 5]));
    let start = create_rw_signal(minutes_to_hhmm(row.start_minute.or(Some(540))));
    let end = create_rw_signal(minutes_to_hhmm(row.end_minute.or(Some(1020))));
    let fr_target = create_rw_signal(
        row.first_response_target_min
            .map(|n| n.to_string())
            .unwrap_or_default(),
    );
    let res_target = create_rw_signal(
        row.resolution_target_min
            .map(|n| n.to_string())
            .unwrap_or_default(),
    );

    let submit = {
        let on_done = Rc::clone(&on_done);
        move |_| {
            let Some(start_min) = hhmm_to_minutes(&start.get()) else {
                toasts::error("Day starts must be HH:MM.");
                return;
            };
            let Some(end_min) = hhmm_to_minutes(&end.get()) else {
                toasts::error("Day ends must be HH:MM.");
                return;
            };
            if end_min <= start_min {
                toasts::error("Day end must be after day start.");
                return;
            }
            let body = serde_json::json!({
                "timezone": timezone.get(),
                "days": days.get(),
                "start_minute": start_min,
                "end_minute": end_min,
                "first_response_target_min": parse_optional_positive(&fr_target.get()),
                "resolution_target_min": parse_optional_positive(&res_target.get()),
            });
            let on_done = Rc::clone(&on_done);
            wasm_bindgen_futures::spawn_local(async move {
                let path = format!("/api/settings/business-hours/{mailbox_id}");
                match crate::api::put_json::<serde_json::Value>(&path, &body).await {
                    Ok(r) => {
                        let ok = r.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
                        let message = r
                            .get("message")
                            .and_then(|v| v.as_str())
                            .unwrap_or("Business hours could not be saved.");
                        if ok {
                            toasts::success(message);
                        } else {
                            toasts::error(message);
                        }
                        on_done();
                    }
                    Err(e) => toasts::error(e),
                }
            });
        }
    };

    view! {
        <div class="spp-settings__editor">
            <strong class="spp-text-sm">{format!("Business hours for {name}")}</strong>
            <div class="spp-settings__field">
                <label class="spp-settings__label" for="bh-tz">"Timezone (IANA)"</label>
                <input
                    id="bh-tz"
                    class="spp-input spp-mono"
                    type="text"
                    list="bh-tz-list"
                    prop:value=move || timezone.get()
                    on:input=move |ev| timezone.set(event_target_value(&ev))
                    placeholder="Europe/Berlin"
                />
            </div>
            <datalist id="bh-tz-list">
                {TIMEZONE_SUGGESTIONS
                    .iter()
                    .map(|t| view! { <option value=*t /> })
                    .collect::<Vec<_>>()}
            </datalist>
            <div class="spp-settings__field">
                <span class="spp-settings__label">"Active weekdays"</span>
                <div class="spp-flex spp-flex--wrap">
                    {DAY_LABELS
                        .iter()
                        .enumerate()
                        .map(|(i, label)| {
                            view! {
                                <button
                                    class="spp-button spp-button--tiny"
                                    class:is-active=move || days.get().contains(&(i as i64))
                                    type="button"
                                    aria-pressed=move || days.get().contains(&(i as i64))
                                    on:click=move |_| days.update(|d| {
                                        if let Some(pos) = d.iter().position(|x| *x == i as i64) {
                                            d.remove(pos);
                                        } else {
                                            d.push(i as i64);
                                            d.sort_unstable();
                                        }
                                    })
                                >
                                    {*label}
                                </button>
                            }
                        })
                        .collect::<Vec<_>>()}
                </div>
            </div>
            <div class="spp-grid-2">
                <div class="spp-settings__field">
                    <label class="spp-settings__label" for="bh-start">"Day starts"</label>
                    <input
                        id="bh-start"
                        class="spp-input"
                        type="time"
                        prop:value=move || start.get()
                        on:input=move |ev| start.set(event_target_value(&ev))
                    />
                </div>
                <div class="spp-settings__field">
                    <label class="spp-settings__label" for="bh-end">"Day ends"</label>
                    <input
                        id="bh-end"
                        class="spp-input"
                        type="time"
                        prop:value=move || end.get()
                        on:input=move |ev| end.set(event_target_value(&ev))
                    />
                </div>
            </div>
            <div class="spp-grid-2">
                <div class="spp-settings__field">
                    <label class="spp-settings__label" for="bh-fr">"First-response SLA target (business minutes, blank = none)"</label>
                    <input
                        id="bh-fr"
                        class="spp-input"
                        type="number"
                        min="1"
                        prop:value=move || fr_target.get()
                        on:input=move |ev| fr_target.set(event_target_value(&ev))
                        placeholder="e.g. 240 for 4 business hours"
                    />
                </div>
                <div class="spp-settings__field">
                    <label class="spp-settings__label" for="bh-res">"Resolution SLA target (business minutes, blank = none)"</label>
                    <input
                        id="bh-res"
                        class="spp-input"
                        type="number"
                        min="1"
                        prop:value=move || res_target.get()
                        on:input=move |ev| res_target.set(event_target_value(&ev))
                        placeholder="e.g. 2880 for 2 business days"
                    />
                </div>
            </div>
            <div class="spp-flex">
                <button class="spp-button spp-button--primary" type="button" on:click=submit>
                    "Save schedule"
                </button>
                <button class="spp-button spp-button--ghost" type="button" on:click=move |_| on_cancel()>
                    "Cancel"
                </button>
            </div>
        </div>
    }
}

/// "" -> None (no target), positive int -> Some, anything else -> None
/// (reference: blank/invalid -> null, > 0 -> number).
fn parse_optional_positive(raw: &str) -> Option<i64> {
    let t = raw.trim();
    if t.is_empty() {
        return None;
    }
    t.parse::<i64>().ok().filter(|n| *n > 0)
}

// ---------------------------------------------------------------------------
// Backups & export
// ---------------------------------------------------------------------------

/// One backup file row of `GET /api/backups`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BackupRow {
    pub file: String,
    pub size_bytes: i64,
    pub created_at: String,
    pub verified: bool,
}

/// Parse the backups payload `{backups: [...]}`.
#[must_use]
pub fn parse_backups(v: &serde_json::Value) -> Vec<BackupRow> {
    v.get("backups")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .map(|b| BackupRow {
                    file: b
                        .get("file")
                        .and_then(|x| x.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    size_bytes: b.get("size_bytes").and_then(|x| x.as_i64()).unwrap_or(0),
                    created_at: b
                        .get("created_at")
                        .and_then(|x| x.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    verified: b.get("verified").and_then(|x| x.as_bool()).unwrap_or(false),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// "12.3 MB" formatting (reference `(bytes/1024/1024).toFixed(1)`).
#[must_use]
pub fn format_mb(bytes: i64) -> String {
    format!("{:.1} MB", bytes as f64 / 1024.0 / 1024.0)
}

/// The Backups tab — create + JSON/CSV exports + the file table
/// (reference `BackupsSettings`).
#[component]
fn BackupsSettingsTab() -> impl IntoView {
    let backups = create_rw_signal(Vec::<BackupRow>::new());
    let loading = create_rw_signal(true);
    let error_msg = create_rw_signal(None::<String>);

    let load: Rc<dyn Fn()> = Rc::new(move || {
        let backups = backups;
        let loading = loading;
        let error_msg = error_msg;
        wasm_bindgen_futures::spawn_local(async move {
            match crate::api::get_json::<serde_json::Value>("/api/backups").await {
                Ok(v) => {
                    backups.set(parse_backups(&v));
                    error_msg.set(None);
                    loading.set(false);
                }
                Err(e) => {
                    error_msg.set(Some(e));
                    loading.set(false);
                }
            }
        });
    });
    load();

    view! {
        <section class="spp-card spp-settings__card">
            <h3 class="spp-settings__card-title">"Backups & export"</h3>
            <Show when=move || loading.get() fallback=|| ().into_view()>
                <LoadingState />
            </Show>
            <Show when=move || error_msg.get().is_some() fallback=|| ().into_view()>
                <ErrorState message="Could not load backups." retry=None />
            </Show>
            <Show
                when=move || !loading.get() && error_msg.get().is_none()
                fallback=|| ().into_view()
            >
                <BackupsContent backups=backups load=Rc::clone(&load) />
            </Show>
        </section>
    }
}

/// The loaded backups content — action buttons + file table (FnOnce
/// context owning its handlers — `Show` children are `Fn` and cannot move
/// captures).
#[component]
fn BackupsContent(backups: RwSignal<Vec<BackupRow>>, load: Rc<dyn Fn()>) -> impl IntoView {
    // Shared POST helper: toast the message, refetch on ok (create appends
    // "(integrity verified)" like the reference).
    let run_backup_action: Rc<dyn Fn(&'static str)> = Rc::new({
        let load = Rc::clone(&load);
        move |path: &'static str| {
            let load = Rc::clone(&load);
            wasm_bindgen_futures::spawn_local(async move {
                match crate::api::post_json::<serde_json::Value>(path, None).await {
                    Ok(r) => {
                        let ok = r.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
                        let verified = r.get("verified").and_then(|v| v.as_bool());
                        let mut message = r
                            .get("message")
                            .and_then(|v| v.as_str())
                            .unwrap_or("Request failed.")
                            .to_string();
                        if verified == Some(true) {
                            message.push_str(" (integrity verified)");
                        }
                        if ok {
                            toasts::success(message);
                            load();
                        } else {
                            toasts::error(message);
                        }
                    }
                    Err(e) => toasts::error(e),
                }
            });
        }
    });
    let create_backup = {
        let r = Rc::clone(&run_backup_action);
        move |_| r("/api/backups/create")
    };
    let export_json = {
        let r = Rc::clone(&run_backup_action);
        move |_| r("/api/backups/export-json")
    };
    let export_csv = {
        let r = Rc::clone(&run_backup_action);
        move |_| r("/api/backups/export-csv")
    };

    view! {
        <div class="spp-flex spp-flex--wrap">
            <button class="spp-button spp-button--primary" type="button" on:click=create_backup>
                "Backup database now"
            </button>
            <button class="spp-button" type="button" on:click=export_json>
                "Export JSON"
            </button>
            <button class="spp-button" type="button" on:click=export_csv>
                "Export conversations CSV"
            </button>
        </div>
        <Show
                    when=move || !backups.get().is_empty()
                    fallback=|| {
                        view! { <EmptyState message="No backups yet." /> }.into_view()
                    }
                >
                    <table class="spp-table">
                        <thead>
                            <tr>
                                <th>"Backup file"</th>
                                <th>"Size"</th>
                                <th>"Created"</th>
                                <th>"Integrity"</th>
                            </tr>
                        </thead>
                        <tbody>
                            {move || {
                                backups.get()
                                    .into_iter()
                                    .map(|b| {
                                        let verified = b.verified;
                                        let file = b.file.clone();
                                        let created = b.created_at.clone();
                                        let size = format_mb(b.size_bytes);
                                        view! {
                                            <tr>
                                                <td class="spp-mono spp-text-xs">{file}</td>
                                                <td>{size}</td>
                                                <td>{created}</td>
                                                <td>
                                                    {if verified {
                                                        view! { <span class="spp-badge spp-badge--ok">"verified"</span> }.into_view()
                                                    } else {
                                                        view! { <span class="spp-badge spp-badge--err">"check failed"</span> }.into_view()
                                                    }}
                                                </td>
                                            </tr>
                                        }.into_view()
                                    })
                                    .collect::<Vec<_>>()
                            }}
                        </tbody>
                    </table>
                </Show>
                <p class="spp-settings__hint">
                    "Restore from the CLI: npm run db:restore -- backups/<file>.db (the app must be stopped). Exports contain customer data - handle carefully. See docs/BACKUP-RESTORE.md."
                </p>
    }
}

// ---------------------------------------------------------------------------
// Encrypted sync
// ---------------------------------------------------------------------------

/// One sync-ledger entry of `GET /api/sync/encrypted`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SyncLogEntry {
    pub direction: String,
    pub file_path: String,
    pub size_bytes: i64,
    pub conversations: Option<i64>,
}

/// The `GET /api/sync/encrypted` payload the tab renders.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EncryptedSyncData {
    pub bundle_dir: String,
    pub log: Vec<SyncLogEntry>,
}

/// Parse the encrypted-sync payload.
#[must_use]
pub fn parse_encrypted_sync(v: &serde_json::Value) -> EncryptedSyncData {
    EncryptedSyncData {
        bundle_dir: v
            .get("bundle_dir")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string(),
        log: v
            .get("log")
            .and_then(|x| x.as_array())
            .map(|arr| {
                arr.iter()
                    .map(|l| SyncLogEntry {
                        direction: l
                            .get("direction")
                            .and_then(|x| x.as_str())
                            .unwrap_or_default()
                            .to_string(),
                        file_path: l
                            .get("file_path")
                            .and_then(|x| x.as_str())
                            .unwrap_or_default()
                            .to_string(),
                        size_bytes: l.get("size_bytes").and_then(|x| x.as_i64()).unwrap_or(0),
                        conversations: l.get("conversations").and_then(|x| x.as_i64()),
                    })
                    .collect()
            })
            .unwrap_or_default(),
    }
}

/// Passphrase strength label (reference: >= 12 chars + a non-alphanumeric
/// char -> strong, >= 8 -> ok, else weak).
#[must_use]
pub fn passphrase_strength(pass: &str) -> &'static str {
    let has_symbol = pass.chars().any(|c| !c.is_ascii_alphanumeric());
    if pass.chars().count() >= 12 && has_symbol {
        "strong"
    } else if pass.chars().count() >= 8 {
        "ok"
    } else {
        "weak"
    }
}

/// The Encrypted sync tab — export with passphrase, verify/import with a
/// path or an uploaded bundle, sync ledger (reference
/// `EncryptedSyncSettings`).
#[component]
fn EncryptedSyncSettingsTab() -> impl IntoView {
    let passphrase = create_rw_signal(String::new());
    let confirm = create_rw_signal(String::new());
    let import_path = create_rw_signal(String::new());
    let import_pass = create_rw_signal(String::new());
    let data = create_rw_signal(EncryptedSyncData::default());

    let load: Rc<dyn Fn()> = Rc::new(move || {
        wasm_bindgen_futures::spawn_local(async move {
            if let Ok(v) = crate::api::get_json::<serde_json::Value>("/api/sync/encrypted").await {
                data.set(parse_encrypted_sync(&v));
            }
            // Errors leave prior data rendered; the ledger is informational,
            // not a blocking error state (reference behavior).
        });
    });
    load();

    // Export (reference: POST /api/sync/encrypted/export {passphrase}).
    let export_bundle = {
        let load = Rc::clone(&load);
        move |_| {
            let pass = passphrase.get();
            let load = Rc::clone(&load);
            wasm_bindgen_futures::spawn_local(async move {
                let body = serde_json::json!({ "passphrase": pass });
                match crate::api::post_json::<serde_json::Value>(
                    "/api/sync/encrypted/export",
                    Some(&body),
                )
                .await
                {
                    Ok(r) => {
                        let ok = r.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
                        let message = r
                            .get("message")
                            .and_then(|v| v.as_str())
                            .unwrap_or("Export failed.");
                        if ok {
                            toasts::success(message);
                            load();
                        } else {
                            toasts::error(message);
                        }
                    }
                    Err(e) => toasts::error(e),
                }
            });
        }
    };

    // Verify (dry run) + import — both POST {path, passphrase}. Import
    // success is a WARNING toast (restart required), like the reference.
    let verify_or_import: Rc<dyn Fn(bool)> = Rc::new({
        let load = Rc::clone(&load);
        move |import: bool| {
            let path = import_path.get();
            let pass = import_pass.get();
            let load = Rc::clone(&load);
            wasm_bindgen_futures::spawn_local(async move {
                let body = serde_json::json!({ "path": path, "passphrase": pass });
                let endpoint = if import {
                    "/api/sync/encrypted/import"
                } else {
                    "/api/sync/encrypted/verify"
                };
                match crate::api::post_json::<serde_json::Value>(endpoint, Some(&body)).await {
                    Ok(r) => {
                        let ok = r.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
                        let message = r
                            .get("message")
                            .and_then(|v| v.as_str())
                            .unwrap_or("Request failed.");
                        if import && ok {
                            toasts::warning(message);
                            load();
                        } else if ok {
                            toasts::success(message);
                        } else {
                            toasts::error(message);
                        }
                    }
                    Err(e) => toasts::error(e),
                }
            });
        }
    });
    let do_verify = {
        let r = Rc::clone(&verify_or_import);
        move |_| r(false)
    };
    let do_import = {
        let r = Rc::clone(&verify_or_import);
        move |_| r(true)
    };

    // Upload a .sosync bundle (reference: raw octet-stream POST, then the
    // returned path lands in the import field).
    let on_upload = move |ev: ev::Event| {
        let Some(input) = ev
            .target()
            .and_then(|t| t.dyn_into::<web_sys::HtmlInputElement>().ok())
        else {
            return;
        };
        let Some(file) = input.files().and_then(|f| f.get(0)) else {
            return;
        };
        let load = Rc::clone(&load);
        wasm_bindgen_futures::spawn_local(async move {
            match upload_bundle(&file).await {
                Ok(r) => {
                    let ok = r.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
                    let message = r
                        .get("message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Upload failed.");
                    if ok {
                        toasts::success(message);
                        if let Some(p) = r.get("path").and_then(|v| v.as_str()) {
                            import_path.set(p.to_string());
                        }
                        load();
                    } else {
                        toasts::error(message);
                    }
                }
                Err(e) => toasts::error(e),
            }
        });
    };

    view! {
        <div class="spp-grid-2">
            <section class="spp-card spp-settings__card">
                <h3 class="spp-settings__card-title">"Export an encrypted bundle"</h3>
                <p class="spp-settings__hint">
                    "A .sosync file is your whole support database (customers, conversations, AI analysis, segments, campaigns) encrypted with AES-256-GCM. Attachments are not bundled - they re-download from Help Scout automatically on the other device. No relay server exists by design: move the file yourself (cloud drive, USB, company share). Only the passphrase holder can open it."
                </p>
                <div class="spp-settings__field">
                    <label class="spp-settings__label" for="enc-pass">"Passphrase (min 8 chars)"</label>
                    <input
                        id="enc-pass"
                        class="spp-input"
                        type="password"
                        autocomplete="new-password"
                        prop:value=move || passphrase.get()
                        on:input=move |ev| passphrase.set(event_target_value(&ev))
                    />
                </div>
                <div class="spp-settings__field">
                    <label class="spp-settings__label" for="enc-confirm">"Confirm passphrase"</label>
                    <input
                        id="enc-confirm"
                        class="spp-input"
                        type="password"
                        autocomplete="new-password"
                        prop:value=move || confirm.get()
                        on:input=move |ev| confirm.set(event_target_value(&ev))
                    />
                </div>
                {move || {
                    let pass = passphrase.get();
                    if pass.is_empty() {
                        ().into_view()
                    } else {
                        let strength = passphrase_strength(&pass);
                        let class = match strength {
                            "strong" => "spp-badge spp-badge--ok",
                            "ok" => "spp-badge spp-badge--warn",
                            _ => "spp-badge spp-badge--err",
                        };
                        view! {
                            <span class=class>{format!("{strength} passphrase")}</span>
                        }.into_view()
                    }
                }}
                <div class="spp-mt-4">
                    <button class="spp-button spp-button--primary" type="button" on:click=export_bundle>
                        "Create encrypted bundle"
                    </button>
                </div>
                <p class="spp-settings__hint">
                    {move || {
                        let dir = data.get().bundle_dir;
                        format!(
                            "Bundles land in {} (newest 5 are kept). There is NO passphrase recovery - losing it means the bundle cannot be decrypted by anyone.",
                            if dir.is_empty() { "…".to_string() } else { dir }
                        )
                    }}
                </p>
            </section>
            <section class="spp-card spp-settings__card">
                <h3 class="spp-settings__card-title">"Import on this device"</h3>
                <div class="spp-settings__field">
                    <label class="spp-settings__label" for="enc-upload">"Upload a .sosync bundle"</label>
                    <input
                        id="enc-upload"
                        class="spp-input"
                        type="file"
                        accept=".sosync,application/octet-stream"
                        on:change=on_upload
                    />
                </div>
                <div class="spp-settings__field">
                    <label class="spp-settings__label" for="enc-path">"…or server-side path"</label>
                    <input
                        id="enc-path"
                        class="spp-input spp-mono"
                        type="text"
                        prop:value=move || import_path.get()
                        on:input=move |ev| import_path.set(event_target_value(&ev))
                        placeholder="/path/to/supportos-sync-….sosync"
                    />
                </div>
                <div class="spp-settings__field">
                    <label class="spp-settings__label" for="enc-import-pass">"Passphrase"</label>
                    <input
                        id="enc-import-pass"
                        class="spp-input"
                        type="password"
                        autocomplete="off"
                        prop:value=move || import_pass.get()
                        on:input=move |ev| import_pass.set(event_target_value(&ev))
                    />
                </div>
                <div class="spp-flex">
                    <button class="spp-button" type="button" on:click=do_verify>
                        "Verify first (dry run)"
                    </button>
                    <button class="spp-button spp-button--danger" type="button" on:click=do_import>
                        "Import & replace local data"
                    </button>
                </div>
                <p class="spp-settings__hint">
                    "Import checks integrity + schema compatibility first, writes an automatic safety backup, then swaps the database. Restart SupportOS after importing."
                </p>
                {move || {
                    let d = data.get();
                    if d.log.is_empty() {
                        ().into_view()
                    } else {
                        view! {
                            <div class="spp-mt-4">
                                <strong class="spp-text-sm">"Sync ledger"</strong>
                                {d.log
                                    .into_iter()
                                    .map(|l| {
                                        let filename = l
                                            .file_path
                                            .rsplit('/')
                                            .next()
                                            .unwrap_or(&l.file_path)
                                            .to_string();
                                        let badge = if l.direction == "export" {
                                            "spp-badge spp-badge--ok"
                                        } else {
                                            "spp-badge"
                                        };
                                        let conv = match l.conversations {
                                            Some(n) => n.to_string(),
                                            None => "?".to_string(),
                                        };
                                        view! {
                                            <div class="spp-flex spp-settings__ledger-row">
                                                <span class=badge>{l.direction.clone()}</span>
                                                <span class="spp-text-xs spp-mono">{filename}</span>
                                                <span class="spp-muted spp-text-xs">
                                                    {format!("{} · {} conv", format_mb(l.size_bytes), conv)}
                                                </span>
                                            </div>
                                        }.into_view()
                                    })
                                    .collect::<Vec<_>>()}
                            </div>
                        }.into_view()
                    }
                }}
            </section>
        </div>
    }
}

/// POST the picked file to `/api/sync/encrypted/upload` as an
/// octet-stream (the reference's one raw-body fetch — `fetch()` accepts a
/// Blob/File as the body directly).
async fn upload_bundle(file: &web_sys::File) -> Result<serde_json::Value, String> {
    let window = web_sys::window().ok_or("no window")?;
    let url = crate::api::url_for("/api/sync/encrypted/upload");
    let init = web_sys::RequestInit::new();
    init.set_method("POST");
    let headers = web_sys::Headers::new().map_err(|e| format!("upload: headers failed: {e:?}"))?;
    headers
        .set("Content-Type", "application/octet-stream")
        .map_err(|e| format!("upload: headers failed: {e:?}"))?;
    init.set_headers(&headers.into());
    let body: &wasm_bindgen::JsValue = file.as_ref();
    init.set_body(body);
    let response =
        wasm_bindgen_futures::JsFuture::from(window.fetch_with_str_and_init(&url, &init))
            .await
            .map_err(|e| format!("upload failed: {e:?}"))?;
    let response: web_sys::Response = response
        .dyn_into()
        .map_err(|_| "upload: fetch did not return a Response".to_string())?;
    let text = wasm_bindgen_futures::JsFuture::from(
        response
            .text()
            .map_err(|e| format!("upload: body read failed: {e:?}"))?,
    )
    .await
    .map_err(|e| format!("upload: body read failed: {e:?}"))?
    .as_string()
    .ok_or("upload: body is not UTF-8 text")?;
    serde_json::from_str(&text).map_err(|e| format!("upload: invalid JSON: {e}"))
}

// ---------------------------------------------------------------------------
// Capability matrix
// ---------------------------------------------------------------------------

/// One row of `GET /api/system/capabilities`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CapabilityRow {
    pub resource: String,
    pub operation: String,
    pub endpoint: String,
    pub api_version: String,
    pub read_write: String,
    pub implemented: bool,
    pub tested: bool,
    pub notes: String,
}

/// The capability payload.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CapabilityData {
    pub matrix: Vec<CapabilityRow>,
    pub implemented: i64,
    pub total: i64,
    pub tested: i64,
}

/// Parse the capabilities payload.
#[must_use]
pub fn parse_capabilities(v: &serde_json::Value) -> CapabilityData {
    CapabilityData {
        matrix: v
            .get("matrix")
            .and_then(|x| x.as_array())
            .map(|arr| {
                arr.iter()
                    .map(|c| CapabilityRow {
                        resource: c
                            .get("resource")
                            .and_then(|x| x.as_str())
                            .unwrap_or_default()
                            .to_string(),
                        operation: c
                            .get("operation")
                            .and_then(|x| x.as_str())
                            .unwrap_or_default()
                            .to_string(),
                        endpoint: c
                            .get("endpoint")
                            .and_then(|x| x.as_str())
                            .unwrap_or_default()
                            .to_string(),
                        api_version: c
                            .get("api_version")
                            .and_then(|x| x.as_str())
                            .unwrap_or_default()
                            .to_string(),
                        read_write: c
                            .get("read_write")
                            .and_then(|x| x.as_str())
                            .unwrap_or_default()
                            .to_string(),
                        implemented: c
                            .get("implemented")
                            .and_then(|x| x.as_bool())
                            .unwrap_or(false),
                        tested: c.get("tested").and_then(|x| x.as_bool()).unwrap_or(false),
                        notes: c
                            .get("notes")
                            .and_then(|x| x.as_str())
                            .unwrap_or_default()
                            .to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        implemented: v
            .pointer("/summary/implemented")
            .and_then(|x| x.as_i64())
            .unwrap_or(0),
        total: v
            .pointer("/summary/total")
            .and_then(|x| x.as_i64())
            .unwrap_or(0),
        tested: v
            .pointer("/summary/tested")
            .and_then(|x| x.as_i64())
            .unwrap_or(0),
    }
}

/// The Capability matrix tab — read-only table (reference capability tab).
#[component]
fn CapabilitySettingsTab() -> impl IntoView {
    let data = create_rw_signal(CapabilityData::default());
    let loading = create_rw_signal(true);
    let error_msg = create_rw_signal(None::<String>);

    wasm_bindgen_futures::spawn_local(async move {
        match crate::api::get_json::<serde_json::Value>("/api/system/capabilities").await {
            Ok(v) => {
                data.set(parse_capabilities(&v));
                loading.set(false);
            }
            Err(e) => {
                error_msg.set(Some(e));
                loading.set(false);
            }
        }
    });

    view! {
        <section class="spp-card spp-settings__matrix">
            <div class="spp-settings__matrix-head">
                <strong>"API capability matrix"</strong>
                <p class="spp-muted spp-text-xs">
                    {move || {
                        let d = data.get();
                        format!(
                            "{}/{} operations implemented ({} covered by automated tests). Verified against current official docs; limitations noted per row. Unsupported features are never faked.",
                            d.implemented, d.total, d.tested
                        )
                    }}
                </p>
            </div>
            <Show when=move || loading.get() fallback=|| ().into_view()>
                <LoadingState />
            </Show>
            <Show when=move || error_msg.get().is_some() fallback=|| ().into_view()>
                <ErrorState message="Could not load the capability matrix." retry=None />
            </Show>
            <Show
                when=move || !loading.get() && error_msg.get().is_none()
                fallback=|| ().into_view()
            >
                <Show
                    when=move || !data.get().matrix.is_empty()
                    fallback=|| {
                        view! {
                            <EmptyState message="The capability matrix is empty on this build." />
                        }
                        .into_view()
                    }
                >
                    <div class="spp-settings__matrix-scroll">
                        <table class="spp-table">
                            <thead>
                                <tr>
                                    <th>"Resource"</th>
                                    <th>"Operation"</th>
                                    <th>"Endpoint"</th>
                                    <th>"v"</th>
                                    <th>"R/W"</th>
                                    <th>"Implemented"</th>
                                    <th>"Notes"</th>
                                </tr>
                            </thead>
                            <tbody>
                                {move || {
                                    data.get().matrix
                                        .into_iter()
                                        .map(|c| {
                                            let implemented = c.implemented;
                                            let tested = c.tested;
                                            let resource = c.resource.clone();
                                            let operation = c.operation.clone();
                                            let endpoint = c.endpoint.clone();
                                            let api_version = c.api_version.clone();
                                            let read_write = c.read_write.clone();
                                            let notes = c.notes.clone();
                                            view! {
                                                <tr>
                                                    <td>{resource}</td>
                                                    <td class="spp-text-sm">{operation}</td>
                                                    <td class="spp-mono spp-text-xs">{endpoint}</td>
                                                    <td><span class="spp-badge">{api_version}</span></td>
                                                    <td class="spp-text-xs">{read_write}</td>
                                                    <td>
                                                        {if implemented {
                                                            let label = if tested {
                                                                "yes · tested".to_string()
                                                            } else {
                                                                "yes".to_string()
                                                            };
                                                            view! {
                                                                <span class="spp-badge spp-badge--ok">{label}</span>
                                                            }.into_view()
                                                        } else {
                                                            view! {
                                                                <span class="spp-badge spp-badge--err">"no"</span>
                                                            }.into_view()
                                                        }}
                                                    </td>
                                                    <td class="spp-muted spp-text-xs">{notes}</td>
                                                </tr>
                                            }.into_view()
                                        })
                                        .collect::<Vec<_>>()
                                }}
                            </tbody>
                        </table>
                    </div>
                </Show>
            </Show>
        </section>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ---- SettingsTab --------------------------------------------------

    #[test]
    fn tab_labels_match_reference_names() {
        assert_eq!(SettingsTab::General.label(), "Synchronization & AI");
        assert_eq!(SettingsTab::HelpScout.label(), "Help Scout");
        assert_eq!(SettingsTab::LmStudio.label(), "LM Studio");
        assert_eq!(SettingsTab::Qdrant.label(), "Qdrant");
        assert_eq!(SettingsTab::Hours.label(), "Business hours");
        assert_eq!(SettingsTab::Backups.label(), "Backups & export");
        assert_eq!(SettingsTab::EncSync.label(), "Encrypted sync");
        assert_eq!(SettingsTab::Capability.label(), "Capability matrix");
    }

    #[test]
    fn tabs_keep_the_reference_order() {
        let labels: Vec<&str> = SettingsTab::ALL.iter().map(|t| t.label()).collect();
        assert_eq!(
            labels,
            vec![
                "Synchronization & AI",
                "Help Scout",
                "LM Studio",
                "Qdrant",
                "Business hours",
                "Backups & export",
                "Encrypted sync",
                "Capability matrix",
            ]
        );
    }

    // ---- parse_general_settings ---------------------------------------

    #[test]
    fn general_settings_parse_the_flat_object() {
        let v = json!({
            "sync_interval_minutes": 10,
            "api_concurrency": 4,
            "attachment_auto_download": false,
            "retention_days": 90,
            "backup_interval_hours": null,
            "ai_enabled": true,
            "automatic_analysis_enabled": false,
            "automatic_note_enabled": true,
            "automatic_draft_enabled": false,
            "automation_enabled": true,
            "automation_write_actions_enabled": false,
            "redaction_enabled": false,
            "agent_language": "de",
        });
        let s = parse_general_settings(&v);
        assert_eq!(s.sync_interval_minutes, 10);
        assert_eq!(s.api_concurrency, 4);
        assert!(!s.attachment_auto_download);
        assert_eq!(s.retention_days, Some(90));
        assert_eq!(s.backup_interval_hours, None);
        assert!(s.ai_enabled);
        assert!(!s.automatic_analysis_enabled);
        assert!(s.automatic_note_enabled);
        assert!(!s.automatic_draft_enabled);
        assert!(s.automation_enabled);
        assert!(!s.automation_write_actions_enabled);
        assert!(!s.redaction_enabled);
        assert_eq!(s.agent_language, "de");
    }

    #[test]
    fn general_settings_defaults_match_the_reference() {
        // Missing keys fall back to the reference defaults (5/2/true/…/en).
        let s = parse_general_settings(&json!({}));
        assert_eq!(s.sync_interval_minutes, 5);
        assert_eq!(s.api_concurrency, 2);
        assert!(s.attachment_auto_download);
        assert_eq!(s.retention_days, None);
        assert!(s.ai_enabled);
        assert!(s.automatic_analysis_enabled);
        assert!(!s.automatic_note_enabled);
        assert!(!s.automatic_draft_enabled);
        assert!(!s.automation_enabled);
        assert!(!s.automation_write_actions_enabled);
        assert!(s.redaction_enabled);
        assert_eq!(s.agent_language, "en");
    }

    // ---- number_patch (blur semantics) ---------------------------------

    #[test]
    fn number_patch_saves_changed_integers() {
        assert_eq!(
            number_patch("10", false, Some(5)),
            Some(serde_json::json!(10))
        );
        assert_eq!(
            number_patch(" 10 ", false, Some(5)),
            Some(serde_json::json!(10)) // trims, like Number()
        );
    }

    #[test]
    fn number_patch_skips_unchanged_values() {
        assert_eq!(number_patch("5", false, Some(5)), None);
        assert_eq!(number_patch("", false, None), None);
    }

    #[test]
    fn number_patch_blank_nullable_clears_to_null() {
        // blank + nullable + currently set -> null (keep-forever / off).
        assert_eq!(
            number_patch("", true, Some(90)),
            Some(serde_json::Value::Null)
        );
        // blank + nullable + already null -> no write.
        assert_eq!(number_patch("", true, None), None);
        // blank + NOT nullable -> no write (sync interval keeps its value).
        assert_eq!(number_patch("", false, Some(5)), None);
    }

    #[test]
    fn number_patch_ignores_invalid_input() {
        // The reference requires Number.isFinite — garbage never writes.
        assert_eq!(number_patch("abc", false, Some(5)), None);
        assert_eq!(number_patch("12.5", false, Some(5)), None);
    }

    // ---- SUPPORTED_LANGUAGES ------------------------------------------

    #[test]
    fn language_list_matches_the_reference_eighteen() {
        assert_eq!(SUPPORTED_LANGUAGES.len(), 18);
        assert_eq!(SUPPORTED_LANGUAGES[0], ("en", "English"));
        assert_eq!(SUPPORTED_LANGUAGES[8], ("zh", "Chinese"));
        assert_eq!(SUPPORTED_LANGUAGES[17], ("sv", "Swedish"));
    }

    // ---- oauth ---------------------------------------------------------

    #[test]
    fn oauth_status_parses_me_and_null_me() {
        let v = json!({
            "configured": true,
            "authenticated": true,
            "demo_mode": false,
            "expires_at": "2026-11-01T00:00:00.000Z",
            "me": { "name": "Kim Pearce", "email": "kim@example.com" },
        });
        let o = parse_oauth_status(&v);
        assert!(o.configured && o.authenticated && !o.demo_mode);
        assert_eq!(o.me_name.as_deref(), Some("Kim Pearce"));
        assert_eq!(o.me_email.as_deref(), Some("kim@example.com"));

        let demo = json!({
            "configured": false,
            "authenticated": true,
            "demo_mode": true,
            "expires_at": null,
            "me": null,
        });
        let o = parse_oauth_status(&demo);
        assert!(o.demo_mode);
        assert_eq!(o.expires_at, None);
        assert_eq!(format_oauth_me(&o), "—");
    }

    #[test]
    fn oauth_me_formats_name_email_pair() {
        let o = OauthStatus {
            me_name: Some("Ada".into()),
            me_email: None,
            ..OauthStatus::default()
        };
        assert_eq!(format_oauth_me(&o), "Ada (—)");
    }

    // ---- lmstudio / qdrant forms ---------------------------------------

    #[test]
    fn lmstudio_form_parses_and_defaults() {
        let f = parse_lmstudio(&json!({
            "base_url": "http://localhost:9999",
            "chat_model": "qwen2.5-7b-instruct",
            "embedding_model": null,
            "timeout_ms": 30000,
            "concurrency": 4,
        }));
        assert_eq!(f.base_url, "http://localhost:9999");
        assert_eq!(f.chat_model, "qwen2.5-7b-instruct");
        assert_eq!(f.embedding_model, "");
        assert_eq!(f.timeout_ms, 30_000);
        assert_eq!(f.concurrency, 4);

        let d = parse_lmstudio(&json!({}));
        assert_eq!(d.base_url, "http://127.0.0.1:1234");
        assert_eq!(d.timeout_ms, 120_000);
        assert_eq!(d.concurrency, 2);
    }

    #[test]
    fn lmstudio_patch_body_nulls_blank_models() {
        let body = lmstudio_patch_body(&LmStudioForm {
            base_url: "http://127.0.0.1:1234".into(),
            chat_model: "m1".into(),
            embedding_model: String::new(),
            timeout_ms: 60_000,
            concurrency: 1,
        });
        assert_eq!(body["chat_model"], json!("m1"));
        assert_eq!(body["embedding_model"], serde_json::Value::Null);
        assert_eq!(body["base_url"], json!("http://127.0.0.1:1234"));
        assert_eq!(body["timeout_ms"], json!(60_000));
    }

    #[test]
    fn qdrant_form_parses_and_defaults() {
        let f = parse_qdrant(&json!({ "url": "http://qdrant.local:6333", "enabled": false }));
        assert_eq!(f.url, "http://qdrant.local:6333");
        assert!(!f.enabled);

        let d = parse_qdrant(&json!({}));
        assert_eq!(d.url, "http://127.0.0.1:6333");
        assert!(d.enabled);
    }

    // ---- business hours helpers ----------------------------------------

    #[test]
    fn minutes_hhmm_round_trips() {
        assert_eq!(minutes_to_hhmm(Some(540)), "09:00");
        assert_eq!(minutes_to_hhmm(Some(1020)), "17:00");
        assert_eq!(minutes_to_hhmm(Some(0)), "00:00");
        assert_eq!(minutes_to_hhmm(None), "");
        for m in [0, 1, 599, 600, 1439] {
            assert_eq!(hhmm_to_minutes(&minutes_to_hhmm(Some(m))), Some(m));
        }
    }

    #[test]
    fn hhmm_rejects_invalid_times() {
        assert_eq!(hhmm_to_minutes(""), None);
        assert_eq!(hhmm_to_minutes("9"), None);
        assert_eq!(hhmm_to_minutes("0900"), None);
        assert_eq!(hhmm_to_minutes("24:00"), Some(1440)); // reference allows h == 24
        assert_eq!(hhmm_to_minutes("25:00"), None); // h > 24
        assert_eq!(hhmm_to_minutes("09:60"), None); // min > 59
        assert_eq!(hhmm_to_minutes("-1:00"), None);
        assert_eq!(hhmm_to_minutes("09:30"), Some(570));
        assert_eq!(hhmm_to_minutes("9:05"), Some(545));
    }

    #[test]
    fn bh_schedule_label_formats_configured_rows() {
        let row = BusinessHoursRow {
            mailbox_id: 1,
            name: "Support".into(),
            configured: true,
            timezone: Some("Europe/Berlin".into()),
            days: Some(vec![1, 2, 3, 4, 5]),
            start_minute: Some(540),
            end_minute: Some(1020),
            ..BusinessHoursRow::default()
        };
        assert_eq!(
            bh_schedule_label(&row).as_deref(),
            Some("Mon Tue Wed Thu Fri · 09:00–17:00 · Europe/Berlin")
        );
        // Unconfigured rows have no label (wall-clock badge instead).
        assert_eq!(bh_schedule_label(&BusinessHoursRow::default()), None);
    }

    #[test]
    fn bh_rows_parse_the_mailboxes_payload() {
        let v = json!({ "mailboxes": [
            {
                "mailbox_id": 2,
                "name": "Billing",
                "configured": true,
                "timezone": "Asia/Kolkata",
                "days": [1, 3, 5],
                "start_minute": 600,
                "end_minute": 1080,
                "first_response_target_min": 240,
                "resolution_target_min": null
            },
            {
                "mailbox_id": 3,
                "name": "Sales",
                "configured": false,
                "timezone": null,
                "days": null,
                "start_minute": null,
                "end_minute": null,
                "first_response_target_min": null,
                "resolution_target_min": null
            }
        ]});
        let rows = parse_business_hours(&v);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "Billing");
        assert_eq!(rows[0].days, Some(vec![1, 3, 5]));
        assert_eq!(rows[0].first_response_target_min, Some(240));
        assert_eq!(rows[0].resolution_target_min, None);
        assert!(!rows[1].configured);
        assert!(parse_business_hours(&json!({})).is_empty());
    }

    #[test]
    fn optional_positive_targets_parse_blank_as_none() {
        assert_eq!(parse_optional_positive(""), None);
        assert_eq!(parse_optional_positive("  "), None);
        assert_eq!(parse_optional_positive("240"), Some(240));
        assert_eq!(parse_optional_positive("0"), None); // must be > 0
        assert_eq!(parse_optional_positive("-5"), None);
        assert_eq!(parse_optional_positive("abc"), None);
    }

    // ---- backups --------------------------------------------------------

    #[test]
    fn backups_parse_and_format_sizes() {
        let v = json!({ "backups": [
            { "file": "backup-2026-10-04.db", "size_bytes": 2_621_440, "created_at": "2026-10-04T02:00:00Z", "verified": true },
            { "file": "backup-2026-10-05.db", "size_bytes": 0, "created_at": "2026-10-05T02:00:00Z", "verified": false }
        ]});
        let b = parse_backups(&v);
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].file, "backup-2026-10-04.db");
        assert!(b[0].verified);
        assert!(!b[1].verified);
        assert_eq!(format_mb(2_621_440), "2.5 MB");
        assert_eq!(format_mb(0), "0.0 MB");
        assert!(parse_backups(&json!({})).is_empty());
    }

    // ---- encrypted sync --------------------------------------------------

    #[test]
    fn encrypted_sync_parses_ledger_and_bundle_dir() {
        let v = json!({
            "bundles": [],
            "log": [
                { "id": 1, "direction": "export", "file_path": "/x/supportos-sync-a.sosync", "size_bytes": 1_048_576, "conversations": 42 },
                { "id": 2, "direction": "import", "file_path": "/x/supportos-sync-b.sosync", "size_bytes": 2, "conversations": null }
            ],
            "bundle_dir": "/home/user/backups",
            "design": "…"
        });
        let d = parse_encrypted_sync(&v);
        assert_eq!(d.bundle_dir, "/home/user/backups");
        assert_eq!(d.log.len(), 2);
        assert_eq!(d.log[0].direction, "export");
        assert_eq!(d.log[0].conversations, Some(42));
        assert_eq!(d.log[1].conversations, None);
    }

    #[test]
    fn passphrase_strength_matches_reference_thresholds() {
        assert_eq!(passphrase_strength(""), "weak");
        assert_eq!(passphrase_strength("short"), "weak");
        assert_eq!(passphrase_strength("7chars!"), "weak"); // 7 chars
        assert_eq!(passphrase_strength("abcd1234"), "ok"); // 8 chars, no symbol
        assert_eq!(passphrase_strength("12chars-with!"), "strong"); // >= 12 + symbol
        assert_eq!(passphrase_strength("12charsnosymbol"), "ok"); // >= 12, no symbol
    }

    // ---- capability matrix ----------------------------------------------

    #[test]
    fn capabilities_parse_matrix_and_summary() {
        let v = json!({
            "matrix": [
                { "resource": "conversations", "operation": "list", "endpoint": "/v2/conversations", "api_version": "v2", "read_write": "read", "implemented": true, "tested": true, "notes": "" },
                { "resource": "customers", "operation": "create", "endpoint": "/v1/customers", "api_version": "v1", "read_write": "write", "implemented": false, "tested": false, "notes": "not available" }
            ],
            "summary": { "implemented": 1, "total": 2, "tested": 1 }
        });
        let d = parse_capabilities(&v);
        assert_eq!(d.matrix.len(), 2);
        assert!(d.matrix[0].implemented && d.matrix[0].tested);
        assert!(!d.matrix[1].implemented);
        assert_eq!(d.implemented, 1);
        assert_eq!(d.total, 2);
        assert_eq!(d.tested, 1);
    }
}
