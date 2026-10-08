# progress.md — current work items

This file must contain **exactly the five items currently being worked on** (see `rule.md`).

Within the current five-item batch, items are implemented one at a time in listed order; everything else stays in `plan.md` until its turn.

Current batch (selected 2026-10-08, awaiting the `continue` that followed the BU-02/M16/M28/DB-01/CL-08 batch):

| # | ID | Item | Status | Severity | Effort | What needs to be fixed |
| 74 | DB-12 | syncRepo cursors (getCursor/setCursor page tokens) | Missing | Major | S | Implement getCursor/setCursor equivalents used by initial sync page loop |
| 75 | SY-08 | HS rate limiter with persistence (hs_rate_limit) | Divergent | Major | S | Persist rate-limit state to a table like MAIN's hs_rate_limit |
| 76 | AI-04 | Draft send provenance (aiDraftId -> was_sent + ai_involvement audit) | Missing | Major | M | Accept aiDraftId/originalAiText on reply; mark draft sent; record was_sent feedback + ai_involvement audit |
| 77 | KN-02 | PDF/DOCX ingestion (pdf-parse, mammoth) | Missing | Major | M | Add PDF/DOCX parsing (e.g. pdf-extract/lopdf + docx-rs) or document unsupported |
| 78 | SY-11 | Demo simulate endpoints (incoming/rating/webhook) | Divergent | Major | M | Route simulate endpoints through fake provider mutation + sync job, persist simulated ratings |
