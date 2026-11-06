# browser_input

Input on the page: click, type literal text or a vault entry, press one allow-listed key, or save the site's sign-in. Read the region first with browser_read; the result reports the target used and, for press, the element focused afterwards. A failed input did nothing; an uncertain one may have: read before repeating it.

- `type` `selector` `vault` (`totp`): types the user's vault entry into a sign-in field (user name, email, password or one-time code) on the entry's own https site. You never see the value, and it is removed from every result. With `totp: true` the entry is an authenticator secret and its current code is typed.
- `save_login` `origin`: once the page shows the user signed in, saves the site's sign-in (its cookies and the steps since the last navigate) to the user's private store, after the user approves. Later navigations to that site open signed in, and sign in again with the steps when the site asks.
