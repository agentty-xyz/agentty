# Forge Review Request Support Plan

Plan for extending `crates/agentty/src/app`, `crates/agentty/src/infra`, and `crates/agentty/src/ui` so review-ready sessions can publish and track forge review requests across GitHub pull requests and GitLab merge requests.

## Cross-Plan Check Before Implementation

- `docs/plan/coverage_follow_up.md` only tracks coverage-ratchet follow-up work and does not overlap forge review-request sequencing, file ownership, or rollout assumptions.
- No other active plan in `docs/plan/` currently claims the remaining review-request poller, session-view action, or usage-doc work.

## Status Maintenance Rule

- After implementing any step in this plan, immediately update its status in this document.

## Current State Snapshot

| Area | Current state in codebase | Status |
|------|---------------------------|--------|
| Manual session review-request workflow | `crates/agentty/src/app/service.rs`, `crates/agentty/src/app/session/workflow/lifecycle.rs`, and `crates/agentty/src/app/session/workflow/refresh.rs` now wire `ReviewRequestClient` into publish, open, and refresh helpers that persist normalized PR/MR linkage and can refresh archived sessions from stored forge URLs after worktree cleanup. | Healthy |
| Forge adapters and persistence | `crates/agentty/src/infra/forge/github.rs`, `crates/agentty/src/infra/forge/gitlab.rs`, `crates/agentty/src/infra/db.rs`, and `crates/agentty/src/app/session/workflow/load.rs` cover normalized create/find/refresh behavior plus durable `session_review_request` loading. | Healthy |
| Background reconciliation | `crates/agentty/src/app/task.rs` still only runs git-status and version-check jobs, and no app event currently polls linked review requests or reconciles merged or closed remote outcomes back into session status. | Not started |
| Session view UI | `crates/agentty/src/runtime/mode/session_view.rs`, `crates/agentty/src/ui/state/help_action.rs`, and `crates/agentty/src/ui/page/session_chat.rs` still expose merge, diff, focused-review, and worktree-open actions only; review-request controls and metadata rendering are not wired yet. | Not started |
| Documentation coverage | `docs/site/content/docs/architecture/module-map.md`, `docs/site/content/docs/architecture/runtime-flow.md`, and `docs/site/content/docs/architecture/testability-boundaries.md` now describe the manual workflow baseline, but `docs/site/content/docs/usage/workflow.md` and `docs/site/content/docs/usage/keybindings.md` still do not cover review-request actions or automatic reconciliation. | Partial |

## Implementation Approach

- Keep the already-landed manual review-request workflow as the working baseline: a review-ready session can publish its branch, link or create a review request, refresh stored metadata, and open the linked URL.
- Surface session-view actions and rendered metadata next so users can drive the manual workflow from the primary TUI without leaving session context.
- Keep the first UI slice compatible with explicit refresh; automatic reconciliation can land afterward as an extension of the visible workflow rather than a prerequisite for it.
- Update usage docs in the same iteration as the session-view action, then update architecture docs when the background reconciliation boundary and reducer flow are stable.

## Updated Priorities

## 1) Ship Manual Session Review-Request Workflows

**Why now:** This is the smallest usable slice of the feature, and it already exists in the branch as the baseline that all later automation extends.
**Usable outcome:** A review-ready session can publish its branch, reuse or create its linked PR/MR, refresh the stored summary, and open the linked review request even after worktree cleanup.

- [x] Keep the cross-forge `ReviewRequestClient` boundary narrow and normalize GitHub and GitLab adapter behavior around create, find, refresh, and openable URLs.
- [x] Persist normalized review-request linkage on `Session` through `session_review_request` and reload it during session snapshot loading.
- [x] Add `SessionManager` publish, open, and refresh helpers that reuse stored links, avoid duplicate review-request creation, and refresh archived sessions from stored forge URLs.
- [x] Cover provider adapters, persistence, and session workflows with mock-driven tests instead of live forge or network calls.

Primary files:

- `crates/agentty/src/infra/forge.rs`
- `crates/agentty/src/infra/forge/github.rs`
- `crates/agentty/src/infra/forge/gitlab.rs`
- `crates/agentty/src/infra/db.rs`
- `crates/agentty/src/app/session/workflow/load.rs`
- `crates/agentty/src/app/session/workflow/lifecycle.rs`
- `crates/agentty/src/app/session/workflow/refresh.rs`

## 2) Surface Review-Request Actions and Manual Usage Docs

**Why now:** The manual workflow already works in the branch, but users still cannot discover or drive it from the main session view. Exposing that flow in-app is the smallest remaining end-to-end slice with immediate user value.
**Usable outcome:** A user can create, open, or refresh a linked PR/MR directly from session view, see the normalized metadata in the TUI, and understand the manual workflow from the usage docs even before automatic reconciliation exists.

