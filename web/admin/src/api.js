// The API client.
//
// Three rules, and each exists because the obvious alternative is a hole:
//
//   1. **No token in storage.** The refresh credential is an HttpOnly cookie the script
//      cannot read; the access token lives in this module's memory and dies with the tab.
//      `localStorage` is readable by any script that gets injected, and a console that can
//      grant authority is the last place to keep a credential there.
//   2. **One refresh attempt, then give up.** A 401 triggers a single refresh and a single
//      retry. Retrying in a loop turns an expired session into a request storm, and
//      refreshing on every 401 lets a revoked session keep trying.
//   3. **Relative paths only.** The console is served from the same origin as the API, so
//      `/v1/...` needs no host. Hard-coding a host would break the test environment the
//      first time it was renamed, and absolute URLs in a bundle are how a build ends up
//      quietly pointing at somebody else's server.

/** Where the API lives. Empty means "this origin". */
const API_BASE = import.meta.env.VITE_API_BASE ?? ''

/** The access token, in memory only. */
let accessToken = null

/** The one refresh in flight, so ten parallel 401s make one request. */
let refreshInFlight = null

/** Thrown when the session is gone and the caller should be sent to the sign-in page. */
export class Unauthorized extends Error {
  constructor() {
    super('the session is not valid')
    this.name = 'Unauthorized'
  }
}

/** Everything a caller can see about the session. */
export const session = {
  accountId: null,
  sessionId: null,
  scopes: [],
  mfaConfirmedAt: null,
  /** False until the first refresh has answered, successfully or not. */
  loaded: false,
  get authenticated() {
    return this.accountId !== null
  },
  /** Whether the account holds a scope. A hint for the interface, never a control. */
  can(scope) {
    return this.scopes.includes(scope)
  },
  set(view) {
    this.accountId = view.accountId
    this.sessionId = view.sessionId
    this.scopes = view.scopes ?? []
    this.mfaConfirmedAt = view.mfaConfirmedAt ?? null
    accessToken = view.accessToken
    this.loaded = true
  },
  clear() {
    this.accountId = null
    this.sessionId = null
    this.scopes = []
    this.mfaConfirmedAt = null
    accessToken = null
    this.loaded = true
  },
}

/**
 * Exchanges the session cookie for an access token.
 *
 * Resolves to the session view, or to `null` when there is no session. A network failure
 * is not the same as "not signed in", so it throws: a console that silently shows the
 * sign-in page because the API is unreachable is a console that loses the user's work.
 */
export async function refresh() {
  if (refreshInFlight) return refreshInFlight

  refreshInFlight = (async () => {
    let response
    try {
      response = await fetch(`${API_BASE}/v1/admin/auth/refresh`, {
        method: 'POST',
        credentials: 'same-origin',
        headers: { accept: 'application/json' },
      })
    } catch (cause) {
      refreshInFlight = null
      throw new NetworkError(cause)
    }

    if (response.status === 401 || response.status === 403) {
      session.clear()
      refreshInFlight = null
      return null
    }
    if (!response.ok) {
      refreshInFlight = null
      throw new ApiError(response.status, await problemOf(response))
    }

    const view = await response.json()
    session.set(view)
    refreshInFlight = null
    return view
  })()

  return refreshInFlight
}

/** The API could not be reached at all. */
export class NetworkError extends Error {
  constructor(cause) {
    super('the API is unreachable')
    this.name = 'NetworkError'
    this.cause = cause
  }
}

/** A response that was not 2xx and not a lost session. */
export class ApiError extends Error {
  constructor(status, problem) {
    super(problem?.detail ?? problem?.title ?? `the API answered ${status}`)
    this.name = 'ApiError'
    this.status = status
    this.problem = problem
    /** The problem's `type`, e.g. `about:blank#reauth-required`. */
    this.type = problem?.type ?? null
  }
}

async function problemOf(response) {
  try {
    return await response.json()
  } catch {
    return null
  }
}

/**
 * Calls the API, refreshing once on 401.
 *
 * `body` is serialised; a caller that needs to send nothing passes none. The retry is
 * exactly one, and only after a refresh succeeded — otherwise a dead session would be
 * retried against an endpoint that will keep refusing it.
 */
export async function api(path, { method = 'GET', body, headers = {}, retry = true } = {}) {
  const request = {
    method,
    credentials: 'same-origin',
    headers: {
      accept: 'application/json',
      ...(body === undefined ? {} : { 'content-type': 'application/json' }),
      ...(accessToken ? { authorization: `Bearer ${accessToken}` } : {}),
      ...headers,
    },
  }
  if (body !== undefined) request.body = JSON.stringify(body)

  let response
  try {
    response = await fetch(`${API_BASE}${path}`, request)
  } catch (cause) {
    throw new NetworkError(cause)
  }

  if (response.status === 401 && retry) {
    const renewed = await refresh()
    if (renewed) return api(path, { method, body, headers, retry: false })
    throw new Unauthorized()
  }

  if (response.status === 204) return null

  if (!response.ok) {
    throw new ApiError(response.status, await problemOf(response))
  }

  const contentType = response.headers.get('content-type') ?? ''
  return contentType.includes('json') ? response.json() : response.text()
}

/** Signs in with a password. Returns either a challenge or a session. */
export async function login(email, password) {
  const result = await api('/v1/admin/auth/login', {
    method: 'POST',
    body: { email, password },
  })
  if (result?.status === 'authenticated') session.set(result)
  return result
}

/** Answers the second factor. On success the session is set. */
export async function answerChallenge(challengeId, method, code) {
  const view = await api('/v1/admin/auth/mfa/challenge', {
    method: 'POST',
    body: { challengeId, method, code },
  })
  session.set(view)
  return view
}

/** Signs out and forgets the session, whatever the API says. */
export async function logout() {
  try {
    await api('/v1/admin/auth/logout', { method: 'POST' })
  } catch {
    // A failed sign-out still clears this tab. The server-side session is gone either
    // way when the cookie is dropped, and refusing to log out because the network is
    // down is the wrong failure.
  }
  session.clear()
}

/** Proves freshness for a role change, an MFA reset or a suspension. */
export async function reauthenticate(password, code) {
  return api('/v1/admin/auth/reauth', { method: 'POST', body: { password, code } })
}
