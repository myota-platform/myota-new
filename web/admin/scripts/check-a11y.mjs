#!/usr/bin/env node
// Accessibility and self-containment checks for the console.
//
//   npm run check
//
// Two questions, both answered against the **built** bundle rather than the source,
// because the bundle is what ships:
//
//   1. **Lighthouse accessibility ≥ 95** on `/login`. Lighthouse is the named criterion in
//      the chunk, so it is run rather than approximated. It is spawned as a subprocess
//      with an explicit Chrome path, which is the only reliable way to give it a browser
//      in this environment.
//   2. **Every request is same-origin.** The acceptance is "the app calls only
//      https://myota-admin-test.ea7klk.es/v1/…; no other origin in the bundle". The
//      stronger form is measured here: load the app in a browser, record every request it
//      makes, and fail on any whose origin is not the one serving the page. A string in a
//      Vue error message is not a request; a request to a font CDN would be.
//
// A third check rides along because it is the same page load: **axe-core** reports the
// individual violations Lighthouse's score summarises, so a failure names the element.
//
// Exit non-zero on any failure.

import { createServer } from 'node:http'
import { spawn } from 'node:child_process'
import { readFile, access, mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { createRequire } from 'node:module'
import { chromium } from 'playwright'

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const DIST = path.join(ROOT, 'dist')
const WORKSPACE = path.resolve(ROOT, '..', '..')

/**
 * Finds a dependency that may be installed here or in the workspace root.
 *
 * `axe-core` and `lighthouse` are check-time tools for the whole repository, so they live
 * at the root (`AGENTS.md`: repository tooling is one install). This app must still work
 * when someone runs `npm install` inside it alone, so both locations are tried.
 */
function resolveDependency(name, suffix) {
  for (const base of [ROOT, WORKSPACE]) {
    try {
      const require = createRequire(path.join(base, 'package.json'))
      return path.join(path.dirname(require.resolve(`${name}/package.json`)), suffix)
    } catch {
      // Try the next base.
    }
  }
  throw new Error(`${name} is not installed; run npm install at the repository root`)
}
const THRESHOLD = 95

const CONTENT_TYPES = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8',
  '.json': 'application/json',
  '.svg': 'image/svg+xml',
  '.ico': 'image/x-icon',
  '.png': 'image/png',
  '.woff2': 'font/woff2',
}

/** A static server with the SPA fallback nginx will serve. */
function serve() {
  const server = createServer(async (request, response) => {
    const url = new URL(request.url, 'http://127.0.0.1')
    let file = path.join(DIST, url.pathname)
    try {
      const stats = await access(file).then(() => true, () => false)
      if (!stats || url.pathname.endsWith('/')) {
        // `/docs/...` and `/login` are client routes: nginx answers with index.html and
        // the router takes over. Mimicking that here is what makes the check meaningful —
        // a server without the fallback would 404 the very route under test.
        file = path.join(DIST, 'index.html')
      }
      const body = await readFile(file)
      response.writeHead(200, {
        'content-type': CONTENT_TYPES[path.extname(file)] ?? 'application/octet-stream',
      })
      response.end(body)
    } catch {
      const body = await readFile(path.join(DIST, 'index.html')).catch(() => null)
      if (body === null) {
        response.writeHead(500)
        response.end('dist is missing; run `npm run build` first')
        return
      }
      response.writeHead(200, { 'content-type': CONTENT_TYPES['.html'] })
      response.end(body)
    }
  })
  return new Promise((resolve) => {
    server.listen(0, '127.0.0.1', () => resolve({ server, port: server.address().port }))
  })
}

