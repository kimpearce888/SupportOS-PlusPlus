# rule.md — mandatory workflow rules

These rules govern how the parity-fix backlog (`plan.md`) is executed. They are mandatory and must not be bypassed.

Statuses: an item moves `plan.md` → `progress.md` → implementation → verification → `completed.md`.

## Rule 1 — One item at a time

Fix **exactly one item at a time** from `plan.md`.

Never work on multiple plan items simultaneously.

## Rule 2 — Start of work

Before starting an item:

1. Select the next item from `plan.md`.
2. Remove that item from `plan.md`.
3. Add that item to `progress.md`.
4. Begin implementation only after the item has been moved to `progress.md`.

## Rule 3 — During work

Only work on the item currently recorded in `progress.md`.

Do not start another item until the current item has been completed and verified.

## Rule 4 — Completion

After the current item is fixed and verified:

1. Remove it from `progress.md`.
2. Add it to `completed.md`.
3. Record the completed work and verification result.
4. Stop and report the result to the user.

## Rule 5 — Verification

An item is complete only after the relevant code, tests, builds, or other required checks have been successfully verified.

Do not mark an item as completed based only on code changes.

## Rule 6 — No skipped items

Every item originally identified in `plan.md` must eventually be processed.

Do not skip, merge, or silently discard items.

## Rule 7 — Keep files synchronized

At all times:

- `plan.md` = remaining TODO items
- `progress.md` = exactly one active item
- `completed.md` = completed and verified items

The same item must never exist in more than one of these files at the same time.

## Rule 8 — Do not automatically start the next item

After completing an item, **do not begin the next item automatically**.

Instead:

1. Finish and verify the current item.
2. Update `progress.md` and `completed.md`.
3. Give the user a clear summary of the completed work and verification result.
4. Tell the user to say **`continue`** to proceed to the next item in `plan.md`.

When the user says **`continue`**, resume the workflow by selecting the next item from `plan.md`.

## Rule 9 — Blocked work

If the current item cannot be completed because of a blocker:

- Keep the item in `progress.md`.
- Clearly document the blocker.
- Do not start another item.
- Report the blocker to the user.
- Tell the user what is required to continue.

## Execution workflow

Follow this state transition exactly:

`plan.md → progress.md → implementation → verification → completed.md → STOP`

After stopping, wait for the user to say:

`continue`

Then process the next item.
