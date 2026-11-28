<script setup>
// The Admins section: who can change what others may do.
//
// Three things this screen is careful about, because each is where an admin console
// usually gets it wrong:
//
//   1. **A blocked administrator is surfaced, not buried.** An account with no second
//      factor cannot sign in at all, so the page carries a banner naming them. Finding
//      that out from a support ticket is the failure this prevents.
//   2. **Re-authentication is asked for where it is needed, not up front.** The API
//      refuses a change with `#reauth-required` when the session is stale; this screen
//      catches exactly that answer, asks for both factors, and retries the same action
//      once. Nothing is changed twice.
//   3. **Nothing here can verify a callsign or delete an account.** The section grants
//      authority; participant work is elsewhere, and the contract has no endpoint for it
//      under this prefix.
import { computed, onMounted, reactive, ref } from 'vue'
import { ApiError, api, session } from '../api.js'
import ReauthDialog from '../components/ReauthDialog.vue'

const loading = ref(true)
const error = ref(null)
const items = ref([])
const nextCursor = ref(null)
const roles = ref([])

const filters = reactive({ q: '', mfa: '', role: '' })

/** The account whose detail is open. */
const expanded = ref(null)
const detail = reactive({ scopes: null, sessions: null, loading: false, error: null })

/** The re-authentication prompt: what to retry when it succeeds. */
const reauth = reactive({ open: false, action: '', retry: null })

const blocked = computed(() => items.value.filter((account) => account.state === 'blocked'))

async function load(reset = true) {
  error.value = null
  if (reset) {
    loading.value = true
    nextCursor.value = null
  }
  const query = new URLSearchParams({ limit: '25' })
  if (filters.q.trim()) query.set('q', filters.q.trim())
  if (filters.mfa) query.set('mfa', filters.mfa)
  if (filters.role) query.set('role', filters.role)
  if (!reset && nextCursor.value) query.set('cursor', nextCursor.value)

  try {
    const page = await api(`/v1/admin/identity/admins?${query}`)
    items.value = reset ? page.items : [...items.value, ...page.items]
    nextCursor.value = page.nextCursor
  } catch (cause) {
    if (cause instanceof ApiError && cause.status === 403) {
      error.value =
        'This account cannot manage administrators: it does not hold identity.admins.manage.'
    } else {
      error.value = cause.message ?? 'The list could not be loaded.'
    }
  } finally {
    loading.value = false
  }
}

async function loadRoles() {
  try {
    const body = await api('/v1/admin/identity/roles')
    roles.value = body.items
  } catch {
    // The catalogue is a convenience for the grant form; without it the form still works
    // by typing a code, rather than the screen disappearing.
  }
}

async function openDetail(account) {
  if (expanded.value === account.id) {
    expanded.value = null
    return
  }
  expanded.value = account.id
  detail.scopes = null
  detail.sessions = null
  detail.error = null
  detail.loading = true
  try {
    const [scopes, sessions] = await Promise.all([
      api(`/v1/admin/identity/admins/${account.id}/scopes`),
      api(`/v1/admin/identity/admins/${account.id}/sessions`),
    ])
    detail.scopes = scopes
    detail.sessions = sessions.items
  } catch (cause) {
    detail.error = cause.message ?? 'The account detail could not be loaded.'
  } finally {
    detail.loading = false
  }
}

/**
 * Runs a mutating call, asking for re-authentication if the API says it needs it.
 *
 * One retry, and only after a successful re-authentication: a loop here would hammer an
 * endpoint that is refusing for a reason.
 */
async function mutate(call, label) {
  error.value = null
  try {
    await call()
    await load()
    if (expanded.value) {
      const account = items.value.find((row) => row.id === expanded.value)
      if (account) await openDetail(account)
    }
    return true
  } catch (cause) {
    if (cause instanceof ApiError && cause.type === 'about:blank#reauth-required') {
      reauth.action = label
      reauth.retry = () => mutate(call, label)
      reauth.open = true
      return false
    }
    error.value = cause.message ?? 'That change was refused.'
    return false
  }
}

function onReauthenticated() {
  const retry = reauth.retry
  reauth.open = false
  reauth.retry = null
  retry?.()
}

const grantForm = reactive({ roleCode: '', programmeId: '' })

async function grant(account) {
  await mutate(
    () =>
      api(`/v1/admin/identity/admins/${account.id}/roles`, {
        method: 'POST',
        body: {
          roleCode: grantForm.roleCode,
          ...(grantForm.programmeId.trim() ? { programmeId: grantForm.programmeId.trim() } : {}),
        },
      }),
    `Granting ${grantForm.roleCode} to ${account.email}`,
  )
}

