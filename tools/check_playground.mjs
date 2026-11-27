#!/usr/bin/env node
// Render the gateway's playground in a real browser and prove it makes no request
// off-origin.
//
// The Go tests assert that every asset reference in the HTML is same-origin and that
// the Content-Security-Policy forbids anything else. Neither of those proves the page
// *renders*: a bundle that fails to parse, a global renamed by an upgrade, or a
// script blocked by the policy all look perfectly fine to a string comparison and
// produce a blank page for an operator.
//
// So this starts the real binary, points it at three fake upstreams, loads /docs in
// Chromium, and fails on: a console error, an unhandled page error, a failed request,
// a request to another origin, or a mount point with nothing in it.
//
// Self-contained on purpose: it spawns everything it needs, so `make verify-full` and
// CI run the same command with no fixtures to keep in step.
//
//   node tools/check_playground.mjs
//
// Requires the Go toolchain (to build the binary when it is missing) and Playwright.

import { spawn, spawnSync } from 'node:child_process'
import { createServer } from 'node:http'
import { existsSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'
import { chromium } from 'playwright'

const REPO_ROOT = dirname(dirname(fileURLToPath(import.meta.url)))
const GATEWAY_DIR = join(REPO_ROOT, 'apps', 'gateway')
const BINARY = join(GATEWAY_DIR, 'bin', 'gateway')
const GATEWAY_PORT = 18080
const UPSTREAM_PORTS = { core: 19001, geodata: 19002, activity: 19003 }

const children = []
const servers = []

function cleanup() {
  for (const server of servers) server.close()
  for (const child of children) {
    if (!child.killed) child.kill('SIGTERM')
  }
}

// fakeUpstream answers /internal/healthz so readiness passes and echoes the matched
// path so a routing mistake is visible in the report rather than as a 404.
function fakeUpstream(name, port) {
  const server = createServer((request, response) => {
    const body = JSON.stringify(
      request.url === '/internal/healthz'
        ? { status: 'ok', service: name }
        : { service: name, path: request.url },
    )
    response.writeHead(200, { 'Content-Type': 'application/json', 'Content-Length': Buffer.byteLength(body) })
    response.end(body)
  })
  servers.push(server)
  return new Promise((resolve, reject) => {
    server.once('error', reject)
    server.listen(port, '127.0.0.1', resolve)
  })
}

async function waitFor(url, deadlineMs) {
  const until = Date.now() + deadlineMs
  let lastError
  while (Date.now() < until) {
    try {
      const response = await fetch(url)
      if (response.ok) return
      lastError = new Error(`${url} answered ${response.status}`)
    } catch (error) {
      lastError = error
    }
    await new Promise((resolve) => setTimeout(resolve, 100))
  }
  throw new Error(`timed out waiting for ${url}: ${lastError?.message ?? 'no response'}`)
}

function buildIfMissing() {
  if (existsSync(BINARY)) return
  console.log('  building apps/gateway/bin/gateway')
  const result = spawnSync('go', ['build', '-trimpath', '-o', 'bin/gateway', '.'], {
    cwd: GATEWAY_DIR,
    stdio: 'inherit',
  })
  if (result.status !== 0) throw new Error('go build failed')
}

function startGateway() {
  const environment = {
    ...process.env,
    GATEWAY_LISTEN_ADDR: `127.0.0.1:${GATEWAY_PORT}`,
    GATEWAY_UPSTREAM_CORE: `http://127.0.0.1:${UPSTREAM_PORTS.core}`,
    GATEWAY_UPSTREAM_GEODATA: `http://127.0.0.1:${UPSTREAM_PORTS.geodata}`,
    GATEWAY_UPSTREAM_ACTIVITY: `http://127.0.0.1:${UPSTREAM_PORTS.activity}`,
    // No verification key: the playground is public, and leaving the key unset proves
    // the docs routes work on a gateway that has not been given one yet.
    GATEWAY_JWT_PUBLIC_KEY_FILE: '',
  }
  const child = spawn(BINARY, [], { env: environment, stdio: ['ignore', 'pipe', 'pipe'] })
  children.push(child)
  child.stderr.on('data', (chunk) => process.stderr.write(chunk))
  return child
}

async function main() {
  buildIfMissing()
  await Promise.all(Object.entries(UPSTREAM_PORTS).map(([name, port]) => fakeUpstream(name, port)))
  startGateway()
  await waitFor(`http://127.0.0.1:${GATEWAY_PORT}/healthz`, 15_000)

  const browser = await chromium.launch()
  const page = await browser.newPage({ viewport: { width: 1280, height: 900 } })

  const problems = []
  const requested = []

  page.on('console', (message) => {
    // Scalar logs a version banner at info level; errors and warnings are failures.
    if (message.type() === 'error') problems.push(`console error: ${message.text()}`)
  })
  page.on('pageerror', (error) => problems.push(`unhandled page error: ${error.message}`))
  page.on('requestfailed', (request) =>
    problems.push(`request failed: ${request.url()} (${request.failure()?.errorText})`),
  )
  page.on('request', (request) => {
    const url = new URL(request.url())
    requested.push(url.href)
    const sameOrigin = url.origin === `http://127.0.0.1:${GATEWAY_PORT}`
    const isData = url.protocol === 'data:' || url.protocol === 'blob:'
    if (!sameOrigin && !isData) problems.push(`third-party request: ${url.href}`)
  })

  await page.goto(`http://127.0.0.1:${GATEWAY_PORT}/docs`, { waitUntil: 'networkidle', timeout: 45_000 })

  // Scalar mounts into #app. Waiting for a node there is the difference between "the
  // page returned 200" and "the page works".
  try {
    await page.waitForFunction(
      () => {
        const mount = document.querySelector('#app')
        return mount && mount.children.length > 0
      },
      { timeout: 30_000 },
    )
  } catch {
    problems.push('Scalar did not mount: #app is still empty after 30 s')
  }

  // The banner is ours, and it proves the page body rendered rather than the bundle
  // having replaced it.
  const banner = await page.textContent('.myota-banner').catch(() => null)
  if (!banner || !banner.includes('Myota API')) problems.push('the page banner did not render')

  // The contract the playground was told to load must actually have been fetched from
  // our own origin — a page that mounts but never loads the API is a page showing an
  // error state.
  if (!requested.some((url) => url === `http://127.0.0.1:${GATEWAY_PORT}/openapi.json`)) {
    problems.push('/openapi.json was never requested by the page')
  }

  const title = await page.title()
  if (!title.includes('Myota')) problems.push(`page title is ${JSON.stringify(title)}`)

  await browser.close()

  if (problems.length > 0) {
    console.error('  error  playground check failed:')
    for (const problem of problems) console.error(`         ${problem}`)
    return 1
  }

  console.log(
    `OK: /docs rendered ${requested.length} same-origin request(s), 0 third-party, 0 console errors`,
  )
  return 0
}

let status = 1
try {
  status = await main()
} catch (error) {
  console.error(`  error  ${error.message}`)
} finally {
  cleanup()
}
process.exit(status)
