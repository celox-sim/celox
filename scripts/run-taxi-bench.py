#!/usr/bin/env python3
"""Pinned, paired Taxi/cocotb comparison. Failed/incomplete pairs never publish."""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import signal
import statistics
import subprocess
import sys
import threading
import time
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[1]
MANIFEST = json.loads((ROOT / 'benchmarks/taxi/manifest.json').read_text())
ENGINES = ('celox', 'verilator')


def git(source, *args):
    return subprocess.check_output(['git', '-C', str(source), *args], text=True).strip()


def verify_source(source):
    if git(source, 'rev-parse', 'HEAD') != MANIFEST['revision']:
        raise ValueError('Taxi revision differs from the pinned manifest')
    if git(source, 'status', '--porcelain', '--untracked-files=all'):
        raise ValueError('Taxi checkout has modified or untracked files')


def source_hashes(source, case):
    files = {source / name for name in case['sources']}
    files.update((source / case['testDirectory']).glob('*.py'))
    return {str(p.relative_to(source)): hashlib.sha256(p.read_bytes()).hexdigest()
            for p in sorted(files)}


def read_results(path, expected):
    """Check each test, including skip/duplicate/missing/non-finite records."""
    root = ET.parse(path).getroot()
    tests = list(root.iter('testcase'))
    if len(tests) != expected:
        raise ValueError(f'Expected {expected} tests, got {len(tests)}')
    result = {}
    body = 0.0
    for test in tests:
        if any(test.find(tag) is not None for tag in ('failure', 'error', 'skipped')):
            raise ValueError('Failed or skipped Taxi test')
        key = test.get('classname', '') + '/' + test.get('name', '')
        if not test.get('name') or key in result:
            raise ValueError('Missing or duplicate test identity')
        elapsed = float(test.attrib['time'])
        simulated = float(test.attrib['sim_time_ns'])
        if not math.isfinite(elapsed) or elapsed <= 0 or not math.isfinite(simulated) or simulated <= 0:
            raise ValueError('Invalid test timing')
        body += elapsed
        result[key] = simulated
    return {'body_seconds': body, 'simulation_ns': result}


def check_pair(pair):
    left, right = (pair[e]['simulation_ns'] for e in ENGINES)
    if left.keys() != right.keys() or any(
        not math.isclose(v, right[k], rel_tol=0, abs_tol=1e-6) for k, v in left.items()
    ):
        raise ValueError('Engines executed different tests or simulation durations')


