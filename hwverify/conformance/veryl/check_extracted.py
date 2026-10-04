#!/usr/bin/env python3
"""Captured Rust predicates -> true Veryl analyzer -> finite-trace checks."""
import copy
import hashlib
import json
import os
from pathlib import Path
import subprocess
from analyzer_lower import Lower, Unsupported
from finite_trace_check import run as finite_run

ROOT = Path(__file__).resolve().parent
PROBE = Path(os.environ.get('VERYL_PROBE_BIN', ROOT / 'analyzer-probe/target/debug/veryl-analyzer-probe'))
HWVERIFY = Path(os.environ.get('HWVERIFY_BIN', ROOT.parents[1] / 'target/release/hwverify-rs'))
TRIPWIRE = str(ROOT / 'tools/z3-tripwire.sh')


def run(document, binary, out):
    return finite_run(document, binary, out, z3_path=TRIPWIRE)


def analyze(row, destination):
    if row['status'] != 'extracted':
        raise ValueError('case was not fully extracted')
    if len(row['design']['sources']) != 1:
        raise Unsupported('source_bundle', 'multiple source files outside bounded adapter')
    source = row['design']['sources'][0]
    if hashlib.sha256(source['text'].encode()).hexdigest() != source['sha256']:
        raise ValueError('captured design hash changed')
    path = destination / 'source.veryl'
    path.write_text(source['text'])
    process = subprocess.run([str(PROBE), str(path)], capture_output=True, text=True, timeout=30)
    (destination / 'analyzer.stderr.txt').write_text(process.stderr)
    if process.returncode:
        # Typed diagnostic prefix is a disposition, never a language-rejection pass.
        # Unexpected errors are fatal; pinned known diagnostics are golden-gated.
        prefixes = {'functions or interfaces': 'functions_interfaces', 'unpacked arrays': 'arrays',
                    'unsupported type:': 'type', 'unresolved width:': 'unresolved_width',
                    'index/part-select': 'partial_write_select', 'unpacked index': 'array_index',
                    'non-numeric value': 'non_numeric_value', 'factor outside': 'factor',
                    'expression outside': 'expression', 'statement outside': 'statement',
                    'declaration outside': 'declaration', 'non-module component': 'non_module'}
        message = process.stderr.strip()
        for prefix, code in prefixes.items():
            if message.startswith(prefix):
                raise Unsupported('probe.' + code, message.splitlines()[0])
        for stage in ('parse ', 'pass1:', 'post_pass1:', 'pass2:', 'post_pass2:'):
            if message.startswith(stage):
                diagnostic = message.split(': ', 1)[-1].split(' {', 1)[0]
                raise Unsupported('analyzer.' + stage.split(':')[0].strip() + '.' + diagnostic, message.splitlines()[0])
        if 'panicked at' in message:
            raise Unsupported('analyzer.panic', message)
        raise RuntimeError(f'unexpected analyzer failure {process.returncode}: {message}')
    ir = json.loads(process.stdout)
    (destination / 'analyzer-ir.json').write_text(process.stdout)
    doc = Lower(ir).build(row)
    (destination / 'design.json').write_text(json.dumps(doc, indent=2) + '\n')
    return ir, doc


def check(row, destination, controls=True, analyzed=None):
    destination.mkdir(parents=True, exist_ok=False)
    try:
        ir, doc = analyzed if analyzed else analyze(row, destination)
    except Unsupported as error:
        return {'case': row['case'], 'status': 'unsupported_fv_subset', 'code': error.code, 'reason': str(error)}
    normal = run(doc, HWVERIFY, destination / 'normal')
    result = {'case': row['case'], 'status': normal['status'], 'assertions': row['assertion_count'],
              'frames': len(doc['specs']['Top']['examples']['original_assertions']['trace']), 'normal': normal}
    if controls:
        bad = copy.deepcopy(doc)
        for frame in bad['specs']['Top']['examples']['original_assertions']['trace']:
            if 'ensure' in frame:
                frame['ensure'] = ['not', frame['ensure']]
                break
        result['negated_assertion'] = run(bad, HWVERIFY, destination / 'negated-assertion')
        impossible = copy.deepcopy(doc)
        impossible['specs']['Top']['init'] = False
        result['impossible_initial_state'] = run(impossible, HWVERIFY, destination / 'impossible-initial-state')
        duplicate = copy.deepcopy(row)
        for index, action in enumerate(duplicate['actions']):
            if action['action'] == 'read':
                other = copy.deepcopy(action)
                other['assertion']['comparison'] = 'ne' if action['assertion']['comparison'] == 'eq' else 'eq'
                duplicate['actions'].insert(index + 1, other)
                duplicate['assertion_count'] += 1
                break
        result['contradictory_duplicate'] = run(Lower(ir).build(duplicate), HWVERIFY, destination / 'contradictory-duplicate')
        for control in ('negated_assertion', 'contradictory_duplicate'):
            if result[control]['status'] != 'failed' or result[control]['cases'][0]['feasible'] is not True:
                raise RuntimeError(f'{row["case"]}: {control} must fail while feasible')
        if result['impossible_initial_state']['status'] != 'failed' or result['impossible_initial_state']['cases'][0]['feasible'] is not False:
            raise RuntimeError(f'{row["case"]}: impossible trace must fail as infeasible')
        if normal['status'] != 'passed':
            raise RuntimeError(f'{row["case"]}: expected finite baseline pass, got {normal["status"]}')
    return result
