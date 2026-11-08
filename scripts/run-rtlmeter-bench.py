#!/usr/bin/env python3
"""Cold source-to-finish RTLMeter comparisons; never publish failed timings."""
import argparse
import hashlib
import json
import os
import platform
import re
import resource
import shutil
import signal
import subprocess
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_MANIFEST_PATH = ROOT / 'benchmarks/rtlmeter/manifest.json'
MANIFEST = json.loads(DEFAULT_MANIFEST_PATH.read_text())
ENGINES = ('celox', 'verilator', 'xezim')
STATES = ('two-state', 'four-state')
CONFIGURATION = {
    'celox': 'tiered hotspot JIT O2; threads=1; cold source-to-finish',
    'verilator': '--binary --timing --assert; -O3; C++ -O3; -j1; fresh Mdir',
    'xezim': 'release default VM; persistent caches disabled; full four-state',
}
STEADY_CONFIGURATION = {
    'celox': 'native O2; threads=1; steady-state simulation',
    'verilator': '--binary --timing --assert; -O3; C++ -O3; -j1; steady-state simulation',
    'xezim': 'release default VM; persistent caches disabled; full four-state',
}


def configuration_for(contract):
    if contract == 'rtlmeter-steady-state-v1':
        return dict(STEADY_CONFIGURATION)
    return dict(CONFIGURATION)


def git(directory, *args):
    return subprocess.check_output(['git', '-C', str(directory), *args], text=True).strip()


def sha(path):
    with Path(path).open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def prepare(destination, manifest=None):
    if manifest is None:
        manifest = MANIFEST
    destination = destination.resolve()
    if not destination.exists():
        subprocess.run(['git', 'clone', '--no-checkout', manifest['repository'], str(destination)], check=True)
        subprocess.run(['git', '-C', str(destination), 'checkout', '--detach', manifest['revision']], check=True)
    verify_source(destination, manifest)


def verify_source(source, manifest=None):
    if manifest is None:
        manifest = MANIFEST
    if git(source, 'rev-parse', 'HEAD') != manifest['revision'] or git(source, 'status', '--porcelain'):
        raise ValueError('RTLMeter sources must be the clean pinned revision')


def specification(name, source, manifest=None):
    if manifest is None:
        manifest = MANIFEST
    import yaml
    spec = manifest['cases'][name]
    design = source / 'designs' / spec['design']
    descriptor = yaml.safe_load((design / 'descriptor.yaml').read_text())
    compile_spec = descriptor['compile']
    test = descriptor['execute']['tests'][spec['test']]
    configurations = descriptor.get('configurations', {})
    configuration = configurations[spec['configuration']] if 'configuration' in spec else configurations.get('default', {})
    configured_compile = configuration.get('compile', {})
    configured_execute = configuration.get('execute', {})
    test = {**test, **configured_execute.get('tests', {}).get(spec['test'], {})}
    sources = [design / p for p in compile_spec['verilogSourceFiles']]
    sources.append(source / 'rtl/__rtlmeter_utils.sv')
    files = [design / p for p in (spec.get('inputFiles') or test.get('files', []))]
    defines = {**compile_spec.get('verilogDefines', {}), **configured_compile.get('verilogDefines', {}),
               '__RTLMETER_MAIN_CLOCK': compile_spec['mainClock']}
    # Fingerprint all potential local headers, not only explicit compilation inputs.
    inputs = sorted(set([*sources, *files, design / 'descriptor.yaml', *design.joinpath('src').rglob('*'),
                         source / 'rtl/__rtlmeter_top_include.vh']))
    hashes = {str(p.relative_to(source)): sha(p) for p in inputs if p.is_file()}
    digest = hashlib.sha256(json.dumps(hashes, sort_keys=True, separators=(',', ':')).encode()).hexdigest()
    args = spec.get('args', test.get('args', []))
    return {**spec, 'top': compile_spec['topModule'], 'sources': sources, 'files': files,
            'defines': defines, 'includes': [source / 'rtl', design / 'src'],
            'args': args, 'sourceSha256': digest, 'inputSha256': hashes}


