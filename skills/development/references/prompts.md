# Prompt Changes and Evaluation

Keep policy, task instructions, user steering, and evidence distinct. JSON-encode or use
collision-safe fences for tool output, history, and model-authored reports. Prefer
native additive developer/system instructions when supported; preserve provider coding
defaults and enforce permissions in the runtime. Personality affects presentation, not
authority.

## Ownership

- `crates/ag-protocol/src/template/`: common policy and response contracts. Schemas and
  examples must match each request profile; avoid copying schemas into application
  tasks.
- `crates/ag-agent/src/agent/template/`: replay and presentation. Bootstrap fingerprints
  must cover durable policy; test legacy state, changed contracts, and lost context.
- `crates/agentty/src/app/template/`: utility and workflow tasks. Request only fields
  the consumer needs, and distinguish requested work from observed changes and checks.
- `crates/ag-orchestration/src/template/`: controller and child tasks. Instructions must
  match the tools and permissions actually supplied. Treat child reports as evidence.
- `crates/ag-session/src/template/`: review application instructions.
- `AGENTS.md` and `skills/`: durable contributor rules and focused workflows. Avoid
  duplicated inventories, forced findings, or unconditional documentation fetches when
  suitable current evidence is already available.

## Validation Layers

Use the required gates in `AGENTS.md`. Unit and integration tests should verify schema
selection, native instruction delivery, evidence round trips, bootstrap invalidation,
permission boundaries, and preservation of user-owned content. Line coverage and prompt
string assertions establish these contracts; they do not establish model quality.

Behavioral cases in `crates/agentty/tests/prompt_evaluation.rs` exercise the production
Run Worker → Agent Runtime → Harness → LLM path. Deterministic fixtures test graders and
transport boundaries; live cases are opt-in and fail visibly on missing infrastructure.
Use `prek run prompt-evaluation --all-files --hook-stage manual` with explicit provider,
model, reasoning level, and repetition count as documented in that test. Record prompt
fingerprint, provider/model/effort, case/repetition, success, unsupported claims, false
findings, unnecessary questions, protocol repairs, tool activity, tokens, and latency.
Unavailable telemetry must remain unknown rather than being reported as zero.

Compare a baseline and candidate on identical cases and settings with repeated runs.
Keep artifacts and review failures before claiming an improvement. Include adversarial
reports and diagnostics, resumed policy changes, unavailable research capabilities,
status steering, objective replacement, review omissions in unchanged code, valid versus
invalidated checks, and quoted/multiline utility output. Report live evaluation as not
run when credentials or network are unavailable; passing deterministic checks is not a
substitute. Add task-specific examples only for decisions that evaluations expose.

## Sources

The
[Anthropic prompting guide](https://platform.claude.com/docs/en/build-with-claude/prompt-engineering/claude-prompting-best-practices)
and
[OpenAI prompting guide](https://developers.openai.com/api/docs/guides/prompt-engineering)
recommend explicit task contracts, clear context boundaries, representative examples,
and evaluations. Recheck provider capabilities when changing native transport fields;
these guides are design guidance, not evidence that a local prompt improves behavior.
