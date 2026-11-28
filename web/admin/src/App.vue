<script setup>
// The shell: skip link, header with the five-section nav, and the router view.
//
// The nav is hidden on the sign-in routes — a list of sections that all bounce back to
// the form is noise — and every item carries `aria-current="page"` when it is the one
// being shown, which is what a screen reader announces and what the design system styles.
import { computed } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { logout, session } from './api.js'

const route = useRoute()
const router = useRouter()

const sections = [
  { path: '/operations', label: 'Operations' },
  { path: '/geodata', label: 'Geodata' },
  { path: '/programmes', label: 'Programmes' },
  { path: '/admins', label: 'Admins' },
  { path: '/participants', label: 'Participants' },
]

const showChrome = computed(() => session.authenticated && route.name !== 'login')

const initials = computed(() => {
  const id = session.accountId ?? ''
  return id.slice(0, 2).toUpperCase()
})

async function signOut() {
  await logout()
  await router.push({ name: 'login' })
}
</script>

<template>
  <a class="skip" href="#main">Skip to content</a>

  <header v-if="showChrome" class="hdr">
    <div class="hdr-in">
      <span class="brand"><span class="leaf" aria-hidden="true">M</span>MyOTA</span>
      <nav aria-label="Sections">
        <router-link
          v-for="section in sections"
          :key="section.path"
          :to="section.path"
          :aria-current="route.path === section.path ? 'page' : undefined"
        >
          {{ section.label }}
        </router-link>
      </nav>
      <span class="who">
        <span class="face" aria-hidden="true">{{ initials }}</span>
        <span class="sr-only">Signed in as </span>
        <span class="who-id">{{ session.accountId }}</span>
      </span>
      <button class="btn btn-ghost" type="button" @click="signOut">Sign out</button>
    </div>
  </header>

  <main id="main" :class="showChrome ? 'page' : ''">
    <router-view />
  </main>
</template>

<style scoped>
.who-id {
  font-family: var(--num);
  font-size: 11.5px;
  color: var(--ink-2);
}
.sr-only {
  position: absolute;
  width: 1px;
  height: 1px;
  padding: 0;
  margin: -1px;
  overflow: hidden;
  clip: rect(0, 0, 0, 0);
  white-space: nowrap;
  border: 0;
}
@media (max-width: 760px) {
  .who-id {
    display: none;
  }
}
</style>
