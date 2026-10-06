# progress.md — current work items

This file must contain **exactly the five items currently being worked on** (see `rule.md`).

| ID | Item | Status | Severity | What needs to be fixed |
|---|---|---|---|---|
| SY-10 | Provider write methods (createConversation, updateTags/Fields, snooze/schedule, runWorkflow, getAttachmentData, ping, routing) | Partial | Major | Implement provider write methods (createConversation, updateTags/Fields, snooze/schedule, runWorkflow, getAttachmentData, ping, routing). |
| AI-18 | Interaction forbidden-claim text safety scan | Missing | Major | Port FORBIDDEN_PATTERNS text scan used on interaction free text |
| AI-17 | Interaction 2-stage AI enrichment (observe/recommend prompts + safety gates) | Missing | Major | Add 2-stage AI enrichment with enum filter + evidence whitelist + forbidden-claim gates |
| AI-16 | Interaction intelligence engine (observations/baselines/outcomes/recommendations/card/profile) | Partial | Major | Port observation inserts, baseline rebuild, outcome/recommendation engines, profile assembly, playbook |
| DB-05 | Soft-delete + merge semantics (deleted_at filter, resurrect on upsert) | Divergent | Major | Add deleted_at/merged filters to inbox list; resurrect deleted rows on upsert |
