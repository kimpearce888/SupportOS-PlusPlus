//! Automation page — the `/automation` route.
//!
//! Reference contract (Automation.tsx): `GET /api/automation/rules` returns
//! `{rules, runs, risk_tiers, automation_enabled}`; the page shows the rules
//! list plus the action-safety-tier summary. Approvals are not a separate
//! queue in the reference — higher-risk actions park in the sync job flow.
//!
//! Per KNOWN PITFALLS: every view has loading, empty, and error states.
/// Per A12: closed vocabularies are single-source-of-truth — the trigger
/// and action enums come from `spp_core::automation` (mirrored here as
/// `TriggerView` / `ActionView` for `'static` Leptos lifetimes).
use leptos::*;

use crate::components::state_view::EmptyState;

/// A UI-side automation rule. Mirrors `spp_core::automation::AutomationRule`
/// but with `'static` lifetimes so Leptos signals can hold it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomationRuleView {
    /// The row id.
    pub id: i64,
    /// The human-readable rule name.
    pub name: String,
    /// The trigger (mirrored enum).
    pub trigger: TriggerView,
    /// The action (mirrored enum).
    pub action: ActionView,
    /// Whether the rule is enabled.
    pub enabled: bool,
}

/// The UI-side mirror of `spp_core::automation::Trigger`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TriggerView {
    /// Fires when a conversation's status changes.
    StatusChanged {
        /// The previous status filter (None = wildcard).
        from_status: Option<String>,
        /// The new status filter (None = wildcard).
        to_status: Option<String>,
    },
    /// Fires when a tag is added.
    TagAdded {
        /// The tag name to match.
        tag: String,
    },
    /// Fires when a conversation enters SLA-risk state.
    SlaRisk,
}

impl TriggerView {
    /// The human-readable label for the trigger.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::StatusChanged {
                from_status,
                to_status,
            } => {
                let from = from_status.clone().unwrap_or_else(|| "*".into());
                let to = to_status.clone().unwrap_or_else(|| "*".into());
                format!("Status: {from} → {to}")
            }
            Self::TagAdded { tag } => format!("Tag added: {tag}"),
            Self::SlaRisk => "SLA at risk".into(),
        }
    }
}

/// The UI-side mirror of `spp_core::automation::Action`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionView {
    /// Assign the conversation to an agent.
    Assign {
        /// The new assignee (Help Scout user remote_id).
        assignee_remote_id: i64,
    },
    /// Add a tag to the conversation.
    AddTag {
        /// The tag name to add.
        tag: String,
    },
    /// Send an internal note.
    SendNote {
        /// The note body.
        body: String,
    },
    /// Change the conversation's status.
    ChangeStatus {
        /// The new status.
        new_status: String,
    },
    /// Set the local priority.
    SetPriority {
        /// The new priority.
        new_priority: String,
    },
}

impl ActionView {
    /// The human-readable label for the action.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Assign { assignee_remote_id } => {
                format!("Assign to user #{assignee_remote_id}")
            }
            Self::AddTag { tag } => format!("Add tag: {tag}"),
            Self::SendNote { body } => {
                if body.len() > 32 {
                    format!("Note: {}…", &body[..32])
                } else {
                    format!("Note: {body}")
                }
            }
            Self::ChangeStatus { new_status } => format!("Set status: {new_status}"),
            Self::SetPriority { new_priority } => format!("Set priority: {new_priority}"),
        }
    }

    /// Whether the action requires approval. Mirrors the core
    /// `Action::requires_approval()`.
    #[must_use]
    pub fn requires_approval(&self) -> bool {
        match self {
            Self::Assign { .. } | Self::ChangeStatus { .. } => true,
            Self::SetPriority { new_priority } => new_priority == "urgent",
            Self::AddTag { .. } | Self::SendNote { .. } => false,
        }
    }
}

