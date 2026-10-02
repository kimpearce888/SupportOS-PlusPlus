# TASKS.md — bounded repair tracker

> One line per bounded work item, ordered by the Phase 7 fix priority.
> Canonical status/evidence lives in **PARITY.md** — this file only tracks
> what is queued next. Do not duplicate F-ID detail here.

## Queue (priority order)

- [ ] T1 Webhook correctness: fix 500 column drift; base64 HMAC; dedup key;
      wire `process_webhook` pipeline + boot drain + prune (F-019..F-026)
- [ ] T2 Real HTTP status codes on error paths (F-006)
- [ ] T3 Body limit 20 MB + CORS [::1] origin (F-003, F-005)
- [ ] T4 SSE: named events, 7-type catalog, `: ping` text, 25-stream cap,
      UI client URL + subscribers (F-014..F-018)
- [ ] T5 Dashboard/analytics SQL column drift + computed reports (F-084, F-088)
- [ ] T6 notification_prefs table fix + 16th kind + sweep wiring (F-072..F-074)
- [ ] T7 jobs state vocabulary + queue retry semantics (F-033)
- [ ] T8 Demo endpoint semantics (seed on enable; route through real
      pipeline) (F-010..F-013)
- [ ] T9 Sync engine wiring: real provider, OAuth, registration, rate
      limiter/queue, status (F-027..F-034)
- [ ] T10 .sosync byte-compatible format (F-041)
- [ ] T11 HTML sanitizer + SSRF DNS-resolution parity + AI redaction (F-044..F-046)
- [ ] T12 Remove EXTRA AI providers (Ollama/Generic) (F-051)
- [ ] T13 AI endpoints wiring (analyze/draft/similar/rewrite/verify/
      feedback/cluster) + evaluation mode enforcement (F-052, F-055)
- [ ] T14 Vector store production wiring + hybrid search + exact ticket
      lookup (F-059..F-063)
- [ ] T15 Missing inbox routes (~19) + write-pipeline idempotency (F-077..F-081)
- [ ] T16 SLA business-minutes engine (F-083) + CSV export (F-086)
- [ ] T17 Register 33 unregistered handlers + implement their engines
      (incidents/ai/analytics/issues/CO links/knowledge files) (F-085, F-089..F-093, F-105, F-107)
- [ ] T18 Operations tiles: compute the 7 NotAvailable (F-069)
- [ ] T19 Views tree depth 10 (F-066)
- [ ] T20 UI: wire 17 dead pages, keyboard shortcuts, command palette,
       URL state, theme, toasts, confirmations, IPC/withGlobalTauri,
       polling parity (F-109..F-139)
- [ ] T21 Settings API parity (18 routes, forced flags, strict patch) (F-140..F-142)
- [ ] T22 Connectors refresh/test/rows + wizard (F-102, F-103)
- [ ] T23 Outreach send queue/throttle/reconcile + audit trail (F-098..F-100)
- [ ] T24 Port reference behavioral tests to Rust (F-157)
- [ ] T25 Differential test harness (Rust) vs running reference (Phase 5)
- [ ] T26 Migration recording + self_check version fix (F-036)
- [ ] T27 Retention prunes (F-040) + FTS coverage (F-038)
- [ ] T28 Packaging verification deb+AppImage + docs final pass (F-148, F-149, F-156)

## Done

- [x] Cleanup: Linux-only scope enforced (files/configs/CI/docs) — see PROGRESS.md
- [x] PARITY.md 157-F-ID master checklist from scratch
- [x] Rust port of the E2E WebDriver driver (xtask --bin e2e)
