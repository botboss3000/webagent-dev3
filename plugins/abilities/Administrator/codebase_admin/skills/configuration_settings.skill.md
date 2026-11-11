---
id: configuration_settings
title: Configuration files and setting descriptors
when_to_use: Use whenever a change creates, extends, renames or removes a configuration value — a live JSON configuration file (listed in live-files.json), a key in one, an agent setting stored in agents.metadata, or a per-user setting — so the Configuration Controller can find and change it.
default_mode: discoverable
examples: add a new setting; change a key in a config file; rename an agent setting
keywords: setting, configuration, config file, descriptor
minimum_access: registered
---

# Configuration files and setting descriptors

The Configuration Controller ability (`find_setting` and the `change_user_setting`, `change_agent_setting` and `change_app_setting` tools) and the composer API
(`POST /api/v3/config/propose`, `/apply`) only know settings that have a **descriptor**. Every
configuration change therefore ships its descriptor change in the same patch. The catalog tests
in `crates/modules/configuration/src/catalog.rs` fail when a live file or one of its values is
neither described nor excluded with a reason.

## Where things live

- One live file per owner, next to its code, listed in `live-files.json`. No seeded copies.
- Its descriptors sit in a `<name>.settings.json` beside it (for example
  `crates/modules/chat/chat.settings.json`). Owners whose module is busy may keep them in
  `crates/modules/configuration/settings/`.
- `crates/modules/configuration/settings-catalog.json` lists every owner:
  `{"config": "<live file>", "descriptors": "<settings file>"}`, or `{"store":
  "agent_metadata"|"user_profile", "descriptors": ...}`. Files that are not settings (prompts,
  generated data) go in its `excluded_files` with a reason.
- A save path: the composer writes app-layer values through the administrator configuration save
  (`crates/modules/configuration/host/api.rs`: `IDS`, `live()`, `validate()`). A new live file
  needs an id there and its owner's validator, or the change tools answer that it can't change
  the setting yet. Agent settings save through the agent identity update
  (`IDENTITY_SETTINGS` in `crates/modules/agents/host/api.rs`).

## Writing a descriptor

```json
{
  "id": "chat.limits.attachments.max_file_size_mb",
  "description": "Largest file, in megabytes, that can be attached to a chat message.",
  "type": "number",
  "pointer": "/chat_limits/attachments/max_file_size_mb",
  "layers": ["app"],
  "examples": ["bigger attachments", "allow 50 MB uploads", "files up to 20 megabytes"],
  "areas": ["chat", "attachments", "limits"],
  "unit": "MB",
  "min": 1,
  "max": 100,
  "bounds": "validator"
}
```

- `id`: dotted, stable, owner first. Renaming an id breaks recorded proposals; avoid it.
- `description`: one plain sentence a user would recognise, naming the unit. It is shown in
  pick lists to tell similar settings apart, so say what makes this one different
  ("floating chat window" vs "docked chat panel").
- `type`: `boolean`, `integer`, `number`, `enum` (with `enum` values), `string`, `length`
  (`{"width", "unit": "px"|"%"}`), `object` (with `value_shape`) or `agent_id`.
- `pointer`: JSON pointer into the live file (or store). `{name}` placeholders take values from
  `parameters` (`{"tier": ["operator", "basic", "anonymous"]}`).
- `layers`: where it can be set — `app` (the live file, administrators), `agent` (that agent's
  override; add `agent_pointer` into the identity view), `user` (personal). A file setting always
  includes `app`.
- `examples`: 3–5 short requests in everyday words, including one without the setting's name.
  These drive the matcher; write how users talk, not how the code is named.
- `areas`: 2–4 topic words. `unit`, `min`, `max` (or `recommended_max`, `max_length`) give the
  bounds the composer checks before saving; `bounds` says where they come from (`validator`
  when the owner's code enforces them, `proposed` otherwise). `notes` for anything else.
- `nullable: true` when the live file stores `null` for "no limit"; the composer still sets
  numbers only, so say in `notes` how to make it unlimited.
- Descriptors hold no default or current values; those are always read live.

If a setting has wording its type does not give ("close registration" = `restricted`, "by
accident" = smaller), add it to `crates/modules/composer/value-words.json` under the id.

## Checklist for a configuration change

1. Change the live file (and its validator) as usual.
2. Add, update or remove the descriptor in the owner's `*.settings.json`; register a new owner
   in `settings-catalog.json`, or exclude a non-setting file with a reason.
3. New live file: add its id, live path and validator to the configuration host API.
4. Run the configuration crate tests (`cargo test -p webagent-configuration`): they validate
   every descriptor against the live file and fail on undescribed values.
5. Optionally check the match: `webagent-v3 --composer-eval` in the deployed container scores the
   request sets in `crates/modules/composer/eval/`; add a request there for a new setting.
