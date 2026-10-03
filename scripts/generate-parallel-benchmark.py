#!/usr/bin/env python3
"""Generate a name-independent flat or hierarchical arithmetic workload.

This is a synthetic throughput/control experiment, not a hardware timing model.
The same lane equations are used in both forms; --coupled adds cross-lane reads.
"""
import argparse
from pathlib import Path

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--output', type=Path, required=True)
p.add_argument('--lanes', type=int, default=32)
p.add_argument('--depth', type=int, default=64)
p.add_argument('--hierarchical', action='store_true')
p.add_argument('--coupled', action='store_true')
a = p.parse_args()
assert a.lanes > 0 and a.depth > 0
a.output.mkdir(parents=True, exist_ok=False)
(a.output / 'src').mkdir()
(a.output / 'Veryl.toml').write_text('[project]\nname = "parallel_workload"\nversion = "0.1.0"\n[build]\nsources = ["src"]\n')


def body(q, d, seed):
    lines = [f'var x_{q}: logic<64>[{a.depth + 1}];', f'assign x_{q}[0] = {d};']
    for stage in range(a.depth):
        # Each stage depends on the previous value; do not feed a constant-only
        # circuit to the optimizer and mistake compile-time evaluation for speed.
        lines.append(f'assign x_{q}[{stage + 1}] = ((x_{q}[{stage}] << 7) ^ (x_{q}[{stage}] >> 3) ^ x_{q}[{stage}]) + 64\'h9e3779b97f4a7c15;')
    lines.append(f'always_ff (clk) {{ if clear {{ {q} = {seed}; }} else {{ {q} = x_{q}[{a.depth}]; }} }}')
    return '\n'.join(lines)

source = []
if a.hierarchical:
    source.append('module Arithmetic #(param SEED: u32 = 1) (clk: input clock, clear: input logic, d: input logic<64>, q: output logic<64>) {')
    source.append(body('q', 'd', 'SEED'))
    source.append('}')
source.append('#[test(workload)] module workload { inst clk: $tb::clock_gen; var clear: logic;')
for i in range(a.lanes):
    source.append(f'var q{i}: logic<64>;')
for i in range(a.lanes):
    d = f'q{(i + a.lanes - 1) % a.lanes}' if a.coupled else f'q{i}'
    if a.hierarchical:
        source.append(f'inst arbitrary_name_{i}: Arithmetic #(SEED: {i + 1}) (clk, clear, d: {d}, q: q{i});')
    else:
        source.append(body(f'q{i}', d, i + 1))
source.append('initial { clear = 1; clk.next(1); clear = 0; for epoch in 0..100 { clk.next(10000);')
source.append('$display("%h", ' + ' ^ '.join(f'q{i}' for i in range(a.lanes)) + ');')
source.append('} $finish(); } }')
(a.output / 'src/workload.veryl').write_text('\n'.join(source) + '\n')
