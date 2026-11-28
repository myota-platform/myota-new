<script setup>
// A section whose screen has not been built yet. It says which chunk builds it, so the
// console is honest about being a shell rather than showing a fake empty table that looks
// like a working feature with no data.
defineProps({
  title: { type: String, required: true },
  chunk: { type: String, required: true },
  detail: { type: String, default: '' },
})
</script>

<template>
  <div class="card">
    <div class="empty">
      <h2>{{ title }}</h2>
      <p>{{ detail || 'This screen is not built yet.' }}</p>
      <p class="tiny" style="margin-top: 10px">Arrives with chunk {{ chunk }}.</p>
    </div>
  </div>
</template>
