"""Fresh source-bound queue invariants; no architectural refinement claim.

All predicates are pure physical-state relations. Each proof imports the pinned
source anew and retains its transition without normalization or assumptions.
"""
import argparse
import copy
import json
import os
from pathlib import Path
from audit.veryl_scaling import rv32i_latency_common as p
from audit.veryl_scaling.rv32i_latency_variants import VARIANTS

c, s = p.c, p.s
VARIANT = VARIANTS['onehot']
CHECKER_SHA256 = '98a6f23026d9d33acfb8b39cebc935582a1e7c45c8a610e5904d239454d6bc31'


def predicates(prefix='s.'):
    """Pure D dominance and 32-bit wraparound fetch/D adjacency."""
    v = lambda key: prefix + key
    plus4 = lambda key: ['add', v(key), s.b(32, 4)]
    return {
        'd-dominates': c.implies(s.lor(v('x_valid'), v('m_valid'), v('w_valid')), v('d_valid')),
        'fetch-follows-d': c.implies(v('d_valid'), s.eq(v('fetch_pc'), plus4('d_pc'))),

    }


def invariant(prefix='s.', adjacency=False):
    if adjacency: raise ValueError('full slot adjacency has not been proved')
    return c.conj(predicates(prefix).values())


def document(raw, adjacency=False):
    # Retain all fields mentioned by the invariant; forget only unrelated
    # prestate by turning it into universal inputs, preserving each expression.
    selected = ('d_valid','x_valid','m_valid','w_valid','fetch_pc','d_pc',
                'x_pc','m_pc','w_pc')
    state = {k:raw['state'][k] for k in selected}
    rename = {'s.'+k:'i.pre_'+k for k in raw['state'] if k not in selected}
    machine = {'state':state, 'reset':{k:raw['reset'][k] for k in selected},
        'next':c.rename({k:raw['next'][k] for k in selected}, rename),
        'wires':c.rename(raw['wires'], rename)}
    inputs = {**p.INPUTS, **{'pre_'+k:v for k,v in raw['state'].items() if k not in selected}}
    return c.scoped_document('Selected physical pipeline queue invariant', state, inputs, {},
        c.conj(s.eq('s.' + k, val) for k, val in machine['reset'].items()),
        invariant(adjacency=adjacency), True, machine)


def run(out, checker):
    out, checker = Path(out), Path(checker)
    if any(k.startswith('LYDITE_') and k != 'LYDITE_SOLVER' for k in os.environ):
        raise ValueError('nondefault LYDITE environment is not permitted')
    if p.sha(checker) != CHECKER_SHA256:
        raise ValueError('checker differs from audited frozen binary')
    frontend = p.ROOT / 'conformance/veryl-proof/target/debug/veryl-proof-frontend'
    lifter = p.ROOT / '../target/release/lydite-sir-lift'
    paths = [Path(__file__), Path(p.__file__), Path(c.__file__), Path(s.__file__),
             Path(c.RUNNER.__file__), Path(c.registers.__file__),
             Path(__file__).with_name('rv32i_latency_variants.py'),
             p.ROOT/'examples/build_pipeline.py', p.ROOT/'audit/interpreter.py',
             p.source_path(VARIANT), frontend, lifter, checker]
    fingerprints = {str(x.resolve()): p.sha(x) for x in paths}
    out.mkdir(parents=True, exist_ok=False)
    snapshots = out/'source-snapshots'; snapshots.mkdir()
    for path, digest in fingerprints.items():
        if path.endswith(('.py', '.veryl')):
            data = Path(path).read_bytes()
            if p.sha(path) != digest: raise ValueError('snapshot race')
            (snapshots/(digest+'-'+Path(path).name)).write_bytes(data)
    raw, _ = p.compile_machine(out/'import', frontend, lifter, VARIANT)
    result = {'status':'incomplete', 'source_sha256':p.sha(out/'import/source.veryl'),
        'machine_sha256':p.digest(raw), 'files':fingerprints, 'proofs':{},
        'scope':'Physical queue invariants only; no CPU correctness claim',
        'budgets':{'max_work':100000000,'max_clauses':1000000,'timeout_seconds':10}}
    for name, adjacency, mutation in [('dominance-fetch',False,None),
            ('negative-invalid-d',False,'d_valid'), ('negative-stale-fetch',False,'fetch_pc'),
            ]:
        machine = copy.deepcopy(raw)
        if mutation: machine['next'][mutation] = False if mutation == 'd_valid' else 's.'+mutation
        doc = document(machine, adjacency)
        c.RUNNER.write_json(out/(name+'.json'), doc)
        report, seconds = c.execute([checker,out/(name+'.json'),'--out',out/(name+'-proof')],
                                     out/(name+'-report.json'), (0,1,3))
        try:
            (p.validate_mutation if mutation else p.validate_lemma)(report)
        except ValueError:
            result['failed_query'] = name
            c.RUNNER.write_json(out/'incomplete.json', result)
            raise
        result['proofs'][name] = {'negative_control':bool(mutation),'seconds':seconds,
            'report_sha256':p.sha(out/(name+'-report.json')),'document_sha256':p.sha(out/(name+'.json'))}
    if any(p.sha(path) != digest for path,digest in fingerprints.items()):
        raise ValueError('source/tool changed during proof')
    result['artifacts'] = {str(x.relative_to(out)):p.sha(x) for x in out.rglob('*') if x.is_file()}
    result['status'] = 'verified'
    c.RUNNER.write_json(out/'certificate.json', result)
    return result


if __name__ == '__main__':
    ap = argparse.ArgumentParser()
    ap.add_argument('--out', type=Path, required=True)
    ap.add_argument('--checker', type=Path, required=True)
    args = ap.parse_args()
    print(json.dumps(run(args.out, args.checker), indent=2))
