#!/usr/bin/env python3
"""Generate the architecture SVGs in docs/architecture/.

Diagrams are generated rather than hand-drawn so that layout stays consistent
and a change to the model is a change to data, not to a pile of coordinates.

    python3 tools/render_diagrams.py

Outputs:
    docs/architecture/system-context.svg
    docs/architecture/erd.svg
"""

from __future__ import annotations

import html
import pathlib

ROOT = pathlib.Path(__file__).resolve().parent.parent
OUT = ROOT / "docs" / "architecture"

FONT = "ui-sans-serif, system-ui, -apple-system, Segoe UI, Roboto, sans-serif"
MONO = "ui-monospace, SFMono-Regular, Menlo, Consolas, monospace"

# Light palette — diagrams are intended for a white page or a light viewer.
INK = "#17201b"
MUTED = "#5c6b62"
FAINT = "#8a978f"
BORDER = "#c7cec8"
SURFACE = "#ffffff"
PAGE = "#f6f7f5"

ACCENT = "#2f6f4e"
ACCENT_SOFT = "#e6f1ea"
GEO = "#1f5d8f"
GEO_SOFT = "#e5eff8"
ACT = "#9a6208"
ACT_SOFT = "#fdf1dc"
CORE_LINE = "#8fb9a1"


def esc(text: str) -> str:
    return html.escape(text, quote=True)


def text(x, y, s, size=12, weight=400, fill=INK, anchor="start", family=None,
         letter=None, opacity=None):
    parts = [
        f'x="{x}"', f'y="{y}"', f'font-size="{size}"', f'font-weight="{weight}"',
        f'fill="{fill}"', f'text-anchor="{anchor}"',
        f'font-family="{family or FONT}"',
    ]
    if letter is not None:
        parts.append(f'letter-spacing="{letter}"')
    if opacity is not None:
        parts.append(f'opacity="{opacity}"')
    return f'  <text {" ".join(parts)}>{esc(s)}</text>'


# --------------------------------------------------------------------------
# System context
# --------------------------------------------------------------------------

