# Four design directions

The same two screens — an admin operations overview and a participant discover
page — drawn in four deliberately different visual languages. Identical content
and information hierarchy; different voice.

Open **[index.html](index.html)** in a browser to compare them side by side, or
read the renders below, which GitHub can display.

| | Direction | Character | Shape | Accent |
| --- | --- | --- | --- | --- |
| 01 | [Editorial](#01--editorial) | serif display, hairline rules, ruled tables | square | vermillion |
| 02 | [Neo-brutalist](#02--neo-brutalist) | monopace capitals, hard shadows | none | acid + electric |
| 03 | [Aurora glass](#03--aurora-glass) | frosted panels over a colour field | 18 px | violet → cyan |
| 04 | [Warm organic](#04--warm-organic) | sand paper, soft shadows, humanist type | 28 px | clay + sage |

Each directory is self-contained: one `style.css`, two pages, one `renders/`
folder. No framework, no network request, no shared base — the styles share
nothing on purpose, so nothing constrains them toward a common average.

---

## 01 · Editorial

Swiss typographic. A serif display face against a grotesque body, hairline rules,
an asymmetric grid, one vermillion accent. Reads like a considered report rather
than a dashboard — dense in information, generous in whitespace.

**Admin**

![Editorial admin](01-editorial/renders/admin.png)

**Participant**

![Editorial participant](01-editorial/renders/participant.png)

Directory: [`01-editorial/`](01-editorial/) · [admin.html](01-editorial/admin.html) · [participant.html](01-editorial/participant.html)

---

## 02 · Neo-brutalist

Thick black rules, hard offset shadows, acid yellow and electric blue. Monospace
headings in capitals, zero border radius. Loud on purpose, and legible at a
glance — the opposite of subtle.

**Admin**

![Brutalist admin](02-brutalist/renders/admin.png)

**Participant**

![Brutalist participant](02-brutalist/renders/participant.png)

Directory: [`02-brutalist/`](02-brutalist/) · [admin.html](02-brutalist/admin.html) · [participant.html](02-brutalist/participant.html)

---

## 03 · Aurora glass

Deep navy with slow colour fields behind frosted panels. Violet-to-cyan
gradients, hairline borders, glow instead of shadow. Data feels lit from within;
the only direction here that is dark by default.

**Admin**

![Aurora admin](03-aurora/renders/admin.png)

**Participant**

![Aurora participant](03-aurora/renders/participant.png)

Directory: [`03-aurora/`](03-aurora/) · [admin.html](03-aurora/admin.html) · [participant.html](03-aurora/participant.html)

---

## 04 · Warm organic

Sand paper, terracotta and sage, very large radii, shadows instead of borders.
Humanist type with generous leading. Soft where the others are sharp — the
direction that reads most like a consumer product and least like an admin tool.

**Selected.** This is the only direction taken to completion — every screen in
both applications, plus the two auth screens.

### Admin

**[Operations overview](04-organic/admin-01-operations.html)**

![Organic operations](04-organic/renders/admin-01-operations.png)

**[Geodata review](04-organic/admin-02-geodata-review.html)**

![Organic geodata review](04-organic/renders/admin-02-geodata-review.png)

**[Import run detail](04-organic/admin-03-import-run.html)**

![Organic import run](04-organic/renders/admin-03-import-run.png)

**[Programmes and awards](04-organic/admin-04-programmes-awards.html)**

![Organic programmes](04-organic/renders/admin-04-programmes-awards.png)

**[Admins](04-organic/admin-05-admins.html)** — `identity.admins.manage`

![Organic admins](04-organic/renders/admin-05-admins.png)

**[Participants](04-organic/admin-06-participants.html)** — `identity.participants.manage`

![Organic participants](04-organic/renders/admin-06-participants.png)

User administration is **two sections, not one**. Managing an admin grants
authority and is a privilege-escalation surface — it needs a second factor and
re-authentication, and cannot grant a scope the actor does not hold. Managing a
participant is support work at volume: callsign evidence, registration state,
reachability. A single "Users" screen would give one action surface to two
activities that fail differently.

### Participant

**[Discover](04-organic/participant-01-discover.html)**

![Organic discover](04-organic/renders/participant-01-discover.png)

**[Map](04-organic/participant-02-map.html)**

![Organic map](04-organic/renders/participant-02-map.png)

**[Live activation](04-organic/participant-03-activation.html)**

![Organic activation](04-organic/renders/participant-03-activation.png)

**[Awards](04-organic/participant-04-awards.html)**

![Organic awards](04-organic/renders/participant-04-awards.png)

**[Check your email](04-organic/participant-05-check-email.html)**

![Organic check email](04-organic/renders/participant-05-check-email.png)

**[Set a new password](04-organic/participant-06-reset-password.html)**

![Organic reset password](04-organic/renders/participant-06-reset-password.png)

### On a phone

The responsive behaviour is visible rather than claimed — these are the three
screens where a narrow layout is hardest to get right:

<p align="center">
  <img src="04-organic/renders/participant-02-map-mobile.png" alt="Organic map on a phone" width="270">
  <img src="04-organic/renders/participant-03-activation-mobile.png" alt="Organic activation on a phone" width="270">
  <img src="04-organic/renders/admin-02-geodata-review-mobile.png" alt="Organic review on a phone" width="270">
</p>

Directory: [`04-organic/`](04-organic/) — twelve pages, one stylesheet, one
`renders/` folder.

---

## What is shared, and what is not

**Not shared:** colour, type, spacing scale, shadow language, border treatment,
header pattern, table style, status indicator style. Each style defines its own
custom properties in its own `style.css`; there is no common base file to drift
toward a compromise.

**Shared:** the content. Same six import runs, same four places, same award
progress, same numbers. That is what makes the comparison meaningful — you are
looking at four design decisions applied to one problem, not four different
products.

All four are responsive at 390, 768 and 1440 px, and all four honour
`prefers-reduced-motion`.

## Choosing one

The question is not which looks best in isolation. It is what the product should
feel like to the two audiences:

- **Editorial** suits a programme that wants to read as authoritative — published
  rules, curated places, an organisation with a point of view.
- **Neo-brutalist** suits a tool its users open daily and want to scan in two
  seconds. It trades elegance for immediacy.
- **Aurora glass** suits long evening sessions — a live activation at dusk, or an
  operator watching an import overnight. It is also the most expensive to keep
  consistent as screens multiply.
- **Warm organic** suits a participant-first product where the hobby is the point
  and the software should feel like an invitation, not an instrument.

Chosen: **warm organic**, because the platform's purpose is to make obscure
places feel worth visiting, and the tool should feel like an invitation rather
than an instrument. It is also the only one taken to completion — all ten screens,
both colour schemes, three viewports.

## Regenerating

```sh
node tools/render_mockups.mjs          # all 21 renders, including these
node tools/render_mockups.mjs --check  # fails if any render is stale
```

The gallery image at [`renders/index.png`](renders/index.png) is itself one of the
renders, so it is regenerated last, after the thumbnails it embeds exist.
