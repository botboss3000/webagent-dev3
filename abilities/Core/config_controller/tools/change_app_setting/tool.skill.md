# change_app_setting

Takes `setting`, the full `value` (for a width `{"width": 40, "unit": "%"}`), `params` when the setting has them (for example `{"tier": "basic"}`) and the `revision` exactly as `find_setting` returned it. A conflict means the setting changed meanwhile: call `find_setting` again and show the new current value. A refusal for permission means that layer isn't open to this user: say so rather than trying another layer.
