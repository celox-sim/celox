#!/usr/bin/env python3
"""Show or compare activation profiles (`celox run --activation-profile FILE`).

Every count is normalized per RTL clock: the cycle count given with
--cycles, read from `_rtlmeter_cycles.txt` next to the profile, or else the
runs of the most frequent clock domain.

  activation-profile.py show PROFILE [--cycles N]
  activation-profile.py diff BEFORE AFTER [--cycles N | --cycles-before N --cycles-after N]
"""
import argparse
import json
import sys
from pathlib import Path

# Measures that are a part of another measure of the same subject: shown as
# a share of it.
SHARES = {
    'quiet': 'judged',
    'several': 'judged',
    'dirty': 'entered',
    'dirty: inputs': 'entered',
    'dirty: clock/time': 'entered',
    'dirty: published': 'entered',
    'partial cone': 'entered',
    'changed': 'checked',
}


def load(path):
    data = json.loads(Path(path).read_text())
    if data.get('version') != 1:
        raise SystemExit(f'{path}: unknown activation profile version {data.get("version")}')
    return data


def counters(profile):
    out = {}
    for counter in profile.get('counters') or []:
        key = (counter['kind'], counter['subject'], counter['measure'])
        out[key] = out.get(key, 0) + counter['count']
    return out


def domain_runs(profile):
    runs = {}
    for (kind, subject, measure), count in counters(profile).items():
        if kind == 'domain' and measure in ('eval_apply', 'eval_only', 'fused'):
            runs[subject] = runs.get(subject, 0) + count
    return runs


def clocks(path, profile, given):
    """The RTL clocks to normalize by, and where the number comes from."""
    if given:
        return given, 'given'
    cycles = Path(path).resolve().parent / '_rtlmeter_cycles.txt'
    if cycles.is_file() and cycles.read_text().strip().isdigit():
        return int(cycles.read_text().strip()), 'rtlmeter'
    runs = domain_runs(profile)
    if runs:
        subject, count = max(runs.items(), key=lambda item: item[1])
        return max(count, 1), f'runs of domain {subject}'
    return 1, 'no clock (absolute counts)'


def share(table, key):
    kind, subject, measure = key
    whole = SHARES.get(measure)
    if whole is None:
        if kind == 'settle_call':
            entered = table.get(('settle', '', 'entered'))
            return table[key] / entered if entered else None
        return None
    total = table.get((kind, subject, whole))
    return table[key] / total if total else None


def label(key):
    kind, subject, measure = key
    return f'{kind:<13} {measure:<18} {subject}'


def partitions(profile):
    settle = profile.get('settle_partitions')
    if not settle:
        return None
    runs, changed = settle['runs'], settle['changed']
    return settle['settles'], runs, changed


