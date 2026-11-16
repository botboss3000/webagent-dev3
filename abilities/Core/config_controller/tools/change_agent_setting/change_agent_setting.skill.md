# change_agent_setting

## Usage

- Save one setting of an agent the signed-in user manages (this conversation's agent unless `agent` names another one they manage), using the revision find_setting returned for the agent layer.
- The agent's owner checks apply.

Takes `setting`, the full `value` (for a width `{"width": 40, "unit": "%"}`), `params` when the setting has them (for example `{"tier": "free"}`) and the `revision` exactly as `find_setting` returned it. A conflict means the setting changed meanwhile: call `find_setting` again and show the new current value. A refusal for permission means that layer isn't open to this user: say so rather than trying another layer.
