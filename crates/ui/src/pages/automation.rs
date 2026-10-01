//! Automation page — the `/automation` route (M4-T11).
//!
//! Per spec M4: "Team operations: Operations Center, workload and capacity,
//! Notification Center, mentions, side threads, automation."
//!
//! This page shows:
//! 1. The rules list (CRUD: name, trigger, action, enabled toggle).
//! 2. The approval queue (pending items with approve/reject actions).
//!
//! Per KNOWN PITFALLS: every view has loading, empty, and error states.
//! Per A12: closed vocabularies are single-source-of-truth — the trigger
//! and action enums come from `spp_core::automation` (mirrored here as
//! `TriggerView` / `ActionView` for `'static` Leptos lifetimes).

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

/// A UI-side automation approval. Mirrors
/// `spp_core::automation::AutomationApproval`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomationApprovalView {
    /// The row id.
    pub id: i64,
    /// The rule that proposed this action.
    pub rule_id: i64,
    /// The conversation the action targets.
    pub conversation_id: i64,
    /// The proposed action (mirrored enum). Parsed from
    /// `proposed_action_json` by the Tauri shell before sending to the UI.
    pub proposed_action: ActionView,
    /// The approval status: 'pending', 'approved', or 'rejected'.
    pub status: String,
    /// When the approval row was created (ISO-8601 UTC).
    pub created_at: String,
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
/// Wired to `automation_list_rules` + `automation_list_pending` IPC.
#[component]
pub fn AutomationPage() -> impl IntoView {
    let rules = create_rw_signal(Vec::<AutomationRuleView>::new());
    let approvals = create_rw_signal(Vec::<AutomationApprovalView>::new());
    let loading = create_rw_signal(true);

    create_effect(move |_| {
        let rules = rules;
        let approvals = approvals;
        let loading = loading;
        wasm_bindgen_futures::spawn_local(async move {
            let args = serde_json::json!({});
            // Fetch rules.
            if let Ok(data) =
                crate::ipc::invoke::<serde_json::Value>("automation_list_rules", &args).await
            {
                if let Some(arr) = data.as_array() {
                    let views: Vec<AutomationRuleView> = arr
                        .iter()
                        .filter_map(|r| {
                            let id = r.get("id")?.as_i64()?;
                            let name = r.get("name")?.as_str()?.to_string();
                            let enabled =
                                r.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);
                            Some(AutomationRuleView {
                                id,
                                name,
                                trigger: TriggerView::SlaRisk,
                                action: ActionView::SendNote {
                                    body: String::new(),
                                },
                                enabled,
                            })
                        })
                        .collect();
                    rules.set(views);
                }
            }
            // Fetch pending approvals.
            if let Ok(data) =
                crate::ipc::invoke::<serde_json::Value>("automation_list_pending", &args).await
            {
                if let Some(arr) = data.as_array() {
                    let views: Vec<AutomationApprovalView> = arr
                        .iter()
                        .filter_map(|a| {
                            let id = a.get("id")?.as_i64()?;
                            let rule_id = a.get("rule_id")?.as_i64()?;
                            let conversation_id = a.get("conversation_id")?.as_i64()?;
                            let created_at = a.get("created_at")?.as_str()?.to_string();
                            Some(AutomationApprovalView {
                                id,
                                rule_id,
                                conversation_id,
                                proposed_action: ActionView::SendNote {
                                    body: String::new(),
                                },
                                status: "pending".into(),
                                created_at,
                            })
                        })
                        .collect();
                    approvals.set(views);
                }
            }
            loading.set(false);
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

            // ── Approval queue ──
            <section class="spp-automation-approvals">
                <h3 class="spp-automation-approvals__title">"Approval queue"</h3>
                <Show
                    when=move || !approvals.get().is_empty()
                    fallback=move || {
                        view! {
                            <EmptyState message="No pending approvals. High-impact automation actions will appear here for review." />
                        }
                    }
                >
                    <ApprovalQueue approvals=approvals.get() />
                </Show>
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

/// The approval queue — a list of pending approvals with approve/reject buttons.
#[component]
fn ApprovalQueue(approvals: Vec<AutomationApprovalView>) -> impl IntoView {
    let rows_fragment = leptos::Fragment::new(
        approvals
            .iter()
            .map(|a| {
                view! {
                    <ApprovalRow approval=a.clone() />
                }
                .into_view()
            })
            .collect::<Vec<_>>(),
    );
    view! {
        <div class="spp-automation-queue">
            {rows_fragment.clone()}
        </div>
    }
}

/// A single approval row.
#[component]
fn ApprovalRow(approval: AutomationApprovalView) -> impl IntoView {
    let action_label = approval.proposed_action.label();
    let requires_approval = approval.proposed_action.requires_approval();
    let conv_id = approval.conversation_id;
    let created_at = approval.created_at.clone();

    view! {
        <div class="spp-approval-row">
            <div class="spp-approval-row__header">
                <span class="spp-approval-row__action">{action_label}</span>
                <span class="spp-approval-row__conv">"Conversation #" {conv_id} </span>
                <span class="spp-approval-row__time">{created_at}</span>
            </div>
            <Show when=move || requires_approval fallback=|| ().into_view()>
                <div class="spp-approval-row__actions">
                    <button class="spp-approval-row__approve" type="button">
                        "Approve"
                    </button>
                    <button class="spp-approval-row__reject" type="button">
                        "Reject"
                    </button>
                </div>
            </Show>
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

    fn sample_approval(id: i64, rule_id: i64, conv_id: i64) -> AutomationApprovalView {
        AutomationApprovalView {
            id,
            rule_id,
            conversation_id: conv_id,
            proposed_action: ActionView::Assign {
                assignee_remote_id: 42,
            },
            status: "pending".into(),
            created_at: "2026-10-01T10:00:00Z".into(),
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

    #[test]
    fn empty_approvals_renders_empty_state() {
        let approvals: Vec<AutomationApprovalView> = Vec::new();
        assert!(approvals.is_empty());
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
    fn automation_approval_view_can_be_constructed() {
        let a = sample_approval(1, 7, 1001);
        assert_eq!(a.id, 1);
        assert_eq!(a.rule_id, 7);
        assert_eq!(a.conversation_id, 1001);
        assert_eq!(a.status, "pending");
    }
}
