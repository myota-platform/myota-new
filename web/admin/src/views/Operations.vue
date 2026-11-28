<script setup>
// Operations: the shell. Chunk A8 fills it.
import SectionPage from '../components/SectionPage.vue'
import PendingSection from '../components/PendingSection.vue'
</script>

<template>
  <SectionPage
    tagline="Operations"
    title="The operations screen arrives with chunk A8."
    lede="The route, the navigation entry and the session guard exist now; the screen itself is the next chunk's work."
  >
    <PendingSection title="Operations" chunk="A8" />
  </SectionPage>
</template>