def system_context() -> str:
    W, H = 1180, 806
    o: list[str] = []

    o.append(f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W} {H}" '
             f'width="{W}" height="{H}" role="img" aria-labelledby="ctxTitle ctxDesc" '
             f'font-family="{FONT}">')
    o.append('<title id="ctxTitle">MyOTA target system context</title>')
    o.append('<desc id="ctxDesc">Two browser applications reach one API gateway over '
             'HTTPS. The gateway serves the composed OpenAPI contract and the playground, '
             'and routes to three domain services. Each service owns exactly one PostgreSQL '
             'database. Services exchange events through their own outbox tables, read '
             'by peers over an internal feed; there is no message broker.</desc>')

    o.append(f'''<defs>
    <marker id="arw" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="6.5"
            markerHeight="6.5" orient="auto-start-reverse">
      <path d="M0 0 L10 5 L0 10 z" fill="{MUTED}"/>
    </marker>
    <marker id="arwA" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="6.5"
            markerHeight="6.5" orient="auto-start-reverse">
      <path d="M0 0 L10 5 L0 10 z" fill="{ACCENT}"/>
    </marker>
    <marker id="arwD" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="6.5"
            markerHeight="6.5" orient="auto-start-reverse">
      <path d="M0 0 L10 5 L0 10 z" fill="{FAINT}"/>
    </marker>
  </defs>''')

    o.append(f'  <rect width="{W}" height="{H}" fill="{PAGE}"/>')
    o.append(text(40, 44, "MyOTA — target system context", 19, 700))
    o.append(text(40, 65, "One gateway, three domain services, three databases. "
                          "No service reads another service's database.", 12.5, 400, MUTED))

    def label(x, y, s, colour):
        o.append(text(x, y, s.upper(), 10.5, 700, colour, letter="0.09em"))

    def box(x, y, w, h, fill=SURFACE, stroke=BORDER, sw=1.4, rx=10, dash=None):
        d = f' stroke-dasharray="{dash}"' if dash else ""
        o.append(f'  <rect x="{x}" y="{y}" width="{w}" height="{h}" rx="{rx}" '
                 f'fill="{fill}" stroke="{stroke}" stroke-width="{sw}"{d}/>')

    # ---- clients -----------------------------------------------------------
    label(40, 108, "Clients", MUTED)
    box(40, 118, 196, 66)
    o.append(text(58, 143, "Admin console", 13.5, 650))
    o.append(text(58, 162, "Separate login · MFA required", 11, 400, MUTED))
    o.append(text(58, 176, "audience=admin", 10.5, 500, FAINT, family=MONO))

    box(40, 200, 196, 66)
    o.append(text(58, 225, "Participant app", 13.5, 650))
    o.append(text(58, 244, "Separate login · mobile first", 11, 400, MUTED))
    o.append(text(58, 258, "audience=participant", 10.5, 500, FAINT, family=MONO))

    # ---- gateway -----------------------------------------------------------
    box(300, 118, 214, 148, ACCENT_SOFT, ACCENT, 1.8, 12)
    o.append(text(318, 145, "API gateway", 16, 750, ACCENT))
    for i, line in enumerate([
        "Routing · edge audience checks",
        "Holds the contract set",
        "Serves /docs playground",
        "CORS · request id · routing",
    ]):
        o.append(text(318, 167 + i * 19, line, 11.5, 400, "#2c4a3a"))
    o.append(text(318, 253, "stateless · limits live in Traefik", 10.5, 600, MUTED))

    # ---- services ----------------------------------------------------------
    label(560, 108, "Domain services", MUTED)

    services = [
        ("core", "Identity + Programmes", [
            "accounts · credentials · sessions",
            "callsigns · roles · MFA",
            "programmes · content · policy",
            "notifications (one ledger)",
        ], CORE_LINE),
        ("geodata", "Spatial catalogue", [
            "entities · geometry · tiles",
            "imports · adapters · snapshots",
            "conflation · review · provenance",
            "PostGIS in this database only",
        ], GEO),
        ("activity", "Activations + awards", [
            "activations · QSOs · ADIF",
            "award rules · issuance",
            "statistics · leaderboards",
            "no spatial columns",
        ], ACT),
    ]
    svc_y = [118, 258, 398]
    for (name, subtitle, lines, colour), y in zip(services, svc_y):
        box(560, y, 214, 134)
        o.append(f'  <rect x="560" y="{y}" width="4" height="134" rx="2" fill="{colour}"/>')
        o.append(text(578, y + 26, name, 15, 700))
        o.append(text(578, y + 45, subtitle, 11.5, 400, MUTED))
        for i, line in enumerate(lines):
            o.append(text(578, y + 63 + i * 14, line, 10.5, 400, FAINT))
        o.append(text(578, y + 124, "modes: api · worker · relay", 10, 650, colour))

    # ---- databases ---------------------------------------------------------
    label(830, 108, "Data", MUTED)
    dbs = [
        ("myota_core", "PostgreSQL 16 · owned by core", CORE_LINE),
        ("myota_geo", "PostgreSQL 16 + PostGIS · geodata", GEO),
        ("myota_activity", "PostgreSQL 16 · activity", ACT),
    ]
    for (name, note, colour), y in zip(dbs, svc_y):
        box(830, y + 24, 240, 80)
        o.append(f'  <rect x="830" y="{y + 24}" width="4" height="80" rx="2" fill="{colour}"/>')
        o.append(text(848, y + 52, name, 13.5, 700, family=MONO))
        o.append(text(848, y + 72, note, 10.5, 400, MUTED))
        o.append(text(848, y + 88, "one owner · no shared access", 10.5, 400, FAINT))

    # ---- infrastructure ----------------------------------------------------
    label(40, 592, "No broker · shared infrastructure only", MUTED)
    infra = [
        ("Event feed", ["outbox + cursors", "services pull"], 560),
        ("S3-compatible store", ["uploads go direct", "presigned URLs"], 758),
        ("PgBouncer", ["transaction mode", "one per database"], 956),
    ]
    for name, lines, x in infra:
        box(x, 596, 176, 74, "#fbfcfb", BORDER, 1.2, 9, dash="5 4")
        o.append(text(x + 14, 620, name, 12.5, 650))
        for i, line in enumerate(lines):
            o.append(text(x + 14, 638 + i * 15, line, 10.5, 400, FAINT))

    # SMTP sits under core
    box(560, 684, 176, 46, "#fbfcfb", BORDER, 1.2, 9, dash="5 4")
    o.append(text(574, 703, "SMTP relay", 12.5, 650))
    o.append(text(574, 719, "consumed by core only", 10.5, 400, FAINT))

    # ---- arrows ------------------------------------------------------------
    def arrow(d, colour=ACCENT, marker="arwA", w=1.8, dash=None):
        da = f' stroke-dasharray="{dash}"' if dash else ""
        o.append(f'  <path d="{d}" fill="none" stroke="{colour}" stroke-width="{w}" '
                 f'marker-end="url(#{marker})"{da}/>')

    # clients -> gateway
    arrow("M236 151 H296")
    arrow("M236 233 H296")

    # gateway -> services
    arrow(f"M514 168 H556", ACCENT, "arwA", 1.6)
    arrow(f"M514 196 C 534 196, 536 200, 556 202", ACCENT, "arwA", 1.6)
    arrow(f"M514 224 C 534 300, 536 350, 556 356", ACCENT, "arwA", 1.6)

    # services -> their database
    for y in svc_y:
        arrow(f"M774 {y + 64} H826", MUTED, "arw", 1.4)

    # services -> infrastructure (dashed)
    o.append(f'  <path d="M612 536 C 612 566, 610 584, 612 592" fill="none" '
             f'stroke="{FAINT}" stroke-width="1.3" stroke-dasharray="5 4" '
             f'marker-end="url(#arwD)"/>')
    o.append(f'  <path d="M706 536 C 736 566, 764 584, 794 592" fill="none" '
             f'stroke="{FAINT}" stroke-width="1.3" stroke-dasharray="5 4" '
             f'marker-end="url(#arwD)"/>')

    o.append(text(266, 144, "HTTPS", 9.5, 650, MUTED, anchor="middle"))
    o.append(text(266, 226, "HTTPS", 9.5, 650, MUTED, anchor="middle"))
    o.append(text(798, svc_y[0] + 58, "SQL", 9.5, 650, FAINT, anchor="end"))
    o.append(text(798, svc_y[1] + 58, "SQL", 9.5, 650, FAINT, anchor="end"))
    o.append(text(798, svc_y[2] + 58, "SQL", 9.5, 650, FAINT, anchor="end"))
    o.append(text(620, 570, "events (pull)", 9.5, 650, FAINT))

    # ---- boundary rules ----------------------------------------------------
    box(40, 750, 1100, 38, SURFACE, "#dfe3df", 1.1, 8)
    o.append(text(58, 774,
                  "One database per service · cross-service references are opaque ids, "
                  "never foreign keys · the outbox is the event log · every workload "
                  "runs at N replicas with no session affinity.",
                  11, 500, MUTED))

    o.append("</svg>")
    return "\n".join(o)


