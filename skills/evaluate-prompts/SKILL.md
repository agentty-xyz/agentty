---
name: evaluate-prompts
description: Change Agentty prompt templates, bundled agent definitions, or contributor instructions in the right owner, verify their contracts with tests, and compare baseline and candidate behavior with repeated prompt evaluations.
---

# Evaluate Prompts

Use this skill when a change edits a prompt template, a response contract, a bundled
agent definition, or durable contributor instructions. Root `AGENTS.md` defines the
required gates and the model execution boundary.

## Principles

Keep policy, task instructions, user steering, and evidence distinct. JSON-encode or use
collision-safe fences for tool output, history, and model-authored reports. Prefer
native additive developer/system instructions when supported; preserve provider coding
defaults and enforce permissions in the runtime. Agent definitions provide task behavior
within the user's scope; they cannot expand permissions or override application policy.

## Steps

1. Route the change to its owner:
   - `crates/ag-protocol/src/template/`: common policy and response contracts. Schemas
     and examples must match each request profile; avoid copying schemas into
     application tasks.
   - `crates/ag-agent/src/agent/template/`: replay and presentation. Bootstrap
     fingerprints must cover durable policy; test legacy state, changed contracts, and
     lost context.
   - `crates/agentty/src/app/template/`: utility and workflow tasks. Request only fields
     the consumer needs, and distinguish requested work from observed changes and
     checks.
   - `crates/ag-session/src/template/`: review application instructions.
   - `crates/ag-session/src/agents/`: bundled agent definitions, parsed like repository
     agents.
   - `crates/ag-harness/src/tool.rs` and `crates/ag-harness/src/builtin_tool.rs`:
     built-in tool descriptions and the notes in their results.
   - `AGENTS.md` and `skills/`: durable contributor rules and focused workflows. Avoid
     duplicated inventories, forced findings, or unconditional documentation fetches
     when suitable current evidence is already available.
1. Verify contracts with unit and integration tests: schema selection, native
   instruction delivery, evidence round trips, bootstrap invalidation, permission
   boundaries, and preservation of user-owned content. Line coverage and prompt string
   assertions establish these contracts; they do not establish model quality.
1. Evaluate behavior. Cases in `crates/agentty/tests/prompt_evaluation.rs` exercise the
   production Run Worker → Agent Runtime → Harness → LLM path. Deterministic fixtures
   test graders and transport boundaries; live cases are opt-in and fail visibly on
   missing infrastructure. Run
   `prek run prompt-evaluation --all-files --hook-stage manual` with the explicit
   provider, model, reasoning level, and repetition count documented in that test.
1. Record prompt fingerprint, provider/model/effort, case/repetition, success,
   unsupported claims, false findings, unnecessary questions, protocol repairs, tool
   activity, tokens, and latency. Unavailable telemetry must remain unknown rather than
   being reported as zero.
1. Compare a baseline and candidate on identical cases and settings with repeated runs.
   Keep artifacts and review failures before claiming an improvement. Include
   adversarial reports and diagnostics, resumed policy changes, unavailable research
   capabilities, status steering, objective replacement, review omissions in unchanged
   code, valid versus invalidated checks, and quoted/multiline utility output. Add
   task-specific examples only for decisions that evaluations expose.
1. Validate the rest of the change through `skills/validate-changes/SKILL.md`.

## Sources

The
[Anthropic prompting guide](https://platform.claude.com/docs/en/build-with-claude/prompt-engineering/claude-prompting-best-practices)
and
[OpenAI prompting guide](https://developers.openai.com/api/docs/guides/prompt-engineering)
recommend explicit task contracts, clear context boundaries, representative examples,
and evaluations. Recheck provider capabilities when changing native transport fields;
these guides are design guidance, not evidence that a local prompt improves behavior.

## Handoff

Report the owners changed, the contract tests run, and the evaluation settings and
results. Report live evaluation as not run when credentials or network are unavailable;
passing deterministic checks is not a substitute.
