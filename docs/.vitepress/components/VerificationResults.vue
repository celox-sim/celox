<script setup>
import { computed, onMounted, ref } from 'vue'
import { useData, withBase } from 'vitepress'
import { data as snapshots } from '../../verification/results.data.mjs'
const { lang } = useData()
const japanese = computed(() => lang.value.startsWith('ja'))
const reports = ref(snapshots)
const selected = ref('main')
const state = ref('both')
const query = ref('')
const failuresOnly = ref(false)
const names = { main: 'Main tests', cores: 'Cores', imported: 'Imported' }
const states = computed(() => state.value === 'both' ? ['2state', '4state'] : [state.value])
const report = computed(() => reports.value[selected.value])
const counts = computed(() => report.value.groups?.[selected.value] ?? {})
const labels = snapshots.main.labels
const sourceLabel = computed(() => report.value.source_kind === 'reviewed_baseline'
  ? (japanese.value ? 'レビュー済み期待値（再測定ではありません）' : 'Reviewed baseline — not a fresh run')
  : (japanese.value ? '公式 runner による測定' : 'Official runner measurement'))
const rows = computed(() => {
  const tags = new Set(states.value.flatMap(mode => [...Object.keys(counts.value[mode]?.by_tag ?? {}), ...Object.keys(counts.value[mode]?.skipped_by_tag ?? {})]))
  const search = query.value.toLowerCase().trim()
  return [...tags].sort((a, b) => a.localeCompare(b, undefined, { numeric: true })).filter(tag => {
    if (search && !`${tag} ${labels[tag]?.title ?? ''}`.toLowerCase().includes(search)) return false
    return !failuresOnly.value || states.value.some(mode => {
      const total = counts.value[mode]?.by_tag?.[tag]
      return total && total.passed < total.executed
    })
  }).map(tag => ({ tag, title: labels[tag]?.title ?? tag }))
})
const totalFor = (tag, mode) => {
  const total = counts.value[mode]?.by_tag?.[tag]
  const skipped = counts.value[mode]?.skipped_by_tag?.[tag] ?? 0
  return total || skipped ? { passed: 0, executed: 0, ...total, skipped } : null
}
const percent = total => total?.executed ? Math.round(1000 * total.passed / total.executed) / 10 : null
const scoreClass = total => !total?.executed ? '' : total.passed === total.executed ? 'all-pass' : total.passed === 0 ? 'no-pass' : 'some-pass'
onMounted(async () => {
  await Promise.all(['imported', 'cores'].map(async group => {
    try {
      // Each group has an independent publication time; an imported run cannot
      // replace main's reviewed baseline or erase a separately measured core run.
      const url = group === 'imported' ? '/verification-data/latest.json' : '/verification-data/cores.json'
      const response = await fetch(withBase(url), { cache: 'no-store' })
      if (!response.ok) return
      const latest = await response.json()
      if (latest.complete === true && latest.groups?.[group]
          && latest.measured_at > (reports.value[group]?.measured_at ?? '')) {
        reports.value = { ...reports.value, [group]: latest }
      }
    } catch { /* Keep the checked-in measurement when offline. */ }
  }))
})
</script>

