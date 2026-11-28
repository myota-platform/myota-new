// The client's rules, tested where they are cheap to test.
//
// Three behaviours are worth a unit test because they are the ones that would fail
// silently: one refresh on a 401 and not a loop, no credential in storage, and a `next`
// parameter that cannot leave the origin.

import { beforeEach, describe, expect, it, vi } from 'vitest'

import { ApiError, Unauthorized, api, refresh, session } from './api.js'
import { safeNext } from './router.js'

/** A fetch double that answers from a queue and records what it was asked. */
function stubFetch(responses) {
  const calls = []
  const fetch = vi.fn(async (url, options = {}) => {
    calls.push({ url, options })
    const next = responses.shift()
    if (!next) throw new Error(`unexpected request to ${url}`)
    return next
  })
  vi.stubGlobal('fetch', fetch)
  return calls
}

function json(status, body) {
  return {
    status,
    ok: status >= 200 && status < 300,
    headers: new Map([['content-type', 'application/json']]),
    get(name) {
      return this.headers.get(name) ?? null
    },
    async json() {
      return body
    },
    async text() {
      return JSON.stringify(body)
    },
  }
}

const sessionView = {
  accountId: '0f7d2b19-4c6e-4a83-b5d2-9e1c7a3f6b48',
  sessionId: '5a3e9c17-2d4b-4f68-8a0e-7c1b5d9f2e34',
  accessToken: 'a-token',
  scopes: ['identity.admins.manage'],
  mfaConfirmedAt: '2026-11-02T08:31:00Z',
}

beforeEach(() => {
  session.clear()
  session.loaded = false
  vi.unstubAllGlobals()
})

describe('the session', () => {
  it('is reactive, so the shell re-renders when it changes', async () => {
    // The regression this exists for: `session` was a plain object, so the computed that
    // decides whether to show the nav was evaluated once and never again.
    const { watchEffect, nextTick } = await import('vue')
    const seen = []
    const stop = watchEffect(() => seen.push(session.authenticated))
    session.set(sessionView)
    await nextTick()
    session.clear()
    await nextTick()
    stop()
    expect(seen).toEqual([false, true, false])
  })

  it('holds the access token in memory and never in storage', async () => {
    stubFetch([json(200, sessionView)])
    await refresh()
    expect(session.authenticated).toBe(true)
    expect(session.can('identity.admins.manage')).toBe(true)
    expect(session.can('geodata.review')).toBe(false)

    // Nothing written to either storage API. A token in `localStorage` is readable by any
    // injected script, and this console can grant authority.
    expect(window.localStorage.length).toBe(0)
    expect(window.sessionStorage.length).toBe(0)
  })

  it('reports not-signed-in on a 401 rather than throwing', async () => {
    stubFetch([json(401, { type: 'about:blank#unauthorized' })])
    expect(await refresh()).toBeNull()
    expect(session.authenticated).toBe(false)
    expect(session.loaded).toBe(true)
  })
})

describe('the request path', () => {
  it('refreshes once on a 401 and retries the request once', async () => {
    const calls = stubFetch([
      json(401, { type: 'about:blank#unauthorized' }),
      json(200, sessionView),
      json(200, { items: [] }),
    ])

    const result = await api('/v1/admin/identity/admins')

    expect(result).toEqual({ items: [] })
    expect(calls).toHaveLength(3)
    expect(calls[0].url).toBe('/v1/admin/identity/admins')
    expect(calls[1].url).toBe('/v1/admin/auth/refresh')
    expect(calls[2].url).toBe('/v1/admin/identity/admins')
    // The retry carries the token the refresh produced.
    expect(calls[2].options.headers.authorization).toBe('Bearer a-token')
  })

  it('gives up when the refresh also fails', async () => {
    stubFetch([
      json(401, { type: 'about:blank#unauthorized' }),
      json(401, { type: 'about:blank#unauthorized' }),
    ])

    await expect(api('/v1/admin/identity/admins')).rejects.toBeInstanceOf(Unauthorized)
    expect(session.authenticated).toBe(false)
  })

  it('does not retry a 403: the audience is right and the authority is not', async () => {
    const calls = stubFetch([json(403, { type: 'about:blank#scope-not-held', detail: 'no' })])

    const failure = await api('/v1/admin/identity/admins/x/roles', {
      method: 'POST',
      body: { roleCode: 'platform_admin' },
    }).catch((error) => error)

    expect(failure).toBeInstanceOf(ApiError)
    expect(failure.status).toBe(403)
    expect(failure.type).toBe('about:blank#scope-not-held')
    expect(calls).toHaveLength(1)
  })

  it('surfaces the problem detail rather than a status code', async () => {
    stubFetch([
      json(409, {
        type: 'about:blank#conflict',
        detail: 'This is your last administrator role.',
      }),
    ])
    const failure = await api('/v1/admin/identity/admins/x/roles/y', { method: 'DELETE' })
      .catch((error) => error)
    expect(failure.message).toBe('This is your last administrator role.')
  })

  it('uses only relative paths, so the bundle names no other origin', async () => {
    const calls = stubFetch([json(200, {})])
    await api('/v1/admin/identity/admins')
    expect(calls[0].url.startsWith('/v1/')).toBe(true)
  })

  it('treats a 204 as an empty success', async () => {
    stubFetch([{ status: 204, ok: true, headers: new Map(), get: () => null }])
    expect(await api('/v1/admin/auth/logout', { method: 'POST' })).toBeNull()
  })
})

describe('the next parameter', () => {
  it('keeps a path on this origin', () => {
    expect(safeNext('/admins')).toBe('/admins')
    expect(safeNext('/admins?q=ana')).toBe('/admins?q=ana')
  })

  it('refuses anything that could leave the origin', () => {
    // The open redirect: a login page that forwards to another host is a phishing tool
    // with a valid certificate.
    for (const value of [
      'https://evil.example/login',
      '//evil.example/login',
      'javascript:alert(1)',
      '',
      null,
      undefined,
      42,
    ]) {
      expect(safeNext(value)).toBe('/operations')
    }
  })
})
