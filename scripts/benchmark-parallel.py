#!/usr/bin/env python3
"""Compile once, compare serial/parallel execution of an arbitrary Veryl project.

Requires Linux and a celox-heliodor runner. Each output directory is immutable;
use a new directory for different source, options, or repeated experiments.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import subprocess
import time


def digest(path):
    with path.open('rb') as source:
        return hashlib.file_digest(source, 'sha256').hexdigest()


def cpu_order():
    cores = {}
    for cpu in sorted(os.sched_getaffinity(0)):
        base = Path(f'/sys/devices/system/cpu/cpu{cpu}/topology')
        try:
            key = tuple(int((base / field).read_text()) for field in ('physical_package_id', 'core_id'))
        except (OSError, ValueError):
            key = (0, cpu)
        cores.setdefault(key, []).append(cpu)
    return [siblings[i] for i in range(max(map(len, cores.values())))
            for _, siblings in sorted(cores.items()) if i < len(siblings)]


def observations(path):
    data = path.read_bytes()
    output = re.sub(rb'(?:CELOX_[A-Z_]+[^\n]*|parallel auto:[^\n]*)\n?', b'', data)
    events = re.findall(rb'CELOX_PARALLEL_CONSOLE tick=(\d+) message=(.*)$', data, re.M)
    # Compiler-eliminated internal publications need not match. Top-level
    # observations and emitted output must match, including event drain ticks.
    snapshot = dict(line.split('\t', 1) for line in path.with_suffix('.snapshot').read_text().splitlines())
    live = {key: value for key, value in snapshot.items() if key.count('.') == 1}
    return output, events, live


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--runner', type=Path, required=True)
    p.add_argument('--project', type=Path, required=True)
    p.add_argument('--test', required=True)
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--ticks', type=int, default=2_000_000)
    p.add_argument('--rounds', type=int, default=2)
    p.add_argument('--threads', default='2,4,8,16')
    p.add_argument('--phase-ticks', type=int, default=250_000)
    p.add_argument('--window', type=int)
    p.add_argument('--warmup', type=int)
    p.add_argument('--cooldown', type=int)
    p.add_argument('--auto-only', action='store_true', help='Compare auto with standalone optimized serial')
    p.add_argument('--compile-only', action='store_true', help='Prepare images and resource records without execution trials')
    p.add_argument('--serial-image', type=Path)
    p.add_argument('--parallel-image', type=Path)
    a = p.parse_args()
    assert a.ticks > 0 and a.rounds > 0 and a.phase_ticks > 0
    a.runner, a.project, a.output = (x.resolve() for x in (a.runner, a.project, a.output))
    a.output.mkdir(parents=True, exist_ok=False)
    cpus = cpu_order()
    widths = sorted(set(map(int, a.threads.split(','))))
    assert all(0 < w <= len(cpus) for w in widths), f'admitted CPUs: {cpus}'
    env = {k: v for k, v in os.environ.items()
           if not k.startswith(('CELOX_PARALLEL', 'CELOX_RESEARCH'))}
    base = [str(a.runner), '--project', str(a.project), '--test', a.test, '--backend', 'native']
    provenance = dict(runner=str(a.runner), runner_sha256=digest(a.runner),
                      project=str(a.project), test=a.test, cpu_order=cpus, partition_lanes=min(64, len(cpus)),
                      ticks=a.ticks, rounds=a.rounds, threads=widths,
                      command_line=vars(a).copy())
    provenance['harness_sha256'] = digest(Path(__file__))
    provenance['inputs'] = {str(path.relative_to(a.project)): digest(path)
                            for path in sorted(a.project.rglob('*')) if path.is_file()
                            and (path.suffix in ('.veryl', '.hex') or path.name in ('Veryl.toml', 'Veryl.lock'))}
    provenance['kernel'] = dict(system=platform.system(), release=platform.release(), machine=platform.machine())
    provenance['cpu_model'] = next((line.split(':', 1)[1].strip() for line in Path('/proc/cpuinfo').read_text().splitlines() if line.startswith('model name')), None)
    provenance['cpu_topology'] = json.loads(subprocess.check_output(
        ['lscpu', '--json', '--extended=CPU,CORE,SOCKET,ONLINE'], text=True))
    provenance['command_line'] = {k: str(v) if isinstance(v, Path) else v
                                  for k, v in provenance['command_line'].items()}
    try:
        provenance['project_revision'] = subprocess.check_output(
            ['git', '-C', str(a.project), 'rev-parse', 'HEAD'], text=True).strip()
    except subprocess.CalledProcessError:
        provenance['project_revision'] = None
    images = {}
    for kind, provided in [('serial', a.serial_image), ('parallel', a.parallel_image)]:
        image = provided.resolve() if provided else a.output / f'{kind}.celox'
        if not provided:
            compile_env = dict(env, CELOX_PARALLEL='off' if kind == 'serial' else 'auto')
            if kind == 'parallel':
                compile_env['CELOX_PARALLEL_PARTITION'] = str(a.output / 'groups')
                compile_env['CELOX_PARALLEL_PARTITIONS'] = str(provenance['partition_lanes'])
            command = base + ['--compile-only', '--native-image-output', str(image)]
            print('COMPILE', kind, flush=True)
            start = time.monotonic()
            with (a.output / f'compile-{kind}.log').open('w') as log:
                process = subprocess.Popen(command, cwd=a.project, env=compile_env, stdout=log,
                                           stderr=subprocess.STDOUT)
                _, status, usage = os.wait4(process.pid, 0)
                process.returncode = os.waitstatus_to_exitcode(status)
            provenance[f'compile_{kind}_max_rss_kib'] = usage.ru_maxrss
            provenance[f'compile_{kind}_cpu_s'] = usage.ru_utime + usage.ru_stime
            provenance[f'compile_{kind}_wall_s'] = time.monotonic() - start
            provenance[f'compile_{kind}_returncode'] = process.returncode
            # Preserve resource usage even if compilation hits a memory limit.
            (a.output / 'provenance.json').write_text(json.dumps(provenance, indent=2) + '\n')
            assert process.returncode == 0, f'{kind} compilation failed; see compile log'
        images[kind] = image
        provenance[f'{kind}_image'] = dict(path=str(image), sha256=digest(image))
    (a.output / 'provenance.json').write_text(json.dumps(provenance, indent=2) + '\n')
    if a.compile_only:
        return
    reference = None
    for round_no in range(1, a.rounds + 1):
        modes = ['serial', 'auto'] if a.auto_only else ['serial', 'off', *map(str, widths), 'auto']
        if round_no % 2 == 0:
            modes.reverse()
        for mode in modes:
            name = f'{round_no}-{mode}'
            log_path = a.output / f'{name}.log'
            run_env = dict(env, CELOX_PARALLEL_DIAGNOSTICS="1", CELOX_PARALLEL='off' if mode == 'serial' else mode,
                           CELOX_PARALLEL_PHASE_TICKS=str(a.phase_ticks),
                           CELOX_PARALLEL_SNAPSHOT=str(log_path.with_suffix('.snapshot')))
            for parameter in ('window', 'warmup', 'cooldown'):
                value = getattr(a, parameter)
                if value is not None:
                    assert value >= 0 if parameter == 'warmup' else value > 0
                    run_env['CELOX_PARALLEL_' + parameter.upper()] = str(value)
            serial = mode in ('serial', 'off')
            if not serial:
                run_env.update(CELOX_PARALLEL_PIN='1', CELOX_PARALLEL_CPUS=','.join(map(str, cpus)))
            image = images['serial' if mode == 'serial' else 'parallel']
            assert digest(image) == provenance[f'{"serial" if mode == "serial" else "parallel"}_image']['sha256']
            assert digest(a.runner) == provenance['runner_sha256']
            command = ['taskset', '-c', str(cpus[0]) if serial else ','.join(map(str, cpus))] + base + [
                '--native-image-input', str(image), '--tick-limit', str(a.ticks)]
            print('RUN', name, flush=True)
            with log_path.open('w') as log, (a.output / f'{name}.host.jsonl').open('w') as host:
                process = subprocess.Popen(command, env=run_env, cwd=a.project, stdout=log, stderr=subprocess.STDOUT)
                while True:
                    done, status, usage = os.wait4(process.pid, os.WNOHANG)
                    if done:
                        process.returncode = os.waitstatus_to_exitcode(status)
                        break
                    sample = dict(time=time.time(), loadavg=Path('/proc/loadavg').read_text().strip(),
                                  proc_stat=Path('/proc/stat').read_text().splitlines()[0])
                    masks = {}
                    for task in Path(f'/proc/{process.pid}/task').glob('*'):
                        try:
                            masks[task.name] = sorted(os.sched_getaffinity(int(task.name)))
                        except ProcessLookupError:
                            pass
                    sample['thread_affinities'] = masks
                    try:
                        sample['process_stat'] = Path(f'/proc/{process.pid}/stat').read_text()
                    except FileNotFoundError:
                        pass
                    host.write(json.dumps(sample) + '\n')
                    host.flush()
                    time.sleep(5)
                assert process.returncode == 0, f'{name} failed; see {log_path}'
            log = log_path.read_text()
            timing = re.search(r'CELOX_TEST_TIMING (.*)$', log, re.M)
            assert timing, f'no execution timing in {log_path}'
            row = dict(name=name, mode=mode, round=round_no,
                       max_rss_kib=usage.ru_maxrss,
                       process_cpu_s=usage.ru_utime + usage.ru_stime,
                       timing=dict(re.findall(r'(\w+)=([^ ]+)', timing[1])),
                       phases=[tuple(map(int, m)) for m in re.findall(
                           r'CELOX_PARALLEL_PHASE tick=(\d+) wall_ns=(\d+) cpu_ns=(\d+)', log)])
            if mode.isdecimal():
                assert f'CELOX_PARALLEL_POOL width={mode} ' in log, 'requested workers were not created'
            actual = observations(log_path)
            if reference is None:
                reference = actual
            assert actual == reference, f'{name}: output, event ticks, or live state differs'
            row['observations_match'] = True
            with (a.output / 'timings.jsonl').open('a') as output:
                output.write(json.dumps(row) + '\n')
            print('PASS', name, row['timing'], flush=True)


if __name__ == '__main__':
    main()
