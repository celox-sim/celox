import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import test from "node:test";
import { rtlmeterMetrics } from "./publish-rtlmeter-bench.mjs";

const manifest = JSON.parse(readFileSync(new URL("../benchmarks/rtlmeter/manifest.json", import.meta.url)));
const locked = JSON.parse(readFileSync(new URL("../flake.lock", import.meta.url))).nodes.xezim;
const commit = 'a'.repeat(40);
function report() {
  const inputSha256 = Object.fromEntries([
    ...Array.from({ length: 110 }, (_, i) => `designs/XuanTie-E902/src/${i}.v`),
    'designs/XuanTie-E902/descriptor.yaml', 'designs/XuanTie-E902/tests/hello/case.pat',
    'rtl/__rtlmeter_utils.sv', 'rtl/__rtlmeter_top_include.vh',
  ].sort().map(path => [path, 'b'.repeat(64)]));
  return { schemaVersion: 1, contract: manifest.contract, case: 'e902', status: 'completed',
    rtlmeterRevision: manifest.revision, celoxCommit: commit, dirty: false,
    requestedSamples: 3, maxCycles: manifest.cases.e902.maxCycles, host: { architecture: 'x86_64', cpu: 'test CPU', affinityCpu: 0 },
    xezimRelease: locked.original.ref, xezimRevision: locked.locked.rev,
    versions: { celox: 'celox 0.12.0', verilator: 'Verilator 5.052', xezim: `xezim version ${locked.original.ref.replace(/^v/, '')}\ngit unknown` },
    binarySha256: { celox: 'a'.repeat(64), verilator: 'b'.repeat(64), xezim: 'c'.repeat(64) },
    configuration: { celox: 'native O2; threads=1; cold source-to-finish',
      verilator: '--binary --timing --assert; -O3; C++ -O3; -j1; fresh Mdir',
      xezim: 'release default VM; persistent caches disabled; full four-state' },
    inputSha256, sourceSha256: createHash('sha256').update(JSON.stringify(inputSha256)).digest('hex'),
    results: ['two-state', 'four-state'].flatMap(stateModel => ['celox', 'verilator', 'xezim'].map(engine => ({
      engine, stateModel, experimental: engine === 'verilator' && stateModel === 'four-state',
      ...(engine === 'xezim' && stateModel === 'two-state' ? {
        status: 'unsupported', reason: 'No global two-state mode', samples: [],
      } : { status: 'passed', probe: 'passed', negativeControl: 'passed',
        samples: [3e9, 1e9, 2e9].map(processNs => ({ status: 'passed', processNs, cycles: 5392 })) }),
    }))),
  };
}

test('RTLMeter uses median cold totals with separate state identities', () => {
  const metrics = rtlmeterMetrics(report(), commit);
  assert.equal(metrics.length, 5);
  assert.ok(metrics.every(metric => metric.value === 2e6 && metric.unit === 'us'));
  assert.ok(metrics.some(metric => metric.name.includes('_two-state_')));
  assert.ok(metrics.some(metric => metric.name.includes('_four-state_')));
  assert.ok(!metrics.some(metric => metric.name.startsWith('rtlmeter-xezim/') && metric.name.includes('_two-state_')));
});

test('unavailable configurations never become zero or partial performance samples', () => {
  const r = report();
  r.results[0].status = 'failed'; r.results[0].reason = 'TEST FAILED';
  r.results[0].samples = r.results[0].samples.slice(0, 1);
  assert.equal(rtlmeterMetrics(r, commit).length, 4);
});

test('publisher rejects stale, incomplete, warm, unqualified or inconsistent comparisons', () => {
  for (const mutate of [
    r => { r.celoxCommit = 'wrong'; }, r => { r.dirty = true; },
    r => { r.rtlmeterRevision = 'wrong'; }, r => { r.xezimRevision = 'wrong'; },
    r => { r.results.pop(); }, r => { r.results[1] = r.results[0]; },
    r => { r.results[0].samples.pop(); }, r => { r.results[0].probe = 'failed'; },
    r => { r.results[0].negativeControl = 'failed'; },
    r => { r.results[0].samples[0].processNs = 0; },
    r => { r.results[0].samples.forEach(s => { s.cycles += 10; }); },
    r => { r.configuration.verilator = 'prebuilt'; },
    r => { r.maxCycles = 1; },
    r => { delete r.inputSha256['rtl/__rtlmeter_utils.sv']; },
  ]) {
    const r = report(); mutate(r);
    assert.throws(() => rtlmeterMetrics(r, commit));
  }
});

test('RTLMeter runs daily/manual with locked tools, raw failures and master-only publication', () => {
  const workflow = readFileSync(new URL('../.github/workflows/rtlmeter-bench.yml', import.meta.url), 'utf8');
  assert.match(workflow, /schedule:[\s\S]+workflow_dispatch:/);
  assert.doesNotMatch(workflow, /^\s+(pull_request|push|merge_group):/m);
  assert.match(workflow, /cancel-in-progress: false/);
  assert.match(workflow, /fail-fast: false/);
  assert.match(workflow, /case: \[e902, e906\]/);
  assert.match(workflow, /nix build --no-update-lock-file \.#xezim/);
  assert.match(workflow, /revision: ea338be98e1e838d3518809ce8899f85a009963c/);
  assert.match(workflow, /needs: comparison\n    if: github.ref == 'refs\/heads\/master'/);
  assert.doesNotMatch(workflow, /continue-on-error|--no-verify/);
  assert.match(workflow, /if: always\(\)[\s\S]+retention-days: 30/);
  assert.ok(workflow.indexOf('--validate-only') < workflow.lastIndexOf('node scripts/publish-rtlmeter-bench.mjs'));
});
