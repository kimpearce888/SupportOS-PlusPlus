# OPEN-ITEMS.md — SupportOS++

> Every TODO, FIXME, stub, placeholder, open deviation, and BLOCKED item.
> Generated: Session 39 (owner "FINISH" directive).
> This list drives STEP 1b — each item must be implemented or marked BLOCKED with evidence.

## Summary

| Category | Count | Status |
|---|---|---|
| TODO / not-yet-implemented (Qdrant adapter) | 5 | To implement in STEP 1b |
| TODO / not-yet-implemented (Filter translation) | 1 | To implement in STEP 1b |
| Stubbed Operations Center tiles | 7 | To implement or document |
| Open deviations (pending owner approval) | 3 | Awaiting owner |
| BLOCKED items | 1 | Awaiting owner (signing certs) |
| macOS-specific exclusion | 1 | Moot (DEV-006 removes macOS) |

---

## 1. Qdrant Edge adapter (DEV-002) — TODOs to implement

These are in `crates/core/src/vectorstore_qdrant.rs` (behind `--features qdrant`):

| # | Item | Location | What's needed |
|---|---|---|---|
| 1 | `search_sparse` not implemented | line 244-248 | Configure a named sparse vector in EdgeConfig; upsert sparse vectors; translate our SparseVector to qdrant-edge's sparse format. |
| 2 | `snapshot` not implemented | line 277-280 | Bridge qdrant-edge's on-disk shard format to our adapter-agnostic CollectionSnapshot JSON. Read all points via scroll + serialize. |
| 3 | `restore` not implemented | line 285-287 | Deserialize our CollectionSnapshot JSON and upsert all points into a new shard. |
| 4 | Filter translation not implemented | line 208 | Translate our `Filter` (must-equal HashMap) to qdrant-edge's native Filter type. Currently `filter` is ignored in `search_dense`. |
| 5 | Sparse-only points not supported in upsert | line 166-168 | Currently requires `point.dense` to be Some; sparse-only points return an error. |

## 2. Operations Center stubbed tiles

These tiles return `TileCount::NotAvailable { milestone }` instead of a real count.
Located in `crates/core/src/operations.rs`.

| # | Tile | Milestone | What's needed |
|---|---|---|---|
| 1 | `SlaBreached` | M7 | Wire to `count_by_response_state(SlaBreached)` from M004 |
| 2 | `HighFrictionCount` | M8 | Wire to `friction_scores` table from M020 |
| 3 | `HighFrictionRate` | M8 | Wire to `friction_scores` table (rate calculation) |
| 4 | `IssueLinkedShare` | M7 | Wire to `known_issue_links` table from M015 |
| 5 | `AiAttributeShare` | M6 | Wire to `ai_attributes` table from M011 |
| 6 | `CampaignSent` | M9 | Wire to `campaigns` + `campaign_recipients` from M024 |
| 7 | `CampaignReplyRate` | M9 | Wire to `campaign_recipients` (reply tracking) |

## 3. Open deviations (pending owner approval)

| ID | Description | Status |
|---|---|---|
| DEV-002 | QdrantEdge adapter implements only dense-vector subset | pending — will be resolved by STEP 1b |
| DEV-003 | macOS app-crate tests excluded | pending — MOOT (DEV-006 removes macOS from scope) |
| DEV-005 | Production builds use InMemoryVectorStore, not Qdrant Edge | pending — depends on DEV-002 resolution |

## 4. BLOCKED items

| ID | Description | What's needed |
|---|---|---|
| M1-T10 | Installer signing | Owner certificates (per spec A5: "Signing and notarization need my certificates and accounts") |

## 5. Other items

| # | Item | Location | Notes |
|---|---|---|---|
| 1 | Loopback route stubs | `crates/core/src/loopback.rs:62` | JSON body for routes still using placeholder handlers. The axum Router is in place but some routes return stub responses. These are internal (loopback only, not user-facing). |
| 2 | Generic report metric fallback | `crates/core/src/reports.rs:312` | Unknown metrics fall back to `COUNT(*)` with a note. This is by design (graceful degradation), not a TODO. |
| 3 | FakeAiProvider placeholder text | `crates/core/src/ai_provider.rs:252` | The Fake provider returns "[Fake AI demo response]" text. This is by design (spec A12: Fake adapter for demo/tests only). |
