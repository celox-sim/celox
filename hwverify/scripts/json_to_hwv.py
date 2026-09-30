#!/usr/bin/env python3
"""v2 JSON -> readable hwv source migration helper.

The hwverify parser/validator, not this printer, is the authority for acceptance.
Call syntax deliberately preserves each legacy expression tree and literal value.
"""
import argparse
import json
from pathlib import Path


def expression(value, indent=0, infix=False):
    if isinstance(value, bool):
        return str(value).lower()
    if isinstance(value, str):
        return value
    if isinstance(value, int):
        return str(value)
    if not isinstance(value, list) or not value or not isinstance(value[0], str):
        raise ValueError(f"not a v2 expression: {value!r}")
    op, *args = value
    rendered = [expression(x, indent + 2, infix) for x in args]
    operators = {"and": "&&", "or": "||", "eq": "==", "ne": "!=", "add": "+", "sub": "-", "mul": "*", "band": "&", "bor": "|", "bxor": "^", "shl": "<<", "lshr": ">>", "ult": "<", "ule": "<="}
    if infix and op in operators and len(rendered) == 2:
        return f"({rendered[0]} {operators[op]} {rendered[1]})"
    if infix and op in {"not", "bnot"} and len(rendered) == 1:
        return "(" + {"not": "!", "bnot": "~"}[op] + rendered[0] + ")"
    short = f"{op}({', '.join(rendered)})"
    if '\n' not in short and len(short) + indent <= 100:
        return short
    pad = ' ' * (indent + 2)
    return f"{op}(\n" + pad + (',\n' + pad).join(rendered) + '\n' + ' ' * indent + ')'


def type_name(value):
    if value == 'bool':
        return 'bool'
    if set(value) == {'bv'}:
        return f"bv<{value['bv']}>"
    if set(value) == {'mem'}:
        a, w = value['mem']
        return f'mem<{a}, {w}>'
    raise ValueError(f'not a type: {value!r}')


def print_document(doc, infix=False):
    if doc['version'] != 2:
        raise ValueError('only schema version 2 is supported')
    lines = [f"design {json.dumps(doc.get('name', ''), ensure_ascii=False)} {{"]

    def declarations(name, values, level):
        pad = ' ' * level
        lines.append(f'{pad}{name} {{')
        lines.extend(f'{pad}  {n}: {type_name(t)};' for n, t in values.items())
        lines.append(f'{pad}}}')

    def assignments(name, values, level):
        pad = ' ' * level
        lines.append(f'{pad}{name} {{')
        lines.extend(f'{pad}  {n} = {expression(v, level + 2, infix)};' for n, v in values.items())
        lines.append(f'{pad}}}')

    def field(name, value, level=2):
        lines.append(' ' * level + name + ' ' + expression(value, level, infix) + ';')

    declarations('inputs', doc['inputs'], 2)
    field('reset_input', doc['reset_input'])
    for machine in ['spec', 'impl']:
        lines.extend(['', f'  {machine} {{'])
        declarations('state', doc[machine]['state'], 4)
        for section in ['reset', 'wires', 'next', 'outputs']:
            if section in doc[machine]:
                assignments(section, doc[machine][section], 4)
        lines.append('  }')
    lines.append('')
    for key in ['binding', 'commit', 'can_step', 'hold_when']:
        if key in doc:
            field(key, doc[key])
    lines.append('  progress {')
    for key in ['enabled', 'rank']:
        field(key, doc['progress'][key], 4)
    lines.append('  }')
    if 'program_contract' in doc:
        c = doc['program_contract']
        lines.extend(['', '  contract {'])
        declarations('parameters', c['parameters'], 4)
        for dsl, key in [('pre', 'precondition'), ('invariant', 'invariant'), ('terminal', 'terminal'), ('post', 'postcondition'), ('rank', 'rank')]:
            field(dsl, c[key], 4)
        if 'partitioning' in c:
            field('partition', c['partitioning'], 4)
        if 'cases' in c:
            assignments('cases', c['cases'], 4)
        if 'split' in c:
            assignments('split', {name: ['range', hint['expr'], hint['min'], hint['max']] for name, hint in c['split'].items()}, 4)
        lines.append('  }')
    lines.append('}')
    return '\n'.join(lines) + '\n'


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('source', type=Path)
    parser.add_argument('destination', type=Path)
    parser.add_argument('--infix', action='store_true', help='print parenthesized infix operators where exact')
    args = parser.parse_args()
    args.destination.write_text(print_document(json.loads(args.source.read_text()), args.infix), encoding='utf-8')
