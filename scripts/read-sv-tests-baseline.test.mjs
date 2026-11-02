import { test } from 'node:test'
import assert from 'node:assert/strict'
import { readBaseline } from './read-sv-tests-baseline.mjs'
const metadata = { revision: 'pinned', cases: { 'a.sv': ['one', 'shared'], 'b.sv': ['shared'] }, labels: {} }
const header = 'test\tstate_mode\tresult\n'
const rows = 'a.sv\t2state\tpass\nb.sv\t2state\tfail\na.sv\t4state\tpass\nb.sv\t4state\tskip\n'
test('keeps states separate, excludes skips and counts overlapping tags', () => {
  const report = readBaseline(header + rows, metadata)
  assert.equal(report.groups.main['2state'].pass_percent, 50)
  assert.equal(report.groups.main['4state'].pass_percent, 100)
  assert.deepEqual(report.groups.main['2state'].by_tag.shared, { passed: 1, executed: 2 })
  assert.equal(report.groups.main['2state'].tags_passed, 1)
  assert.equal(report.source_kind, 'reviewed_baseline')
})
test('rejects incomplete, duplicate, unknown and invalid verdict rows', () => {
  for (const value of [rows.split('\n').slice(1).join('\n'), rows + 'a.sv\t2state\tpass\n', rows.replace('a.sv', 'unknown.sv'), rows.replace('pass', 'broken')]) {
    assert.throws(() => readBaseline(header + value, metadata))
  }
})