async function revoke(account, grantItem) {
  await mutate(
    () => api(`/v1/admin/identity/admins/${account.id}/roles/${grantItem.id}`, { method: 'DELETE' }),
    `Revoking ${grantItem.roleCode} from ${account.email}`,
  )
}

async function resetMfa(account) {
  if (!window.confirm(`Remove ${account.email}'s second factor and sign them out everywhere?`)) {
    return
  }
  await mutate(
    () => api(`/v1/admin/identity/admins/${account.id}/mfa/reset`, { method: 'POST' }),
    `Resetting the second factor for ${account.email}`,
  )
}

async function suspend(account) {
  if (!window.confirm(`Suspend ${account.email}? They will be signed out immediately.`)) {
    return
  }
  await mutate(
    () => api(`/v1/admin/identity/admins/${account.id}/suspend`, { method: 'POST', body: {} }),
    `Suspending ${account.email}`,
  )
}

async function reactivate(account) {
  await mutate(
    () => api(`/v1/admin/identity/admins/${account.id}/reactivate`, { method: 'POST' }),
    `Reactivating ${account.email}`,
  )
}

async function revokeSession(account, sessionRow) {
  await mutate(
    () =>
      api(`/v1/admin/identity/admins/${account.id}/sessions/${sessionRow.id}`, { method: 'DELETE' }),
    `Revoking a session for ${account.email}`,
  )
}

const stateLabel = (state) =>
  ({ active: 'Active', blocked: 'Blocked', suspended: 'Suspended', idle: 'Idle' })[state] ?? state

const stateHint = (state) =>
  ({
    active: 'Can sign in and manage.',
    blocked: 'No confirmed second factor: sign-in is refused.',
    suspended: 'Stopped by an administrator. Sessions revoked.',
    idle: 'Usable, but no sign-in for thirty days.',
  })[state] ?? ''

function when(value) {
  if (!value) return 'never'
  const date = new Date(value)
  const days = Math.floor((Date.now() - date.getTime()) / 86_400_000)
  if (days <= 0) return 'today'
  if (days === 1) return 'yesterday'
  if (days < 30) return `${days} days ago`
  return date.toLocaleDateString()
}

onMounted(async () => {
  await Promise.all([load(), loadRoles()])
})
</script>

