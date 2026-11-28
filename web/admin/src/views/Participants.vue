<script setup>
// Participant accounts: the shell. Chunk A5 fills it.
import SectionPage from '../components/SectionPage.vue'
import PendingSection from '../components/PendingSection.vue'
</script>

<template>
  <SectionPage
    tagline="Participants"
    title="Participant support arrives with chunk A5."
    lede="Callsign evidence, registration state and account closure are the day-to-day work at volume; the route and the guard exist now."
  >
    <PendingSection
      title="Participant accounts"
      chunk="A5"
      detail="List, search, callsign evidence and account state. This section cannot grant authority, by design."
    />
  </SectionPage>
</template>
