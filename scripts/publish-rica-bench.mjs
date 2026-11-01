#!/usr/bin/env node
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { parseTiered, runners } from './tiered-benchmark.mjs';
import { publishResult } from './publish-heliodor-bench.mjs';

export const kernels = ['kernel-tasks', 'kernel-syscalls', 'kernel-atomics', 'kernel-user', 'kernel-supervisor'];
const pinnedRevision = readFileSync(new URL('./rica-revision', import.meta.url), 'utf8').trim();
const digest = value => /^[a-f0-9]{64}$/.test(value ?? '');
const positive = value => Number.isSafeInteger(value) && value > 0;
const settings = { frontend: 'veryl', testbenchLanguage: 'veryl', backend: 'tiered', cache: 'cold', stateModel: 'two-state', threads: 1, optimization: 'default-tiered' };

export function metrics(report, commit) {
  if (report.schemaVersion !== 2 || report.comparison !== 'cold-tiered-process-v1' ||
      report.workload !== 'board-kernel-tiered-tb-v1' || report.verylVersion !== '0.22.0' ||
      report.revision !== pinnedRevision || report.celox?.commit !== commit || report.celox?.dirty !== false ||
      !report.host?.cpu || !['x64', 'arm64'].includes(report.host.arch) ||
      !digest(report.binaries?.celox) || !digest(report.binaries?.veryl) ||
      !Number.isFinite(Date.parse(report.date)) ||
      Object.entries(settings).some(([key, value]) => report.settings?.[key] !== value) ||
      report.results?.length !== kernels.length * runners.length) throw new Error('Invalid or incomplete rica report');
  const benches = [];
  for (const kernel of kernels) {
    const pair = report.results.filter(result => result.kernel === kernel);
    if (pair.length !== 2 || !runners.every(runner => pair.some(result => result.runner === runner)) ||
        pair[0].sourceSha256 !== pair[1].sourceSha256 ||
        pair[0].qualification?.cycles !== pair[1].qualification?.cycles ||
        pair[0].qualification?.commits !== pair[1].qualification?.commits) throw new Error('Incomplete or mismatched rica pair');
    for (const result of pair) {
      if (result.status !== 'measured' || result.qualification?.status !== 'passed' ||
          result.negativeControl?.status !== 'passed' || !digest(result.sourceSha256) ||
          !positive(result.qualification.cycles) || !positive(result.qualification.commits) ||
          result.samples?.length !== 3) throw new Error('Unqualified rica measurement');
      for (const sample of result.samples) {
        if (sample.status !== 'passed' || ![sample.compileNs, sample.executeNs, sample.totalNs, sample.processNs].every(positive) ||
            sample.compileNs + sample.executeNs > sample.totalNs || sample.processNs < sample.totalNs ||
            sample.cycles !== result.qualification.cycles || sample.commits !== result.qualification.commits) throw new Error('Invalid rica sample');
        const prefix = result.runner === 'veryl-tiered' ? 'VERYL' : 'CELOX';
        parseTiered(`VERYL_TEST_CONFIG test=test_rica backend=cc aot_c_async=true compile_only=false\n${prefix}_TIERED_STATS ${Object.entries(sample.tiered ?? {}).map(([k, v]) => `${k}=${v}`).join(' ')}`, 'test_rica', result.runner);
      }
      const contract = { comparison: report.comparison, revision: report.revision, verylVersion: report.verylVersion,
        workload: report.workload, kernel, sourceSha256: result.sourceSha256, ...settings,
        cycles: result.qualification.cycles, commits: result.qualification.commits };
      const id = createHash('sha256').update(JSON.stringify(contract)).digest('hex').slice(0, 12);
      const values = result.samples.map(sample => sample.processNs).sort((a, b) => a - b);
      benches.push({ name: `collection-${result.runner}/collection_rica_${kernel}_${id}_${report.host.arch}_total`,
        unit: 'ns', value: values[1], range: `${values[0]}–${values[2]}`,
        extra: JSON.stringify({ ...contract, runner: result.runner, binaries: report.binaries, phase: 'total' }) });
    }
  }
  return benches;
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  if (process.env.GITHUB_REF !== 'refs/heads/master' || !['schedule', 'workflow_dispatch'].includes(process.env.GITHUB_EVENT_NAME)) {
    throw new Error('Publication requires a scheduled or manual master run');
  }
  const [file, option] = process.argv.slice(2);
  if (!file || (option && option !== '--validate-only')) throw new Error('Usage: publish-rica-bench.mjs RESULTS_JSON [--validate-only]');
  const git = (...args) => execFileSync('git', args, { encoding: 'utf8' }).trim();
  const id = git('rev-parse', 'HEAD');
  const report = JSON.parse(readFileSync(file, 'utf8'));
  const benches = metrics(report, id);
  if (option === '--validate-only') {
    console.log(`Validated ${benches.length} rica metrics`);
  } else {
    const [name, email] = git('show', '-s', '--format=%an%x00%ae', 'HEAD').split('\0');
    publishResult(git('remote', 'get-url', 'origin'), {
      commit: { id, message: git('show', '-s', '--format=%s', 'HEAD'), timestamp: git('show', '-s', '--format=%cI', 'HEAD'),
        author: { name, email }, committer: { name, email }, url: `${process.env.GITHUB_SERVER_URL}/${process.env.GITHUB_REPOSITORY}/commit/${id}` },
      date: Date.parse(report.date), cpu: report.host.cpu, tool: 'customSmallerIsBetter', benches,
    }, { historyName: 'rica Benchmarks' });
  }
}
