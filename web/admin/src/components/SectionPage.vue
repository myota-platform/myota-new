<script setup>
// The frame every section shares: a hero with the section's tagline, and a slot for the
// card below it. One component rather than four copies, so the heading level, the
// landmark and the spacing are decided once.
defineProps({
  tagline: { type: String, required: true },
  title: { type: String, required: true },
  lede: { type: String, default: '' },
})
</script>

<template>
  <div>
    <section class="hero" style="padding: 30px 36px">
      <span class="tagline">{{ tagline }}</span>
      <h1 style="font-size: 30px; margin: 6px 0 10px">{{ title }}</h1>
      <p v-if="lede" class="lede" style="margin: 0">{{ lede }}</p>
    </section>
    <slot />
  </div>
</template>