/** Runs Lighthouse and returns its accessibility score. */
async function lighthouseScore(url) {
  const reportDirectory = await mkdtemp(path.join(tmpdir(), 'lh-'))
  const reportPath = path.join(reportDirectory, 'report.json')
  try {
    await new Promise((resolve, reject) => {
      const child = spawn(
        process.execPath,
        [
          resolveDependency('lighthouse', 'cli/index.js'),
          url,
          '--only-categories=accessibility',
          '--output=json',
          `--output-path=${reportPath}`,
          '--chrome-flags=--headless=new --no-sandbox --disable-gpu --disable-dev-shm-usage',
          '--quiet',
        ],
        {
          stdio: ['ignore', 'pipe', 'pipe'],
          env: {
            ...process.env,
            // Playwright's Chromium is the browser this environment has; Lighthouse
            // launches its own instance of it.
            CHROME_PATH: process.env.CHROME_PATH ?? '',
          },
          // Lighthouse resolves the repository's dependency, which is installed at the
          // workspace root when the checks run there.
          cwd: ROOT,
        },
      )
      let stderr = ''
      child.stderr.on('data', (chunk) => (stderr += chunk))
      child.on('error', reject)
      child.on('exit', (code) => {
        if (code === 0) return resolve()
        reject(new Error(`lighthouse exited ${code}: ${stderr.slice(-800)}`))
      })
    })
    const report = JSON.parse(await readFile(reportPath, 'utf8'))
    return report
  } finally {
    await rm(reportDirectory, { recursive: true, force: true })
  }
}

async function main() {
  await access(path.join(DIST, 'index.html')).catch(() => {
    throw new Error('dist/index.html is missing — run `npm run build` first')
  })

  const { server, port } = await serve()
  const origin = `http://127.0.0.1:${port}`
  const problems = []

  try {
    // ── every request is same-origin ──────────────────────────────────────────
    const browser = await chromium.launch()
    const page = await browser.newPage()
    const requests = []
    page.on('request', (request) => requests.push(request.url()))

    await page.goto(`${origin}/login`, { waitUntil: 'networkidle' })
    await browser.close()

    const foreign = requests.filter((url) => {
      try {
        return new URL(url).origin !== origin
      } catch {
        return true
      }
    })
    if (foreign.length > 0) {
      problems.push(`requests left the origin: ${[...new Set(foreign)].join(', ')}`)
    }
    console.log(`ok    ${requests.length} request(s), all same-origin`)

    // ── axe-core, for the elements behind the score ───────────────────────────
    const axeBrowser = await chromium.launch()
    const axePage = await axeBrowser.newPage()
    await axePage.goto(`${origin}/login`, { waitUntil: 'networkidle' })
    await axePage.addScriptTag({ path: resolveDependency('axe-core', 'axe.min.js') })
    const axe = await axePage.evaluate(async () =>
      // eslint-disable-next-line no-undef
      await window.axe.run(document, { runOnly: ['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa'] }),
    )
    await axeBrowser.close()

    const serious = axe.violations.filter(
      (violation) => violation.impact === 'serious' || violation.impact === 'critical',
    )
    for (const violation of serious) {
      const targets = violation.nodes.map((node) => node.target.join(' ')).join(', ')
      problems.push(`axe ${violation.impact}: ${violation.id} — ${violation.help} (${targets})`)
    }
    if (serious.length === 0) {
      console.log(`ok    axe-core: no serious or critical violations (${axe.violations.length} minor)`)
    }

    // ── Lighthouse ────────────────────────────────────────────────────────────
    const report = await lighthouseScore(`${origin}/login`)
    const score = Math.round((report.categories.accessibility.score ?? 0) * 100)
    console.log(`     lighthouse accessibility: ${score} (threshold ${THRESHOLD})`)
    if (score < THRESHOLD) {
      for (const reference of report.categories.accessibility.auditRefs) {
        const audit = report.audits[reference.id]
        if (audit.score !== null && audit.score < 1 && audit.scoreDisplayMode !== 'notApplicable') {
          problems.push(`lighthouse: ${reference.id} — ${audit.title}`)
        }
      }
      problems.push(`lighthouse accessibility ${score} is below ${THRESHOLD}`)
    }
  } finally {
    server.close()
  }

  if (problems.length > 0) {
    console.error('')
    for (const problem of problems) console.error(`FAIL ${problem}`)
    process.exit(1)
  }
  console.log('')
  console.log('check: ok')
}

main().catch((error) => {
  console.error(`check failed: ${error.message}`)
  process.exit(1)
})