<template>
  <div>
    <section class="hero" style="padding: 30px 36px">
      <span class="tagline">Admins · identity.admins.manage</span>
      <h1 style="font-size: 30px; margin: 6px 0 10px">Who can change what others may do.</h1>
      <p class="lede" style="margin: 0">
        Every change here needs a second factor and a re-authentication within five minutes,
        and cannot grant a scope the actor does not hold.
      </p>
    </section>

    <p v-if="error" class="alert" role="alert" style="margin-bottom: 18px">{{ error }}</p>

    <div
      v-if="blocked.length"
      class="card"
      style="margin-bottom: 18px; border-left: 4px solid var(--ochre)"
    >
      <div class="card-body" style="padding: 16px 22px">
        <strong>
          {{ blocked.length }} administrator{{ blocked.length === 1 ? '' : 's' }} cannot sign in
        </strong>
        — no second factor is enrolled.
        <ul style="margin: 8px 0 0; padding-left: 20px; font-size: 14px; color: var(--ink-2)">
          <li v-for="account in blocked" :key="account.id">
            {{ account.displayName }} ({{ account.email }})
          </li>
        </ul>
        <p class="tiny" style="margin: 8px 0 0">
          Enrolment needs a session, so whoever provisioned the account has to set the factor
          up; there is no self-service path.
        </p>
      </div>
    </div>

    <div class="card">
      <div class="card-head" style="display: flex; gap: 12px; align-items: center; flex-wrap: wrap">
        <h2 style="margin: 0">Admin accounts</h2>
        <span style="flex: 1"></span>
        <form
          style="display: flex; gap: 8px; flex-wrap: wrap; align-items: center"
          @submit.prevent="load()"
        >
          <label class="sr-only" for="filter-q">Search by name or address</label>
          <input
            id="filter-q"
            v-model="filters.q"
            class="inp"
            type="search"
            placeholder="Search"
            style="width: 190px"
          />
          <label class="sr-only" for="filter-mfa">Second factor</label>
          <select id="filter-mfa" v-model="filters.mfa" class="sel" style="width: 150px">
            <option value="">Any factor</option>
            <option value="enrolled">Enrolled</option>
            <option value="not_enrolled">Not enrolled</option>
          </select>
          <label class="sr-only" for="filter-role">Role</label>
          <select id="filter-role" v-model="filters.role" class="sel" style="width: 170px">
            <option value="">Any role</option>
            <option v-for="role in roles" :key="role.roleCode" :value="role.roleCode">
              {{ role.displayName }}
            </option>
          </select>
          <button class="go" type="submit">Filter</button>
        </form>
      </div>

      <div class="card-body" style="padding: 6px 22px 18px">
        <p v-if="loading" class="skeleton" style="margin: 18px 0" aria-live="polite">Loading…</p>

        <table v-else style="width: 100%; border-collapse: collapse; font-size: 14px">
          <caption class="sr-only">
            Administrator accounts, their second factor and the roles they hold
          </caption>
          <thead>
            <tr style="text-align: left; color: var(--ink-2); font-size: 12.5px">
              <th scope="col" style="padding: 10px 8px">Account</th>
              <th scope="col" style="padding: 10px 8px">State</th>
              <th scope="col" style="padding: 10px 8px">Factor</th>
              <th scope="col" style="padding: 10px 8px">Roles</th>
              <th scope="col" style="padding: 10px 8px">Last seen</th>
              <th scope="col" style="padding: 10px 8px"><span class="sr-only">Actions</span></th>
            </tr>
          </thead>
          <tbody>
            <template v-for="account in items" :key="account.id">
              <tr style="border-top: 1px solid var(--line)">
                <td style="padding: 12px 8px">
                  <div style="font-weight: 620">{{ account.displayName }}</div>
                  <div class="tiny">{{ account.email }}</div>
                </td>
                <td style="padding: 12px 8px">
                  <span :class="['badge', `badge-${account.state}`]" :title="stateHint(account.state)">
                    {{ stateLabel(account.state) }}
                  </span>
                </td>
                <td style="padding: 12px 8px">
                  {{ account.mfa === 'enrolled' ? 'Enrolled' : 'None' }}
                </td>
                <td style="padding: 12px 8px">
                  <span v-for="role in account.roles" :key="role.id" class="scope">
                    {{ role.roleCode }}
                  </span>
                  <span v-if="!account.roles.length" class="tiny">none</span>
                </td>
                <td style="padding: 12px 8px">{{ when(account.lastSeenAt) }}</td>
                <td style="padding: 12px 8px; text-align: right">
                  <button
                    class="go go-quiet"
                    type="button"
                    :aria-expanded="expanded === account.id"
                    :aria-controls="`detail-${account.id}`"
                    @click="openDetail(account)"
                  >
                    {{ expanded === account.id ? 'Close' : 'Manage' }}
                  </button>
                </td>
              </tr>

              <tr v-if="expanded === account.id" :id="`detail-${account.id}`">
                <td colspan="6" style="padding: 6px 8px 22px; background: var(--sand-2)">
                  <p v-if="detail.loading" class="tiny">Loading the account…</p>
                  <p v-else-if="detail.error" class="alert" role="alert">{{ detail.error }}</p>

                  <div v-else style="display: grid; gap: 20px; padding: 12px 4px 0">
                    <section>
                      <h3 style="margin: 0 0 6px; font-size: 15px">Effective scopes</h3>
                      <div style="display: flex; flex-wrap: wrap; gap: 6px">
                        <span v-for="scope in detail.scopes?.scopes" :key="scope" class="scope">
                          {{ scope }}
                        </span>
                        <span v-if="!detail.scopes?.scopes?.length" class="tiny">none</span>
                      </div>
                    </section>

                    <section>
                      <h3 style="margin: 0 0 6px; font-size: 15px">Roles</h3>
                      <ul style="margin: 0; padding-left: 18px">
                        <li
                          v-for="grantItem in detail.scopes?.roles"
                          :key="grantItem.id"
                          style="margin-bottom: 6px"
                        >
                          <span class="scope">{{ grantItem.roleCode }}</span>
                          <span v-if="grantItem.programmeId" class="tiny">
                            · programme {{ grantItem.programmeId.slice(0, 8) }}…
                          </span>
                          <button
                            class="go go-quiet"
                            type="button"
                            style="margin-left: 10px"
                            @click="revoke(account, grantItem)"
                          >
                            Revoke
                          </button>
                        </li>
                        <li v-if="!detail.scopes?.roles?.length" class="tiny">no open grants</li>
                      </ul>

                      <form
                        style="display: flex; gap: 8px; flex-wrap: wrap; align-items: end; margin-top: 10px"
                        @submit.prevent="grant(account)"
                      >
                        <div>
                          <label class="sr-only" :for="`grant-role-${account.id}`">Role to grant</label>
                          <select
                            :id="`grant-role-${account.id}`"
                            v-model="grantForm.roleCode"
                            class="sel"
                            required
                            style="width: 230px"
                          >
                            <option value="" disabled>Choose a role…</option>
                            <option v-for="role in roles" :key="role.roleCode" :value="role.roleCode">
                              {{ role.displayName }}
                            </option>
                          </select>
                        </div>
                        <div>
                          <label class="sr-only" :for="`grant-programme-${account.id}`">
                            Programme id
                          </label>
                          <input
                            :id="`grant-programme-${account.id}`"
                            v-model="grantForm.programmeId"
                            class="inp"
                            placeholder="Programme id (optional)"
                            style="width: 230px"
                          />
                        </div>
                        <button class="go" type="submit" :disabled="!grantForm.roleCode">Grant</button>
                      </form>
                      <p class="tiny" style="margin: 6px 0 0">
                        A grant is refused unless every scope the role carries is one you hold
                        yourself, for the same programme.
                      </p>
                    </section>

                    <section>
                      <h3 style="margin: 0 0 6px; font-size: 15px">Sessions</h3>
                      <ul style="margin: 0; padding-left: 18px">
                        <li v-for="row in detail.sessions" :key="row.id" style="margin-bottom: 6px">
                          <span class="tiny">{{ row.userAgent || 'unknown browser' }}</span>
                          <span class="tiny"> · {{ row.remoteAddr || 'unknown address' }}</span>
                          <span class="tiny"> · last used {{ when(row.lastSeenAt) }}</span>
                          <span v-if="row.current" class="scope" style="margin-left: 8px">
                            this session
                          </span>
                          <button
                            v-else
                            class="go go-quiet"
                            type="button"
                            style="margin-left: 10px"
                            @click="revokeSession(account, row)"
                          >
                            Revoke
                          </button>
                        </li>
                        <li v-if="!detail.sessions?.length" class="tiny">no open sessions</li>
                      </ul>
                    </section>

                    <section style="display: flex; gap: 10px; flex-wrap: wrap">
                      <button class="btn btn-ghost" type="button" @click="resetMfa(account)">
                        Reset second factor
                      </button>
                      <button
                        v-if="account.state === 'suspended'"
                        class="btn btn-ghost"
                        type="button"
                        @click="reactivate(account)"
                      >
                        Reactivate
                      </button>
                      <button
                        v-else
                        class="btn btn-ghost"
                        type="button"
                        :disabled="account.id === session.accountId"
                        @click="suspend(account)"
                      >
                        Suspend
                      </button>
                    </section>

                    <p class="tiny" style="margin: 0">
                      This section grants authority. It cannot verify a callsign, close or delete
                      an account — those live in Participants and in retention.
                    </p>
                  </div>
                </td>
              </tr>
            </template>
          </tbody>
        </table>

        <p v-if="!loading && !items.length" class="empty">No administrator matches that filter.</p>

        <button
          v-if="nextCursor"
          class="go go-quiet"
          type="button"
          style="margin-top: 14px"
          @click="load(false)"
        >
          Load more
        </button>
      </div>
    </div>
  </div>

  <ReauthDialog
    :open="reauth.open"
    :action="reauth.action"
    @done="onReauthenticated"
    @cancel="reauth.open = false"
  />
</template>

<style scoped>
.badge {
  display: inline-block;
  padding: 3px 10px;
  border-radius: 999px;
  font-size: 12px;
  font-weight: 650;
}
.badge-active {
  background: var(--sage-soft);
  color: #2f4a2c;
}
.badge-blocked {
  background: var(--clay-soft);
  color: #6d2f14;
}
.badge-suspended {
  background: var(--ochre-soft);
  color: #6a4a10;
}
.badge-idle {
  background: var(--sand-2);
  color: var(--ink-2);
}
.scope {
  font-family: var(--num);
  font-size: 11px;
  padding: 2px 8px;
  border-radius: 6px;
  background: var(--card);
  color: var(--ink-2);
  display: inline-block;
  margin-right: 4px;
}
.go-quiet {
  background: var(--sand-2);
  color: var(--ink);
  padding: 6px 14px;
}
</style>