- [ ] Add one session-view action for create, open, or refresh review-request behavior based on the current forge and link state.
- [ ] Extend `AppMode`, help/footer projections, and view-mode key handling to show loading, success, and blocked states without leaving session context.
- [ ] Render normalized review-request metadata in session UI without displacing existing diff and focused-review behavior.
- [ ] Show actionable blocked states when `gh` or `glab` is missing or unauthenticated so users can fix local CLI setup from the same review flow.
- [ ] Add UI-focused tests that keep the new action availability, footer/help text, and session rendering aligned with session status and linked review-request state.
- [ ] Update `docs/site/content/docs/usage/workflow.md` and `docs/site/content/docs/usage/keybindings.md` to document the manual session-view review-request flow and its local CLI prerequisites.

Primary files:

- `crates/agentty/src/runtime/mode/session_view.rs`
- `crates/agentty/src/ui/state/app_mode.rs`
- `crates/agentty/src/ui/state/help_action.rs`
- `crates/agentty/src/ui/page/session_chat.rs`
- `docs/site/content/docs/usage/workflow.md`
- `docs/site/content/docs/usage/keybindings.md`

## 3) Add Background Review-Request Status Reconciliation and Final Architecture Docs

**Why now:** Once users can already drive and inspect review requests from session view, the next increment should automate remote lifecycle transitions without hiding the baseline capability behind background infrastructure first.
**Usable outcome:** Linked sessions automatically reconcile to `Done` or `Canceled` after the remote review request is observed as merged or closed, and the architecture docs describe the final poller and reducer boundaries.

- [ ] Add an app-scoped background job that periodically checks linked review-request state for active sessions with forge metadata.
- [ ] Route poller results through `AppEvent` or an equivalent reducer-driven path instead of mutating session state directly inside the task.
- [ ] Reuse the `gh` and `glab` adapter refresh commands inside the poller instead of introducing a second direct network client for background reconciliation.
- [ ] Move a session to `Done` when the linked review request is merged and to `Canceled` when it is closed without merge, while preserving explicit local terminal states when no transition is needed.
- [ ] Define guardrails for polling cadence, unsupported or unauthenticated forge failures, and stale-session behavior so the poller stays low-noise and cheap.
- [ ] Add deterministic tests for poll scheduling, event reduction, and status-transition rules for merged, closed, reopened, and unavailable review-request states.
- [ ] Update `docs/site/content/docs/usage/workflow.md`, `docs/site/content/docs/architecture/runtime-flow.md`, `docs/site/content/docs/architecture/testability-boundaries.md`, and `docs/site/content/docs/architecture/module-map.md` to describe the automatic reconciliation behavior and its new runtime boundary.

Primary files:

- `crates/agentty/src/app/task.rs`
- `crates/agentty/src/app/core.rs`
- `crates/agentty/src/app/session/workflow/refresh.rs`
- `crates/agentty/src/app/session/workflow/task.rs`
- `crates/agentty/src/domain/session.rs`
- `docs/site/content/docs/usage/workflow.md`
- `docs/site/content/docs/architecture/runtime-flow.md`
- `docs/site/content/docs/architecture/testability-boundaries.md`
- `docs/site/content/docs/architecture/module-map.md`

## Suggested Execution Order

```mermaid
graph TD
    P1[1. Manual session review-request workflows] --> P2[2. Session-view actions and manual docs]
    P2 --> P3[3. Background reconciliation and final architecture docs]
```

1. Treat `1) Ship Manual Session Review-Request Workflows` as the already-landed baseline and use it as the validation target for all remaining work.
1. Start `2) Surface Review-Request Actions and Manual Usage Docs` immediately after `1)` because it is the next smallest user-visible slice and depends only on the existing manual publish, open, and refresh contract.
1. Start `3) Add Background Review-Request Status Reconciliation and Final Architecture Docs` only after `2)` is merged, because the poller should extend an already-discoverable session-view workflow instead of defining it.
1. No top-level priorities are safe to run in parallel right now; the usage-doc tasks inside `2)` can trail the final keybinding and action names, and the architecture-doc tasks inside `3)` can trail the final poller boundary once the reducer flow is settled.

## Out of Scope for This Pass

- A repository-wide inbox for browsing arbitrary pull requests or merge requests independent of sessions.
- Inline review-comment authoring, draft review management, or one-click merge parity with forge web UIs.
- Support for forges beyond GitHub and GitLab.
- Webhook-driven reconciliation or any server-side push infrastructure beyond the local CLI-based polling planned here.
