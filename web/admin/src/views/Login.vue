<script setup>
// Sign in: password first, then the second factor.
//
// Two steps in one view rather than two routes, because the second step is not a page a
// person can navigate to — it is bound to a challenge that only exists after a password
// was verified. Making it a route would let someone deep-link to a form that cannot work.
//
// The refusals are shown as what to do about them, not as the problem type:
//   - `#mfa-not-enrolled` → another administrator has to enrol a factor; there is no
//     self-service path, by design.
//   - 429 with "locked" → an administrator has to unlock the account, and waiting also works.
//   - 429 otherwise → too many attempts from this address; the retry time is in the body.
import { computed, onMounted, ref, nextTick } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { ApiError, NetworkError, answerChallenge, login } from '../api.js'
import { safeNext } from '../router.js'

const route = useRoute()
const router = useRouter()

const step = ref('password')
const email = ref('')
const password = ref('')
const code = ref('')
const method = ref('totp')
const challengeId = ref(null)
const methods = ref(['totp'])
const busy = ref(false)
const error = ref(null)
const codeInput = ref(null)
const passwordInput = ref(null)

const next = computed(() => safeNext(route.query.next))

/** The label under the code field, which depends on the method selected. */
const codeHint = computed(() =>
  method.value === 'recovery_code'
    ? 'One of the ten codes you were given when the factor was enrolled. Each works once.'
    : 'The six-digit code from your authenticator app.',
)

const methodLabel = (value) =>
  ({ totp: 'Authenticator app', recovery_code: 'Recovery code', webauthn: 'Passkey' })[value] ?? value

function describe(cause) {
  if (cause instanceof NetworkError) {
    return 'The API could not be reached. Check your connection and try again.'
  }
  if (cause instanceof ApiError) {
    if (cause.type === 'about:blank#mfa-not-enrolled') {
      return 'This account has no second factor yet. Another administrator has to enrol one before you can sign in.'
    }
    if (cause.status === 429) {
      return cause.message
    }
    if (cause.type === 'about:blank#unauthorized') {
      return step.value === 'password'
        ? 'That address and password do not match an account.'
        : 'That code was not accepted. Check the time on your device and try the next one.'
    }
    return cause.message
  }
  return 'Something went wrong. Try again.'
}

async function submitPassword() {
  error.value = null
  busy.value = true
  try {
    const result = await login(email.value.trim(), password.value)
    if (result?.status === 'mfa_required') {
      challengeId.value = result.challengeId
      methods.value = result.methods ?? ['totp']
      method.value = methods.value.includes('totp') ? 'totp' : methods.value[0]
      step.value = 'code'
      await nextTick()
      codeInput.value?.focus()
      return
    }
    // A passkey satisfied the challenge during sign-in.
    await router.push(next.value)
  } catch (cause) {
    error.value = describe(cause)
  } finally {
    busy.value = false
  }
}

async function submitCode() {
  error.value = null
  busy.value = true
  try {
    await answerChallenge(challengeId.value, method.value, code.value.trim())
    await router.push(next.value)
  } catch (cause) {
    error.value = describe(cause)
    code.value = ''
    await nextTick()
    codeInput.value?.focus()
  } finally {
    busy.value = false
  }
}

function back() {
  step.value = 'password'
  code.value = ''
  error.value = null
  nextTick(() => passwordInput.value?.focus())
}

onMounted(() => passwordInput.value?.focus())
</script>

<template>
  <div class="page-narrow">
    <section class="hero" style="padding: 38px 6px 22px">
      <span class="tagline">MyOTA administration</span>
      <h1 style="font-size: 30px; margin: 6px 0 10px">Sign in</h1>
      <p class="lede" style="margin: 0">
        Administrator accounts need a second factor. A password alone never produces a
        session.
      </p>
    </section>

    <div class="card">
      <div class="card-body" style="padding: 26px 26px 22px">
        <p v-if="error" class="alert" role="alert" style="margin: 0 0 18px">{{ error }}</p>

        <form v-if="step === 'password'" @submit.prevent="submitPassword" novalidate>
          <div class="field">
            <label for="email">Email address</label>
            <input
              id="email"
              ref="passwordInput"
              v-model="email"
              type="email"
              autocomplete="username"
              required
              :aria-invalid="error ? 'true' : undefined"
            />
          </div>
          <div class="field">
            <label for="password">Password</label>
            <input
              id="password"
              v-model="password"
              type="password"
              autocomplete="current-password"
              required
              :aria-invalid="error ? 'true' : undefined"
            />
          </div>
          <button class="btn" type="submit" :disabled="busy || !email || !password">
            {{ busy ? 'Checking…' : 'Continue' }}
          </button>
        </form>

        <form v-else @submit.prevent="submitCode" novalidate>
          <div class="field">
            <label for="method">Second factor</label>
            <select id="method" v-model="method">
              <option v-for="option in methods" :key="option" :value="option">
                {{ methodLabel(option) }}
              </option>
            </select>
          </div>
          <div class="field">
            <label for="code">{{ method === 'recovery_code' ? 'Recovery code' : 'Six-digit code' }}</label>
            <input
              id="code"
              ref="codeInput"
              v-model="code"
              type="text"
              inputmode="numeric"
              autocomplete="one-time-code"
              :aria-describedby="'code-hint'"
              :aria-invalid="error ? 'true' : undefined"
              required
            />
            <p id="code-hint" class="hint">{{ codeHint }}</p>
          </div>
          <div style="display: flex; gap: 10px; align-items: center">
            <button class="btn" type="submit" :disabled="busy || !code">
              {{ busy ? 'Checking…' : 'Sign in' }}
            </button>
            <button class="btn btn-ghost" type="button" @click="back">Back</button>
          </div>
        </form>
      </div>
    </div>

    <p class="tiny" style="margin-top: 18px">
      Locked out, or no second factor yet? Another administrator has to reset it — there is
      no self-service path, deliberately.
    </p>
  </div>
</template>
