// Render every UI mockup to a static PNG.
//
//   node tools/render_mockups.mjs            # writes docs/ui/renders/*.png
//   node tools/render_mockups.mjs --check    # fail if a render is stale
//
// Why this exists: the mockups are HTML, and github.com shows HTML as source.
// The PNGs are what a reader actually sees in the repository, so they are
// committed. This script is the only correct way to produce them — a screenshot
// taken by hand drifts the moment someone edits the CSS.
//
// Requires Playwright with a Chromium build. Set PLAYWRIGHT_CHROMIUM to an
// explicit executable if the bundled browser is not installed.

import { chromium } from 'playwright';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const UI = path.join(ROOT, 'docs', 'ui');
const OUT = path.join(UI, 'renders');
const CHECK = process.argv.includes('--check');

// Desktop is the canonical render. Mobile is included for two screens so the
// responsive behaviour is visible in the repository, not just claimed.
const SHOTS = [
  { file: 'admin-01-operations.html',      out: 'admin-01-operations',      w: 1440, h: 1000, full: true },
  { file: 'admin-02-geodata-review.html',  out: 'admin-02-geodata-review',  w: 1440, h: 1000, full: true },
  { file: 'admin-03-import-run.html',      out: 'admin-03-import-run',      w: 1440, h: 1000, full: true },
  { file: 'admin-04-programmes-awards.html', out: 'admin-04-programmes-awards', w: 1440, h: 1000, full: true },
  { file: 'participant-01-home.html',      out: 'participant-01-home',      w: 1440, h: 1000, full: true },
  { file: 'participant-02-map.html',       out: 'participant-02-map',       w: 1440, h: 1000, full: true },
  { file: 'participant-03-activation.html', out: 'participant-03-activation', w: 1440, h: 1000, full: true },
  { file: 'participant-04-awards.html',    out: 'participant-04-awards',    w: 1440, h: 1000, full: true },
  { file: 'admin-02-geodata-review.html',  out: 'admin-02-geodata-review-mobile', w: 390, h: 844, full: true },
  { file: 'participant-02-map.html',       out: 'participant-02-map-mobile',      w: 390, h: 844, full: true },
  // index.html embeds the renders above as thumbnails, so it must be last —
  // otherwise it captures whatever thumbnails existed from the previous run.
  { file: 'index.html',                    out: 'index',                    w: 1200, h: 900,  full: true },
];

const executablePath = process.env.PLAYWRIGHT_CHROMIUM || undefined;

fs.mkdirSync(OUT, { recursive: true });

const browser = await chromium.launch({
  executablePath,
  args: ['--no-sandbox', '--disable-setuid-sandbox'],
});
// reducedMotion matters for determinism, not just accessibility: the live
// activation screen animates a pulsing dot, so a screenshot taken at a random
// animation phase would differ on every run and the --check gate would never
// pass. The stylesheet already disables animation under this preference.
const context = await browser.newContext({
  colorScheme: 'light',
  deviceScaleFactor: 1,
  reducedMotion: 'reduce',
});
const page = await context.newPage();

let stale = [];
let written = 0;

for (const shot of SHOTS) {
  const src = path.join(UI, shot.file);
  const dest = path.join(OUT, `${shot.out}.png`);

  await page.setViewportSize({ width: shot.w, height: shot.h });
  await page.goto(`file://${src}`, { waitUntil: 'load' });
  // Fonts and layout settle within a frame; the mockups have no async content.
  await page.waitForTimeout(150);

  const tmp = path.join(OUT, `.${shot.out}.png`);
  await page.screenshot({ path: tmp, fullPage: shot.full });
  const next = fs.readFileSync(tmp);

  if (CHECK) {
    fs.unlinkSync(tmp);
    const prev = fs.existsSync(dest) ? fs.readFileSync(dest) : null;
    if (!prev) {
      stale.push(`${shot.out}.png — missing`);
    } else if (!prev.equals(next)) {
      stale.push(`${shot.out}.png — differs from a fresh render`);
    }
  } else {
    fs.renameSync(tmp, dest);
    const kb = Math.round(next.length / 1024);
    console.log(`  wrote ${path.relative(ROOT, dest)}  ${shot.w}x${shot.h}  ${kb} KB`);
    written++;
  }
}

await browser.close();

if (CHECK) {
  if (stale.length) {
    console.error('Mockup renders are stale:');
    for (const s of stale) console.error(`  - ${s}`);
    console.error('\nRun: node tools/render_mockups.mjs');
    process.exit(1);
  }
  console.log(`OK: ${SHOTS.length} mockup render(s) match a fresh render`);
} else {
  console.log(`\n${written} render(s) written to docs/ui/renders/`);
}
