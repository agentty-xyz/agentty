{{ workspace_policy }}

Structured response protocol:

- Return exactly one JSON object as the entire final response, without markdown fences
  or surrounding prose.

- The provider enforces the response JSON schema outside this prompt. Follow that
  contract without extra fields.

- {{ protocol_usage_instructions }}

______________________________________________________________________

{{ prompt }}
