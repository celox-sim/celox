import { readFileSync } from 'node:fs'

export function readBaseline(tsv, metadata, provenance = {}) {
  const lines = tsv.trim().split(/\r?\n/)
  if (lines.shift() !== 'test\tstate_mode\tresult') throw new Error('Invalid sv-tests baseline header')
  const groups = { main: {} }
  const seen = new Set()
  for (const line of lines) {
    const [test, state, result, extra] = line.split('\t')
    const key = `${test}\t${state}`
    if (extra !== undefined || !['2state', '4state'].includes(state) || !['pass', 'fail', 'skip'].includes(result) || seen.has(key)) throw new Error(`Invalid baseline row: ${line}`)
    if (!metadata.cases[test]) throw new Error(`No pinned metadata for ${test}`)
    seen.add(key)
    const counts = groups.main[state] ??= { passed: 0, failed: 0, skipped: 0, executed: 0, by_tag: {} }
    counts[{ pass: 'passed', fail: 'failed', skip: 'skipped' }[result]]++
    if (result === 'skip') continue
    counts.executed++
    for (const tag of metadata.cases[test]) {
      const totals = counts.by_tag[tag] ??= { passed: 0, executed: 0 }
      totals.executed++
      if (result === 'pass') totals.passed++
    }
  }
  for (const state of ['2state', '4state']) {
    const counts = groups.main[state]
    if (!counts || counts.passed + counts.failed + counts.skipped !== Object.keys(metadata.cases).length) throw new Error(`Incomplete baseline for ${state}`)
    counts.pass_percent = counts.executed ? Math.round(10000 * counts.passed / counts.executed) / 100 : null
    counts.tags_tested = Object.keys(counts.by_tag).length
    counts.tags_passed = Object.values(counts.by_tag).filter(tag => tag.passed === tag.executed).length
  }
  return { revision: metadata.revision, groups, labels: metadata.labels, source_kind: 'reviewed_baseline', ...provenance }
}

export function readBaselineFile(path, metadata, provenance) {
  return readBaseline(readFileSync(path, 'utf8'), metadata, provenance)
}