# --------------------------------------------------------------------------
# ERD
# --------------------------------------------------------------------------

# (name, [(line, kind), ...], colour) where kind: pk | fk | xfk | plain | muted
CORE_TABLES = [
    ("account", [("PK id", "pk"), ("email · citext", "plain"),
                 ("participation_type", "plain"), ("status", "plain")], ACCENT),
    ("account_credential", [("PK account_id", "pk"), ("password_hash", "plain"),
                            ("locked_until", "plain")], ACCENT),
    ("account_mfa", [("PK account_id", "pk"), ("method", "plain"),
                     ("confirmed_at", "plain")], ACCENT),
    ("auth_session", [("PK id", "pk"), ("FK account_id", "fk"),
                      ("audience · participant|admin", "plain")], ACCENT),
    ("callsign", [("PK id", "pk"), ("FK account_id", "fk"),
                  ("value · UNIQUE(acct)", "plain"), ("lifecycle_status", "plain")], ACCENT),
    ("account_primary_callsign", [("PK account_id", "pk"),
                                  ("FK callsign_id · UNIQUE", "fk")], ACCENT),
    ("role_definition", [("PK role_code", "pk"), ("scopes text[]", "plain")], ACCENT),
    ("account_role", [("PK id", "pk"), ("FK account_id · FK role_code", "fk"),
                      ("programme_id → ext", "xfk")], ACCENT),
    ("callsign_evidence", [("PK id", "pk"), ("FK callsign_id", "fk"),
                           ("status", "plain")], ACCENT),
    ("programme", [("PK id", "pk"), ("slug · UNIQUE", "plain"),
                   ("status", "plain")], ACCENT),
    ("programme_entity_type", [("FK programme_id", "fk"),
                               ("category_code", "plain")], ACCENT),
    ("programme_policy_version", [("FK programme_id", "fk"),
                                  ("version · rules jsonb", "plain")], ACCENT),
    ("notification", [("PK id", "pk"), ("FK account_id", "fk"),
                      ("status · attempts", "plain")], ACCENT),
    ("ops.outbox_event", [("shared by all three databases", "muted"),
                          ("idempotency · checkpoint", "muted"),
                          ("dead_letter · job", "muted")], FAINT),
]

