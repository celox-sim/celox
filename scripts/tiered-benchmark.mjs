// Shared cold-process runner for Veryl native testbenches.
import { spawnSync } from 'node:child_process';
import { mkdirSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';

export const runners = ['celox-tiered', 'veryl-tiered'];
export function parseTiming(log, top, exitCode, signal, error, runner) {
  const prefix = runner === 'veryl-tiered' ? 'VERYL' : 'CELOX';
  const timing = [...log.matchAll(new RegExp(`^${prefix}_TEST_TIMING test=(\\S+) compile_ns=(\\d+) execute_ns=(\\d+)${prefix === 'CELOX' ? ' jit_execute_ns=(?:\\d+|NA)(?: execute_cpu_ns=\\d+)?' : ''}\\r?$`, 'gm'))];
  const result = [...log.matchAll(new RegExp(`^${prefix}_TEST_RESULT test=(\\S+) status=(\\S+) elapsed_ns=(\\d+)\\r?$`, 'gm'))];
  if (error || signal || exitCode !== 0 || timing.length !== 1 || result.length !== 1 ||
      timing[0][1] !== top || result[0][1] !== top || result[0][2] !== 'pass') {
    return { status: 'failed', exitCode, signal: signal ?? null, error: error?.message ?? null,
      reportedStatus: result.length === 1 ? result[0][2] : 'missing-or-ambiguous' };
  }
  const [compileNs, executeNs, totalNs] = [timing[0][2], timing[0][3], result[0][3]].map(Number);
  if (![compileNs, executeNs, totalNs].every(n => Number.isSafeInteger(n) && n > 0) || compileNs + executeNs > totalNs) {
    return { status: 'failed', error: 'Invalid split timings' };
  }
  return { status: 'passed', compileNs, executeNs, totalNs };
}

export function parseTiered(log, top, runner) {
  const prefix = runner === 'veryl-tiered' ? 'VERYL' : 'CELOX';
  const lines = log.split(/\r?\n/).filter(l => l.startsWith(`${prefix}_TIERED_STATS `));
  if (lines.length !== 1) throw new Error('Missing or ambiguous tiered statistics');
  const fields = Object.fromEntries(lines[0].split(' ').slice(1).map(v => v.split('=')));
  if (fields.test !== top) throw new Error('Tiered statistics test mismatch');
  const names = runner === 'veryl-tiered' ? ['compiled_dispatches', 'fallback_dispatches'] : ['compiled_evaluations', 'interpreted_evaluations'];
  const counts = names.map(n => /^\d+$/.test(fields[n] ?? '') ? Number(fields[n]) : NaN);
  if (!counts.every(n => Number.isSafeInteger(n) && n >= 0) || counts[0] + counts[1] === 0 ||
      (runner === 'celox-tiered' && !['pending', 'promoted'].includes(fields.promotion))) {
    throw new Error('Invalid or failed tiered execution');
  }
  if (runner === 'veryl-tiered' && !log.split(/\r?\n/).includes(`VERYL_TEST_CONFIG test=${top} backend=cc aot_c_async=true compile_only=false`)) {
    throw new Error('Veryl asynchronous C compilation was not enabled');
  }
  // Completion before promotion is valid. Preserve evidence rather than forcing a warm tier.
  return fields;
}

export function executeTiered({ runner, binary, project, top, directory, name, timeoutMs = 600_000 }) {
  if (!runners.includes(runner)) throw new Error(`Unknown runner ${runner}`);
  directory = resolve(directory); project = resolve(project); binary = resolve(binary);
  const cache = join(directory, `${name}-${runner}-cache`);
  mkdirSync(cache); // A fresh private AOT cache for every process, including qualification.
  const args = runner === 'celox-tiered'
    ? ['run', '--project', project, '--top', top, '--backend', 'tiered', '--report', '--unlimited', '--two-state', '--threads', '1', '-O', 'O2']
    : ['--project', project, '--test', top, '--aot-c-async'];
  // Ambient tuning must not silently change the recorded comparison contract.
  const env = Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith('VERYL_') && !key.startsWith('CELOX_')));
  Object.assign(env, { VERYL_AOT_CACHE_DIR: cache, CCACHE_DISABLE: '1' });
  const start = process.hrtime.bigint();
  const result = spawnSync(binary, args, { cwd: project, env, encoding: 'utf8', timeout: timeoutMs, killSignal: 'SIGKILL', detached: process.platform !== 'win32', maxBuffer: 32 * 1024 * 1024 });
  const processNs = Number(process.hrtime.bigint() - start);
  // Do not let an unfinished background C compiler contend with the next run.
  // The process wall time ends at runner exit; cleanup is outside that interval.
  if (process.platform !== 'win32' && result.pid) {
    try { process.kill(-result.pid, 'SIGKILL'); }
    catch (error) { if (error.code !== 'ESRCH') throw error; }
  }
  const log = `${result.stdout ?? ''}${result.stderr ?? ''}`;
  writeFileSync(join(directory, `${name}-${runner}.log`), log);
  const parsed = parseTiming(log, top, result.status, result.signal, result.error, runner);
  if (parsed.status === 'passed') {
    try { parsed.tiered = parseTiered(log, top, runner); }
    catch (error) { parsed.status = 'failed'; parsed.error = error.message; }
  }
  return { ...parsed, processNs, log };
}
