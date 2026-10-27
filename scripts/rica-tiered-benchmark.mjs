#!/usr/bin/env node
import { createHash } from 'node:crypto';
import { readdirSync, readFileSync, writeFileSync, mkdirSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { arch, cpus, hostname } from 'node:os';
import { executeTiered, runners } from './tiered-benchmark.mjs';

const [projectsArg, outputArg, celoxArg, verylArg, repeatArg = '3'] = process.argv.slice(2);
if (!verylArg) throw new Error('Usage: rica-tiered-benchmark.mjs PROJECTS OUTPUT CELOX VERYL [REPEAT]');
const [projects, output, celox, veryl] = [projectsArg, outputArg, celoxArg, verylArg].map(p => resolve(p));
const repeat = Number(repeatArg);
if (!Number.isSafeInteger(repeat) || repeat < 1 || repeat > 20) throw new Error('REPEAT must be 1..20');
const hash = value => createHash('sha256').update(value).digest('hex');
mkdirSync(output);
const report = { schemaVersion: 2, date: new Date().toISOString(), revision: process.env.RICA_SOURCE_REVISION ?? null, verylVersion: '0.22.0', comparison: 'cold-tiered-process-v1', workload: 'board-kernel-tiered-tb-v1',
  settings: { frontend: 'veryl', testbenchLanguage: 'veryl', backend: 'tiered', cache: 'cold', stateModel: 'two-state', threads: 1, optimization: 'default-tiered' },
  host: { cpu: cpus()[0]?.model, arch: arch(), hostname: hostname() },
  binaries: { celox: hash(readFileSync(celox)), veryl: hash(readFileSync(veryl)) }, results: [] };
function completion(log, kernel) {
  const lines = log.split(/\r?\n/).filter(line => line.startsWith('RICA_COMPLETE '));
  const match = lines.length === 1 && /^RICA_COMPLETE ([a-zA-Z0-9_-]+) cycles=(\d+) commits=(\d+)$/.exec(lines[0]);
  if (!match || match[1] !== kernel || !match.slice(2).map(Number).every(n => Number.isSafeInteger(n) && n > 0)) {
    throw new Error('Missing or invalid kernel completion');
  }
  return { cycles: Number(match[2]), commits: Number(match[3]) };
}
const save = () => writeFileSync(join(output, 'results.json'), JSON.stringify(report, null, 2) + '\n');
save();
for (const kernel of readdirSync(projects).sort()) {
  const project = join(projects, kernel);
  const tb = join(project, 'test_rica.veryl');
  const original = readFileSync(tb, 'utf8');
  const sourceSha256 = hash(['dut.veryl', 'test_rica.veryl', 'image.hex', 'Veryl.toml'].map(f => hash(readFileSync(join(project, f)))).join(''));
  for (const runner of runners) {
    const entry = { kernel, runner, sourceSha256, status: 'failed', samples: [] };
    report.results.push(entry);
    const directory = join(output, `${kernel}-${runner}`);
    mkdirSync(directory);
    const execute = name => executeTiered({ runner, binary: runner === 'celox-tiered' ? celox : veryl,
      project, top: 'test_rica', directory, name, timeoutMs: 900_000 });
    try {
      const anchor = "const EXPECTED_TOHOST: logic<64> = 64'd1;";
      if (!original.includes(anchor)) throw new Error('Mailbox oracle anchor missing');
      const { log: positiveLog, ...positive } = execute('positive');
      entry.qualification = positive;
      if (positive.status !== 'passed') throw new Error('Kernel did not complete');
      Object.assign(positive, completion(positiveLog, kernel));
      writeFileSync(tb, original.replace(anchor, "const EXPECTED_TOHOST: logic<64> = 64'd2;"));
      const negative = execute('negative');
      writeFileSync(tb, original);
      entry.negativeControl = { status: negative.exitCode === 1 && !negative.signal && !negative.error &&
        negative.reportedStatus === 'fail' && /assert(?:ion)?[^\n]*fail/i.test(negative.log) ? 'passed' : 'failed' };
      if (entry.negativeControl.status !== 'passed') throw new Error('Broken mailbox oracle was not rejected');
      for (let i = 0; i < repeat; i++) {
        const { log, ...sample } = execute(`sample-${i}`);
        if (sample.status !== 'passed') throw new Error('Incomplete measurement');
        Object.assign(sample, completion(log, kernel));
        if (sample.cycles !== positive.cycles || sample.commits !== positive.commits) throw new Error('Kernel completion changed across repetitions');
        entry.samples.push(sample);
      }
      entry.status = 'measured';
    } catch (error) { entry.error = error.message; }
    finally { writeFileSync(tb, original); }
    save();
    console.log(`${kernel} ${runner}: ${entry.status}${entry.error ? ` (${entry.error})` : ''}`);
  }
  const pair = report.results.filter(r => r.kernel === kernel);
  if (pair.some(r => r.status !== 'measured') || pair[0].qualification.cycles !== pair[1].qualification.cycles ||
      pair[0].qualification.commits !== pair[1].qualification.commits) {
    for (const entry of pair) { entry.status = 'failed'; entry.error ??= 'Incomplete or mismatched simulator pair'; }
    save();
    console.error(`${kernel}: incomplete or mismatched simulator pair`);
  }
}
if (!report.results.length || report.results.some(r => r.status !== 'measured')) process.exitCode = 1;
