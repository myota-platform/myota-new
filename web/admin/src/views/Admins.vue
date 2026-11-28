<script setup>
// Administrators: the shell. Chunk A4 fills it.
import SectionPage from '../components/SectionPage.vue'
import PendingSection from '../components/PendingSection.vue'
</script>

<template>
  <SectionPage
    tagline="Admins"
    title="Who can change what others may do."
    lede="Every change here needs a second factor and re-authentication within five minutes, and cannot grant a scope the actor does not hold."
  >
    <PendingSection title="Admin accounts" chunk="A4" />
  </SectionPage>
</template>
