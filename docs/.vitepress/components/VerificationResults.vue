<script setup>
import { ref, onMounted } from 'vue'
import { withBase } from 'vitepress'
import snapshot from '../../verification/sv-tests.json'
const report = ref(snapshot)
onMounted(async () => {
  try {
    const response = await fetch(withBase('/verification-data/latest.json'), { cache: 'no-store' })
    if (response.ok) {
      const latest = await response.json()
      if (latest.complete === true && latest.measured_at > report.value.measured_at) report.value = latest
    }
  } catch { /* The reviewed snapshot remains available offline. */ }
})
</script>
<template>
  <p>Measured: {{ report.measured_at }}<br>Driver profile: {{ report.driver_profile }}<br>Celox: <code>{{ report.celox_revision }}</code><br>sv-tests: <code>{{ report.revision }}</code></p>
  <p v-if="report.run_url"><a :href="report.run_url">Daily run and detailed artifacts</a></p>
  <table>
    <thead><tr><th>Group</th><th>State</th><th>Pass</th><th>Fail</th><th>Skip</th><th>Pass / executed</th></tr></thead>
    <tbody><template v-for="(states, group) in report.groups" :key="group"><tr v-for="(counts, state) in states" :key="state"><td>{{ group }}</td><td>{{ state }}</td><td>{{ counts.passed }}</td><td>{{ counts.failed }}</td><td>{{ counts.skipped }}</td><td>{{ counts.pass_percent }}%</td></tr></template></tbody>
  </table>
  <details v-for="(states, group) in report.groups" :key="group">
    <summary>{{ group }}: execution modes and failure diagnostics</summary>
    <div v-for="(counts, state) in states" :key="state">
      <h4>{{ state }}</h4>
      <table>
        <thead><tr><th>Execution mode</th><th>Pass</th><th>Fail</th></tr></thead>
        <tbody><tr v-for="(totals, mode) in counts.by_execution_mode" :key="mode"><td>{{ mode }}</td><td>{{ totals.pass || 0 }}</td><td>{{ totals.fail || 0 }}</td></tr></tbody>
      </table>
      <pre>{{ JSON.stringify(counts.by_failure_category, null, 2) }}</pre>
    </div>
  </details>
</template>
