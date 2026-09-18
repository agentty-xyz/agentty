# Domain Layer

Pure Agentty-specific entities, policies, and display-neutral projections.

- Keep I/O, persistence rows, subprocesses, and terminal rendering out of this layer.
- Reuse selection and session models from `ag-session`, execution settings from
  `ag-contracts`, and CLI discovery metadata from `ag-session`; do not create competing
  domain types.
- Update `docs/site/content/docs/agents/backends.md` for visible provider/model changes
  and `docs/site/content/docs/usage/workflow.md` for session lifecycle or size changes.
