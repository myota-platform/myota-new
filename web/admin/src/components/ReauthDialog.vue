<script setup>
// The re-authentication dialog.
//
// Shown when the API answers `403 about:blank#reauth-required`: the session is valid and
// the action is authorised, but the session has not proved within five minutes that the
// person holding it is the person who signed in. Both factors again, then the action is
// retried once.
//
// A native `<dialog>` with `showModal()` rather than a div: focus trapping, Escape, and
// the rest of the page being inert are the browser's job and it does them correctly.
import { nextTick, ref, watch } from 'vue'
import { ApiError, reauthenticate } from '../api.js'

const props = defineProps({
  open: { type: Boolean, default: false },
  /** What the user was trying to do, so the prompt explains itself. */
  action: { type: String, default: 'this change' },
})
const emit = defineEmits(['done', 'cancel'])

const dialog = ref(null)
const password = ref('')
const code = ref('')
const busy = ref(false)
const error = ref(null)
const codeInput = ref(null)

watch(
  () => props.open,
  async (isOpen) => {
    if (isOpen) {
      error.value = null
      password.value = ''
      code.value = ''
      await nextTick()
      dialog.value?.showModal()
      await nextTick()
      codeInput.value?.focus()
    } else {
      dialog.value?.close()
    }
  },
)

async function submit() {
  error.value = null
  busy.value = true
  try {
    await reauthenticate(password.value, code.value.trim())
    emit('done')
  } catch (cause) {
    if (cause instanceof ApiError && cause.type === 'about:blank#mfa-failed') {
      error.value = 'That code was not accepted. Check the time on your device and try the next one.'
    } else if (cause instanceof ApiError && cause.status === 401) {
      error.value = 'That password and code did not match. Nothing was changed.'
    } else {
      error.value = cause.message ?? 'The re-authentication failed.'
    }
    code.value = ''
    await nextTick()
    codeInput.value?.focus()
  } finally {
    busy.value = false
  }
}

function cancel() {
  emit('cancel')
}
</script>

<template>
  <dialog ref="dialog" aria-labelledby="reauth-title" @cancel.prevent="cancel" @close="cancel">
    <form method="dialog" @submit.prevent="submit">
      <h2 id="reauth-title">Confirm it is you</h2>
      <p class="hint" style="margin-top: 0">
        {{ action }} needs a second confirmation within five minutes of signing in. Enter
        your password and a code.
      </p>

      <p v-if="error" class="alert" role="alert">{{ error }}</p>

      <div class="field">
        <label for="reauth-password">Password</label>
        <input
          id="reauth-password"
          v-model="password"
          type="password"
          autocomplete="current-password"
          required
        />
      </div>
      <div class="field">
        <label for="reauth-code">Code from your authenticator</label>
        <input
          id="reauth-code"
          ref="codeInput"
          v-model="code"
          type="text"
          inputmode="numeric"
          autocomplete="one-time-code"
          required
        />
      </div>

      <div style="display: flex; gap: 10px; justify-content: flex-end; margin-top: 18px">
        <button class="btn btn-ghost" type="button" @click="cancel">Cancel</button>
        <button class="btn" type="submit" :disabled="busy || !password || !code">
          {{ busy ? 'Checking…' : 'Confirm and continue' }}
        </button>
      </div>
    </form>
  </dialog>
</template>

<style scoped>
dialog {
  border: none;
  border-radius: var(--r-lg);
  padding: 26px 26px 22px;
  background: var(--card);
  color: var(--ink);
  box-shadow: var(--sh-3);
  max-width: 420px;
  width: calc(100vw - 44px);
}
dialog::backdrop {
  background: rgba(52, 42, 32, 0.42);
}
h2 {
  margin: 0 0 6px;
  font-size: 20px;
}
</style>