def show(args):
    profile = load(args.profile)
    if profile.get('counters') is None:
        print(f'no activation counters: {profile.get("unavailable", "")}')
    base, source = clocks(args.profile, profile, args.cycles)
    table = counters(profile)
    print(f'activation profile {args.profile}: per RTL clock = /{base} ({source})')
    scheduler = profile.get('scheduler')
    if scheduler:
        print('scheduler: ' + ', '.join(f'{k} {v}' for k, v in scheduler.items()))
    findings = []
    for key, count in table.items():
        part = share(table, key)
        if key[2] == 'quiet' and part is not None and part >= 0.2:
            findings.append(f'{part:.1%} of the {key[1]} domain judgments found no edge ({count / base:.3f}/clock)')
        if key == ('settle', '', 'dirty') and part is not None:
            findings.append(f'the settle ran in {part:.1%} of its {table[("settle", "", "entered")] / base:.3f} entries per clock')
    settle = partitions(profile)
    if settle:
        settles, runs, changed = settle
        total, unchanged = sum(runs), sum(runs) - sum(changed)
        if total:
            findings.append(f'partitioned settle: {total / base:.2f} partition runs/clock, {unchanged / total:.1%} changed nothing they compare')
    if findings:
        print('\nfindings:')
        for finding in findings:
            print(f'  - {finding}')
    print(f'\n{"kind":<13} {"measure":<18} {"count":>14} {"/clock":>10} {"share":>7}  subject')
    order = sorted(table, key=lambda key: (key[0], -table[key]))
    idle = [key for key in order if table[key] == 0]
    if not args.all:
        order = [key for key in order if table[key]]
    for key in order:
        count = table[key]
        part = share(table, key)
        part = f'{part:7.1%}' if part is not None else ' ' * 7
        print(f'{key[0]:<13} {key[2]:<18} {count:>14} {count / base:>10.3f} {part}  {key[1]}')
    if idle and not args.all:
        kinds = sorted({key[0] for key in idle})
        print(f'({len(idle)} counters never counted: {", ".join(kinds)}; --all lists them)')
    if settle:
        settles, runs, changed = settle
        print(f'\npartitioned settle: {settles} settles ({settles / base:.3f}/clock), {len(runs)} partitions')
        print(f'{"partition":>9} {"runs/clock":>10} {"unchanged":>9}')
        ranked = sorted(range(len(runs)), key=lambda i: runs[i] - changed[i], reverse=True)
        for i in ranked[:args.top]:
            if runs[i]:
                print(f'{i:>9} {runs[i] / base:>10.3f} {(runs[i] - changed[i]) / runs[i]:>9.1%}')


def diff(args):
    before, after = load(args.before), load(args.after)
    cycles_before = args.cycles_before or args.cycles
    cycles_after = args.cycles_after or args.cycles
    base_a, source_a = clocks(args.before, before, cycles_before)
    base_b, source_b = clocks(args.after, after, cycles_after)
    a, b = counters(before), counters(after)
    print(f'before {args.before}: /{base_a} ({source_a})')
    print(f'after  {args.after}: /{base_b} ({source_b})')
    rows = []
    for key in set(a) | set(b):
        ra = a.get(key, 0) / base_a
        rb = b.get(key, 0) / base_b
        if ra != rb:
            rows.append((abs(rb - ra), key, ra, rb))
    rows.sort(key=lambda row: (-row[0], row[1]))
    print(f'\n{"kind":<13} {"measure":<18} {"before/clk":>10} {"after/clk":>10} {"delta":>10}  subject')
    for _, key, ra, rb in rows[:args.top]:
        mark = '' if key in a and key in b else ('  (after only)' if key not in a else '  (before only)')
        print(f'{key[0]:<13} {key[2]:<18} {ra:>10.3f} {rb:>10.3f} {rb - ra:>+10.3f}  {key[1]}{mark}')
    if len(rows) > args.top:
        print(f'... {len(rows) - args.top} more changed counters (--top)')
    sa, sb = partitions(before), partitions(after)
    if sa and sb:
        ta, tb = sum(sa[1]), sum(sb[1])
        ua, ub = ta - sum(sa[2]), tb - sum(sb[2])
        print(f'\npartition runs/clock {ta / base_a:.3f} -> {tb / base_b:.3f}; '
              f'unchanged {ua / base_a:.3f} -> {ub / base_b:.3f}')


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest='command', required=True)
    p = sub.add_parser('show')
    p.add_argument('profile')
    p.add_argument('--cycles', type=int)
    p.add_argument('--top', type=int, default=20)
    p.add_argument('--all', action='store_true', help='also list the counters that never counted')
    p.set_defaults(run=show)
    p = sub.add_parser('diff')
    p.add_argument('before')
    p.add_argument('after')
    p.add_argument('--cycles', type=int)
    p.add_argument('--cycles-before', type=int)
    p.add_argument('--cycles-after', type=int)
    p.add_argument('--top', type=int, default=40)
    p.set_defaults(run=diff)
    args = parser.parse_args(argv)
    args.run(args)


if __name__ == '__main__':
    sys.exit(main())