<template>
  <section class="verification-dashboard" aria-label="Celox sv-tests results">
    <div class="suite-tabs" role="tablist" aria-label="sv-tests group">
      <button v-for="(name, group) in names" :key="group" type="button" role="tab"
        :aria-selected="selected === group" @click="selected = group">{{ name }}</button>
    </div>
    <p class="source-kind">{{ sourceLabel }}</p>
    <div class="score-cards">
      <article v-for="mode in states" :key="mode" class="score-card">
        <h3>Celox · {{ mode }}</h3>
        <template v-if="counts[mode]">
          <strong>{{ counts[mode].pass_percent ?? '—' }}{{ counts[mode].executed ? '%' : '' }}</strong>
          <p>{{ counts[mode].passed }} / {{ counts[mode].executed }} {{ japanese ? '成功' : 'passed' }}</p>
          <p v-if="counts[mode].upstream_input_issues" class="muted">{{ counts[mode].upstream_input_issues }} {{ japanese ? '件は upstream の入力不備' : 'upstream input issues' }}</p>
          <p class="muted">{{ counts[mode].failed }} {{ japanese ? '失敗' : 'failed' }} · {{ counts[mode].skipped }} {{ japanese ? 'スキップ' : 'skipped' }}</p>
        </template>
        <p v-else>{{ japanese ? '測定結果なし' : 'No measurement' }}</p>
      </article>
    </div>
    <details class="provenance">
      <summary>{{ japanese ? '結果の出典・実行条件' : 'Source and execution conditions' }}</summary>
      <dl>
        <dt>{{ japanese ? '記録日時' : 'Recorded at' }}</dt><dd>{{ report.measured_at ?? report.recorded_at ?? '—' }}</dd>
        <dt>{{ report.source_kind === 'reviewed_baseline' ? 'Baseline source revision' : 'Celox revision' }}</dt><dd><code>{{ report.baseline_revision ?? report.celox_revision ?? '—' }}</code></dd>
        <dt>sv-tests revision</dt><dd><code>{{ report.revision }}</code></dd>
        <dt v-if="report.driver_profile">Driver profile</dt><dd v-if="report.driver_profile">{{ report.driver_profile }}</dd>
      </dl>
      <a v-if="report.run_url" :href="report.run_url">{{ japanese ? '実行ログ・詳細 artifact' : 'Run logs and detailed artifacts' }}</a>
      <a v-if="report.source_kind === 'reviewed_baseline'" :href="`https://github.com/celox-sim/celox/blob/${report.baseline_revision}/conformance/sv-tests/expected.tsv`">expected.tsv</a>
    </details>
    <div class="result-filters">
      <label>{{ japanese ? 'タグ・機能を検索' : 'Search tags and features' }}<input v-model="query" type="search" placeholder="enum, 6.19, ibex…"></label>
      <label>{{ japanese ? '状態モード' : 'State mode' }}<select v-model="state"><option value="both">2state + 4state</option><option>2state</option><option>4state</option></select></label>
      <label class="checkbox"><input v-model="failuresOnly" type="checkbox">{{ japanese ? '失敗を含むタグのみ' : 'Tags with failures only' }}</label>
    </div>
    <p class="muted">{{ rows.length }} {{ japanese ? 'タグ。複数タグに属するケースは各行に重複します。' : 'tags. A case with multiple tags appears in multiple rows.' }}</p>
    <div class="results-scroll">
      <table class="tag-results">
        <thead><tr><th>{{ japanese ? '機能・テスト群' : 'Feature / test suite' }}</th><th>Tag</th><th v-for="mode in states" :key="mode">Celox {{ mode }}</th></tr></thead>
        <tbody><tr v-for="row in rows" :key="row.tag">
          <th scope="row">{{ row.title }}</th><td><code>{{ row.tag }}</code></td>
          <td v-for="mode in states" :key="mode" :class="scoreClass(totalFor(row.tag, mode))">
            <template v-if="totalFor(row.tag, mode)"><span>{{ totalFor(row.tag, mode).passed }} / {{ totalFor(row.tag, mode).executed }}</span><small>{{ percent(totalFor(row.tag, mode)) === null ? '—' : `${percent(totalFor(row.tag, mode))}%` }}</small><small v-if="totalFor(row.tag, mode).skipped">{{ totalFor(row.tag, mode).skipped }} skipped</small></template>
            <span v-else>—</span>
          </td>
        </tr></tbody>
      </table>
      <p v-if="!rows.length">{{ japanese ? '条件に一致するタグはありません。' : 'No tags match these filters.' }}</p>
    </div>
    <details v-if="selected === 'cores' && report.cases?.length" class="execution-details">
      <summary>{{ japanese ? 'コア別の判定と診断' : 'Individual core verdicts and diagnostics' }}</summary>
      <div class="results-scroll"><table class="tag-results">
        <thead><tr><th>Case</th><th>State</th><th>Result</th><th>Mode / diagnostic</th></tr></thead>
        <tbody><tr v-for="item in report.cases.filter(item => states.includes(item.state_mode))" :key="`${item.test}-${item.state_mode}`">
          <th scope="row">{{ item.test.replace(/^generated\//, '') }}</th><td>{{ item.state_mode }}</td><td>{{ item.result }}</td>
          <td>{{ item.input_issue ? (japanese ? 'upstream 入力不備' : 'upstream input issue') : item.diagnostic || item.mode || (item.result === 'skip' ? 'upstream compatible-runners filter' : '—') }}</td>
        </tr></tbody>
      </table></div>
    </details>
    <details v-if="report.source_kind !== 'reviewed_baseline'" class="execution-details">
      <summary>{{ japanese ? '実行モード・診断の内訳' : 'Execution modes and diagnostic categories' }}</summary>
      <div v-for="mode in states" :key="mode">
        <h3>{{ mode }}</h3>
        <table><thead><tr><th>Mode</th><th>Pass</th><th>Fail</th></tr></thead><tbody><tr v-for="(totals, kind) in counts[mode]?.by_execution_mode" :key="kind"><td>{{ kind }}</td><td>{{ totals.pass ?? 0 }}</td><td>{{ totals.fail ?? 0 }}</td></tr></tbody></table>
        <table><thead><tr><th>{{ japanese ? '診断分類' : 'Diagnostic category' }}</th><th>{{ japanese ? '件数' : 'Cases' }}</th></tr></thead><tbody><tr v-for="(number, category) in counts[mode]?.by_failure_category" :key="category"><td>{{ category }}</td><td>{{ number }}</td></tr></tbody></table>
      </div>
    </details>
  </section>
</template>

<style scoped>
.suite-tabs { display: flex; gap: 8px; flex-wrap: wrap; margin: 24px 0 12px; }
.suite-tabs button { border: 1px solid var(--vp-c-divider); border-radius: 8px; padding: 8px 18px; font-weight: 600; }
.suite-tabs button[aria-selected="true"] { background: var(--vp-c-brand-1); color: var(--vp-c-white); border-color: var(--vp-c-brand-1); }
.source-kind, .muted { font-size: 13px; color: var(--vp-c-text-2); }
.score-cards { display: grid; grid-template-columns: repeat(auto-fit, minmax(210px, 1fr)); gap: 16px; }
.score-card { padding: 20px; border-radius: 12px; background: var(--vp-c-bg-soft); border: 1px solid var(--vp-c-divider); }
.score-card h3 { margin: 0 0 16px; font-size: 15px; }
.score-card strong { font-size: 36px; line-height: 1.1; }
.score-card p { margin: 8px 0 0; }
.provenance { margin: 20px 0; font-size: 13px; }
summary { cursor: pointer; font-weight: 600; }
dl { display: grid; grid-template-columns: auto minmax(0, 1fr); gap: 8px 16px; }
dd { margin: 0; overflow-wrap: anywhere; }
dd code { font-size: 11px; }
.result-filters { display: flex; flex-wrap: wrap; gap: 16px; align-items: end; margin-top: 24px; }
.result-filters label { display: flex; flex-direction: column; gap: 4px; font-size: 13px; font-weight: 500; }
.result-filters input[type="search"], select { border: 1px solid var(--vp-c-divider); border-radius: 6px; padding: 6px 10px; background: var(--vp-c-bg); color: var(--vp-c-text-1); }
.result-filters .checkbox { flex-direction: row; align-items: center; padding-bottom: 6px; }
.results-scroll { overflow-x: auto; }
.tag-results { display: table; width: 100%; font-size: 13px; }
.tag-results th, .tag-results td { padding: 9px 12px; }
.tag-results tbody th { font-weight: 400; text-align: left; }
.tag-results td:nth-child(n+3) { text-align: center; white-space: nowrap; }
.tag-results small { display: block; opacity: .8; }
.all-pass { background: var(--vp-c-green-soft); }
.no-pass { background: var(--vp-c-red-soft); }
.some-pass { background: var(--vp-c-yellow-soft); }
.execution-details { margin: 24px 0; }
button:focus-visible, input:focus-visible, select:focus-visible, summary:focus-visible { outline: 2px solid var(--vp-c-brand-1); outline-offset: 3px; }
@media (max-width: 600px) { dl { grid-template-columns: 1fr; gap: 4px; } dd { margin-bottom: 8px; } .result-filters label:first-child { width: 100%; } }
</style>