def environment():
    # No caller-specific simulator settings, warm persistent cache or extra workers.
    env = {k: v for k, v in os.environ.items() if not k.startswith(('XEZIM_', 'CELOX_', 'VERILATOR_', 'COCOTB_'))}
    env.update(XEZIM_NO_CACHE='1', XEZIM_NO_NATIVE_CACHE='1', RAYON_NUM_THREADS='1',
               MAKEFLAGS='', CCACHE_DISABLE='1', RUST_LOG='warn', LC_ALL='C')
    return env


def execute(command, cwd, log, timeout, cpu):
    log.parent.mkdir(parents=True, exist_ok=True)
    start = time.monotonic_ns()
    with log.open('wb') as stream:
        process = subprocess.Popen(['timeout', '-k', '10s', f'{timeout}s', 'taskset', '-c', str(cpu), *map(str, command)], cwd=cwd,
                                   env=environment(), stdout=stream, stderr=subprocess.STDOUT,
                                   start_new_session=True)
        try:
            code = process.wait()
        except BaseException:
            os.killpg(process.pid, signal.SIGTERM)
            try:
                process.wait(10)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait()
            raise
    return {'exitCode': code, 'timedOut': code in (124, 137), 'elapsedNs': time.monotonic_ns() - start}


def accept(stdout, cycles, outcome, payload, success_marker='TEST PASSED'):
    if outcome['timedOut'] or outcome['exitCode'] != 0:
        raise ValueError('process failed or timed out')
    if re.search(r'TEST[\s_]+FAIL(?:ED)?|Time Out !!!|simulation finished with error|\$fatal|%Error:', stdout, re.I):
        raise ValueError('testbench reported failure')
    payload_count = sum(1 for line in stdout.splitlines() if line == payload or line.startswith(payload))
    if stdout.splitlines().count(success_marker) != 1 or payload_count != 1:
        raise ValueError('missing unique upstream success marker or expected program output')
    if not re.fullmatch(r'[1-9][0-9]*', cycles.strip()):
        raise ValueError('missing valid final RTLMeter clock count')
    return int(cycles.strip())


def commands(engine, state, spec, files, build, binaries, plusargs, contract='rtlmeter-cold-process-v1'):
    includes = [f'-I{p}' for p in spec['includes']]
    defines = [f'-D{k}={v}' for k, v in sorted(spec['defines'].items())]
    if engine == 'celox':
        if contract == 'rtlmeter-steady-state-v1':
            return [[binaries[engine], 'run', '-f', files, '--top', spec['top'], '--backend', 'native',
                     '--threads', '1', '-O', 'O2', '--four-state' if state == 'four-state' else '--two-state',
                     '--unlimited', '--report', '--no-checkpoint-on-signal', *includes, *defines, *plusargs]]
        return [[binaries[engine], 'run', '-f', files, '--top', spec['top'], '--backend', 'tiered',
                 '--jit', 'hotspot', '--report', '--threads', '1', '-O', 'O2', '--four-state' if state == 'four-state' else '--two-state',
                 '--unlimited', '--no-checkpoint-on-signal', *includes, *defines, *plusargs]]
    if engine == 'xezim':
        if state != 'four-state':
            raise ValueError('xezim has no global two-state mode; zero initialization is not equivalent')
        if contract == 'rtlmeter-steady-state-v1':
            return [[binaries[engine], '--error-exit', '--verbose', '--max-time', '60s', '-s', spec['top'],
                     '-f', files, *includes, *defines, *plusargs]]
        return [[binaries[engine], '--error-exit', '--max-time', '1s', '-s', spec['top'],
                 '-f', files, *includes, *defines, *plusargs]]
    model = build / 'sim'
    flags = ['--fourstate'] if state == 'four-state' else ['--no-fourstate', '--x-initial', '0', '--x-assign', '0']
    return [[binaries[engine], '--binary', '--timing', '--assert', '--top-module', spec['top'],
             '--Mdir', build, '-o', model, '-j', '1', '-O3', '-CFLAGS', '-O3', '-Wno-fatal',
             *flags, *includes, *defines, '-f', files], [model, *plusargs]]


