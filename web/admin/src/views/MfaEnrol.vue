<script setup>
// Second-factor enrolment.
//
// Three steps: ask for a pending secret, show it once, confirm with a code. The secret and
// the recovery codes are shown exactly once — the seed is stored encrypted and the codes
// are stored hashed, so this screen is the only chance to write them down. That is said
// plainly rather than in a footnote, because it is the one irreversible thing here.
import { ref } from 'vue'
import { useRouter } from 'vue-router'
import { ApiError, api } from '../api.js'

const router = useRouter()

const step = ref('idle')
const enrolment = ref(null)
const code = ref('')
const busy = ref(false)
const error = ref(null)
const copied = ref(false)

async function start() {
  error.value = null
  busy.value = true
  try {
    enrolment.value = await api('/v1/admin/auth/mfa/enrol', {
      method: 'POST',
      body: { method: 'totp' },
    })
    step.value = 'show'
  } catch (cause) {
    error.value =
      cause instanceof ApiError && cause.status === 409
        ? 'This account already has a confirmed second factor. Another administrator has to reset it before a new one can be enrolled.'
        : cause.message ?? 'The enrolment could not be started.'
  } finally {
    busy.value = false
  }
}

async function copySecret() {
  try {
    await navigator.clipboard.writeText(enrolment.value.secret)
    copied.value = true
  } catch {
    // Clipboard access is denied in some contexts; the value is selectable in the page.
    copied.value = false
  }
}

async function confirm() {
  error.value = null
  busy.value = true
  try {
    await api('/v1/admin/auth/mfa/enrol/confirm', {
      method: 'POST',
      body: { code: code.value.trim() },
    })
    step.value = 'done'
  } catch (cause) {
    error.value =
      cause instanceof ApiError && cause.type === 'about:blank#mfa-failed'
        ? 'That code was not accepted. Check the time on your device and try the next one.'
        : cause.message ?? 'The code could not be confirmed.'
  } finally {
    busy.value = false
  }
}
</script>

<template>
  <div class="page-narrow">
    <section class="hero" style="padding: 38px 6px 22px">
      <span class="tagline">Account</span>
      <h1 style="font-size: 30px; margin: 6px 0 10px">Second factor</h1>
      <p class="lede" style="margin: 0">
        An administrator account cannot sign in without a confirmed authenticator. This
        enrols one.
      </p>
    </section>

    <p v-if="error" class="alert" role="alert">{{ error }}</p>

    <div v-if="step === 'idle'" class="card">
      <div class="card-body" style="padding: 26px">
        <p style="margin-top: 0">
          You will be shown a secret to add to your authenticator app, and ten recovery
          codes. Both are shown once.
        </p>
        <button class="btn" type="button" :disabled="busy" @click="start">
          {{ busy ? 'Preparing…' : 'Begin enrolment' }}
        </button>
      </div>
    </div>

    <div v-else-if="step === 'show'" class="card">
      <div class="card-body" style="padding: 26px">
        <h2 style="margin-top: 0; font-size: 18px">1 · Add the secret</h2>
        <p style="margin-top: 0; font-size: 14px; color: var(--ink-2)">
          Scan the URI as a QR code, or type the secret into your app by hand.
        </p>
        <p class="code" aria-label="Secret">{{ enrolment.secret }}</p>
        <button class="btn btn-ghost" type="button" style="margin-top: 10px" @click="copySecret">
          {{ copied ? 'Copied' : 'Copy secret' }}
        </button>
        <p class="code" style="margin-top: 12px" aria-label="otpauth URI">{{ enrolment.otpauthUri }}</p>

        <h2 style="margin-top: 26px; font-size: 18px">2 · Write down the recovery codes</h2>
        <p style="margin-top: 0; font-size: 14px; color: var(--ink-2)">
          Each works once, and they are stored hashed — this is the only time they are shown.
        </p>
        <ul class="code-list">
          <li v-for="recovery in enrolment.recoveryCodes" :key="recovery">{{ recovery }}</li>
        </ul>

        <h2 style="margin-top: 26px; font-size: 18px">3 · Prove it works</h2>
        <form @submit.prevent="confirm" novalidate>
          <div class="field">
            <label for="confirm-code">Code from the app</label>
            <input
              id="confirm-code"
              v-model="code"
              type="text"
              inputmode="numeric"
              autocomplete="one-time-code"
              required
            />
          </div>
          <button class="btn" type="submit" :disabled="busy || !code">
            {{ busy ? 'Confirming…' : 'Confirm and finish' }}
          </button>
        </form>
      </div>
    </div>

    <div v-else class="card">
      <div class="card-body" style="padding: 26px">
        <p class="notice" role="status" style="margin-top: 0">
          The second factor is confirmed. It is required at your next sign-in.
        </p>
        <button class="btn" type="button" @click="router.push('/operations')">
          Back to operations
        </button>
      </div>
    </div>
  </div>
</template>