GEO_TABLES = [
    ("entity", [("PK id", "pk"), ("programme_id → ext:core", "xfk"),
                ("geom · centroid · bbox", "plain"), ("status · revision", "plain")], GEO),
    ("entity_category", [("FK entity_id", "fk"), ("category_code", "plain")], GEO),
    ("entity_review", [("FK entity_id", "fk"), ("decision · reviewer", "plain")], GEO),
    ("entity_provenance", [("FK entity_id", "fk"), ("snapshot_id · confidence", "plain")], GEO),
    ("source", [("PK id", "pk"), ("adapter · licence", "plain")], GEO),
    ("source_snapshot", [("PK id", "pk"), ("FK source_id", "fk"),
                         ("checksum · object_key", "plain")], GEO),
    ("import_run", [("PK id", "pk"), ("stage · status", "plain"),
                    ("counts jsonb", "plain")], GEO),
    ("import_candidate", [("PK id", "pk"), ("FK run_id", "fk"),
                          ("state · geom", "plain")], GEO),
    ("conflation_candidate", [("PK id", "pk"), ("FK candidate_id", "fk"),
                              ("score · resolution", "plain")], GEO),
    ("import_schedule", [("PK id", "pk"), ("FK source_id", "fk"),
                         ("cron · enabled", "plain")], GEO),
]

ACT_TABLES = [
    ("activation", [("PK id", "pk"), ("account_id → ext:core", "xfk"),
                    ("entity_id → ext:geodata", "xfk"), ("status · points", "plain")], ACT),
    ("qso", [("PK id", "pk"), ("FK activation_id", "fk"),
             ("band · mode · worked_at", "plain")], ACT),
    ("log_import", [("PK id", "pk"), ("FK activation_id", "fk"),
                    ("format · accepted", "plain")], ACT),
    ("qso_correction", [("PK id", "pk"), ("FK qso_id", "fk"),
                        ("changes jsonb", "plain")], ACT),
    ("award_definition", [("PK id", "pk"), ("programme_id → ext:core", "xfk"),
                          ("rules jsonb · version", "plain")], ACT),
    ("award_request", [("PK id", "pk"), ("FK award_id", "fk"),
                       ("account_id → ext:core", "xfk")], ACT),
    ("award_progress", [("FK award_id · account_id", "fk"),
                        ("satisfied · progress", "plain")], ACT),
    ("award_issuance", [("PK id", "pk"), ("FK award_id", "fk"),
                        ("certificate_key", "plain")], ACT),
    ("statistic_snapshot", [("PK id", "pk"), ("scope · scope_id", "plain"),
                            ("payload jsonb", "plain")], ACT),
    ("leaderboard_entry", [("FK snapshot_id", "fk"), ("rank · value", "plain")], ACT),
]