def probe_source(state):
    check = "a !== 1'bx || z !== 1'bz" if state == 'four-state' else "a !== 1'b0 || z !== 1'b0"
    return f"`timescale 1ns/1ns\nmodule tb; logic a; wire z; initial begin #1; $display(\"STATE PROBE a=%b z=%b\", a, z); if ({check}) $fatal(1, \"state probe failed\"); $display(\"STATE PROBE PASSED\"); $finish; end endmodule\n"


def run_commands(sequence, cwd, logs, timeout, cpu):
    total = 0
    command_outcomes = []
    for i, command in enumerate(sequence):
        result = execute(command, cwd, logs / f'{i}.log', timeout, cpu)
        total += result['elapsedNs']
        command_outcomes.append(result)
        if result['timedOut'] or result['exitCode'] != 0:
            return {**result, 'elapsedNs': total, 'commands': command_outcomes}
    return {**result, 'elapsedNs': total, 'commands': command_outcomes}


def extract_sim_ns(engine, stdout, outcome):
    if engine == 'celox':
        match = re.search(r'^CELOX_TEST_TIMING\s+.*?\bexecute_ns=(\d+)', stdout, re.M)
        if match:
            return int(match.group(1))
    elif engine == 'verilator':
        cmds = outcome.get('commands', [])
        if len(cmds) > 1:
            return cmds[1]['elapsedNs']
    elif engine == 'xezim':
        match = re.search(r'\[PHASE\]\s+simulation:\s*([\d.]+)ms', stdout)
        if match:
            return int(float(match.group(1)) * 1_000_000)
        match = re.search(r'\[PROF\]\s+simulation_loop=([\d.]+)ms', stdout)
        if match:
            return int(float(match.group(1)) * 1_000_000)
    return outcome['elapsedNs']


def build_directory(root, name):
    # These are external C++ artifacts, not Cargo output. Keep their lifetime
    # bounded and separate from targets that mbx can collect between Cargo runs.
    return tempfile.TemporaryDirectory(prefix=name + '-', dir=root)


