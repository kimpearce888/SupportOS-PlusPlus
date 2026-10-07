# progress.md — current work items

This file must contain **exactly the five items currently being worked on** (see `rule.md`).

Within the current five-item batch, items are implemented one at a time in listed order; everything else stays in `plan.md` until its turn.

| # | ID | Item | Status | Severity | Effort | Audit ref | What needs to be fixed |
|---|---|---|---|---|---|---|---|
| 59 | UI-22 | Command palette (live search, 12 hits, keyboard nav) | Stub | Major | M | M9 | Live search via POST /api/search, 200ms debounce, keyboard nav, 12 hits |
| 60 | UI-26 | SSE toasts + cross-page invalidation | Missing | Major | M | - | Toast center for rating/webhook/campaign/critical events; subscribe error |
| 61 | UI-27 | URL-backed state / deep links (view=, days=, doc=, article=, tab=) | Missing | Major | M | - | URL-backed filter state + deep links on dashboard/inbox/knowledge/docs/issues |
| 62 | UI-01 | Dashboard page (ranges, mailbox/channel filters, charts, radar card, KPI links) | Partial | Major | L | - | Ranges, mailbox/channel filters, charts, radar card, linked KPIs, days param |
| 63 | UI-04 | Customers + detail (properties, memories, ratings, interaction profile, health) | Partial | Major | L | - | Properties, memories, ratings, resolutions, interaction profile, support health, clickable conversations, pagination |
