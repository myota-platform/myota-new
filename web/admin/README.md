# web/admin

The administrators' interface, served at `myota-admin-test.ea7klk.es`.

| | |
| --- | --- |
| Chunk | **A3** (shell), **A4** (Admins) |
| Framework | Vue 3 + Vite |
| Output | a static tree in nginx, no server runtime |
| Design | `libs/ui-tokens/tokens.css` + `docs/ui/styles/04-organic/style.css`, imported — not copied |

## What exists

- Routes `/login`, `/mfa/enrol`, `/operations`, `/geodata`, `/programmes`, `/admins`,
  `/participants`, with a guard that sends an unauthenticated visitor to `/login` and
  back to where they were going afterwards.
- Sign-in through the real flow: password → challenge → session. A password alone never
  produces a session; the form says so.
- Second-factor enrolment: shows the secret, the `otpauth://` URI and ten recovery codes
  once, then confirms with a code.
- The **Admins** section: list, filter, read scopes, grant and revoke roles, MFA reset,
  session revocation — with re-authentication in the UI where the API requires it.

## The three rules the client keeps

1. **No token in storage.** The refresh credential is the `__Host-myota_admin_sid` cookie,
   which is `HttpOnly`; the access token lives in a module variable and dies with the tab.
   `localStorage` is readable by any injected script, and this console can grant authority.
2. **One refresh on a 401, then give up.** A loop turns an expired session into a request
   storm; a single retry after a successful refresh is the most a caller should do.
3. **Relative paths only.** The console and the API share an origin, so the bundle names
   no other host — asserted by loading the built app and recording every request.

## Checks

```sh
make -C web/admin build    # npm ci && vite build
make -C web/admin test     # vitest: the client's rules
make -C web/admin check    # build, Lighthouse accessibility ≥ 95, same-origin, axe
```

`check` needs a Chromium build. It uses `$CHROME_PATH`, defaulting to the Playwright
browser the repository's other UI checks already use.

## Why the design system is imported, not copied

`docs/ui/styles/04-organic/style.css` is what the mockups are drawn with, and
`libs/ui-tokens/tokens.css` is where the palette lives. The app imports both by relative
path, so a token change reaches the console and the mockups together. A copy is how two
screens stop looking like one product — which is what `tools/check_design_system.py` exists
to catch, and it now has something to check.

Contrast was measured, not eyeballed: white on `--clay` is 4.25:1, which fails the 4.5:1
floor at button sizes, so filled controls use `--clay-ink` (5.71:1). The label colour moved
from `--ink-3` (3.26:1) to `--ink-2` (6.27:1). `/login` scores 100.
