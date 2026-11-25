#!/usr/bin/env python3
"""Profile an RTLMeter case per RTL clock, and compare two profiles.

  rtlmeter-profile.py run CASE --celox BIN --out DIR [--manifest M] [--source RTLMETER]
  rtlmeter-profile.py verilator CASE --out DIR [--verilator verilator]
  rtlmeter-profile.py compare BEFORE_DIR AFTER_DIR
  rtlmeter-profile.py disasm RUN_DIR [--match TEXT]

`run` runs the case twice with the steady-state contract of
run-rtlmeter-bench.py: once with `--activation-profile` (what each part of
the compiled scheduler ran), once without any counters (the user
instructions and cycles of the execute phase, `CELOX_TEST_COUNTERS`, the
figures to judge a change by). Both
runs must reach the same clock count and print the same program output.
`verilator` measures the Verilator model of the case the same way (whole
executable, so an upper bound of the model's own work). `compare` checks that
two runs did the same work, then lists totals per clock and the activation
counters that changed the most per clock. With `run --sample`, a third
run samples user instructions (`perf record`) with the JIT perf map: the
report and `compare` then give instructions per clock and per activation
for each role of native code (an outlined scheduler body is named after
the domain, kernel or settle group it runs), and `disasm` shows the
machine code of the hottest blocks of that same run.
"""
import argparse
import hashlib
import importlib.util
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
sys.dont_write_bytecode = True


