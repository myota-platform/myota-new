<script setup>
// Geodata: the shell. Chunk A7 fills it.
import SectionPage from '../components/SectionPage.vue'
import PendingSection from '../components/PendingSection.vue'
</script>

<template>
  <SectionPage
    tagline="Geodata"
    title="The geodata screen arrives with chunk A7."
    lede="The route, the navigation entry and the session guard exist now; the screen itself is the next chunk's work."
  >
    <PendingSection title="Geodata" chunk="A7" />
  </SectionPage>
</template>
