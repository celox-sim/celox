"""Strict fixed-matrix acceptance for hint-free automatic proof discovery."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import time

from audit.automatic_proof import run as evaluation
from audit.automatic_proof.test_generate import evaluate
from audit.veryl_scaling.rv32i_pipeline import validate_conjunctive_obligation

ROOT = Path(__file__).resolve().parents[2]
OBLIGATIONS = {'binding_nonempty', 'reset_binding', 'microstep_refinement',
               'commit_eligible', 'progress_nonvacuity',
               'commit_reachable_in_relation', 'noncommit_rank_decreases'}
EXPECTED_RESULTS = {name: ('sat' if name in ('binding_nonempty', 'progress_nonvacuity',
                    'commit_reachable_in_relation') else 'unsat') for name in OBLIGATIONS}
NAME = re.compile(r'[a-z][a-z0-9_]*\Z')


def require(ok, message):
    if not ok:
        raise ValueError(message)


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def matrix():
    value = json.loads((Path(__file__).with_name('ci-matrix.json')).read_text())
    require(value['schema'] == 'automatic-proof-ci-v1', 'invalid CI matrix')
    rows = [r for g in value['groups'] for r in g['cases']]
    require(len(rows) == 29 and sum(r['expected'] == 'verified' for r in rows) == 11
            and sum(r['expected'] == 'counterexample' for r in rows) == 18,
            'missing or unexpected matrix coverage')
    require(len({g['label'] for g in value['groups']}) == len(value['groups']),
            'duplicate group')
    for group in value['groups']:
        require(NAME.fullmatch(group['label']) and group['width'] in (24, 32, 64)
                and group['alpha_seed'] in (None, 7301), 'unapproved matrix shape')
        require(len({r['name'] for r in group['cases']}) == len(group['cases']),
                'duplicate case')
        for row in group['cases']:
            require(NAME.fullmatch(row['name']) and NAME.fullmatch(row['base_name']),
                    'invalid case name')
    return value


def relative_file(root, name):
    require(isinstance(name, str) and name and Path(name).name == name
            and name not in ('.', '..'), 'unsafe or missing evidence name')
    path = root / name
    require(path.is_file() and not path.is_symlink(), 'missing evidence: ' + name)
    return path


def query_evidence(root, query):
    """Require referenced exact-query and model sidecars, including attempts."""
    require(query.get('backend') in ('finite_bv', 'structural_kernel',
            'conjunctive_lemmas', 'checked_proof_bundle'), 'unknown or external proof backend')
    require(query.get('z3_seconds', 0) == 0, 'external solver time recorded')
    expectation, verdict, status = (query.get('logical_expectation'),
                                    query.get('solver_result'), query.get('status'))
    require(expectation in ('sat', 'unsat') and status in ('passed', 'unknown', 'counterexample')
            and ((status == 'passed' and verdict == expectation)
                 or (status == 'unknown' and verdict == 'unknown')
                 or (status == 'counterexample' and verdict in ('sat', 'unsat')
                     and verdict != expectation)), 'query status/result/expectation mismatch')
    if query.get('backend') == 'structural_kernel':
        require(query.get('solver_result') == 'unsat' and query.get('status') == 'passed',
                'structural kernel has invalid verdict')
    if query.get('backend') in ('finite_bv', 'structural_kernel', 'checked_proof_bundle'):
        require(query.get('evidence') and query.get('solver_output', query.get('backend') == 'checked_proof_bundle'),
                'missing primitive query evidence')
    if query.get('backend') == 'finite_bv':
        require(query.get('finite_diagnostics'), 'missing finite diagnostics')
        finite = query.get('finite', {})
        require(finite.get('solver_result') == query.get('solver_result'),
                'finite/report solver verdict differs')
        for key, limit in (('terms', 100000), ('variables', 200000)):
            value = finite.get(key)
            require(type(value) is int and 0 <= value <= limit,
                    'primitive term/variable budget changed or exceeded')
        work, clauses = finite.get('work'), finite.get('clauses')
        require(type(clauses) is int and 0 <= clauses <= 1000000,
                'primitive clause budget changed or exceeded')
        exhausted = (query.get('status') == 'unknown' and query.get('solver_result') == 'unknown'
                     and finite.get('solver_result') == 'unknown'
                     and finite.get('reason') == 'finite solver work budget exhausted')
        require(type(work) is int and (0 <= work <= 100000000
                or (work == 100000001 and exhausted)),
                'primitive work budget changed or exceeded')
    for key in ('evidence', 'solver_output', 'finite_diagnostics'):
        if key in query:
            path = relative_file(root, query[key])
            if key == 'evidence' and query['backend'] in ('finite_bv', 'structural_kernel'):
                timeouts = re.findall(r'\(set-option\s+:timeout\s+(\d+)\s*\)', path.read_text())
                require(timeouts == ['10000'], 'emitted primitive timeout changed or missing')
            if key == 'solver_output':
                lines = path.read_text().splitlines()
                require(lines and lines[0].strip() == query['solver_result'],
                        'solver output contradicts fresh report')
            if key == 'finite_diagnostics':
                require(json.loads(path.read_text()) == query.get('finite'),
                        'finite sidecar differs from fresh report')
    for node in query.get('proof_graph', []):
        relative_file(root, node['statement'])
    if query.get('kernel', {}).get('residual'):
        relative_file(root, query['kernel']['residual'])
    for key in ('original_attempt', 'original_recheck'):
        if isinstance(query.get(key), dict):
            query_evidence(root, query[key])
    for key in ('children', 'conjunctive_children'):
        for child in query.get(key, []):
            query_evidence(root, child)


def replay_mutant(doc, finite):
    """Independent concrete replay for these fixed registered datapath families."""
    values = finite['context_values']
    env = {}
    for name, sort in doc['inputs'].items():
        item = values['i.' + name]
        if sort == 'bool':
            require(item['sort'] == 'Bool' and type(item['value']) is bool,
                    'invalid Boolean witness')
        else:
            require(item['sort'] == 'Bv' and item['width'] == sort['bv']
                    and type(item['value']) is int
                    and 0 <= item['value'] < (1 << sort['bv']), 'invalid word witness')
        env['i.' + name] = item['value']
    require(env['i.' + doc['reset_input']] is False, 'reset witness is not a step')
    state = next(iter(doc['spec']['state']))
    require(doc['spec']['state'] == doc['impl']['state'] and len(doc['spec']['state']) == 1,
            'unexpected fixture state')
    width = doc['spec']['state'][state]['bv']
    require(values['spec.' + state]['value'] == values['impl.' + state]['value'],
            'witness does not satisfy original pre-binding')
    require(values['binding_before']['value'] is True and values['commit']['value'] is True
            and values['binding_after']['value'] is False, 'missing original step context')
    left = evaluate(doc['spec']['next'][state], env, (1 << width) - 1)
    right = evaluate(doc['impl']['next'][state], env, (1 << width) - 1)
    require(left != right, 'original generated formulas do not replay the mutant')
    return {'original_formula_replayed_by_python': True, 'spec_next': left, 'impl_next': right}


def validate_group(out, group, summary, checker_hash):
    expected = {r['name']: r for r in group['cases']}
    rows = summary.get('rows', [])
    require(len(rows) == len(expected) and {r['name'] for r in rows} == set(expected),
            'missing, duplicate or extra executed cases')
    require(summary['checker_sha256'] == checker_hash and summary['automatic'] is True
            and summary['per_query_limits_unchanged'] is True, 'checker or mode changed')
    require(summary['environment'] == {'HWVERIFY_SOLVER': 'finite',
            'HWVERIFY_CONJUNCTIVE_LEMMAS': '1',
            'HWVERIFY_AUTOMATIC_PROOFS': 'independent_lemmas'}, 'solver overrides changed')
    verified = []
    for row in rows:
        wanted = expected[row['name']]
        input_path = relative_file(out, row['name'] + '.json')
        require(sha(input_path) == wanted['input_sha256'], 'fixed fixture changed')
        doc = json.loads(input_path.read_text())
        require('proof_programs' not in doc, 'fixture unexpectedly contains proof hints')
        case = out / row['name']
        report_path = relative_file(case, 'report.json')
        report = json.loads(report_path.read_text())
        require(report.get('name') == row['name'] and report['status'] == row['status'],
                'report identity or status mismatch')
        require(report.get('engine_summary', {}).get('z3_queries') == 0
                and report.get('engine_summary', {}).get('z3_seconds') == 0,
                'external solver query recorded')
        obligations = report.get('obligations', [])
        for query in obligations:
            query_evidence(case, query)
        micro = [q for q in obligations if q['name'] == 'microstep_refinement']
        require(len(micro) == 1, 'missing or duplicate original microstep')
        record = {'name': row['name'], 'expected': wanted['expected'],
                  'input_sha256': wanted['input_sha256'], 'report_sha256': sha(report_path)}
        if wanted['expected'] == 'verified':
            require(row['exit_code'] == 0 and report['status'] == 'stuttering_refinement_verified'
                    and len(obligations) == len(OBLIGATIONS)
                    and {q['name'] for q in obligations} == OBLIGATIONS
                    and all(q['status'] == 'passed' for q in obligations),
                    'positive is Unknown, incomplete, or failed')
            for query in obligations:
                require(query.get('logical_expectation') == EXPECTED_RESULTS[query['name']]
                        and query.get('solver_result') == EXPECTED_RESULTS[query['name']],
                        'covered obligation has wrong logical expectation or solver verdict')
                require(query['backend'] in ('finite_bv', 'structural_kernel', 'conjunctive_lemmas'),
                        'unexpected proof backend')
                if query['backend'] == 'conjunctive_lemmas':
                    validate_conjunctive_obligation(query)
        else:
            q = micro[0]
            require(row['exit_code'] == 1 and report['status'] == 'counterexample'
                    and q['status'] == 'counterexample' and q['solver_result'] == 'sat'
                    and q.get('logical_expectation') == 'unsat'
                    and q['backend'] == 'finite_bv'
                    and q.get('finite', {}).get('original_formula_validated') is True,
                    'negative lacks replay-validated original SAT')
            require(q.get('finite_diagnostics') and q.get('evidence') and q.get('solver_output'),
                    'negative lacks original formula or model sidecars')
            require(relative_file(case, q['solver_output']).read_text().splitlines()[0].strip() == 'sat',
                    'negative solver output is not SAT')
            record.update(replay_mutant(doc, q['finite']))
        verified.append(record)
    return verified


def source_files():
    repo = ROOT.parent
    paths = {repo / 'Cargo.toml', repo / 'Cargo.lock', Path(__file__).with_name('ci-matrix.json'),
             repo / '.github/workflows/hwverify.yml',
             ROOT / 'conformance/veryl-symbolic/run_automatic_ci.sh',
             ROOT / 'conformance/veryl-proof/tools/z3-tripwire.sh',
             Path(__file__).with_name('test_ci.py')}
    for crate in repo.joinpath('crates').glob('hwverify-*'):
        paths.update(crate.rglob('*.rs'))
        paths.update(crate.rglob('Cargo.toml'))
    for module in tuple(sys.modules.values()):
        name = getattr(module, '__file__', None)
        if name:
            path = Path(name).resolve()
            if path.suffix == '.py' and ROOT in path.parents:
                paths.add(path)
    return sorted(paths)


def check_tripwire(marker):
    require(not marker.exists(), 'external solver tripwire fired')


def check_seal(fingerprints):
    require(all(Path(p).is_file() and sha(p) == digest for p, digest in fingerprints.items()),
            'source, fixture matrix, or checker changed during gate')


def gate(out, checker):
    out = Path(out).resolve()
    checker = Path(checker).resolve()
    require(out != ROOT and ROOT not in out.parents, 'evidence must be outside checkout')
    out.mkdir(parents=True, exist_ok=False)
    started = time.monotonic()
    result = {'status': 'failed', 'positive_passed': 0, 'mutants_original_replayed': 0,
              'scope': 'fixed generic automatic-proof CI; no RV32I hint-removal claim'}
    marker = out / 'external-solver-invoked.txt'
    old_env = {key: os.environ.get(key) for key in ('PATH', 'Z3_BIN', 'Z3_TRIPWIRE_MARKER')}
    try:
        require(__debug__, 'PYTHONOPTIMIZE is forbidden')
        plan = matrix()
        snapshots = out / 'source-snapshots'
        snapshots.mkdir()
        fingerprints = {}
        for path in source_files():
            fingerprints[str(path)] = sha(path)
            (snapshots / (sha(path) + '-' + path.name)).write_bytes(path.read_bytes())
        fingerprints[str(checker)] = sha(checker)
        revision = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip()
        (out / 'provenance.json').write_text(json.dumps({'revision': revision,
            'python': sys.version, 'files': fingerprints,
            'source_snapshots': 'source-snapshots'}, indent=2) + '\n')
        scope = ['../crates/hwverify-ir', '../crates/hwverify-rs', '../crates/hwverify-sir', '../crates/hwverify-solver', '../crates/hwverify-syntax', '../crates/hwverify-verify', 'audit/automatic_proof', 'conformance/veryl-symbolic/run_automatic_ci.sh',
                 '../.github/workflows/hwverify.yml', '../Cargo.toml', '../Cargo.lock']
        (out / 'dirty.patch').write_bytes(subprocess.check_output(['git', 'diff', '--binary', 'HEAD', '--', *scope], cwd=ROOT))
        (out / 'untracked-source-paths.txt').write_bytes(subprocess.check_output(['git', 'ls-files', '--others', '--exclude-standard', '--', *scope], cwd=ROOT))
        tripwire = out / 'tripwire-bin'
        tripwire.mkdir()
        for name in ('z3', 'FORBIDDEN_EXTERNAL_SOLVER'):
            shutil.copy2(ROOT / 'conformance/veryl-proof/tools/z3-tripwire.sh', tripwire / name)
        os.environ.update(PATH=str(tripwire) + os.pathsep + old_env['PATH'],
            Z3_BIN=str(tripwire / 'z3'), Z3_TRIPWIRE_MARKER=str(marker))
        # Prove the sentinel wiring works, then distinguish the deliberate smoke marker.
        smoke = subprocess.run([str(tripwire / 'FORBIDDEN_EXTERNAL_SOLVER'), '--tripwire-smoke'],
                       check=False, env={**os.environ, 'Z3_TRIPWIRE_MARKER': str(out / 'tripwire-smoke.txt')})
        require(smoke.returncode == 99 and (out / 'tripwire-smoke.txt').is_file()
                and (out / 'tripwire-smoke.txt').stat().st_size > 0, 'tripwire smoke failed')
        verified = []
        for group in plan['groups']:
            summary = evaluation.run(checker, out / group['label'], group['width'], True,
                [r['base_name'] for r in group['cases']], group['alpha_seed'])
            verified.extend(validate_group(out / group['label'], group, summary, fingerprints[str(checker)]))
            check_seal(fingerprints)
            check_tripwire(marker)
        require(len(verified) == 29, 'incomplete matrix')
        result.update(status='passed', positive_passed=11, mutants_original_replayed=18,
                      checker_sha256=fingerprints[str(checker)], cases=verified,
                      per_query_limits={'work': 100000000, 'clauses': 1000000, 'timeout_ms': 10000},
                      external_solver_invoked=False)
        return result
    except Exception as error:
        result['error'] = {'type': type(error).__name__, 'message': str(error)}
        raise
    finally:
        for key, value in old_env.items():
            if value is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = value
        result['wall_seconds'] = time.monotonic() - started
        result['artifacts'] = {str(p.relative_to(out)): sha(p) for p in sorted(out.rglob('*')) if p.is_file()}
        (out / 'run-status.json').write_text(json.dumps(result, indent=2) + '\n')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--checker', type=Path, default=ROOT / 'target/release/hwverify-rs')
    args = parser.parse_args()
    gate(args.out, args.checker)


if __name__ == '__main__':
    main()
