# Sign the browser in as a test account

`test_login` with `tier` (an account tier id from the access policy, such as `free`, a paid
tier, or `anonymous`) signs this chat's Browser Control browser in to the app as that tier's
test account. Then use `browser_read` / `browser_input` on the app (`navigate` to
`https://dev.webagent.live/app` or one of its routes) as you would as that person.

- Administrators only, in your own chat. `administrator` is never a test tier.
- Each signed-in tier has one fixed test account (`test-<tier>`); `anonymous` gets a fresh
  test visitor each call. The result names the account (`signed_in_as`, `user_id`) and when
  the sign-in ends (`expires_at`). You never see a password, cookie or token: there is none
  to see, and the sign-in goes straight into the browser.
- Inside the browser you are that account: its own chats, messages, settings and agents, with
  its tier's limits. Administrator pages, other accounts' chats and other people's data are
  refused by the app, exactly as for a real person of that tier. A refusal there is the
  expected result of such a check, not a tool failure.
- Never send feedback (the app's Send feedback control or `POST /api/v3/feedback`) while
  signed in as a test account: it files a real issue in the project's feedback repository.
- The test sign-in ends when this turn ends, when the browser navigates away from the app, or
  after the configured time (`config/accounts/test-accounts.json`, `sign_in_minutes`),
  whichever is first. Call `test_login` again in a later turn to continue; a repeat call
  replaces the current test sign-in.
- Test accounts cannot be made administrators, cannot manage other accounts' agents and never
  share a chat with a real account. Their runs count against the app's limits and are capped
  (`max_active_runs`); their chats are deleted after `session_retention_days`; their usage
  is left out of the administrator usage totals.

Errors: `denied` (not an administrator), `invalid_arguments` (missing or extra arguments),
`test_login_unavailable` with the reason: unknown tier (the message lists the tiers), the
administrator tier, not during a turn of your own chat, or the tier's reserved name already
belongs to another account. Nothing was signed in after an error.
