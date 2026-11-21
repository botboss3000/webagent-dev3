---
id: configuration_settings
title: Configuration files and setting descriptors
when_to_use: Use whenever a change creates, extends, renames or removes a configuration value — a live JSON configuration file (listed in live-files.json), a key in one, an agent setting stored in agents.metadata, or a per-user setting — so the Configuration Controller can find and change it.
default_mode: on_request
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

The descriptors also draw the **All Settings** pages (App → All Settings, and an agent's All
Settings tab; `ui/shared/config-rows/settings-page.js`, fed by `GET /api/v3/config/settings`).
Each setting gets one row picked by its `type`: `boolean` a switch, `enum` a dropdown,
`integer`/`number` a stepper bounded by `min`/`max` with `unit` shown, `length` a number with
px/%, `string` a text box. The row title comes from the id's last part, so name ids plainly,
and the description is the row's hint. `object` settings and settings without a save path are
listed as not changeable there.

## Standing rules

Keep every live config file under `config/<module>/` (the owning module's name), with no
seeded copies; the code reads it by its `config/...` path.
Move hard-coded tunables into JSON config rather than adding constants. Ship the
setting descriptor in the same patch as the value.

## Where things live

- One live file per owner in `config/<module>/` (for example `config/engine/loop-runtime.json`),
  listed in `live-files.json`. No seeded copies. UI settings go in `config/ui/` (served
  at `/config/ui/<path>`); page folders keep their own `page.json` and page data.
- Ability JSON and Markdown (`tool.json`, `plugin.json`, `_group.json`, skills) stay in
  `abilities/` and are live too (listed in `live-files.json`, so the broker commits their
  edits): the app lays the checkout's versions over the published release within a second
  (`webagent_abilities::live`), including permission fields, argument schemas and deleted
  skill files; every reader of tool definitions follows. An edit that fails the release
  check (`ability_problems` in `src/app.rs`: the catalog loads, schemas keep the argument
  check's rules and what the tool's code receives) is ignored and the last good version
  stays. The App Default Save runs the same check before writing, so it refuses instead.
  Ability icons (`<ability>.svg`, `_group.svg`) are live too; the catalog ignores one that
  fails `icons::svg_problem`. Compiled plugin code still changes only through a release.
- Its descriptors sit in a `<name>.settings.json` beside it (for example
  `config/chat/chat.settings.json`). Owners whose module is busy may keep them in
  `config/configuration/settings/`.
- `config/configuration/settings-catalog.json` lists every owner:
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
  `parameters` (`{"tier": ["administrator", "free", "anonymous"]}`).
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

Chat settings (`config/ui/chat/chat.json`) take a `surface` parameter
(`common`, `desktop`, `mobile`, `widget`) with `"pointer": "/chat_{surface}/..."` and
`"inherit_pointer": "/chat_common/..."`: a surface without its own value shows Common's, and
saving creates it. The matcher reads the surface from the request ("on my phone" → mobile) and
otherwise uses Common. On an agent's page they save into that agent's chat overlay; `reset`
removes an agent override, or an app surface value so Common's applies again.

App → All Settings also has an Advanced section listing every live file under `config/`
(JSON and Markdown) for administrators: `GET/PUT /api/v3/admin/config-files/{path}` with a
digest fence, the owner's validator where one exists (administrator configuration ids, browser
pages, provider schema) and otherwise a JSON-object check. Give a new config file a validator
there when a malformed value could break its owner.

If a setting has wording its type does not give ("close registration" = `restricted`, "by
accident" = smaller), add it to `config/inference/value-words.json` under the id.

## Checklist for a configuration change

1. Change the live file (and its validator) as usual.
2. Add, update or remove the descriptor in the owner's `*.settings.json`; register a new owner
   in `settings-catalog.json`, or exclude a non-setting file with a reason.
3. New live file: add its id, live path and validator to the configuration host API.
4. Run the configuration crate tests (`cargo test -p webagent-configuration`): they validate
   every descriptor against the live file and fail on undescribed values.
5. Optionally check the match: `webagent-v3 --config-match-eval` in the deployed container scores the
   request sets in `crates/modules/inference/eval/`; add a request there for a new setting.
