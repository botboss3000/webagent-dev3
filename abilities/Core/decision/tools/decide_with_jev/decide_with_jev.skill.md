# decide_with_jev

Argument: request_json, a JSON string with state and named questions.
Supported question types: choice, score, noul. The assigned Decision model
determines the upstream model and credential. A call does not grant access
to files or other tools. Use the returned answers to guide subsequent
authorized tool calls, and verify important results against source evidence.

Follow TypeSafe's System One schema. choice uses named criteria options, score uses ordered rubric levels starting at zero, noul asks for the probability of a yes. For file triage, use a choice question to pick what to inspect first or one score question per candidate. An optional model field must match the Decision role's model.