def measure_case(name, source, celox, xezim, output, build_root, timeout=1800, samples=3, verilator='verilator', manifest=None):
    if manifest is None:
        manifest = MANIFEST
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
    verify_source(source, manifest)
    spec = specification(name, source, manifest)
    output.mkdir(parents=True, exist_ok=False)
    build_root = build_root / name
    build_root.mkdir(parents=True, exist_ok=True)
    binaries = {k: shutil.which(str(v)) for k, v in {'celox': celox, 'verilator': verilator, 'xezim': xezim}.items()}
    if not all(binaries.values()):
        raise ValueError('All three simulator binaries are required')
    cpu = min(os.sched_getaffinity(0))
    cpu_model = next(line.split(':', 1)[1].strip() for line in Path('/proc/cpuinfo').read_text().splitlines() if line.startswith('model name'))
    version = lambda engine: subprocess.check_output([binaries[engine], '-V' if engine == 'xezim' else '--version'], text=True, stderr=subprocess.STDOUT).strip()
    versions = {engine: version(engine) for engine in ENGINES}
    locked = json.loads((ROOT / 'flake.lock').read_text())['nodes']['xezim']
    if not re.search(r'Verilator ' + re.escape(manifest['verilatorVersion']) + r'\b', versions['verilator']):
        raise ValueError('Verilator version does not match the measurement contract')
    release = locked['original'].get('ref')
    if release is None or versions['xezim'].splitlines()[0] != f'xezim version {release.removeprefix("v")}':
        raise ValueError('xezim must be built from the release fixed by flake.lock')
    config = configuration_for(manifest['contract'])
    report = {'schemaVersion': 1, 'contract': manifest['contract'], 'case': name, 'caseSpec': manifest['cases'][name],
              'rtlmeterRevision': manifest['revision'], 'sourceSha256': spec['sourceSha256'],
              'inputSha256': spec['inputSha256'], 'celoxCommit': git(ROOT, 'rev-parse', 'HEAD'),
              'dirty': bool(git(ROOT, 'status', '--porcelain')), 'requestedSamples': samples,
              'maxCycles': spec['maxCycles'],
              'host': {'cpu': cpu_model, 'architecture': platform.machine(), 'affinityCpu': cpu},
              'versions': versions, 'binarySha256': {k: sha(v) for k, v in binaries.items()},
              'configuration': config,
              'xezimRelease': locked['original']['ref'], 'xezimRevision': locked['locked']['rev'],
              'date': time.time_ns() // 1_000_000, 'status': 'incomplete', 'results': []}
    def save():
        (output / 'results.json').write_text(json.dumps(report, indent=2) + '\n')
    save()
    configurations = [(engine, state) for state in STATES for engine in ENGINES]
    rows = {}
    files = output / 'sources.f'
    files.write_text(''.join(json.dumps(str(p)) + '\n' for p in spec['sources']))
    for engine, state in configurations:
        row = {'engine': engine, 'stateModel': state, 'status': 'incomplete', 'samples': [],
               'experimental': engine == 'verilator' and state == 'four-state'}
        report['results'].append(row)
        rows[engine, state] = row
        if engine == 'xezim' and state == 'two-state':
            row.update(status='unsupported', reason='No global two-state mode; XEZIM_INIT_ZERO only changes initialization')
            continue
        directory = output / f'{engine}-{state}'
        directory.mkdir()
        probe = directory / 'probe.sv'
        probe.write_text(probe_source(state))
        probe_files = directory / 'probe.f'
        probe_files.write_text(json.dumps(str(probe)) + '\n')
        probe_spec = {**spec, 'top': 'tb', 'defines': {}, 'includes': []}
        with build_directory(build_root, f'{engine}-{state}-probe') as build:
            outcome = run_commands(commands(engine, state, probe_spec, probe_files, Path(build), binaries, [], manifest['contract']),
                                   directory, directory / 'probe-logs', timeout, cpu)
        log = ''.join(p.read_text(errors='replace') for p in sorted((directory / 'probe-logs').glob('*.log')))
        if outcome['exitCode'] != 0 or outcome['timedOut'] or 'STATE PROBE PASSED' not in log:
            row.update(status='failed', probe='failed', reason='State-model probe failed; see probe logs')
        else:
            row['probe'] = 'passed'
            control = directory / 'negative-control'
            control.mkdir()
            for path in spec['files']:
                shutil.copyfile(path, control / path.name)
            with build_directory(build_root, f'{engine}-{state}-negative') as build:
                outcome = run_commands(commands(engine, state, spec, files, Path(build),
                                               binaries, [*spec['args'], '+max_cycles=1'], manifest['contract']),
                                       control, control / 'logs', timeout, cpu)
            text = ''.join(p.read_text(errors='replace') for p in sorted((control / 'logs').glob('*.log')))
            control_cycles = control / '_rtlmeter_cycles.txt'
            if (outcome['exitCode'] == 0 and not outcome['timedOut']
                    and 'RTLMeter: +max_cycles reached, exiting' in text and spec.get('successMarker', 'TEST PASSED') not in text
                    and control_cycles.exists() and control_cycles.read_text().strip() == '1'):
                row['negativeControl'] = 'passed'
            else:
                row.update(status='failed', reason='One-cycle negative control did not reject an incomplete workload')
        save()
    # Rotate tool order between repetitions; every process starts with fresh
    # compiled artifacts, and no tool runs while another tool is compiling.
    for repetition in range(samples):
        order = configurations[repetition % len(configurations):] + configurations[:repetition % len(configurations)]
        for engine, state in order:
            row = rows[engine, state]
            if row['status'] not in ('incomplete',):
                continue
            directory = output / f'{engine}-{state}' / f'sample-{repetition + 1}'
            directory.mkdir()
            for path in spec['files']:
                shutil.copyfile(path, directory / path.name)
            print(f'RUN {name} {engine} {state} {repetition + 1}/{samples}', flush=True)
            with build_directory(build_root, f'{engine}-{state}-{repetition}') as build:
                outcome = run_commands(commands(engine, state, spec, files, Path(build), binaries,
                                               [*spec['args'], f'+max_cycles={spec["maxCycles"]}'], manifest['contract']),
                                       directory, directory / 'logs', timeout, cpu)
            stdout = ''.join(p.read_text(errors='replace') for p in sorted((directory / 'logs').glob('*.log')))
            cycles = directory / '_rtlmeter_cycles.txt'
            try:
                count = accept(stdout, cycles.read_text() if cycles.exists() else '', outcome, spec['payload'], spec.get('successMarker', 'TEST PASSED'))
                if count >= spec['maxCycles']:
                    raise ValueError('Workload reached the cycle limit instead of completing normally')
                if engine == 'celox':
                    if manifest['contract'] == 'rtlmeter-steady-state-v1':
                        if not any(line.startswith('CELOX_TEST_TIMING ') for line in stdout.splitlines()):
                            raise ValueError('Celox did not emit CELOX_TEST_TIMING')
                    elif not any(line.startswith('CELOX_JIT_CONFIG mode=hotspot ') for line in stdout.splitlines()):
                        raise ValueError('Celox did not run with the hotspot JIT')
                sim_ns = extract_sim_ns(engine, stdout, outcome)
                cycles_per_sec = round(count / (sim_ns / 1e9), 2) if sim_ns > 0 else 0.0
                row['samples'].append({
                    'status': 'passed',
                    'processNs': outcome['elapsedNs'],
                    'simNs': sim_ns,
                    'cycles': count,
                    'cyclesPerSec': cycles_per_sec,
                    'throughputKhz': round(cycles_per_sec / 1000.0, 2),
                })
            except ValueError as error:
                row.update(status='failed', reason=str(error))
            save()
    for row in report['results']:
        if row['status'] == 'incomplete':
            counts = {s['cycles'] for s in row['samples']}
            if len(row['samples']) != samples or len(counts) != 1:
                row.update(status='failed', reason='Incomplete or nondeterministic samples')
            else:
                row['status'] = 'passed'
    for state in STATES:
        passed = [r for r in report['results'] if r['stateModel'] == state and r['status'] == 'passed']
        # $finish and the utility's counter can fire on the same active edge.
        if passed and max(r['samples'][0]['cycles'] for r in passed) - min(r['samples'][0]['cycles'] for r in passed) > 1:
            for row in passed:
                row.update(status='mismatch', reason='Reference clock counts differ by more than one finish edge')
    report['status'] = 'completed'
    save()
    return report


def check_case(case_name, source, celox, states=('two-state',), timeout=300, output=None):
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
    verify_source(source)
    spec = specification(case_name, source)
    out_dir = Path(output).resolve() if output is not None else None
    if out_dir is not None:
        out_dir.mkdir(parents=True, exist_ok=True)
    cpu = min(os.sched_getaffinity(0))
    celox_bin = shutil.which(str(celox))
    if not celox_bin:
        raise ValueError(f'Celox binary not found: {celox}')
    binaries = {'celox': celox_bin}
    results = []
    with tempfile.TemporaryDirectory(prefix=f'rtlmeter-{case_name}-check-') as temp_dir:
        temp_path = Path(temp_dir)
        files = temp_path / 'sources.f'
        files.write_text(''.join(json.dumps(str(p)) + '\n' for p in spec['sources']))
        plusargs = [*spec['args'], f'+max_cycles={spec["maxCycles"]}']
        for state in states:
            work_dir = temp_path / state
            work_dir.mkdir()
            for path in spec['files']:
                shutil.copyfile(path, work_dir / path.name)
            logs = work_dir / 'logs'
            cmds = commands('celox', state, spec, files, work_dir, binaries, plusargs)
            outcome = run_commands(cmds, work_dir, logs, timeout, cpu)
            log_files = sorted(logs.glob('*.log'))
            stdout = ''.join(p.read_text(errors='replace') for p in log_files)
            if out_dir is not None:
                (out_dir / f'{case_name}-{state}.log').write_text(stdout)
            cycles_file = work_dir / '_rtlmeter_cycles.txt'
            cycles_text = cycles_file.read_text() if cycles_file.exists() else ''
            success_marker = spec.get('successMarker', 'TEST PASSED')
            payload = spec['payload']
            try:
                cycles = accept(stdout, cycles_text, outcome, payload, success_marker)
            except ValueError as error:
                tail = ''.join(stdout.splitlines(keepends=True)[-30:])
                raise ValueError(
                    f"RTLMeter {case_name} ({state}) failed: {error} (exitCode={outcome['exitCode']}, timedOut={outcome['timedOut']})\n"
                    f"Tail of stdout:\n{tail}"
                ) from error
            expected = spec.get('expectedCycles')
            if isinstance(expected, dict):
                expected = expected.get(state)
            if expected is not None and cycles != expected:
                tail = ''.join(stdout.splitlines(keepends=True)[-30:])
                raise ValueError(
                    f"RTLMeter {case_name} ({state}) cycle mismatch: expected {expected}, got {cycles}\n"
                    f"Tail of stdout:\n{tail}"
                )
            elapsed_s = outcome['elapsedNs'] / 1e9
            print(f"RTLMeter check PASSED: {case_name} ({state}) in {elapsed_s:.2f}s ({cycles} cycles)")
            results.append({'case': case_name, 'state': state, 'cycles': cycles, 'elapsedNs': outcome['elapsedNs']})
    return results


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='command', required=True)
    prepare_parser = sub.add_parser('prepare')
    prepare_parser.add_argument('destination', type=Path)
    prepare_parser.add_argument('--manifest', type=Path, default=DEFAULT_MANIFEST_PATH)
    check = sub.add_parser('check')
    check.add_argument('case', choices=[*MANIFEST['cases'], 'all'])
    check.add_argument('source', type=Path)
    check.add_argument('celox', type=Path)
    check.add_argument('--states', default='two-state,four-state',
                       help='Comma-separated state models (default: two-state,four-state)')
    check.add_argument('--output', type=Path, default=None,
                       help='Optional output directory to retain simulation logs')
    check.add_argument('--timeout', type=int, default=300)
    run = sub.add_parser('run')
    run.add_argument('case')
    run.add_argument('source', type=Path)
    run.add_argument('celox', type=Path)
    run.add_argument('output', type=Path)
    run.add_argument('--manifest', type=Path, default=DEFAULT_MANIFEST_PATH)
    run.add_argument('--xezim', default='xezim')
    run.add_argument('--verilator', default='verilator')
    run.add_argument('--build-root', type=Path, default=ROOT / 'work/rtlmeter-build')
    run.add_argument('--timeout', type=int, default=1800)
    run.add_argument('--samples', type=int, default=None)
    args = parser.parse_args()
    manifest = json.loads(args.manifest.read_text()) if hasattr(args, 'manifest') else MANIFEST
    if args.command == 'prepare':
        prepare(args.destination, manifest)
    elif args.command == 'check':
        states = [s.strip() for s in args.states.split(',') if s.strip()]
        for s in states:
            if s not in STATES:
                parser.error(f'unknown state model: {s}')
        cases = list(MANIFEST['cases'].keys()) if args.case == 'all' else [args.case]
        for c in cases:
            check_case(c, args.source.resolve(), args.celox.resolve(),
                       states=states, timeout=args.timeout, output=args.output)
    else:
        if args.case not in manifest['cases']:
            parser.error(f"unknown case '{args.case}', expected one of {list(manifest['cases'])}")
        samples = args.samples if args.samples is not None else manifest['samples']
        if not 1 <= samples <= 30 or args.timeout <= 0:
            parser.error('samples must be 1..30; timeout must be positive')
        measure_case(args.case, args.source.resolve(), args.celox.resolve(), args.xezim,
                     args.output.resolve(), args.build_root.resolve(), args.timeout, samples, args.verilator, manifest)


if __name__ == '__main__':
    main()
