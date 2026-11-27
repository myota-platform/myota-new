# web/participant

The participants' interface, served at `myota-test.ea7klk.es`.

| | |
| --- | --- |
| Chunk | **P2** |
| Framework | Svelte 5 + Vite |
| Output | static tree, no server runtime |

## Current state

Empty. Chunk **P2** scaffolds the app: `package.json`, `vite.config`, the
routes and the design system.

There is deliberately no `package.json` yet. The CI workflow for this app checks
for one and reports "not scaffolded" rather than running `npm ci` against a
lockfile that does not exist — a red build for work that has not happened yet is
noise, and noise in a green build is how people stop reading logs.

## Design system

`04-organic`, from `docs/ui/styles/04-organic/`. Shared tokens go in
`libs/ui-tokens/tokens.css` and both front ends import them rather than copying the
palette; `tools/check_design_system.py` fails a build that redefines a token
locally. See `AGENTS.md` rule 7.
