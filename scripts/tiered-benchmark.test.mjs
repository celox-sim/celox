import assert from 'node:assert/strict';
import test from 'node:test';
import { mkdtempSync, writeFileSync, readFileSync, readdirSync, rmSync, existsSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { executeTiered, parseTiered, parseTiming } from './tiered-benchmark.mjs';

const celox = 'CELOX_TIERED_STATS test=tb tier=interpreter promotion=pending interpreted_evaluations=10 compiled_evaluations=0';
const veryl = 'VERYL_TEST_CONFIG test=tb backend=cc aot_c_async=true compile_only=false\nVERYL_TIERED_STATS test=tb compiled_dispatches=0 fallback_dispatches=10';
test('cold completion before promotion is valid; failed promotion or missing dispatch is not', () => {
  assert.equal(parseTiered(celox, 'tb', 'celox-tiered').promotion, 'pending');
  assert.equal(parseTiered(veryl, 'tb', 'veryl-tiered').compiled_dispatches, '0');
  for (const log of [celox + '\n' + celox, celox.replace('pending', 'failed'), celox.replace('evaluations=10', 'evaluations=0'), celox.replace('test=tb', 'test=wrong')]) {
    assert.throws(() => parseTiered(log, 'tb', 'celox-tiered'));
  }
  for (const log of [veryl.replace('async=true', 'async=false'), veryl.replace('dispatches=10', 'dispatches=0'), '']) {
    assert.throws(() => parseTiered(log, 'tb', 'veryl-tiered'));
  }
  assert.equal(parseTiming('VERYL_TEST_TIMING test=tb compile_ns=1 execute_ns=2\nVERYL_TEST_RESULT test=tb status=pass elapsed_ns=4', 'tb', 0, null, null, 'veryl-tiered').status, 'passed');
});

test('both adapters use fresh caches and preserve failed evidence', () => {
  const dir = mkdtempSync(join(tmpdir(), 'tiered-adapter-'));
  try {
    const binary = join(dir, 'runner');
    writeFileSync(binary, `#!/usr/bin/env node
const v = process.argv.includes('--aot-c-async');
const prefix = v ? 'VERYL' : 'CELOX';
console.log('CACHE=' + process.env.VERYL_AOT_CACHE_DIR);
console.log('ARGS=' + process.argv.slice(2).join(' '));
console.log(prefix + '_TEST_TIMING test=tb compile_ns=1 execute_ns=2' + (v ? '' : ' jit_execute_ns=NA'));
console.log(prefix + '_TEST_RESULT test=tb status=pass elapsed_ns=4');
console.log(v ? ${JSON.stringify(veryl)} : ${JSON.stringify(celox)});
`, { mode: 0o755 });
    for (const runner of ['celox-tiered', 'veryl-tiered']) {
      const sample = executeTiered({ runner, binary, project: dir, top: 'tb', directory: dir, name: 'sample' });
      assert.equal(sample.status, 'passed');
      assert.ok(sample.processNs > sample.totalNs);
      assert.match(sample.log, runner === 'celox-tiered' ? /--backend tiered/ : /--aot-c-async/);
      assert.deepEqual(readdirSync(join(dir, `sample-${runner}-cache`)), []);
      assert.throws(() => executeTiered({ runner, binary, project: dir, top: 'tb', directory: dir, name: 'sample' }));
    }
    writeFileSync(binary, '#!/bin/sh\nexit 1\n', { mode: 0o755 });
    assert.equal(executeTiered({ runner: 'veryl-tiered', binary, project: dir, top: 'tb', directory: dir, name: 'failed' }).status, 'failed');
  } finally { rmSync(dir, { recursive: true, force: true }); }
});

test('Rica comparison checks both mailbox oracles and does not accept silent success', () => {
  const dir = mkdtempSync(join(tmpdir(), 'rica-tiered-'));
  try {
    const projects = join(dir, 'projects');
    execFileSync('mkdir', ['-p', join(projects, 'kernel-user')]);
    for (const file of ['dut.veryl', 'image.hex', 'Veryl.toml']) writeFileSync(join(projects, 'kernel-user', file), 'fixture');
    const tb = join(projects, 'kernel-user/test_rica.veryl');
    writeFileSync(tb, "const EXPECTED_TOHOST: logic<64> = 64'd1;");
    const binary = join(dir, 'runner');
    writeFileSync(binary, `#!/usr/bin/env node
const fs = require('node:fs');
const tb = fs.readFileSync(process.argv[process.argv.indexOf('--project') + 1] + '/test_rica.veryl', 'utf8');
const v = process.argv.includes('--aot-c-async');
const prefix = v ? 'VERYL' : 'CELOX';
if (tb.includes("64'd2")) { console.log(prefix + '_TEST_RESULT test=test_rica status=fail elapsed_ns=4'); console.error('assertion failed'); process.exit(1); }
console.log('RICA_COMPLETE kernel-user cycles=1 commits=1');
console.log(prefix + '_TEST_TIMING test=test_rica compile_ns=1 execute_ns=2' + (v ? '' : ' jit_execute_ns=NA'));
console.log(prefix + '_TEST_RESULT test=test_rica status=pass elapsed_ns=4');
console.log(v ? ${JSON.stringify(veryl.replaceAll('test=tb', 'test=test_rica'))} : ${JSON.stringify(celox.replaceAll('test=tb', 'test=test_rica'))});
`, { mode: 0o755 });
    const script = new URL('./rica-tiered-benchmark.mjs', import.meta.url);
    execFileSync('node', [script.pathname, projects, join(dir, 'results'), binary, binary, '1']);
    const result = JSON.parse(readFileSync(join(dir, 'results/results.json')));
    assert.deepEqual(result.results.map(r => r.status), ['measured', 'measured']);
    assert.equal(result.results[0].sourceSha256, result.results[1].sourceSha256);
    assert.match(readFileSync(tb, 'utf8'), /64'd1/);
    const passingRunner = readFileSync(binary, 'utf8');
    writeFileSync(binary, passingRunner.replace("console.log('RICA_COMPLETE kernel-user cycles=1 commits=1');", "console.log('RICA_COMPLETE kernel-user cycles=' + (v ? 2 : 1) + ' commits=1');"));
    assert.throws(() => execFileSync('node', [script.pathname, projects, join(dir, 'mismatch'), binary, binary, '1']));
    const mismatch = JSON.parse(readFileSync(join(dir, 'mismatch/results.json')));
    assert.deepEqual(mismatch.results.map(r => r.status), ['failed', 'failed']);
    writeFileSync(binary, '#!/bin/sh\nexit 0\n', { mode: 0o755 });
    assert.throws(() => execFileSync('node', [script.pathname, projects, join(dir, 'failed'), binary, binary, '1']));
  } finally { rmSync(dir, { recursive: true, force: true }); }
});


test('background compiler children cannot survive into the next cold run', { skip: process.platform === 'win32' }, async () => {
  const dir = mkdtempSync(join(tmpdir(), 'tiered-cleanup-'));
  try {
    const marker = join(dir, 'child-survived');
    const binary = join(dir, 'runner');
    writeFileSync(binary, `#!/usr/bin/env node
const {spawn} = require('node:child_process');
spawn(process.execPath, ['-e', ${JSON.stringify("setTimeout(() => require('node:fs').writeFileSync(" + JSON.stringify(marker) + ", 'alive'), 300)")}], {stdio:'ignore'}).unref();
console.log('CELOX_TEST_TIMING test=tb compile_ns=1 execute_ns=2 jit_execute_ns=NA');
console.log('CELOX_TEST_RESULT test=tb status=pass elapsed_ns=4');
console.log(${JSON.stringify(celox)});
`, { mode: 0o755 });
    const result = executeTiered({ runner: 'celox-tiered', binary, project: dir, top: 'tb', directory: dir, name: 'sample' });
    assert.equal(result.status, 'passed');
    await new Promise(resolve => setTimeout(resolve, 500));
    assert.equal(existsSync(marker), false);
  } finally { rmSync(dir, { recursive: true, force: true }); }
});
