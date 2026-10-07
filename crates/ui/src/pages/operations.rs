//! Operations Center page — the `/operations` route (M4-T02).
//!
//! Per spec M4: "Team operations: Operations Center, workload and capacity,
//! Notification Center, mentions, side threads, automation."
//!
//! This page is the team-ops hub: 16 tiles grouped by severity
//! (critical / warning / info), each tile linking to the filtered inbox
//! (`/inbox?view=<tile>&scope=<mailbox>`). Tiles report
//! `{key, count, available}` exactly like the reference snapshot.
//!
//! Per KNOWN PITFALLS: every view has loading, empty, and error states.
//! Per A12: closed vocabularies are single-source-of-truth — the 16 tiles
//! come from `OperationsTileKey::ALL` in the catalog, and severity grouping
//! uses the `Severity` enum from the theming tokens.

use leptos::*;
use leptos_router::*;

use crate::catalog::OperationsTileKey;
use crate::components::state_view::EmptyState;
use crate::components::theming::Severity;

/// The Operations Center snapshot returned by the Tauri IPC command
/// `operations_snapshot` (wired in a later task). For M4-T02 we render
/// this shape locally so the page is testable without the IPC layer.
///
/// Mirrors `spp_core::operations::OperationsSnapshot` but with `'static`
/// lifetimes so Leptos signals can hold it.
#[derive(Debug, Clone, Default)]
pub struct OperationsSnapshotView {
    /// All 16 tile counts, in `OperationsTileKey::ALL` order.
    pub tiles: Vec<(OperationsTileKey, TileCountView)>,
    /// The mailbox the snapshot was scoped to, or `None` for "all mailboxes".
    pub mailbox_id: Option<i64>,
    /// ISO-8601 timestamp the snapshot was built.
    pub built_at: String,
}

/// The UI-side mirror of `spp_core::operations::TileCount`. Tagged union
/// so the page can render either an integer or a "Not yet available" badge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TileCountView {
    /// The tile has a real count.
    Available {
        /// The number of items the tile counts.
        count: u32,
    },
    /// The tile's data is not available (reference: `available: false`).
    NotAvailable,
}

impl TileCountView {
    /// The human-readable label for the tile count.
    #[must_use]
    pub fn label(self) -> String {
        match self {
            Self::Available { count } => count.to_string(),
            Self::NotAvailable => "\u{2013}".to_string(),
        }
    }

    /// Whether the tile is `NotAvailable`.
    #[must_use]
    pub fn is_not_available(self) -> bool {
        matches!(self, Self::NotAvailable { .. })
    }

    /// The CSS class suffix for the badge (used as `spp-tile__badge--{suffix}`).
    /// Available tiles use the tile's severity; NotAvailable tiles use "muted".
    #[must_use]
    pub fn badge_class_suffix(self, tile: OperationsTileKey) -> &'static str {
        match self {
            Self::Available { .. } => tile_severity(tile).class_suffix(),
            Self::NotAvailable { .. } => "muted",
        }
    }
}

/// Map a tile to its severity bucket. Delegates to the catalog's
/// `OperationsTileKey::severity()` (single source of truth) and converts
/// the string back to the typed `Severity` enum.
#[must_use]
pub fn tile_severity(tile: OperationsTileKey) -> Severity {
    match tile.severity() {
        "critical" => Severity::Critical,
        "warning" => Severity::Warning,
        _ => Severity::Info,
    }
}

/// The tile's display name (human-readable, title-cased).
#[must_use]
pub fn tile_label(tile: OperationsTileKey) -> &'static str {
    match tile {
        OperationsTileKey::Unassigned => "Unassigned",
        OperationsTileKey::NeedsFirstResponse => "Needs first response",
        OperationsTileKey::CustomerWaiting => "Customer waiting",
        OperationsTileKey::WaitingOverThreshold => "Waiting over threshold",
        OperationsTileKey::Urgent => "Urgent",
        OperationsTileKey::SlaAtRisk => "SLA at risk",
        OperationsTileKey::SlaBreached => "SLA breached",
        OperationsTileKey::HighEffort => "High effort",
        OperationsTileKey::RepeatedIssue => "Repeated issue",
        OperationsTileKey::KnownIssue => "Known issue",
        OperationsTileKey::AiEscalation => "AI escalation",
        OperationsTileKey::IssueSpike => "Issue spike",
        OperationsTileKey::AutomationApprovals => "Automation approvals",
        OperationsTileKey::FailedJobs => "Failed jobs",
        OperationsTileKey::SyncProblems => "Sync problems",
        OperationsTileKey::CampaignActivity => "Campaign activity",
    }
}