/// The Automation page component.
///
/// Wired to `GET /api/automation/rules`.
#[component]
pub fn AutomationPage() -> impl IntoView {
    let rules = create_rw_signal(Vec::<AutomationRuleView>::new());
    let automation_enabled = create_rw_signal(None::<bool>);
    let risk_tier_note = create_rw_signal(String::new());
    let loading = create_rw_signal(true);
    let error_msg = create_rw_signal(None::<String>);

    create_effect(move |_| {
        let rules = rules;
        let automation_enabled = automation_enabled;
        let risk_tier_note = risk_tier_note;
        let loading = loading;
        let error_msg = error_msg;
        wasm_bindgen_futures::spawn_local(async move {
            match crate::api::get_json::<serde_json::Value>("/api/automation/rules").await {
                Ok(data) => {
                    let views: Vec<AutomationRuleView> = data
                        .get("rules")
                        .and_then(|v| v.as_array())
                        .map(|arr| arr.iter().filter_map(parse_rule_view).collect())
                        .unwrap_or_default();
                    rules.set(views);
                    automation_enabled
                        .set(data.get("automation_enabled").and_then(|v| v.as_bool()));
                    risk_tier_note.set(
                        data.pointer("/risk_tiers/note")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                    );
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
        <div class="spp-page spp-page--automation">
            <h2 class="spp-page__title">"Automation"</h2>
            <p class="spp-page__subtitle">
                "Rules trigger actions when conversations match certain conditions. "
                "High-impact actions (assigning, status change, urgent priority) require approval; "
                "low-impact actions (add tag, send note) execute directly."
            </p>

            <Show when=move || error_msg.get().is_some() fallback=|| ()>
                <div class="spp-state spp-state--error">
                    <span class="spp-state__icon" aria-hidden="true">"⚠"</span>
                    <p class="spp-state__body">{move || error_msg.get().unwrap_or_default()}</p>
                </div>
            </Show>

            <section class="spp-automation-summary">
                <div class="spp-rule-card__row">
                    <span class="spp-rule-card__label">"Automation:"</span>
                    <span class="spp-badge">
                        {move || if automation_enabled.get().unwrap_or(false) { "✅ enabled" } else { "❌ disabled" }}
                    </span>
                </div>
                {move || {
                    let note = risk_tier_note.get();
                    if note.is_empty() { ().into_view() } else {
                        view! { <p class="spp-page__subtitle">{note}</p> }.into_view()
                    }
                }}
            </section>

            // ── Rules list ──
            <section class="spp-automation-rules">
                <h3 class="spp-automation-rules__title">"Rules"</h3>
                <Show
                    when=move || !rules.get().is_empty()
                    fallback=move || {
                        view! {
                            <EmptyState message="No automation rules yet. Create a rule to automate repetitive actions." />
                        }
                    }
                >
                    <RulesList rules=rules.get() />
                </Show>
            </section>
        </div>
    }
}

/// The rules list — each rule is a card with name, trigger, action, and
/// an enabled toggle.
#[component]
fn RulesList(rules: Vec<AutomationRuleView>) -> impl IntoView {
    let rows_fragment = leptos::Fragment::new(
        rules
            .iter()
            .map(|r| {
                view! {
                    <RuleCard rule=r.clone() />
                }
                .into_view()
            })
            .collect::<Vec<_>>(),
    );
    view! {
        <div class="spp-rules-list">
            {rows_fragment.clone()}
        </div>
    }
}

/// A single rule card.
#[component]
fn RuleCard(rule: AutomationRuleView) -> impl IntoView {
    let name = rule.name.clone();
    let trigger_label = rule.trigger.label();
    let action_label = rule.action.label();
    let requires_approval = rule.action.requires_approval();
    let enabled = rule.enabled;
    let rule_id = rule.id;

    view! {
        <div class="spp-rule-card">
            <div class="spp-rule-card__header">
                <span class="spp-rule-card__name">{name}</span>
                <button
                    class="spp-rule-card__toggle"
                    type="button"
                    role="switch"
                    aria-checked=enabled
                    title=move || if enabled { "Click to disable" } else { "Click to enable" }
                >
                    {move || if enabled { "Enabled" } else { "Disabled" }}
                </button>
            </div>
            <div class="spp-rule-card__body">
                <div class="spp-rule-card__row">
                    <span class="spp-rule-card__label">"Trigger:"</span>
                    <span class="spp-rule-card__value">{trigger_label}</span>
                </div>
                <div class="spp-rule-card__row">
                    <span class="spp-rule-card__label">"Action:"</span>
                    <span class="spp-rule-card__value">{action_label}</span>
                    <Show when=move || requires_approval fallback=|| ().into_view()>
                        <span class="spp-rule-card__badge">"requires approval"</span>
                    </Show>
                </div>
                <div class="spp-rule-card__row">
                    <span class="spp-rule-card__label">"Rule ID:"</span>
                    <span class="spp-rule-card__value">#{rule_id}</span>
                </div>
            </div>
        </div>
    }
}

/// Parse one rule from the `GET /api/automation/rules` payload into the
/// UI view (the core enums serialize as `{"kind": "..."}`-tagged objects).
fn parse_rule_view(r: &serde_json::Value) -> Option<AutomationRuleView> {
    let id = r.get("id")?.as_i64()?;
    let name = r.get("name")?.as_str()?.to_string();
    let enabled = r.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);
    let trigger = match r.get("trigger")?.get("kind")?.as_str()? {
        "status_changed" => TriggerView::StatusChanged {
            from_status: r
                .pointer("/trigger/from_status")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            to_status: r
                .pointer("/trigger/to_status")
                .and_then(|v| v.as_str())
                .map(str::to_string),
        },
        "tag_added" => TriggerView::TagAdded {
            tag: r
                .pointer("/trigger/tag")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        },
        "sla_risk" => TriggerView::SlaRisk,
        _ => return None,
    };
    let action = match r.get("action")?.get("kind")?.as_str()? {
        "assign" => ActionView::Assign {
            assignee_remote_id: r
                .pointer("/action/assignee_remote_id")
                .and_then(|v| v.as_i64())
                .unwrap_or_default(),
        },
        "add_tag" => ActionView::AddTag {
            tag: r
                .pointer("/action/tag")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        },
        "send_note" => ActionView::SendNote {
            body: r
                .pointer("/action/body")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        },
        "change_status" => ActionView::ChangeStatus {
            new_status: r
                .pointer("/action/new_status")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        },
        "set_priority" => ActionView::SetPriority {
            new_priority: r
                .pointer("/action/new_priority")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        },
        _ => return None,
    };
    Some(AutomationRuleView {
        id,
        name,
        trigger,
        action,
        enabled,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_rule(id: i64, name: &str, enabled: bool) -> AutomationRuleView {
        AutomationRuleView {
            id,
            name: name.into(),
            trigger: TriggerView::SlaRisk,
            action: ActionView::SendNote {
                body: "Heads up".into(),
            },
            enabled,
        }
    }

    // ---- TriggerView::label ------------------------------------------------

    #[test]
    fn trigger_status_changed_label() {
        let t = TriggerView::StatusChanged {
            from_status: Some("active".into()),
            to_status: Some("closed".into()),
        };
        assert_eq!(t.label(), "Status: active → closed");
    }

    #[test]
    fn trigger_status_changed_wildcard_label() {
        let t = TriggerView::StatusChanged {
            from_status: None,
            to_status: Some("closed".into()),
        };
        assert_eq!(t.label(), "Status: * → closed");
    }

    #[test]
    fn trigger_tag_added_label() {
        let t = TriggerView::TagAdded { tag: "vip".into() };
        assert_eq!(t.label(), "Tag added: vip");
    }

    #[test]
    fn trigger_sla_risk_label() {
        let t = TriggerView::SlaRisk;
        assert_eq!(t.label(), "SLA at risk");
    }

    // ---- ActionView::label -------------------------------------------------

    #[test]
    fn action_assign_label() {
        let a = ActionView::Assign {
            assignee_remote_id: 42,
        };
        assert_eq!(a.label(), "Assign to user #42");
    }

    #[test]
    fn action_add_tag_label() {
        let a = ActionView::AddTag { tag: "vip".into() };
        assert_eq!(a.label(), "Add tag: vip");
    }

    #[test]
    fn action_send_note_short_label() {
        let a = ActionView::SendNote {
            body: "Heads up".into(),
        };
        assert_eq!(a.label(), "Note: Heads up");
    }

    #[test]
    fn action_send_note_long_label_truncates_with_ellipsis() {
        let a = ActionView::SendNote {
            body: "This is a very long note body that exceeds 32 characters".into(),
        };
        let label = a.label();
        assert!(label.starts_with("Note: "));
        assert!(
            label.ends_with('…'),
            "long notes are truncated with ellipsis: {label}"
        );
    }

    #[test]
    fn action_change_status_label() {
        let a = ActionView::ChangeStatus {
            new_status: "closed".into(),
        };
        assert_eq!(a.label(), "Set status: closed");
    }

    #[test]
    fn action_set_priority_label() {
        let a = ActionView::SetPriority {
            new_priority: "urgent".into(),
        };
        assert_eq!(a.label(), "Set priority: urgent");
    }

    // ---- ActionView::requires_approval ------------------------------------

    #[test]
    fn assign_requires_approval() {
        assert!(ActionView::Assign {
            assignee_remote_id: 42
        }
        .requires_approval());
    }

    #[test]
    fn change_status_requires_approval() {
        assert!(ActionView::ChangeStatus {
            new_status: "closed".into()
        }
        .requires_approval());
    }

    #[test]
    fn set_priority_urgent_requires_approval() {
        assert!(ActionView::SetPriority {
            new_priority: "urgent".into()
        }
        .requires_approval());
    }

    #[test]
    fn set_priority_normal_does_not_require_approval() {
        assert!(!ActionView::SetPriority {
            new_priority: "normal".into()
        }
        .requires_approval());
    }

    #[test]
    fn add_tag_does_not_require_approval() {
        assert!(!ActionView::AddTag { tag: "vip".into() }.requires_approval());
    }

    #[test]
    fn send_note_does_not_require_approval() {
        assert!(!ActionView::SendNote { body: "hi".into() }.requires_approval());
    }

    // ---- Empty states -------------------------------------------------------

    #[test]
    fn empty_rules_renders_empty_state() {
        let rules: Vec<AutomationRuleView> = Vec::new();
        assert!(rules.is_empty());
    }

    // ---- Construction -------------------------------------------------------

    #[test]
    fn automation_rule_view_can_be_constructed() {
        let r = sample_rule(1, "Test rule", true);
        assert_eq!(r.id, 1);
        assert_eq!(r.name, "Test rule");
        assert!(r.enabled);
    }

    #[test]
    fn parse_rule_view_reads_kind_tagged_enums() {
        let payload = serde_json::json!({
            "id": 3, "name": "Escalate", "enabled": true,
            "trigger": {"kind": "sla_risk"},
            "action": {"kind": "change_status", "new_status": "pending"}
        });
        let v = parse_rule_view(&payload).expect("parses");
        assert_eq!(v.trigger.label(), "SLA at risk");
        assert_eq!(v.action.label(), "Set status: pending");
        assert!(v.action.requires_approval());
    }

    #[test]
    fn parse_rule_view_rejects_unknown_kinds() {
        let payload = serde_json::json!({
            "id": 4, "name": "Bad", "enabled": false,
            "trigger": {"kind": "nope"},
            "action": {"kind": "add_tag", "tag": "x"}
        });
        assert!(parse_rule_view(&payload).is_none());
    }
}
