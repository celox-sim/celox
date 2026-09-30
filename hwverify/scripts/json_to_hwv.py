#!/usr/bin/env python3
"""v2/v3/v4 JSON -> readable hwv source migration helper.

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
        raise ValueError(f"not an expression: {value!r}")
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
    version = doc['version']
    if version not in (2, 3, 4):
        raise ValueError('only schema versions 2, 3 and 4 are supported')
    if version in (3, 4) and doc.get('kind') != 'specification':
        raise ValueError(f'schema version {version} requires kind "specification"')
    kind = 'design' if version == 2 else 'specification'
    lines = [f"{kind} {json.dumps(doc.get('name', ''), ensure_ascii=False)}", '']

    def declarations(name, values, level):
        pad = ' ' * level
        lines.extend(f'{pad}{name} {n}: {type_name(t)};' for n, t in values.items())

    def assignments(name, values, level):
        pad = ' ' * level
        lines.append(f'{pad}{name} {{')
        lines.extend(f'{pad}  {n} = {expression(v, level + 2, infix)};' for n, v in values.items())
        lines.append(f'{pad}}}')

    def field(name, value, level=0):
        lines.append(' ' * level + name + ' ' + expression(value, level, infix) + ';')

    def examples(values, level):
        pad = ' ' * level
        for name, example in values.items():
            lines.append(f'{pad}example {name} {{')
            field('expect', example['expect'], level + 2)
            assignments('initial', example['initial'], level + 2)
            lines.append(f'{pad}  trace {{')
            for step in example['trace']:
                if version == 4:
                    if ('operation' in step) == ('actions' in step):
                        raise ValueError('a v4 trace frame needs exactly one of operation or actions')
                    if 'actions' in step:
                        if not isinstance(step['actions'], list) or not all(isinstance(a, str) for a in step['actions']):
                            raise ValueError('trace actions must be an array of names')
                        action = f"actions({', '.join(step['actions'])})"
                    else:
                        action = step['operation']
                else:
                    action = step['operation']
                lines.append(f'{pad}    {action} {{')
                assignments('inputs', step['inputs'], level + 6)
                assignments('observe', step['observe'], level + 6)
                lines.append(f'{pad}    }}')
            lines.extend([f'{pad}  }}', f'{pad}}}'])

    if version == 4:
        def signature(kind, name, target):
            ports = [f'{direction} {port}: {type_name(sort)}'
                     for direction, field in [('input', 'inputs'), ('output', 'outputs')]
                     for port, sort in target[field].items()]
            lines.append(f"{kind} {name}({', '.join(ports)}) {{")

        for name, spec in doc['specs'].items():
            signature('spec', name, spec)
            declarations('state', spec['state'], 2)
            field('init', spec['init'], 2)
            field('invariant', spec['invariant'], 2)
            for operation, relation in spec['operations'].items():
                lines.append(f'  operation {operation} = {expression(relation, 2, infix)};')
            examples(spec['examples'], 2)
            lines.extend(['}', ''])
        for name, composition in doc['compositions'].items():
            signature('composition', name, composition)
            for alias, instance in composition['instances'].items():
                connections = ', '.join(f'{child}: {parent}' for child, parent in instance['connections'].items())
                lines.append(f"  use {alias}: {instance['target']}({connections});")
            if 'operations' in composition:
                if not composition['operations']:
                    raise ValueError('explicit composition operations must be nonempty')
                for operation, actions in composition['operations'].items():
                    if not isinstance(actions, list) or not actions or not all(isinstance(a, str) for a in actions):
                        raise ValueError('a composition action group must be a nonempty array of names')
                    lines.append(f"  operation {operation} = actions({', '.join(actions)});")
            examples(composition['examples'], 2)
            lines.extend(['}', ''])
        if 'implementation' in doc:
            implementation = doc['implementation']
            lines.append('implementation {')
            field('composition', implementation['composition'], 2)
            declarations('input', implementation['inputs'], 2)
            field('reset_input', implementation['reset_input'], 2)
            declarations('state', implementation['state'], 2)
            for section in ['reset', 'wires', 'next', 'operations']:
                if section in implementation:
                    assignments(section, implementation[section], 2)
            lines.append('  binding {')
            for name, mapping in implementation['binding']['states'].items():
                assignments(f'bind {name}', mapping, 4)
            for name, value in implementation['binding']['outputs'].items():
                lines.append(f'    output {name} = {expression(value, 4, infix)};')
            lines.extend(['  }', '}'])
        return '\n'.join(lines).rstrip() + '\n'

    declarations('input', doc['inputs'], 0)
    if version == 3:
        declarations('observation', doc['observations'], 0)
        for name, operation in doc['operations'].items():
            if operation != {}:
                raise ValueError(f'operation {name!r} must be an empty object')
            lines.append(f'operation {name} {{}}')
        for name, component in doc['components'].items():
            lines.extend(['', f'component {name} {{'])
            declarations('state', component['state'], 2)
            field('init', component['init'], 2)
            field('invariant', component['invariant'], 2)
            assignments('steps', component['steps'], 2)
            examples(component['examples'], 2)
            lines.append('}')
        for name, composition in doc['compositions'].items():
            lines.extend(['', f'composition {name} {{'])
            field('members', ['compose', *composition['members']], 2)
            examples(composition['examples'], 2)
            lines.append('}')
        if 'implementation' in doc:
            implementation = doc['implementation']
            lines.extend(['', 'implementation {'])
            field('composition', implementation['composition'], 2)
            field('reset_input', implementation['reset_input'], 2)
            declarations('state', implementation['state'], 2)
            for section in ['reset', 'wires', 'next', 'operations']:
                if section in implementation:
                    assignments(section, implementation[section], 2)
            lines.append('  binding {')
            for name, mapping in implementation['binding']['states'].items():
                assignments(f'bind {name}', mapping, 4)
            for name, value in implementation['binding']['observations'].items():
                lines.append(f'    observation {name} = {expression(value, 4, infix)};')
            lines.extend(['  }', '}'])
        return '\n'.join(lines).rstrip() + '\n'

    field('reset_input', doc['reset_input'])
    for machine in ['spec', 'impl']:
        lines.extend(['', f'{machine} {{'])
        declarations('state', doc[machine]['state'], 2)
        for section in ['reset', 'wires', 'next', 'outputs']:
            if section in doc[machine]:
                assignments(section, doc[machine][section], 2)
        lines.append('}')
    lines.append('')
    for key in ['binding', 'commit', 'can_step', 'hold_when']:
        if key in doc:
            field(key, doc[key])
    lines.append('progress {')
    for key in ['enabled', 'rank']:
        field(key, doc['progress'][key], 2)
    lines.append('}')
    if 'program_contract' in doc:
        c = doc['program_contract']
        lines.extend(['', 'contract {'])
        declarations('parameter', c['parameters'], 2)
        for dsl, key in [('pre', 'precondition'), ('invariant', 'invariant'), ('terminal', 'terminal'), ('post', 'postcondition'), ('rank', 'rank')]:
            field(dsl, c[key], 2)
        if 'partitioning' in c:
            field('partition', c['partitioning'], 2)
        if 'cases' in c:
            assignments('cases', c['cases'], 2)
        if 'split' in c:
            assignments('split', {name: ['range', hint['expr'], hint['min'], hint['max']] for name, hint in c['split'].items()}, 2)
        lines.append('}')
    return '\n'.join(lines) + '\n'


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('source', type=Path)
    parser.add_argument('destination', type=Path)
    parser.add_argument('--infix', action='store_true', help='print parenthesized infix operators where exact')
    args = parser.parse_args()
    args.destination.write_text(print_document(json.loads(args.source.read_text()), args.infix), encoding='utf-8')