/// The plain-language tooltip describing what the tile counts.
#[must_use]
pub fn tile_description(tile: OperationsTileKey) -> &'static str {
    match tile {
        OperationsTileKey::Unassigned => "Active conversations with no agent assigned.",
        OperationsTileKey::NeedsFirstResponse => "Conversations where no agent has replied yet.",
        OperationsTileKey::CustomerWaiting => {
            "Conversations where the customer sent the last message."
        }
        OperationsTileKey::WaitingOverThreshold => {
            "Customer-waiting conversations older than 1 hour."
        }
        OperationsTileKey::Urgent => "Conversations marked as urgent priority.",
        OperationsTileKey::SlaAtRisk => {
            "Conversations at risk of breaching their SLA. (Ships in M7.)"
        }
        OperationsTileKey::SlaBreached => {
            "Conversations whose SLA has been breached. (Ships in M7.)"
        }
        OperationsTileKey::HighEffort => "Conversations with more than 20 activity events.",
        OperationsTileKey::RepeatedIssue => {
            "Conversations linked to a known recurring issue. (Ships in M7.)"
        }
        OperationsTileKey::KnownIssue => "Conversations matched to a known issue. (Ships in M7.)",
        OperationsTileKey::AiEscalation => "Conversations escalated by the AI. (Ships in M6.)",
        OperationsTileKey::IssueSpike => {
            "Conversations clustered around a spiking issue. (Ships in M7.)"
        }
        OperationsTileKey::AutomationApprovals => "Pending automation approvals awaiting review.",
        OperationsTileKey::FailedJobs => "Background jobs that exhausted their retry budget.",
        OperationsTileKey::SyncProblems => "Help Scout sync runs that ended in failure.",
        OperationsTileKey::CampaignActivity => "Active outreach campaign activity. (Ships in M9.)",
    }
}

/// The `/inbox?view=<tile>` URL the tile links to.
///
/// Returns `None` for tiles that don't have an inbox filter (global tiles
/// like `failed_jobs` + `sync_problems`, plus all stubbed tiles).
#[must_use]
pub fn tile_inbox_link(tile: OperationsTileKey, mailbox_id: Option<i64>) -> Option<String> {
    match tile {
        OperationsTileKey::Unassigned
        | OperationsTileKey::NeedsFirstResponse
        | OperationsTileKey::CustomerWaiting
        | OperationsTileKey::WaitingOverThreshold
        | OperationsTileKey::Urgent
        | OperationsTileKey::HighEffort => {
            let mut url = format!("/inbox?view={}", tile.as_str());
            if let Some(mid) = mailbox_id {
                url.push_str(&format!("&scope={mid}"));
            }
            Some(url)
        }
        // Global + stubbed tiles have no inbox filter.
        OperationsTileKey::FailedJobs
        | OperationsTileKey::SyncProblems
        | OperationsTileKey::AutomationApprovals
        | OperationsTileKey::SlaAtRisk
        | OperationsTileKey::SlaBreached
        | OperationsTileKey::RepeatedIssue
        | OperationsTileKey::KnownIssue
        | OperationsTileKey::AiEscalation
        | OperationsTileKey::IssueSpike
        | OperationsTileKey::CampaignActivity => None,
    }
}