def erd() -> str:
    CW, CH = 268, 74          # card
    GX, GY = 14, 26           # gaps
    COLS = 5
    PAD_X = 22

    bands = [
        ("myota_core", "owned by · core · PostgreSQL 16", CORE_TABLES, ACCENT, ACCENT_SOFT),
        ("myota_geo", "owned by · geodata · PostgreSQL 16 + PostGIS", GEO_TABLES, GEO, GEO_SOFT),
        ("myota_activity", "owned by · activity · PostgreSQL 16", ACT_TABLES, ACT, ACT_SOFT),
    ]

    W = PAD_X * 2 + COLS * CW + (COLS - 1) * GX + 2 * 12
    y = 92
    layout = []
    for name, subtitle, tables, colour, soft in bands:
        rows = (len(tables) + COLS - 1) // COLS
        h = 46 + rows * CH + (rows - 1) * GY + 16
        layout.append((name, subtitle, tables, colour, soft, y, h))
        y += h + 44

    H = y + 18

    o: list[str] = []
    o.append(f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W} {H}" '
             f'width="{W}" height="{H}" role="img" aria-labelledby="erdTitle erdDesc" '
             f'font-family="{FONT}">')
    o.append('<title id="erdTitle">MyOTA entity relationships</title>')
    o.append('<desc id="erdDesc">Three databases shown as bands. Each band lists the tables '
             'owned by one service, with primary and foreign keys. Cross-database references '
             'are opaque identifiers with no foreign key constraint.</desc>')
    o.append(f'''<defs>
    <marker id="fk" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="6"
            markerHeight="6" orient="auto-start-reverse">
      <path d="M0 0 L10 5 L0 10 z" fill="{MUTED}"/>
    </marker>
    <marker id="xfk" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="6"
            markerHeight="6" orient="auto-start-reverse">
      <path d="M0 0 L10 5 L0 10 z" fill="{ACT}"/>
    </marker>
  </defs>''')
    o.append(f'  <rect width="{W}" height="{H}" fill="{PAGE}"/>')
    o.append(text(PAD_X, 40, "MyOTA — entity relationships", 19, 700))
    o.append(text(PAD_X, 61,
                  "Each band is one database with exactly one owning service. "
                  "Amber identifiers cross databases as opaque ids — no foreign key, "
                  "no distributed transaction.", 12, 400, MUTED))

    for name, subtitle, tables, colour, soft, band_y, band_h in layout:
        o.append(f'  <rect x="{PAD_X - 8}" y="{band_y}" width="{W - 2 * (PAD_X - 8)}" '
                 f'height="{band_h}" rx="14" fill="{soft}" stroke="{colour}" '
                 f'stroke-width="1.5" stroke-dasharray="7 5"/>')
        o.append(text(PAD_X + 6, band_y + 26, name, 14.5, 750, colour, family=MONO))
        o.append(text(PAD_X + 6, band_y + 42, subtitle, 10.5, 500, MUTED))

        grid_top = band_y + 58
        for i, (tname, cols, _c) in enumerate(tables):
            r, c = divmod(i, COLS)
            x = PAD_X + c * (CW + GX)
            ty = grid_top + r * (CH + GY)
            o.append(f'  <rect x="{x}" y="{ty}" width="{CW}" height="{CH}" rx="8" '
                     f'fill="{SURFACE}" stroke="{BORDER}" stroke-width="1.2"/>')
            o.append(f'  <rect x="{x}" y="{ty}" width="{CW}" height="22" rx="8" '
                     f'fill="#f1f3f1"/>')
            o.append(f'  <rect x="{x}" y="{ty + 14}" width="{CW}" height="8" fill="#f1f3f1"/>')
            o.append(text(x + 10, ty + 16, tname, 11.5, 700, INK, family=MONO))
            for j, (line, kind) in enumerate(cols[:3]):
                style = {
                    "pk": (9.5, 700, ACCENT),
                    "fk": (9.5, 650, MUTED),
                    "xfk": (9.5, 650, ACT),
                    "plain": (9.5, 400, "#3f4d45"),
                    "muted": (9.5, 400, FAINT),
                }[kind]
                o.append(text(x + 10, ty + 36 + j * 13, line, *style, family=MONO
                              if kind in ("pk", "xfk") else None))

    # cross-database arrows in the gutters between bands
    core = layout[0]
    geo = layout[1]
    act = layout[2]

    def band_bottom(b):
        return b[5] + b[6]

    gutter1 = core[5] + core[6]
    gutter2 = geo[5] + geo[6]

    # core -> geo (programme id)
    o.append(f'  <path d="M{PAD_X + 60} {gutter1} V {gutter1 + 30}" fill="none" '
             f'stroke="{ACT}" stroke-width="1.4" stroke-dasharray="5 4" '
             f'marker-end="url(#xfk)"/>')
    o.append(text(PAD_X + 70, gutter1 + 20, "programme_id", 10, 600, ACT, family=MONO))

    # geo -> activity (entity id)
    o.append(f'  <path d="M{PAD_X + 300} {gutter2} V {gutter2 + 30}" fill="none" '
             f'stroke="{ACT}" stroke-width="1.4" stroke-dasharray="5 4" '
             f'marker-end="url(#xfk)"/>')
    o.append(text(PAD_X + 310, gutter2 + 20, "entity_id", 10, 600, ACT, family=MONO))

    o.append(text(PAD_X + 430, gutter1 + 20,
                  "account_id and programme_id also reach activity the same way",
                  10, 400, FAINT))
    o.append(text(PAD_X + 430, gutter2 + 20,
                  "an account closed in core publishes an event; activity and geodata detach",
                  10, 400, FAINT))

    o.append("</svg>")
    return "\n".join(o)


