"""Fresh selected-RV32I acceptance gate; Unknown is an unsuccessful run.

Saved reports are diagnostic artifacts, never inputs authorizing composition.
This runner is separate from the legacy CI until architectural closure.
"""
import argparse
import hashlib
import json
import os
import subprocess
import sys
from pathlib import Path
import time

from audit.veryl_scaling import rv32i_split_pipeline as selected
from audit.veryl_scaling import rv32i_split_reuse as refinement
from audit.veryl_scaling import rv32i_memory as memory
from audit.veryl_scaling import rv32i_latency_common as latency
from audit.veryl_scaling.rv32i_latency_variants import VARIANTS


def write(path, value):
    path.write_text(json.dumps(value, indent=2) + '\n')


def capture_provenance(out, checker):
    root = selected.ROOT
    scope = ['Cargo.toml', 'Cargo.lock', 'crates', 'audit/veryl_scaling', 'audit/lemma_candidates',
             'conformance/veryl-symbolic', '.github/workflows',
             'examples/build_pipeline.py', 'examples/build_branch_pipeline.py',
             'audit/interpreter.py']
    files = set()
    for name in scope:
        path = root / name
        if path.is_file():
            files.add(path)
        elif path.is_dir():
            files.update(p for p in path.rglob('*') if p.is_file()
                         and '__pycache__' not in p.parts and p.suffix != '.pyc')
    snapshots = out / 'source-snapshots'
    snapshots.mkdir()
    fingerprints = {}
    for path in sorted(files):
        data = path.read_bytes()
        digest = hashlib.sha256(data).hexdigest()
        fingerprints[str(path.resolve())] = digest
        (snapshots / (digest + '-' + path.name)).write_bytes(data)
    tools = [checker, root / 'target/release/hwverify-sir-lift',
             root / 'conformance/veryl-proof/target/debug/veryl-proof-frontend']
    for path in tools:
        fingerprints[str(path.resolve())] = hashlib.sha256(path.read_bytes()).hexdigest()
    def git(*args):
        return subprocess.check_output(['git', *args], cwd=root)
    (out / 'dirty.patch').write_bytes(git('diff', '--binary', 'HEAD', '--', *scope))
    (out / 'untracked-source-paths.txt').write_bytes(git('ls-files', '--others', '--exclude-standard', '--', *scope))
    write(out / 'provenance.json', {'source_root': str(root.resolve()), 'revision': git('rev-parse', 'HEAD').decode().strip(),
          'files': fingerprints, 'source_snapshots': 'source-snapshots',
          'dirty_patch': 'dirty.patch', 'untracked_source_paths': 'untracked-source-paths.txt'})
    return fingerprints


def check_provenance(fingerprints):
    if any(hashlib.sha256(Path(path).read_bytes()).hexdigest() != digest
           for path, digest in fingerprints.items()):
        raise ValueError('gate source, dependency, or tool changed during run')


def execute_program_checks(out):
    commands = [
        [sys.executable, '-m', 'audit.veryl_scaling.test_rv32i_split_refinement',
         '--machine', str(out / 'refinement/import/machine.json')],
        [sys.executable, '-m', 'audit.veryl_scaling.test_rv32i_pipeline',
         '--machine', str(out / 'latency-baseline/import/machine.json'),
         '--memory-machine', str(out / 'refinement/memory-64-import/machine.json')],
    ]
    for index, command in enumerate(commands):
        with (out / ('execution-check-' + str(index) + '.log')).open('w') as log:
            subprocess.run(command, cwd=selected.ROOT, stdout=log,
                           stderr=subprocess.STDOUT, check=True)


def run(out, checker=None, normalize_bank=False):
    if normalize_bank:
        raise ValueError('selected acceptance uses the fixed no-bank proof configuration')
    out = Path(out).resolve()
    if out == selected.ROOT.resolve() or selected.ROOT.resolve() in out.parents:
        raise ValueError('evidence directory must be outside the source repository')
    if any(k.startswith('HWVERIFY_') and k != 'HWVERIFY_SOLVER' for k in os.environ):
        raise ValueError('nondefault HWVERIFY environment is forbidden')
    if os.environ.get('HWVERIFY_SOLVER', 'finite') != 'finite':
        raise ValueError('selected RV32I gate is finite-only')
    out.mkdir(parents=True, exist_ok=False)
    checker = Path(checker or selected.ROOT / 'target/release/hwverify-rs').resolve()
    previous = os.environ.get('HWVERIFY_SOLVER')
    os.environ['HWVERIFY_SOLVER'] = 'finite'
    started = time.monotonic()
    summary = {'status': 'failed', 'architectural_refinement': 'not_verified',
               'composition': 'not_issued', 'selected_source_sha256': selected.SELECTED_SHA256,
               'normalize_bank': bool(normalize_bank), 'completed': [],
               'saved_reports_authorize_composition': False}
    try:
        fingerprints = capture_provenance(out, checker)
        session = refinement.ProofSession(out / 'refinement', checker,
                                         conjunctive_lemmas=True)
        memories = {n: session.prove_memory(n) for n in (4, 16, 64)}
        summary['completed'].append('memory_4_16_64')
        for variant in ('baseline', 'onehot'):
            latency.run(out / ('latency-' + variant), VARIANTS[variant], checker)
            summary['completed'].append('latency_' + variant)
        execute_program_checks(out)
        summary['completed'].append('independent_imported_execution')
        selected.check_mutations(out / 'selected-mutations', checker)
        summary['completed'].append('selected_mutations')
        memory.check_mutations(out / 'memory-mutations', checker)
        summary['completed'].append('memory_mutations')
        session._check()
        options = dict(normalize_register_file=True, normalize_stages=True,
                       stage_scope=('x',), transport_view=True,
                       control_invariant=True, normalize_dispatch=True,
                       stage_fields=('m_result', 'm_address', 'm_taken', 'm_target'),
                       checked_programs=True)
        if normalize_bank:
            options['normalize_bank'] = True
        cpu = session.prove_cpu(**options)
        # Only live handles returned by successful proofs reach composition.
        compositions = [session.compose(cpu, memories[n], n) for n in (4, 16, 64)]
        session._check()
        check_provenance(fingerprints)
        write(out / 'composition.json', compositions)
        summary.update(status='passed', architectural_refinement='verified',
                       composition='issued')
        summary['completed'].append('selected_architecture_and_compositions')
        return summary
    except Exception as exc:
        summary['error'] = {'type': type(exc).__name__, 'message': str(exc)}
        raise
    finally:
        summary['wall_seconds'] = time.monotonic() - started
        summary['artifacts'] = {
            str(path.relative_to(out)): hashlib.sha256(path.read_bytes()).hexdigest()
            for path in sorted(out.rglob('*')) if path.is_file()
        }
        write(out / 'run-status.json', summary)
        if previous is None:
            os.environ.pop('HWVERIFY_SOLVER', None)
        else:
            os.environ['HWVERIFY_SOLVER'] = previous


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--checker', type=Path)
    args = parser.parse_args()
    print(json.dumps(run(args.out, args.checker), indent=2))


if __name__ == '__main__':
    main()