/// The Operations Center page component.
///
/// Shows the 16-tile grid (severity-grouped: critical / warning / info) plus
/// loading/empty/error states. Each tile links to its filtered inbox view
/// (if applicable) or shows a "Not yet available" badge (for stubbed tiles).
///
/// Wired to the `operations_snapshot` Tauri IPC command on mount.
#[component]
pub fn OperationsPage() -> impl IntoView {
    let snapshot = create_rw_signal(OperationsSnapshotView::default());
    let loading = create_rw_signal(true);
    let error_msg = create_rw_signal(None::<String>);
    // Cross-page invalidation (UI-26): the SSE bridge bumps the
    // 'operations-center' counter on conversation/sync/notification events —
    // the same refetch the reference gets from invalidating
    // ['operations-center'].
    let sse_refresh = crate::queries::version("operations-center");

    // Fetch the operations snapshot on mount (re-runs when the bridge
    // invalidates the center).
    create_effect(move |_| {
        let _ = sse_refresh.get();
        let snapshot = snapshot;
        let loading = loading;
        let error_msg = error_msg;
        wasm_bindgen_futures::spawn_local(async move {
            // GET /api/operations/center (reference hook useOperationsCenter):
            // { generated_at, mailbox_scope, tiles: [{key, label, count, available}], ... }
            match crate::api::get_json::<serde_json::Value>("/api/operations/center").await {
                Ok(data) => {
                    let tiles = data
                        .get("tiles")
                        .and_then(|t| t.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|entry| {
                                    let id = entry.get("key")?.as_str()?;
                                    let tile = if entry
                                        .get("available")
                                        .and_then(|v| v.as_bool())
                                        .unwrap_or(false)
                                    {
                                        let count = entry
                                            .get("count")
                                            .and_then(|v| v.as_u64())
                                            .unwrap_or(0)
                                            as u32;
                                        TileCountView::Available { count }
                                    } else {
                                        TileCountView::NotAvailable
                                    };
                                    // Match the key back to the enum variant.
                                    for key in OperationsTileKey::ALL {
                                        if key.as_str() == id {
                                            return Some((key, tile));
                                        }
                                    }
                                    None
                                })
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    snapshot.set(OperationsSnapshotView {
                        tiles,
                        mailbox_id: None,
                        built_at: data
                            .get("generated_at")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string(),
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
        <div class="spp-page spp-page--operations">
            <h2 class="spp-page__title">"Operations Center"</h2>
            <p class="spp-page__subtitle">
                "16 tiles cover the team's current workload, SLA risk, and system health. "
                "Click a tile to filter the inbox."
            </p>

            <Show when=move || loading.get() fallback=|| ()>
                <div class="spp-state">
                    <span class="spp-spinner" aria-label="Loading"></span>
                    <p class="spp-state__body">"Loading operations data…"</p>
                </div>
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
                    when=move || !snapshot.get().tiles.is_empty()
                    fallback=move || {
                        view! {
                            <EmptyState message="No operations data yet. Run a sync or try demo mode to populate tiles." />
                        }
                    }
                >
                    <OperationsTileGrid snapshot=snapshot.get() />
                </Show>
            </Show>
        </div>
    }
}

/// The 16-tile grid, severity-grouped (critical / warning / info).
///
/// Each tile is a `<TileCard>` that either links to the filtered inbox
/// or shows a "Not yet available" badge.
#[component]
fn OperationsTileGrid(snapshot: OperationsSnapshotView) -> impl IntoView {
    let tiles = snapshot.tiles.clone();
    let mailbox = snapshot.mailbox_id;

    // Group tiles by severity (in catalog order; the catalog already orders
    // them, but the grid groups them visually for the team-ops view).
    let critical: Vec<_> = tiles
        .iter()
        .copied()
        .filter(|(t, _)| tile_severity(*t) == Severity::Critical)
        .collect();
    let warning: Vec<_> = tiles
        .iter()
        .copied()
        .filter(|(t, _)| tile_severity(*t) == Severity::Warning)
        .collect();
    let info: Vec<_> = tiles
        .iter()
        .copied()
        .filter(|(t, _)| tile_severity(*t) == Severity::Info)
        .collect();

    view! {
        <div class="spp-operations-grid">
            <TileSection title="Critical" tiles=critical mailbox=mailbox />
            <TileSection title="Warning" tiles=warning mailbox=mailbox />
            <TileSection title="Info" tiles=info mailbox=mailbox />
        </div>
    }
}

/// A severity-grouped section of tiles.
#[component]
fn TileSection(
    title: &'static str,
    tiles: Vec<(OperationsTileKey, TileCountView)>,
    mailbox: Option<i64>,
) -> impl IntoView {
    let has_tiles = !tiles.is_empty();
    // Pre-build the card views as a `Fragment` (Clone via Rc) so the outer
    // view! closure can capture it by move and re-render cheaply. Iterating
    // inside the view! macro would force the closure to be FnOnce.
    let cards_fragment = leptos::Fragment::new(
        tiles
            .iter()
            .copied()
            .map(|(tile, count)| {
                view! {
                    <TileCard tile=tile count=count mailbox=mailbox />
                }
                .into_view()
            })
            .collect::<Vec<_>>(),
    );
    view! {
        <section class="spp-operations-section">
            <h3 class="spp-operations-section__title">{title}</h3>
            <Show
                when=move || has_tiles
                fallback=|| ().into_view()
            >
                <div class="spp-operations-section__grid">
                    {cards_fragment.clone()}
                </div>
            </Show>
        </section>
    }
}

/// A single tile card. Either a link (for real tiles with an inbox filter)
/// or a static card (for global tiles like `failed_jobs` or stubbed tiles).
#[component]
fn TileCard(tile: OperationsTileKey, count: TileCountView, mailbox: Option<i64>) -> impl IntoView {
    let label = tile_label(tile);
    let description = tile_description(tile);
    // Pre-compute the link into a plain String so the view! closure can be Fn.
    let href: String = tile_inbox_link(tile, mailbox).unwrap_or_default();
    let has_link = !href.is_empty();
    let count_label = count.label();
    let is_not_available = count.is_not_available();
    let badge_class = format!("spp-tile__badge--{}", count.badge_class_suffix(tile));
    let tile_class = format!("spp-tile spp-tile--{}", tile_severity(tile).class_suffix());

    view! {
        <div class={tile_class} title={description}>
            <span class="spp-tile__label">{label}</span>
            <span class={badge_class}>{count_label}</span>
            <Show when=move || is_not_available fallback=|| ()>
                <span class="spp-tile__not-available">"Not yet available"</span>
            </Show>
            <Show when=move || has_link fallback=|| ()>
                <A
                    href={href.clone()}
                    class="spp-tile__link"
                    active_class="spp-tile__link--active"
                >
                    "Open in inbox →"
                </A>
            </Show>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tile_label_covers_all_16() {
        // Every variant must have a non-empty label.
        for tile in OperationsTileKey::ALL {
            assert!(!tile_label(tile).is_empty(), "tile {tile:?} missing label");
        }
    }

    #[test]
    fn tile_description_covers_all_16() {
        for tile in OperationsTileKey::ALL {
            assert!(
                !tile_description(tile).is_empty(),
                "tile {tile:?} missing description"
            );
        }
    }

    #[test]
    fn tile_severity_matches_catalog() {
        for tile in OperationsTileKey::ALL {
            let s = tile_severity(tile);
            // The catalog's severity string must round-trip to our Severity enum.
            let catalog_str = tile.severity();
            match catalog_str {
                "critical" => assert_eq!(s, Severity::Critical),
                "warning" => assert_eq!(s, Severity::Warning),
                "info" => assert_eq!(s, Severity::Info),
                _ => panic!("unknown severity: {catalog_str}"),
            }
        }
    }

    #[test]
    fn tile_inbox_link_returns_some_for_real_conversation_tiles() {
        for tile in [
            OperationsTileKey::Unassigned,
            OperationsTileKey::NeedsFirstResponse,
            OperationsTileKey::CustomerWaiting,
            OperationsTileKey::WaitingOverThreshold,
            OperationsTileKey::Urgent,
            OperationsTileKey::HighEffort,
        ] {
            assert!(
                tile_inbox_link(tile, None).is_some(),
                "tile {tile:?} should have an inbox link"
            );
        }
    }

    #[test]
    fn tile_inbox_link_returns_none_for_global_and_stubbed_tiles() {
        for tile in [
            OperationsTileKey::FailedJobs,
            OperationsTileKey::SyncProblems,
            OperationsTileKey::AutomationApprovals,
            OperationsTileKey::SlaAtRisk,
            OperationsTileKey::SlaBreached,
            OperationsTileKey::RepeatedIssue,
            OperationsTileKey::KnownIssue,
            OperationsTileKey::AiEscalation,
            OperationsTileKey::IssueSpike,
            OperationsTileKey::CampaignActivity,
        ] {
            assert!(
                tile_inbox_link(tile, None).is_none(),
                "tile {tile:?} should NOT have an inbox link"
            );
        }
    }

    #[test]
    fn tile_inbox_link_includes_scope_when_mailbox_set() {
        let url = tile_inbox_link(OperationsTileKey::Unassigned, Some(101)).unwrap();
        assert!(url.contains("scope=101"), "url: {url}");
    }

    #[test]
    fn tile_count_view_available_label() {
        let c = TileCountView::Available { count: 42 };
        assert_eq!(c.label(), "42");
        assert!(!c.is_not_available());
    }

    #[test]
    fn tile_count_view_not_available_label_is_dash() {
        let c = TileCountView::NotAvailable;
        assert_eq!(c.label(), "\u{2013}");
        assert!(c.is_not_available());
    }

    #[test]
    fn tile_count_view_badge_class_uses_severity_for_available() {
        // Critical tile (sla_breached) — but it's unavailable, so badge should be muted.
        let c = TileCountView::NotAvailable;
        assert_eq!(
            c.badge_class_suffix(OperationsTileKey::SlaBreached),
            "muted"
        );

        // Available count on a critical-severity tile (sync_problems is warning).
        let c = TileCountView::Available { count: 3 };
        assert_eq!(
            c.badge_class_suffix(OperationsTileKey::SyncProblems),
            "warning"
        );
    }

    #[test]
    fn empty_snapshot_renders_empty_state() {
        // The page's default signal is an empty snapshot.
        // Verify the snapshot type's default has no tiles.
        let s = OperationsSnapshotView::default();
        assert!(s.tiles.is_empty());
        assert!(s.mailbox_id.is_none());
        assert!(s.built_at.is_empty());
    }

    #[test]
    fn snapshot_with_all_16_tiles_can_be_grouped_by_severity() {
        // Build a synthetic snapshot with all 16 tiles stubbed + count=0.
        let tiles: Vec<(OperationsTileKey, TileCountView)> = OperationsTileKey::ALL
            .iter()
            .map(|&t| (t, TileCountView::Available { count: 0 }))
            .collect();
        let snapshot = OperationsSnapshotView {
            tiles,
            mailbox_id: None,
            built_at: "2026-10-01T10:00:00Z".into(),
        };
        // Group by severity.
        let critical = snapshot
            .tiles
            .iter()
            .filter(|(t, _)| tile_severity(*t) == Severity::Critical)
            .count();
        let warning = snapshot
            .tiles
            .iter()
            .filter(|(t, _)| tile_severity(*t) == Severity::Warning)
            .count();
        let info = snapshot
            .tiles
            .iter()
            .filter(|(t, _)| tile_severity(*t) == Severity::Info)
            .count();
        // Critical: only sla_breached (1 tile).
        assert_eq!(critical, 1, "critical: {critical}");
        // Warning (reference operationsCenter.ts): needs_first_response,
        // waiting_over_threshold, urgent, sla_at_risk, ai_escalation,
        // issue_spike, automation_approvals, failed_jobs, sync_problems
        // (9 tiles).
        assert_eq!(warning, 9, "warning: {warning}");
        // Info: the rest (6 tiles).
        assert_eq!(info, 6, "info: {info}");
        // Total: 16.
        assert_eq!(critical + warning + info, 16);
    }
}
