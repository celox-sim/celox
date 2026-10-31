import assert from 'node:assert/strict';
import test from 'node:test';
import { readFileSync } from 'node:fs';
import { kernels, metrics } from './publish-rica-bench.mjs';

function report() {
  return { schemaVersion: 2, comparison: 'cold-tiered-process-v1', workload: 'board-kernel-tiered-tb-v1',
    revision: readFileSync(new URL('./rica-revision', import.meta.url), 'utf8').trim(), verylVersion: '0.22.0',
    date: '2026-10-31T00:00:00Z', celox: { commit: 'abc', dirty: false }, host: { cpu: 'test CPU', arch: 'x64' },
    binaries: { celox: 'a'.repeat(64), veryl: 'b'.repeat(64) },
    settings: { frontend: 'veryl', testbenchLanguage: 'veryl', backend: 'tiered', cache: 'cold', stateModel: 'two-state', threads: 1, optimization: 'default-tiered' },
    results: kernels.flatMap(kernel => ['celox-tiered', 'veryl-tiered'].map(runner => ({
      kernel, runner, sourceSha256: 'c'.repeat(64), status: 'measured',
      qualification: { status: 'passed', cycles: 100, commits: 50 }, negativeControl: { status: 'passed' },
      samples: [30, 10, 20].map(n => ({ status: 'passed', compileNs: n, executeNs: n * 2, totalNs: n * 3, processNs: n * 4, cycles: 100, commits: 50,
        tiered: runner === 'celox-tiered'
          ? { test: 'test_rica', tier: 'interpreter', promotion: 'pending', compiled_evaluations: '0', interpreted_evaluations: '1' }
          : { test: 'test_rica', compiled_dispatches: '0', fallback_dispatches: '1' } })),
    }))) };
}

test('rica publishes paired process medians for all five kernels with separate workload contracts', () => {
  const input = report();
  const benches = metrics(input, 'abc');
  assert.equal(benches.length, 10);
  assert.ok(benches.every(b => b.value === 80 && b.unit === 'ns' && b.range === '40–120'));
  assert.match(benches[0].name, /^collection-celox-tiered\/collection_rica_kernel-tasks_[a-f0-9]{12}_x64_total$/);
  assert.equal(benches[0].name.split('/')[1], benches[1].name.split('/')[1]);
  input.results.slice(0, 2).forEach(r => r.sourceSha256 = 'd'.repeat(64));
  assert.notEqual(metrics(input, 'abc')[0].name, benches[0].name);
  input.host.arch = 'arm64';
  assert.match(metrics(input, 'abc')[0].name, /_arm64_total$/);
});

test('rica refuses missing kernels, unpaired results, failed oracles, changed work and invalid timing', () => {
  for (const change of [
    r => r.results.pop(), r => r.results.splice(0, 2), r => r.results[1] = structuredClone(r.results[0]),
    r => r.results[0].status = 'failed', r => r.results[0].qualification.status = 'failed',
    r => r.results[0].negativeControl.status = 'failed', r => r.results[0].samples.pop(),
    r => r.results[0].sourceSha256 = 'd'.repeat(64), r => r.results[0].sourceSha256 = null,
    r => r.results[0].qualification.cycles++, r => r.results[0].samples[0].commits++,
    r => r.results[0].samples[0].processNs = 1, r => r.results[0].samples[0].totalNs = NaN,
    r => r.results[0].samples[0].status = 'failed', r => r.results[0].samples[0].tiered.promotion = 'failed',
    r => r.results[1].samples[0].tiered.fallback_dispatches = '0',
    r => r.revision = '0'.repeat(40), r => r.celox.dirty = true, r => r.celox.commit = 'old',
    r => r.binaries.celox = '', r => r.settings.threads = 2, r => r.settings.backend = 'native',
    r => r.host.arch = 'unknown', r => r.date = 'invalid', r => r.workload = 'different',
  ]) {
    const input = report(); change(input); assert.throws(() => metrics(input, 'abc'));
  }
});

test('daily rica workflow retains failures and publishes only successful master measurements', () => {
  const workflow = readFileSync(new URL('../.github/workflows/rica-bench.yml', import.meta.url), 'utf8');
  assert.match(workflow, /schedule:/);
  assert.match(workflow, /workflow_dispatch:/);
  assert.doesNotMatch(workflow, /pull_request:|push:|continue-on-error/);
  assert.match(workflow, /rustup toolchain install 1\.98\.1 .*--component rust-src/);
  assert.match(workflow, /apt-get install -y llvm-20/);
  assert.match(workflow, /bash scripts\/run-rica-bench.sh prepare/);
  assert.match(workflow, /bash scripts\/run-rica-bench.sh run/);
  assert.match(workflow, /RICA_REPEAT: "3"/);
  assert.match(workflow, /if: always\(\)/);
  assert.match(workflow, /needs: rica\n    if: github.ref == 'refs\/heads\/master'/);
  assert.match(workflow, /node scripts\/publish-rica-bench.mjs artifacts\/rica\/results.json/);
  const dashboard = readFileSync(new URL('../docs/.vitepress/components/BenchmarkDashboard.vue', import.meta.url), 'utf8');
  assert.match(dashboard, /key: "rica", label: "rica", match: \(n\) => n.startsWith\("collection_rica_"\)/);
  assert.match(dashboard, /!n.startsWith\("collection_rica_"\)/);
});