def execute(command, cwd, env, log, timeout, cpu=None):
    """Retain logs and command metadata; kill the entire process group on timeout."""
    command = list(map(str, command))
    if cpu is not None:
        command = ['taskset', '-c', str(cpu), *command]
    log.with_suffix('.command.json').write_text(json.dumps(command, indent=2) + '\n')
    started = time.perf_counter()
    with log.open('w') as output:
        proc = subprocess.Popen(command, cwd=cwd, env=env, stdout=output,
                                stderr=subprocess.STDOUT, start_new_session=True)
        timed_out = threading.Event()
        def terminate():
            timed_out.set()
            try:
                os.killpg(proc.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        timer = threading.Timer(timeout, terminate)
        timer.start()
        try:
            code = proc.wait()
        except BaseException:
            terminate()
            proc.wait()
            raise
        finally:
            timer.cancel()
            timer.join()
        if timed_out.is_set():
            raise TimeoutError(f'Taxi command exceeded {timeout}s; see {log}')
    seconds = time.perf_counter() - started
    if code:
        raise RuntimeError(f'{command[0]} exited {code}; see {log}')
    return seconds


def build_commands(case, source, celox, directory, libs, share, verilator):
    sources = [source / p for p in case['sources']]
    parameters = [f'-G{k}={v}' for k, v in case['parameters'].items()]
    timescale = ['--timescale', case['timescale']] if 'timescale' in case else []
    native = directory / 'celox-sim'
    model = directory / 'verilator'
    model.mkdir()
    c = [celox, 'vpi', 'build', *sources, '--top', case['top'], '--two-state', '-O', 'O1']
    for warning in ('mixed_timescale', 'unpacked_port_default', 'multiple_always_ff_drivers', 'inferred_latch'):
        c += ['--diagnostic', warning + '=warn']
    for key, value in case['parameters'].items():
        c += ['-G', f'{key}={value}']
    c += [*timescale, '-o', native]
    v = [verilator, '-cc', '--exe', '-Mdir', model, '-DCOCOTB_SIM=1', '--top-module', case['top'],
         '--vpi', '--public-flat-rw', '--prefix', 'Vtop', '-o', case['top'], '-Wno-fatal',
         '-LDFLAGS', f'-Wl,-rpath,{libs} -L{libs} -lcocotbvpi_verilator', *timescale,
         *parameters, share / 'lib/verilator/verilator.cpp', *sources]
    return {'celox': [c], 'verilator': [v, ['make', '-C', model, '-f', 'Vtop.mk', '-j1']]}, native, model / case['top']


def runtime_environment(case, source):
    import find_libpython
    import cocotb_tools.config as config
    env = {k: v for k, v in os.environ.items() if not k.startswith(('COCOTB_', 'CELOX_', 'PARAM_'))}
    for key in ('TESTCASE', 'MODULE', 'TOPLEVEL', 'TOPLEVEL_LANG', 'PYTHONHOME', 'PYTHONOPTIMIZE', 'GPI_EXTRA'):
        env.pop(key, None)
    env.update(COCOTB_LOG_LEVEL='INFO', COCOTB_REDUCED_LOG_FMT='1', COCOTB_RANDOM_SEED=str(MANIFEST['seed']),
               COCOTB_TRUST_INERTIAL_WRITES='1', COCOTB_RESOLVE_X='RANDOM', PYTHONDONTWRITEBYTECODE='1',
               MAKEFLAGS='', CCACHE_DISABLE='1', RUST_LOG='warn', PYGPI_PYTHON_BIN=sys.executable,
               LIBPYTHON_LOC=find_libpython.find_libpython() or '',
               PYTHONPATH=os.pathsep.join([str(source / case['testDirectory']), str(ROOT / 'scripts'), *sys.path]),
               COCOTB_TOPLEVEL=case['top'], COCOTB_TEST_MODULES=case['module'], TOPLEVEL_LANG='verilog')
    env['PATH'] += os.pathsep + str(config.libs_dir)
    env.update({f'PARAM_{k}': str(v) for k, v in case['parameters'].items()})
    env['PYGPI_USERS'] = ','.join([
        'cocotb_tools._coverage:start_cocotb_library_coverage',
        'cocotb.logging:_configure', 'cocotb._init:init_package_from_simulation',
        'taxi_bench_clock:configure', 'cocotb._init:run_regression',
    ])
    return env


def run_case(name, source, celox, output, samples=3, timeout=1800, verilator='verilator'):
    # Import only for actual runs; parser tests need no simulator dependencies.
    import cocotb
    import cocotb_tools.config as config
    case = MANIFEST['cases'][name]
    if samples < 1 or samples > 30:
        raise ValueError('samples must be 1..30')
    source, celox, output = source.resolve(), celox.resolve(), output.resolve()
    verify_source(source)
    output.mkdir(parents=True, exist_ok=False)
    cpu = min(os.sched_getaffinity(0))
    hashes = source_hashes(source, case)
    env = runtime_environment(case, source)
    report = {'schemaVersion': 1, 'contract': MANIFEST['contract'], 'case': name,
              'body_clock': 'cocotb-regression-perf_counter-v1',
              'taxi_revision': MANIFEST['revision'], 'celox_commit': git(ROOT, 'rev-parse', 'HEAD'),
              'celox_binary_sha256': hashlib.sha256(celox.read_bytes()).hexdigest(),
              'verilator_version': subprocess.check_output([verilator, '--version'], text=True).strip(),
              'python_version': sys.version, 'cocotb_version': cocotb.__version__,
              'host': {'cpu': platform.processor(), 'affinity_cpu': cpu, 'hostname': platform.node(), 'architecture': platform.machine()},
              'source_sha256': hashes, 'parameters': case['parameters'], 'expected_tests': case['tests'],
              'seed': MANIFEST['seed'], 'requested_samples': samples, 'status': 'incomplete',
              'configuration': {'celox': 'native O1 two-state', 'verilator': '--vpi --public-flat-rw; C++ default -Os'},
              'compilation_seconds': {}, 'pairs': []}
    def save():
        (output / 'results.json').write_text(json.dumps(report, indent=2) + '\n')
    save()
    try:
        commands, native, reference = build_commands(case, source, celox, output, config.libs_dir, config.share_dir, verilator)
        # All compilation finishes before any paired runtime measurement.
        for engine in ENGINES:
            print(f'COMPILE {name} {engine}', flush=True)
            report['compilation_seconds'][engine] = sum(
                execute(command, ROOT, env, output / f'{engine}-compile-{i}.log', timeout, cpu)
                for i, command in enumerate(commands[engine]))
            save()
        for i in range(samples):
            pair = {}
            report['pairs'].append(pair)
            for engine in ENGINES if i % 2 == 0 else reversed(ENGINES):
                directory = output / f'sample-{i + 1}' / engine
                directory.mkdir(parents=True)
                xml = directory / 'results.xml'
                command = ([native, '--test-module', case['module'], '--python', sys.executable,
                            '--results-file', xml] if engine == 'celox' else [reference])
                run_env = {**env, 'COCOTB_RESULTS_FILE': str(xml)}
                print(f'RUN {name} {i + 1}/{samples} {engine}', flush=True)
                elapsed = execute(command, source / case['testDirectory'], run_env, directory / 'runtime.log', timeout, cpu)
                with (directory / 'runtime.log').open() as log:
                    if 'TAXI_BENCH_CLOCK=perf_counter' not in log.read(65536).splitlines():
                        raise ValueError('Monotonic cocotb timing hook did not run')
                pair[engine] = {**read_results(xml, case['tests']), 'wall_seconds': elapsed}
                save()
            check_pair(pair)
            if i and pair['celox']['simulation_ns'] != report['pairs'][0]['celox']['simulation_ns']:
                raise ValueError('Test workload changed between samples')
        verify_source(source)
        if source_hashes(source, case) != hashes:
            raise ValueError('Taxi input bytes changed during measurement')
        report['status'] = 'passed'
        report['median_paired_body_ratio'] = statistics.median(
            p['celox']['body_seconds'] / p['verilator']['body_seconds'] for p in report['pairs'])
        save()
        return report
    except BaseException as error:
        report['status'] = 'failed'
        report['error'] = str(error)
        save()
        raise


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='command', required=True)
    prepare = sub.add_parser('prepare')
    prepare.add_argument('source', type=Path)
    run = sub.add_parser('run')
    run.add_argument('case', choices=MANIFEST['cases'])
    run.add_argument('source', type=Path)
    run.add_argument('celox', type=Path)
    run.add_argument('output', type=Path)
    run.add_argument('--samples', type=int, default=MANIFEST['samples'])
    run.add_argument('--timeout', type=int, default=1800)
    args = parser.parse_args()
    if args.command == 'prepare':
        if not args.source.exists():
            args.source.mkdir(parents=True)
            git(args.source, 'init', '--quiet')
            git(args.source, 'fetch', '--quiet', '--depth=1', MANIFEST['repository'], MANIFEST['revision'])
            git(args.source, 'checkout', '--quiet', '--detach', 'FETCH_HEAD')
        verify_source(args.source)
    else:
        run_case(args.case, args.source, args.celox, args.output, args.samples, args.timeout)


if __name__ == '__main__':
    main()
