# ADR-0007 — The UI direction is 04‑Organic

**Status:** accepted · 2026-11-27
**Provenance:** directed — "I like the organic design best so far" and "record
`04-Organic` as the preferred UI style in `AGENTS.md`".

## Context

The interface had no stated visual language. The first mockups were a single
functional baseline — competent and interchangeable with any admin template,
which is the problem: when the design is nobody's decision, every screen
accretes styling by whoever touched it last, and the product stops looking like
one thing.

Two applications, two audiences, and one constraint that shaped the candidates:
the participant app is used in the field, on a phone, often at dusk, by people
who are thinking about a hill rather than about software.

## Decision

**`docs/ui/styles/04-organic/` is the preferred and only UI direction.** It lives
as rule 6 in [`AGENTS.md`](../../AGENTS.md), with the visual grammar written so it
can be checked rather than admired.

Four directions were built as two-screen samples before choosing:

| Direction | Character |
| --- | --- |
| 01 Editorial | serif display on a grotesque, hairline rules, asymmetric grid |
| 02 Neo-brutalist | monospace capitals, 4 px rules, hard offset shadows |
| 03 Aurora glass | deep navy, frosted panels, violet-to-cyan glow |
| **04 Warm organic** | sand paper, terracotta and sage, 28 px radii, soft shadows |

The chosen one was then taken to completion: every screen in both applications
plus the two auth screens, twelve pages, both colour schemes, three viewports.

## Rationale

The platform exists to make obscure places feel worth visiting. A tool that looks
like an instrument works against that; a tool that looks like an invitation works
with it. The organic direction is the only one of the four that reads as a
consumer product rather than as an admin console.

Three properties made it the defensible choice rather than just the prettiest:

- **It degrades to a phone honestly.** Large radii and generous spacing survive a
  narrow viewport; dense ruled tables and offset shadows do not.
- **It works at dusk.** The warm palette is legible at low brightness without the
  blue-black harshness of the aurora direction, and the dark variant keeps the
  same sand-and-clay character instead of becoming a different product.
- **Its restraint is checkable.** "No container borders, warm palette, radius
  hierarchy, generous leading" can be verified against a stylesheet. "Feels calm"
  cannot.

## Alternatives considered

**01 Editorial.** The strongest for authority — it reads like a published
catalogue, which suits a programme with rules and curated places. Rejected because
the density that makes it good for reports makes it tiring on a phone in the
field, and the participant app is the higher-traffic surface.

**02 Neo-brutalist.** The most scannable at a glance, genuinely good for an
operator who wants two seconds of information. Rejected because it is a voice for
tools, and half of this product is not a tool. It also ages fastest of the four.

**03 Aurora glass.** The most atmospheric; well suited to a live activation at
dusk or an operator watching an import overnight. Rejected as the *system-wide*
direction because frosted panels over moving colour are expensive to keep
consistent as screens multiply, and the dark default fights daytime field use.

**One system for both applications.** Rejected: they are separate deployables with
separate audiences and could reasonably differ. Kept as one because the product
should feel like one thing, and because two design systems is twice the
maintenance for a team this size.

**No stated direction.** Rejected: that is what produced the interchangeable
baseline. A design that nobody chose is a design that drifts.

## Consequences

**Gained**

- One visual language across twelve screens, with a stylesheet that is the
  contract rather than a component library to version.
- A grammar that can be checked: `tools/check_ui.mjs` enforces the responsive and
  self-contained properties, and the token discipline is visible in review.
- The other three directions remain in the repository as the record of what was
  considered, which is what an ADR's alternatives section is for.

**Cost**

- **The warm palette costs contrast margin.** Sand on white is a narrower range
  than a neutral grey scale, so text colours are constrained and the dark variant
  needed hand-tuning rather than an inversion.
- **It is opinionated.** A future contributor who prefers density will find the
  generous spacing wasteful; that is now a rule disagreement, not a taste
  disagreement, which is the point.
- **Three sample directions are dead weight** in the tree. Kept deliberately: a
  decision record without its alternatives is a press release.

## Verification

- `node tools/check_ui.mjs` — every page, three viewports, two colour schemes, no
  overflow and no clipped content.
- `node tools/render_mockups.mjs --check` — the committed images match a fresh
  render.
- Rule 6 in `AGENTS.md` states the grammar; a change that violates it is a change
  to this ADR.
