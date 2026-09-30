# UI-PARITY.md — SupportOS++

> Screen-by-screen comparison against the reference repo's README screenshots and demo GIF (`docs/screenshots/` of the reference repo).
> Per A11: match structure, labels, states and behavior. Do NOT copy CSS, markup, or code.

## Method

1. Reference screenshots live in `<reference-checkout>/docs/screenshots/` (NOT in this repo). Do not commit reference screenshots into this repo.
2. For each screen, capture a SupportOS++ screenshot at the same milestone state.
3. Compare structure (3-pane layout? tile grid? table?), labels (button text, headings, empty states), states (loading/empty/error/loaded), and behavior (click → drill-down consistency, badge numbers match the list, etc.).
4. Record `✅ matches` / `⚠️ differs: <reason>` / `❌ not implemented yet` per screen.

## Status

| # | Reference screen | M1 | M2 | M3 | M4 | M5 | M6 | M7 | M8 | M9 | M10 | M11 | Notes |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 1 | `dashboard.png` | ⏳ shell only | | | | | | | | | | | KPI tiles, trends |
| 2 | `inbox.png` | ⏳ shell only | | | | | | | | | | | 3-pane ticket workspace |
| 3 | `conversation.png` | | | | | | | | | | | | AI context panel |
| 4 | `search.png` | | | | | | | | | | | | Universal search |
| 5 | `v180-operations-center.png` | | | | | | | | | | | | 16 tiles |
| 6 | `client-intelligence.png` | | | | | | | | | | | | Behavior signals w/ evidence |
| 7 | `ai-center.png` | | | | | | | | | | | | Local model analytics |
| 8 | `issues.png` | | | | | | | | | | | | Issue Radar |
| 9 | `knowledge.png` | | | | | | | | | | | | Knowledge docs mirror |
| 10 | `reports.png` | | | | | | | | | | | | Custom report builder (21×14) |
| 11 | `client-profile.png` | | | | | | | | | | | | |
| 12 | `v130-*.png` (chat / dashboard / docs) | | | | | | | | | | | | v1.3-era reference shots, kept for layout history |
| 13 | `v140-business-hours.png` | | | | | | | | | | | | |
| 14 | `v140-docs-semantic.png` | | | | | | | | | | | | |
| 15 | `v140-sla-configured.png` | | | | | | | | | | | | |
| 16 | `v140-sla-report.png` | | | | | | | | | | | | |

Statuses: `✅ matches` · `⚠️ differs` · `❌ not implemented yet` · `⏳ partial`.

## Demo GIF

Reference `docs/demo.gif` shows the full demo-mode flow. The SupportOS++ equivalent will be captured at the end of M2 (Help Scout mirror + demo mode) and embedded in the README.

## Per-screen rows will be expanded as each milestone lands

For each implemented screen, the responsible milestone will add a sub-section here with:
- reference screenshot path
- SupportOS++ screenshot path (committed under `docs/screenshots/`)
- structural comparison (layout, components, panes)
- label comparison (button text, headings, tooltips)
- state comparison (loading / empty / error / loaded)
- behavior comparison (clicks, drill-downs, keyboard shortcuts)

Per A11: "do not copy CSS, markup or code" — only structure, labels, states, and behavior.
