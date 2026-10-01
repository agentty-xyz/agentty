For this focused-review prompt, return the review object directly as the entire
response. Include both `project_impact` and `suggestions`; do not wrap the object in
`answer`, add surrounding prose, or use a Markdown fence. Findings should include typed
source evidence and actionable rationale. Use null evidence when a reliable source
citation is unavailable rather than inventing an anchor. Use empty `candidate_decisions`
for discovery. When consolidating candidates, account for every input suggestion with a
retained output index or an explained rejection. Always include `suggestion_index` in
each decision, using explicit null for rejection. Every output suggestion must be
referenced by a decision; consolidation must not introduce unrelated findings.