# --------------------------------------------------------------------------
# Scaling model
# --------------------------------------------------------------------------

def scaling() -> str:
    W, H = 1180, 690
    o: list[str] = []

    o.append(f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W} {H}" '
             f'width="{W}" height="{H}" role="img" aria-labelledby="scTitle scDesc" '
             f'font-family="{FONT}">')
    o.append('<title id="scTitle">MyOTA horizontal scaling model</title>')
    o.append('<desc id="scDesc">Requests pass through a single Traefik ingress that owns '
             'rate limiting, then to any of N gateway replicas, then to any of N API, '
             'worker or relay replicas. Replicas share no local state: sessions are in '
             'Postgres, work is claimed with SKIP LOCKED, the relay leases partitions, and '
             'connections pass through PgBouncer.</desc>')

    defs = (
        '<defs>\n'
        '    <marker id="sarw" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="6.5"\n'
        '            markerHeight="6.5" orient="auto-start-reverse">\n'
        f'      <path d="M0 0 L10 5 L0 10 z" fill="{MUTED}"/>\n'
        '    </marker>\n'
        '  </defs>'
    )
    o.append(defs)
    o.append(f'  <rect width="{W}" height="{H}" fill="{PAGE}"/>')
    o.append(text(40, 42, "MyOTA — horizontal scaling", 19, 700))
    o.append(text(40, 63, "Every tier is N replicas. No tier holds local state, so any "
                          "replica serves any request and losing one is invisible.",
                  12, 400, MUTED))

    def box(x, y, w, h, fill=SURFACE, stroke=BORDER, sw=1.3, rx=9, dash=None):
        d = f' stroke-dasharray="{dash}"' if dash else ""
        o.append(f'  <rect x="{x}" y="{y}" width="{w}" height="{h}" rx="{rx}" '
                 f'fill="{fill}" stroke="{stroke}" stroke-width="{sw}"{d}/>')

    def arrow(x1, y1, x2, y2):
        o.append(f'  <path d="M{x1} {y1} L{x2} {y2}" fill="none" stroke="{MUTED}" '
                 f'stroke-width="1.4" marker-end="url(#sarw)"/>')

    def tier(y, label, note=""):
        o.append(text(40, y + 15, label.upper(), 10.5, 700, MUTED, letter="0.09em"))
        if note:
            o.append(text(40, y + 30, note, 9.5, 400, FAINT))

    RX = [300, 470, 640]
    RW, RH = 150, 46

    tier(96, "Clients")
    box(300, 96, 320, 40)
    o.append(text(316, 122, "Admin console  ·  Participant app", 12, 550))

    tier(176, "Ingress", "one layer, global counters")
    box(300, 176, 320, 52, ACCENT_SOFT, ACCENT, 1.6)
    o.append(text(316, 198, "Traefik", 13, 700, ACCENT))
    o.append(text(316, 216, "rateLimit · circuitBreaker · retry · inFlightReq", 10, 400, "#2c4a3a"))

    tier(268, "Gateway", "stateless · contracts embedded")
    for i, x in enumerate(RX):
        box(x, 268, RW, RH)
        o.append(text(x + 12, 288, f"gateway-{i + 1}", 11.5, 650, family=MONO))
        o.append(text(x + 12, 304, "no state · no DB", 9.5, 400, FAINT))
    o.append(text(RX[2] + RW + 16, 296, "… N", 11, 600, FAINT))

    tier(360, "Services", "api · worker · relay from one image")
    groups = [("api", "sessions in Postgres", ACCENT),
              ("worker", "claims with SKIP LOCKED", ACT),
              ("relay", "leases partitions", GEO)]
    for (name, note, colour), x in zip(groups, RX):
        box(x, 360, RW, 84)
        o.append(f'  <rect x="{x}" y="360" width="4" height="84" rx="2" fill="{colour}"/>')
        o.append(text(x + 14, 382, f"{name} ×N", 12.5, 700, colour, family=MONO))
        o.append(text(x + 14, 400, note, 9.5, 400, MUTED))
        o.append(text(x + 14, 416, "kill any replica", 9.5, 400, FAINT))
        o.append(text(x + 14, 432, "→ lease expires", 9.5, 400, FAINT))
    o.append(text(RX[2] + RW + 16, 402, "… N", 11, 600, FAINT))

    tier(486, "Connection pool", "connections are the first wall")
    box(300, 486, 490, 52, "#fbfcfb", BORDER, 1.3, 9, dash="5 4")
    o.append(text(316, 508, "PgBouncer · transaction mode · one per database", 12, 650))
    o.append(text(316, 526, "20 replicas × 20 client connections → 25 server connections", 10, 400, MUTED))

    tier(580, "Data", "one owner per database")
    dbs = [("myota_core", ACCENT), ("myota_geo", GEO), ("myota_activity", ACT)]
    for (name, colour), x in zip(dbs, RX):
        box(x, 580, RW, 46)
        o.append(f'  <rect x="{x}" y="580" width="4" height="46" rx="2" fill="{colour}"/>')
        o.append(text(x + 14, 608, name, 12, 700, family=MONO))

    o.append(text(830, 96, "FRONT ENDS", 10.5, 700, MUTED, letter="0.09em"))
    box(830, 108, 310, 116)
    o.append(text(846, 132, "admin-web · participant-web ×N", 12.5, 700))
    for i, line in enumerate([
        "static file tree · nginx",
        "content-hashed assets, immutable cache",
        "no session · no database client",
        "scales on network throughput alone",
    ]):
        o.append(text(846, 154 + i * 17, line, 10, 400, MUTED))

    o.append(text(830, 268, "WHAT MAKES A TIER SAFE", 10.5, 700, MUTED, letter="0.09em"))
    box(830, 280, 310, 258, "#fbfcfb", BORDER, 1.2, 9, dash="5 4")
    for i, (k, v) in enumerate([
        ("Sessions", "Postgres, never memory"),
        ("Rate limits", "Traefik, never in-process"),
        ("Migrations", "pre-deploy Job + advisory lock"),
        ("Scheduled work", "ops.job UNIQUE (kind, bucket)"),
        ("Job claiming", "FOR UPDATE SKIP LOCKED"),
        ("Relay", "partition lease, not one leader"),
        ("Uploads", "presigned S3, off the API"),
        ("Real-time", "cursor polling, no WebSocket"),
        ("Affinity", "none, anywhere"),
    ]):
        o.append(text(846, 306 + i * 26, k, 10.5, 650))
        o.append(text(846, 320 + i * 26, v, 10, 400, MUTED))

    arrow(460, 136, 460, 172)
    for x in RX:
        arrow(460, 228, x + RW / 2, 264)
    for x in RX:
        arrow(x + RW / 2, 314, x + RW / 2, 356)
    for x in RX:
        arrow(x + RW / 2, 444, x + RW / 2, 482)
    for x in RX:
        arrow(x + RW / 2, 538, x + RW / 2, 576)

    o.append(f'  <path d="M620 136 C 700 136, 760 150, 826 156" fill="none" '
             f'stroke="{MUTED}" stroke-width="1.4" stroke-dasharray="5 4" '
             f'marker-end="url(#sarw)"/>')
    o.append(text(690, 132, "static assets", 9.5, 600, FAINT))

    o.append("</svg>")
    return "\n".join(o)


def main() -> None:
    OUT.mkdir(parents=True, exist_ok=True)
    for name, render in (("system-context.svg", system_context),
                         ("erd.svg", erd),
                         ("scaling.svg", scaling)):
        path = OUT / name
        path.write_text(render(), encoding="utf-8")
        print(f"wrote {path.relative_to(ROOT)} ({path.stat().st_size} bytes)")


if __name__ == "__main__":
    main()
