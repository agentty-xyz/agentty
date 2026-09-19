{{ workspace_policy }}

Structured response protocol:

- Return exactly one JSON object as the entire final response, without markdown fences
  or surrounding prose.

- Follow this JSON Schema exactly; its titles and descriptions are authoritative
  field-level instructions.

- {{ protocol_usage_instructions }}

Authoritative JSON Schema: {{ response_json_schema }}

______________________________________________________________________

{{ prompt }}
