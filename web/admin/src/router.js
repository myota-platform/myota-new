// Routes and the guard.
//
// The guard is the client half of the audience model, and it is a convenience only: the
// gateway refuses a participant credential on `/v1/admin/**` and `core` refuses it again,
// so hiding a screen is not what protects the data. What the guard does protect is the
// user's time — landing on a page that immediately 401s is worse than landing on the
// sign-in form with a note about where they were going.
//
// `next` is carried through the sign-in so a deep link survives. It is validated as a
// same-origin path before use: an open redirect is the classic way a login page becomes a
// phishing vector.

import { createRouter, createWebHistory } from 'vue-router'
import { refresh, session } from './api.js'

import Login from './views/Login.vue'
import MfaEnrol from './views/MfaEnrol.vue'
import Operations from './views/Operations.vue'
import Geodata from './views/Geodata.vue'
import Programmes from './views/Programmes.vue'
import Admins from './views/Admins.vue'
import Participants from './views/Participants.vue'

/** Only a path on this origin, so `?next=https://evil.example` cannot be honoured. */
export function safeNext(value) {
  if (typeof value !== 'string' || !value.startsWith('/') || value.startsWith('//')) {
    return '/operations'
  }
  return value
}

const routes = [
  { path: '/', redirect: '/operations' },
  { path: '/login', name: 'login', component: Login, meta: { title: 'Sign in', public: true } },
  { path: '/mfa/enrol', name: 'mfa-enrol', component: MfaEnrol, meta: { title: 'Second factor' } },
  { path: '/operations', name: 'operations', component: Operations, meta: { title: 'Operations' } },
  { path: '/geodata', name: 'geodata', component: Geodata, meta: { title: 'Geodata' } },
  { path: '/programmes', name: 'programmes', component: Programmes, meta: { title: 'Programmes' } },
  { path: '/admins', name: 'admins', component: Admins, meta: { title: 'Admins' } },
  {
    path: '/participants',
    name: 'participants',
    component: Participants,
    meta: { title: 'Participants' },
  },
  { path: '/:pathMatch(.*)*', redirect: '/operations' },
]

const router = createRouter({
  history: createWebHistory(),
  routes,
  scrollBehavior: () => ({ top: 0 }),
})

/**
 * Establishes the session once, then decides.
 *
 * On the very first navigation the session is unknown, so one refresh is attempted. After
 * that the in-memory session is the answer: a navigation does not re-ask the API, because
 * a router that hits the network on every click is a router that feels slow on a good day
 * and broken on a bad one.
 */
router.beforeEach(async (to) => {
  if (!session.loaded) {
    try {
      await refresh()
    } catch {
      // The API is unreachable. Send them to the form; it will report the problem there
      // rather than showing an empty console that looks signed-in.
    }
  }

  if (to.meta.public) {
    // Already signed in and asking for the sign-in form: go where they were going.
    if (session.authenticated) return safeNext(to.query.next)
    return true
  }

  if (!session.authenticated) {
    return { name: 'login', query: { next: to.fullPath } }
  }

  return true
})

router.afterEach((to) => {
  const title = to.meta.title ? `${to.meta.title} · MyOTA` : 'MyOTA administration'
  document.title = title
})

export default router