def module(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    loaded = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(loaded)
    return loaded


bench = module('rtlmeter_bench', ROOT / 'scripts/run-rtlmeter-bench.py')
profile_tool = module('activation_profile', ROOT / 'scripts/activation-profile.py')

PERF_EVENTS = 'instructions:u,cycles:u'


def perf_binary():
    explicit = os.environ.get('PERF')
    if explicit:
        return explicit
    # Ubuntu's /usr/bin/perf wrapper refuses kernels it has no package for.
    for candidate in sorted(Path('/usr/lib').glob('linux-tools-*/perf'), reverse=True):
        return str(candidate)
    return shutil.which('perf') or 'perf'


def output_digest(text):
    """The program's output, without the simulator's own records and timings."""
    lines = [line for line in text.splitlines()
             if not line.startswith(('CELOX_', 'mbx', '- '))
             and 'Verilator' not in line and 'Walltime' not in line and 'elapsed' not in line]
    return hashlib.sha256('\n'.join(lines).encode()).hexdigest()


def parse_perf(path):
    counts = {}
    for line in Path(path).read_text().splitlines():
        fields = line.split(',')
        if len(fields) > 2 and fields[0].strip().isdigit():
            counts[fields[2].split(':')[0]] = int(fields[0])
    return counts


def run_one(command, directory, spec, timeout, cpu, perf=None, env=None):
    directory.mkdir(parents=True, exist_ok=True)
    for path in spec['files']:
        shutil.copyfile(path, directory / path.name)
    if perf:
        command = [perf_binary(), 'stat', '-x,', '-o', str(directory / 'perf-stat.csv'), '-e', PERF_EVENTS, '--', *command]
    log = directory / 'run.log'
    environment = bench.environment()
    environment.update(env or {})
    started = time.monotonic_ns()
    with log.open('wb') as stream:
        process = subprocess.run(['timeout', '-k', '10s', f'{timeout}s', 'taskset', '-c', str(cpu), *map(str, command)],
                                 cwd=directory, env=environment, stdout=stream, stderr=subprocess.STDOUT)
    outcome = {'exitCode': process.returncode, 'timedOut': process.returncode in (124, 137),
               'elapsedNs': time.monotonic_ns() - started}
    text = log.read_text(errors='replace')
    cycles_file = directory / '_rtlmeter_cycles.txt'
    cycles = bench.accept(text, cycles_file.read_text() if cycles_file.exists() else '', outcome,
                          spec['payload'], spec.get('successMarker', 'TEST PASSED'))
    result = {'cycles': cycles, 'outputSha256': output_digest(text), 'elapsedNs': outcome['elapsedNs']}
    timing = re.search(r'^CELOX_TEST_TIMING .*?compile_ns=(\d+) execute_ns=(\d+)', text, re.M)
    if timing:
        result['compileNs'], result['executeNs'] = map(int, timing.groups())
    counted = re.search(r'^CELOX_TEST_COUNTERS .*?instructions=(\d+) cycles=(\d+)', text, re.M)
    if counted:
        # The execute phase alone, without compilation.
        result['execute'] = {'instructions': int(counted.group(1)), 'cycles': int(counted.group(2))}
    if perf:
        result['perf'] = parse_perf(directory / 'perf-stat.csv')
    return result


def run(args):
    manifest = json.loads(Path(args.manifest).read_text())
    bench.verify_source(args.source, manifest)
    spec = bench.specification(args.case, args.source, manifest)
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    files = out / 'sources.f'
    files.write_text(''.join(json.dumps(str(p)) + '\n' for p in spec['sources']))
    plusargs = [*spec['args'], *args.plusarg]
    celox = Path(args.celox).resolve()
    command = bench.commands('celox', 'two-state', spec, files, out, {'celox': celox}, plusargs,
                             'rtlmeter-steady-state-v1')[0]
    # Options of the build under test go before the plusargs.
    command[2:2] = args.celox_arg
    summary = {'schemaVersion': 1, 'case': args.case, 'engine': 'celox', 'celox': str(celox), 'celoxArgs': args.celox_arg,
               'celoxSha256': bench.sha(celox), 'sourceSha256': spec['sourceSha256'], 'plusargs': plusargs,
               'command': [str(part) for part in command]}
    profile = out / 'profiled' / 'activation.json'
    print(f'{args.case}: profiled run', flush=True)
    summary['profiled'] = run_one([*command, '--activation-profile', profile], out / 'profiled', spec,
                                  args.timeout, args.cpu)
    if not args.no_clean:
        print(f'{args.case}: clean run', flush=True)
        summary['clean'] = run_one(command, out / 'clean', spec, args.timeout, args.cpu)
        if 'execute' not in summary['clean']:
            raise SystemExit('the clean run reported no execute counters (CELOX_TEST_COUNTERS): '
                             'perf events unavailable? (kernel.perf_event_paranoid)')
        same = (summary['clean']['cycles'], summary['clean']['outputSha256']) == (
            summary['profiled']['cycles'], summary['profiled']['outputSha256'])
        if not same:
            raise SystemExit('the profiled and the clean run differ in clock count or output')
    if args.sample:
        print(f'{args.case}: sampled run (perf record, CELOX_PERF_MAP)', flush=True)
        directory = out / 'sampled'
        directory.mkdir()
        record = [perf_binary(), 'record', '-q', '-e', 'instructions:u', '-c', str(args.sample_period),
                  '-o', str(directory / 'perf.data'), '--']
        sampled = run_one([*record, *command], directory, spec, args.timeout, args.cpu,
                          env={'CELOX_PERF_MAP': '1'})
        if (sampled['cycles'], sampled['outputSha256']) != (summary['profiled']['cycles'], summary['profiled']['outputSha256']):
            raise SystemExit('the sampled run differs from the profiled run in clock count or output')
        summary['sampled'] = sampled
        summary['attribution'] = attribute(directory)
    (out / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    report(out)


BLOCK = re.compile(r'\.(bb\d+|prologue)$')


def function_of(symbol):
    """The native function of a perf map symbol `<function>.<label>.bb<N>`,
    whose label is the first word of the function's name."""
    name = BLOCK.sub('', symbol)
    for at in [i for i, c in enumerate(name) if c == '.']:
        if name[at + 1:] == name[:at].split(' ')[0]:
            return name[:at]
    return name


def role_of(function):
    """What a native function runs: the role an outlined scheduler body
    carries in its name, or the function itself. (Blocks of the scheduler
    are already summed by the role they carry.)"""
    match = re.fullmatch(r'scheduler_callee\[\d+\] (.*)', function)
    return match.group(1) if match else function


def attribute(directory):
    """Move the run's perf map and code beside its samples, and sum the
    sampled instructions per native function and per block. Samples are
    weighted by their period; only those between the first and the last
    sample of generated code count, the execute phase, and the caller scales
    them to the phase's counted instructions."""
    data = directory / 'perf.data'
    perf = perf_binary()
    script = subprocess.run([perf, 'script', '-i', str(data), '-F', 'pid,time,period,ip,sym'],
                            capture_output=True, text=True, check=True).stdout
    stats = subprocess.run([perf, 'report', '-i', str(data), '--stats'], capture_output=True, text=True).stdout
    throttled = re.search(r'^\s*THROTTLE events:\s*(\d+)', stats, re.M)
    samples, pids, ranges = [], set(), []
    for line in script.splitlines():
        match = re.match(r'\s*(\d+)\s+([\d.]+):\s+(\d+)\s+([0-9a-f]+)\s*(.*)$', line)
        if not match:
            continue
        pid, time, period, ip = int(match[1]), float(match[2]), int(match[3]), int(match[4], 16)
        if pid not in pids:
            pids.add(pid)
            ranges = load_map(Path(f'/tmp/perf-{pid}.map'), ranges)
        jit = any(start <= ip < end for start, end in ranges)
        samples.append((time, period, jit, match[5].strip() or '[unknown]'))
    jit_times = [time for time, _, jit, _ in samples if jit]
    functions, blocks, host = {}, {}, {}
    if jit_times:
        first, last = min(jit_times), max(jit_times)
        for time, period, jit, symbol in samples:
            if not first <= time <= last:
                continue
            if jit:
                # A block of the compiled scheduler carries the role of what
                # runs in it after its name.
                tagged = re.match(r'(.*\.bb\d+) (.+)$', symbol)
                function = tagged[2] if tagged else function_of(symbol)
                functions[function] = functions.get(function, 0) + period
                blocks[symbol] = blocks.get(symbol, 0) + period
            else:
                host[symbol] = host.get(symbol, 0) + period
    for pid in pids:
        for path in Path('/tmp').glob(f'perf-{pid}*'):
            shutil.move(str(path), directory / path.name)
    top = sorted(blocks.items(), key=lambda item: -item[1])[:200]
    return {'functions': functions, 'blocks': dict(top),
            'host': dict(sorted(host.items(), key=lambda item: -item[1])[:50]),
            'throttleEvents': int(throttled[1]) if throttled else 0}


def load_map(path, ranges):
    ranges = list(ranges or [])
    if path.is_file():
        for line in path.read_text().splitlines():
            start, size = line.split(' ', 2)[:2]
            ranges.append((int(start, 16), int(start, 16) + int(size, 16)))
    return ranges


def activations_of(role, table):
    """The activation counter a role runs once per count of, if any."""
    match = re.fullmatch(r'domain (.*) (eval_apply|eval_only|apply|fused)', role)
    if match:
        return table.get(('domain', match.group(1), match.group(2)))
    if role.startswith('kernel '):
        return table.get(('kernel', role[len('kernel '):], 'runs'))
    if role.startswith('settle group '):
        return table.get(('settle_group', role[len('settle '):], 'runs'))
    if role == 'eager settle':
        return table.get(('eager_settle', '', 'dirty'))
    if role == 'settle' or role.startswith('eval_comb'):
        return table.get(('settle', '', 'dirty'))
    return None


def attribution_rows(summary, out):
    attribution = summary.get('attribution')
    if not attribution:
        return []
    profile = out / 'profiled' / 'activation.json'
    table = profile_tool.counters(json.loads(profile.read_text())) if profile.is_file() else {}
    cycles = summary['profiled']['cycles']
    sampled = sum(attribution['functions'].values()) + sum(attribution.get('host', {}).values())
    execute = (summary.get('sampled') or {}).get('execute', {}).get('instructions')
    # Sampled instructions, scaled to the execute phase's count.
    scale = execute / sampled if execute and sampled else 1
    rows = {}
    for function, instructions in attribution['functions'].items():
        role = role_of(function)
        row = rows.setdefault(role, {'role': role, 'instructions': 0, 'functions': 0})
        row['instructions'] += instructions * scale
        row['functions'] += 1
    host = sum(attribution.get('host', {}).values()) * scale
    if host:
        rows['(host code)'] = {'role': '(host code)', 'instructions': host}
    for row in rows.values():
        row['perClock'] = row['instructions'] / cycles
        count = activations_of(row['role'], table)
        if count:
            row['activationsPerClock'] = count / cycles
            row['perActivation'] = row['instructions'] / count
    return sorted(rows.values(), key=lambda row: -row['perClock'])


def print_attribution(summary, out, top):
    rows = attribution_rows(summary, out)
    if not rows:
        return
    total = sum(row['perClock'] for row in rows)
    throttled = summary['attribution'].get('throttleEvents')
    if throttled:
        print(f'\nwarning: perf throttled the sampling {throttled} times; raise --sample-period '
              f'(kernel.perf_event_max_sample_rate)')
    print(f'\ninstructions per clock by native function role (sampled, {total:.0f}/clock):')
    print(f'{"instr/clock":>11} {"share":>6} {"activ/clock":>11} {"instr/activ":>11}  role')
    for row in rows[:top]:
        activ = f'{row["activationsPerClock"]:>11.3f}' if 'activationsPerClock' in row else ' ' * 11
        per = f'{row["perActivation"]:>11.1f}' if 'perActivation' in row else ' ' * 11
        print(f'{row["perClock"]:>11.1f} {row["perClock"] / total:>6.1%} {activ} {per}  {row["role"]}')


def verilator(args):
    manifest = json.loads(Path(args.manifest).read_text())
    bench.verify_source(args.source, manifest)
    spec = bench.specification(args.case, args.source, manifest)
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    files = out / 'sources.f'
    files.write_text(''.join(json.dumps(str(p)) + '\n' for p in spec['sources']))
    plusargs = [*spec['args'], *args.plusarg]
    with tempfile.TemporaryDirectory(dir=out) as build:
        build = Path(build)
        compile_command, model = bench.commands('verilator', 'two-state', spec, files, build,
                                                {'verilator': args.verilator}, plusargs,
                                                'rtlmeter-steady-state-v1')
        print(f'{args.case}: building the Verilator model', flush=True)
        outcome = bench.execute(compile_command, out, out / 'build.log', args.timeout, args.cpu)
        if outcome['exitCode'] != 0:
            raise SystemExit(f'Verilator build failed: see {out / "build.log"}')
        shutil.copy2(build / 'sim', out / 'sim')
    print(f'{args.case}: Verilator run under perf stat', flush=True)
    result = run_one([out / 'sim', *plusargs], out / 'clean', spec, args.timeout, args.cpu, perf=True)
    summary = {'schemaVersion': 1, 'case': args.case, 'engine': 'verilator', 'sourceSha256': spec['sourceSha256'],
               'plusargs': plusargs, 'clean': result}
    (out / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    report(out)


def totals(summary):
    clean = summary.get('clean') or {}
    cycles = clean.get('cycles') or summary['profiled']['cycles']
    # Celox counts its execute phase; a Verilator model is counted whole.
    perf = clean.get('execute') or clean.get('perf', {})
    row = {'cycles': cycles}
    if 'instructions' in perf:
        row['instructions/clock'] = perf['instructions'] / cycles
    if 'cycles' in perf:
        row['cpu cycles/clock'] = perf['cycles'] / cycles
    if 'executeNs' in clean:
        row['execute ns/clock'] = clean['executeNs'] / cycles
    if 'compileNs' in clean:
        row['compile s'] = clean['compileNs'] / 1e9
    return row


def report(out):
    summary = json.loads((out / 'summary.json').read_text())
    print(f'\n{summary["case"]} ({summary["engine"]}): ' + ', '.join(
        f'{k} {v:.1f}' if isinstance(v, float) else f'{k} {v}' for k, v in totals(summary).items()))
    profiled, clean = summary.get('profiled', {}), summary.get('clean', {})
    if 'execute' in profiled and 'execute' in clean:
        extra = profiled['execute']['instructions'] - clean['execute']['instructions']
        print(f'the counters added {extra / profiled["cycles"]:.1f} instructions/clock to the profiled run')
    profile = out / 'profiled' / 'activation.json'
    if profile.is_file():
        profile_tool.main(['show', str(profile), '--cycles', str(summary['profiled']['cycles'])])
    print_attribution(summary, out, 30)


def compare(args):
    before = json.loads((args.before / 'summary.json').read_text())
    after = json.loads((args.after / 'summary.json').read_text())
    if before['case'] != after['case'] or before.get('plusargs') != after.get('plusargs'):
        raise SystemExit('the runs are of different cases or inputs')
    if before['sourceSha256'] != after['sourceSha256']:
        raise SystemExit('the runs simulated different sources')
    run_a = before.get('clean') or before['profiled']
    run_b = after.get('clean') or after['profiled']
    if run_a['cycles'] != run_b['cycles']:
        print(f'warning: clock counts differ: {run_a["cycles"]} vs {run_b["cycles"]}')
    if run_a['outputSha256'] != run_b['outputSha256']:
        print('warning: the program output differs')
    ta, tb = totals(before), totals(after)
    print(f'{before["case"]}: {before["engine"]} {args.before} -> {after["engine"]} {args.after}')
    print(f'{"":<20} {"before":>14} {"after":>14} {"delta":>12}')
    for key in ta:
        if key in tb and isinstance(ta[key], (int, float)):
            delta = tb[key] - ta[key]
            percent = f' ({delta / ta[key]:+.1%})' if ta[key] else ''
            print(f'{key:<20} {ta[key]:>14.1f} {tb[key]:>14.1f} {delta:>+12.1f}{percent}')
    ra = {row['role']: row for row in attribution_rows(before, args.before)}
    rb = {row['role']: row for row in attribution_rows(after, args.after)}
    if ra and rb:
        print(f'\ninstructions per clock by role (sampled), largest changes first:')
        print(f'{"before":>10} {"after":>10} {"delta":>10} {"activ b->a":>17} {"per activ b->a":>19}  role')
        changes = []
        for role in set(ra) | set(rb):
            a, b = ra.get(role, {}), rb.get(role, {})
            changes.append((b.get('perClock', 0) - a.get('perClock', 0), role, a, b))
        changes.sort(key=lambda row: -abs(row[0]))
        for delta, role, a, b in changes[:args.top]:
            activ = f'{a.get("activationsPerClock", 0):>8.3f}>{b.get("activationsPerClock", 0):<8.3f}' if 'activationsPerClock' in a or 'activationsPerClock' in b else ' ' * 17
            per = f'{a.get("perActivation", 0):>9.1f}>{b.get("perActivation", 0):<9.1f}' if 'perActivation' in a or 'perActivation' in b else ' ' * 19
            print(f'{a.get("perClock", 0):>10.1f} {b.get("perClock", 0):>10.1f} {delta:>+10.1f} {activ} {per}  {role}')
    pa, pb = args.before / 'profiled' / 'activation.json', args.after / 'profiled' / 'activation.json'
    if pa.is_file() and pb.is_file():
        print()
        profile_tool.main(['diff', str(pa), str(pb), '--cycles-before', str(before['profiled']['cycles']),
                           '--cycles-after', str(after['profiled']['cycles']), '--top', str(args.top)])


def disasm(args):
    """Print the machine code of the hottest sampled blocks, from the code
    the run mapped (`CELOX_PERF_MAP` saves it beside the perf map)."""
    summary = json.loads((args.run / 'summary.json').read_text())
    directory = args.run / 'sampled'
    symbols = {}
    for path in directory.glob('perf-*.map'):
        for line in path.read_text().splitlines():
            start, size, name = line.split(' ', 2)
            symbols[name] = (int(start, 16), int(size, 16))
    images = sorted((int(path.stem.rsplit('-', 1)[1], 16), path) for path in directory.glob('perf-*-*.bin'))
    cycles = summary['profiled']['cycles']
    shown = 0
    for symbol, instructions in summary['attribution']['blocks'].items():
        if args.match not in symbol or symbol not in symbols:
            continue
        start, size = symbols[symbol]
        base, image = next(((b, p) for b, p in reversed(images) if b <= start), (None, None))
        print(f'\n=== {symbol}: {instructions / cycles:.1f} instructions/clock, {size} bytes at {start:#x}', flush=True)
        if image is None:
            print('(code not saved)')
        else:
            subprocess.run(['objdump', '-D', '-b', 'binary', '-m', 'i386:x86-64', '-M', 'intel', '--no-show-raw-insn',
                            f'--adjust-vma={base:#x}', f'--start-address={start:#x}', f'--stop-address={start + size:#x}',
                            str(image)], check=False)
        shown += 1
        if shown >= args.top:
            break


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest='command', required=True)
    for name, function in (('run', run), ('verilator', verilator)):
        p = sub.add_parser(name)
        p.add_argument('case')
        p.add_argument('--out', type=Path, required=True)
        p.add_argument('--manifest', type=Path, default=ROOT / 'benchmarks/rtlmeter/steady_manifest.json')
        p.add_argument('--source', type=Path, default=Path(os.environ.get('RTLMETER_SOURCE', ROOT / 'work/rtlmeter-source')))
        p.add_argument('--plusarg', action='append', default=[], help='an extra plusarg, e.g. +max_cycles=100000')
        p.add_argument('--timeout', type=int, default=1800)
        p.add_argument('--cpu', type=int, default=min(os.sched_getaffinity(0)))
        if name == 'run':
            p.add_argument('--celox', required=True)
            p.add_argument('--celox-arg', action='append', default=[],
                           help='an extra celox option, e.g. --sir-pass=-partitioned_settle')
            p.add_argument('--no-clean', action='store_true', help='skip the run without counters')
            p.add_argument('--sample', action='store_true',
                           help='also sample the instructions of each native function (perf record)')
            p.add_argument('--sample-period', type=int, default=4000037,
                           help='instructions per sample; perf throttles more than kernel.perf_event_max_sample_rate per second')
        else:
            p.add_argument('--verilator', default='verilator')
        p.set_defaults(run=function)
    p = sub.add_parser('disasm', help='disassemble the sampled hottest blocks of a run')
    p.add_argument('run', type=Path)
    p.add_argument('--match', default='', help='only blocks whose symbol contains this')
    p.add_argument('--top', type=int, default=5)
    p.set_defaults(run_command=disasm)
    p = sub.add_parser('compare')
    p.add_argument('before', type=Path)
    p.add_argument('after', type=Path)
    p.add_argument('--top', type=int, default=40)
    p.set_defaults(run=compare)
    args = parser.parse_args()
    (getattr(args, 'run_command', None) or args.run)(args)


if __name__ == '__main__':
    main()
