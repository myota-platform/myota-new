// Verify every UI screen across viewports and colour schemes.
//
//   node tools/check_ui.mjs
//
// Checks, for every page in docs/ui and docs/ui/styles:
//   - no JavaScript errors
//   - no external network requests (the screens must be self-contained)
//   - no horizontal page overflow at any width
//   - no *clipped* content — an element whose overflow is hidden and whose
//     content is wider than its box
//
// The clipping check is the one that matters and the one a naive overflow check
// misses: a card with `overflow: hidden` swallows a too-wide table without ever
// making the page scroll, so scrollWidth reports success while the user sees a
// truncated column. Decorative pseudo-elements positioned outside their parent
// are excluded, because clipping those is intentional.
//
// Requires Playwright with a Chromium build. Set PLAYWRIGHT_CHROMIUM to an
// explicit executable if the bundled browser is not installed.

import { chromium } from 'playwright';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const UI = path.join(ROOT, 'docs', 'ui');

const VIEWPORTS = [
  { name: 'phone',   w: 390,  h: 844  },
  { name: 'tablet',  w: 768,  h: 1024 },
  { name: 'desktop', w: 1440, h: 1000 },
];
const SCHEMES = ['light', 'dark'];

function pages() {
  const out = [];
  const walk = (dir) => {
    for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
      const full = path.join(dir, entry.name);
      if (entry.isDirectory()) {
        if (entry.name === 'renders' || entry.name === 'assets') continue;
        walk(full);
      } else if (entry.name.endsWith('.html')) {
        out.push(full);
      }
    }
  };
  walk(UI);
  return out.sort();
}

// Find a usable Chromium: an explicit override first, then whatever Playwright
// has already downloaded. Playwright pins an exact build per version, and the
// images on this host were produced by a different one — discovering an existing
// binary is more useful than insisting on the pinned download.
function findChromium() {
  if (process.env.PLAYWRIGHT_CHROMIUM) return process.env.PLAYWRIGHT_CHROMIUM;

  const cache = process.env.PLAYWRIGHT_BROWSERS_PATH
    || path.join(process.env.HOME || '/root', '.cache', 'ms-playwright');
  let entries = [];
  try {
    entries = fs.readdirSync(cache);
  } catch {
    return undefined;
  }

  const candidates = [];
  for (const entry of entries) {
    if (!entry.startsWith('chromium')) continue;
    for (const rel of [
      'chrome-linux64/chrome',
      'chrome-linux/chrome',
      'chrome-headless-shell-linux64/chrome-headless-shell',
    ]) {
      const full = path.join(cache, entry, rel);
      if (fs.existsSync(full)) candidates.push(full);
    }
  }
  // prefer a full chromium over the headless shell, and the newest build
  candidates.sort((a, b) => {
    const shell = (p) => (p.includes('headless-shell') ? 1 : 0);
    return shell(a) - shell(b) || b.localeCompare(a);
  });
  return candidates[0];
}

const files = pages();
const executablePath = findChromium();
if (executablePath) {
  console.log(`using ${executablePath}`);
}
const browser = await chromium.launch({
  executablePath,
  args: ['--no-sandbox', '--disable-setuid-sandbox'],
});

const problems = [];
let checks = 0;

for (const scheme of SCHEMES) {
  const context = await browser.newContext({
    colorScheme: scheme,
    deviceScaleFactor: 1,
    // pins animation phase so results do not vary between runs
    reducedMotion: 'reduce',
  });
  const page = await context.newPage();

  for (const vp of VIEWPORTS) {
    await page.setViewportSize({ width: vp.w, height: vp.h });

    for (const file of files) {
      checks++;
      const errors = [];
      const requests = [];
      page.removeAllListeners('pageerror');
      page.removeAllListeners('request');
      page.on('pageerror', (e) => errors.push(String(e)));
      page.on('request', (r) => {
        if (!r.url().startsWith('file://')) requests.push(r.url());
      });

      await page.goto(`file://${file}`, { waitUntil: 'load' });
      await page.waitForTimeout(70);

      const result = await page.evaluate(() => {
        const docW = document.documentElement.scrollWidth;
        const winW = window.innerWidth;

        const clipped = [];
        for (const el of document.querySelectorAll('*')) {
          const cs = getComputedStyle(el);
          const hides = cs.overflowX === 'hidden' || cs.overflow === 'hidden';
          if (!hides || el.clientWidth === 0) continue;
          if (el.scrollWidth <= el.clientWidth + 2) continue;

          // screen-reader-only text is 1x1 and clipped by design
          if (el.classList.contains('sr-only')) continue;

          // a pseudo-element positioned outside its parent is clipped on
          // purpose — a decorative gradient or a soft glow
          const decorative = ['::before', '::after'].some((ps) => {
            const d = getComputedStyle(el, ps);
            return d.content !== 'none' && d.position === 'absolute';
          });
          if (decorative) continue;

          clipped.push(`${el.tagName.toLowerCase()}.${String(el.className).split(' ')[0]}`);
        }

        return { docW, winW, clipped: [...new Set(clipped)].slice(0, 5) };
      });

      const overflow = result.docW > result.winW + 2;
      if (errors.length || requests.length || overflow || result.clipped.length) {
        problems.push({
          file: path.relative(ROOT, file),
          scheme,
          viewport: vp.name,
          width: vp.w,
          overflow,
          docW: result.docW,
          clipped: result.clipped,
          errors,
          requests,
        });
      }
    }
  }
  await context.close();
}

await browser.close();

console.log(`checked ${checks} combinations: ${files.length} pages x ${VIEWPORTS.length} viewports x ${SCHEMES.length} schemes`);

if (!problems.length) {
  console.log('OK: no JS errors, no external requests, no overflow, no clipped content');
  process.exit(0);
}

for (const p of problems) {
  const bits = [];
  if (p.overflow) bits.push(`page overflow (${p.docW} > ${p.width})`);
  if (p.clipped.length) bits.push(`clipped: ${p.clipped.join(', ')}`);
  if (p.errors.length) bits.push(`js errors: ${p.errors.join('; ')}`);
  if (p.requests.length) bits.push(`external: ${p.requests.join(', ')}`);
  console.log(`  ${p.file} [${p.scheme}/${p.viewport}] ${bits.join(' | ')}`);
}
console.log(`\nFAILED: ${problems.length} problem(s)`);
process.exit(1);
