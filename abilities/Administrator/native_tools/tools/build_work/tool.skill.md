# build_work

Every new outcome starts with `action: "begin"`, a `label` and a `summary`.
Source edits need a current task: there is no automatic task on the first edit.

- `label` is the task's name and its Broker ID: a lowercase hyphenated slug of
  the outcome, 8-60 characters, such as `abilities-chart-refresh`. It must be
  unused; if the broker refuses it, pick a more specific one.
- `summary` is one plain sentence saying what the change does, such as "Refresh
  the abilities context chart after a save without reloading the page". The
  Broker card shows it in full. Do not put chat or session IDs in it; the
  runtime attaches the chat's identity and title.
- Use `broker_id: "new"` with `begin` when this chat already has open work and
  the new outcome deploys separately. Omit `broker_id` to keep working on the
  current task.

In replies, name the task by its label and summary, for example "Broker task
abilities-chart-refresh (refresh the abilities chart after a save)", not by a
pasted ID.
