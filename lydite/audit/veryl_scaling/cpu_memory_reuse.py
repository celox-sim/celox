#!/usr/bin/env python3
"""Checked specialized immutable-memory composition; reports are not certificates.

The ISA proof contains two immutable total array snapshots, no backing cells.
Concrete imported memory is independently checked against exact capture/hold/read
semantics. Only proof handles issued by successful checker calls in this process
can be consumed: saved JSON verdicts never authorize a composition.
"""
import argparse
import copy
import hashlib
import json
import os
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT))
from audit.veryl_scaling import cpu_memory_contract as c

RULE = 'immutable-total-memory-substitution-v1'
INTERFACE = {'address_bits': c.AB, 'instruction_bits': c.IW, 'data_bits': c.WIDTH,
             'latency': 0, 'writes': False, 'unmapped': 0,
             'reset': 'capture-seeds', 'nonreset': 'hold'}


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(',', ':')).encode()).hexdigest()


def expand(expr, wires, active=frozenset()):
    if isinstance(expr, str) and expr.startswith('w.'):
        key = expr[2:]
        if key in active:
            raise ValueError('wire dependency cycle')
        if key not in wires:
            raise ValueError('unconnected wire: ' + key)
        return expand(wires[key], wires, active | {key})
    if isinstance(expr, list):
        return [expand(v, wires, active) for v in expr]
    if isinstance(expr, dict):
        return {k: expand(v, wires, active) for k, v in expr.items()}
    return expr


def wiring(cpu):
    if set(cpu) != {'state', 'reset', 'next', 'wires', 'outputs'}:
        raise ValueError('unexpected CPU machine fields')
    if cpu['state'] != c.cpu_state() or set(cpu['outputs']) != set(c.PORTS):
        raise ValueError('CPU interface mismatch')
    # Validate every wire, including dead wires, before accepting a DAG.
    for key in cpu['wires']:
        expand('w.' + key, cpu['wires'])
    requests = {k: expand(cpu['outputs'][k], cpu['wires'])
                for k in ('imem_address', 'dmem_address')}
    for value in requests.values():
        if c.RUNNER.references(value, 'i.'):
            raise ValueError('request address depends on inputs; feedback unsupported')
    return {'imem_response': ['read', 's.rom', requests['imem_address']],
            'dmem_response': ['read', 's.data', requests['dmem_address']]}


def abstract_composition(cpu, fault=None):
    """Replace reference muxes with immutable snapshots; import CPU unchanged."""
    doc = c.registers.model(c.GPRS)
    old_memory = c.immutable(4)
    arrays = {'rom': {'mem': [c.AB, c.IW]}, 'data': {'mem': [c.AB, c.WIDTH]}}
    def rewrite(value):
        if isinstance(value, list):
            if (len(value) == 4 and value[0] == 'ite' and isinstance(value[2], str)
                    and value[2] in ('s.rom0', 's.data0', 'impl.rom0', 'impl.data0')):
                prefix, kind = value[2].rsplit('.', 1)
                return ['read', prefix + '.' + kind[:-1], rewrite(value[1][1])]
            if value[:1] == ['bv'] and value[1] == 2:
                return c.b(c.AB, value[2])
            if value[:3] == ['extract', 1, 0]:
                return c.address(rewrite(value[3]))
            if (len(value) == 3 and value[0] == 'eq' and isinstance(value[1], str)
                    and value[1].startswith('spec.') and value[1][5:] in old_memory):
                return True
            return [rewrite(v) for v in value]
        if isinstance(value, dict):
            if value == {'bv': 2}:
                return {'bv': c.AB}
            return {k: rewrite(v) for k, v in value.items()}
        return value
    doc = rewrite(doc)
    for part in ('state', 'reset', 'next'):
        for k in old_memory:
            doc['spec'][part].pop(k)
    doc['spec']['state'].update(arrays)
    doc['spec']['reset'].update({k: 'i.seed_' + k for k in arrays})
    doc['spec']['next'].update({k: 's.' + k for k in arrays})
    doc['inputs'] = {'rst': 'bool', 'stall': 'bool',
                     **{'seed_' + k: ty for k, ty in arrays.items()}}
    doc['binding'] = c.conj([doc['binding'], *[c.eq('spec.' + k, 'impl.' + k) for k in arrays]])
    ports = wiring(cpu)
    if fault == 'load_address_association':
        ports['dmem_response'][2] = ports['imem_response'][2]
    elif fault == 'fetch_address_high_drop':
        ports['imem_response'][2] = ['band', ports['imem_response'][2], c.b(c.AB, 3)]
    elif fault == 'load_response_corruption':
        ports['dmem_response'] = ['bxor', ports['dmem_response'], c.b(c.WIDTH, 1)]
    elif fault is not None:
        raise ValueError('unknown wiring fault')
    mapping = {'i.' + k: value for k, value in ports.items()}
    machine = c.rename(copy.deepcopy(cpu), mapping)
    machine['state'].update(arrays)
    machine['reset'].update({k: 'i.seed_' + k for k in arrays})
    machine['next'].update({k: 's.' + k for k in arrays})
    machine['outputs'] = {'commit': machine['outputs']['commit']}
    doc['impl'] = machine
    doc['name'] = 'immutable_memory_8gpr_cpu_ISA_capacity_independent'
    return doc


class ProofSession:
    """An execution ledger, not an UNSAT certificate loader.

    Reuse is allowed only in this checker execution session. All imported source,
    machine, interface and proof document fingerprints are bound before solving.
    Reports saved to disk are diagnostics and cannot be imported as proof handles.
    """
    def __init__(self, checker):
        self.checker = Path(checker).resolve()
        self.checker_hash = hashlib.sha256(self.checker.read_bytes()).hexdigest()
        self._proofs = {}
        self._imports = {}

    def import_component(self, source, kind, count, out, frontend, lifter):
        machine = c.compile_machine(source, kind, count, Path(out), Path(frontend), Path(lifter))
        paths = [*Path(out).glob('*'), Path(frontend), Path(lifter)]
        fingerprints = {str(p.resolve()): hashlib.sha256(p.read_bytes()).hexdigest()
                        for p in paths if p.is_file()}
        self._imports[id(machine)] = {
            'machine': machine, 'machine_sha256': digest(machine), 'source': source,
            'kind': kind, 'capacity': count, 'artifacts': fingerprints}
        return machine

    def _checked_import(self, machine, source, kind, count):
        imported = self._imports.get(id(machine))
        if (imported is None or imported['machine'] is not machine
                or imported['machine_sha256'] != digest(machine)
                or imported['source'] != source or imported['kind'] != kind
                or imported['capacity'] != count):
            raise ValueError('missing or mismatched checked import')
        for path, expected in imported['artifacts'].items():
            if hashlib.sha256(Path(path).read_bytes()).hexdigest() != expected:
                raise ValueError('stale source/frontend/lift artifact: ' + path)
        return imported['artifacts']

    def prove(self, kind, machine, source, out, count=None):
        provenance = self._checked_import(machine, source, kind, count)
        if hashlib.sha256(self.checker.read_bytes()).hexdigest() != self.checker_hash:
            raise ValueError('checker changed during proof session')
        if kind == 'cpu':
            if count is not None:
                raise ValueError('abstract CPU proof cannot have capacity')
            doc = abstract_composition(machine)
        elif kind == 'memory':
            if (set(machine) != {'state', 'reset', 'next', 'wires', 'outputs'}
                    or set(machine['outputs']) != {'imem_response', 'dmem_response'}
                    or machine['state'] != c.immutable(count)):
                raise ValueError('memory interface mismatch')
            for key in machine['wires']:
                expand('w.' + key, machine['wires'])
            doc = c.memory_contract(count, machine)
        else:
            raise ValueError('unknown proof kind')
        out = Path(out)
        out.mkdir(parents=True, exist_ok=True)
        port_contract = None
        if kind == 'cpu':
            port_contract = c.check_document(c.cpu_contract(machine), out / 'port-contract', self.checker)
        c.RUNNER.write_json(out / 'contract.json', doc)
        report, seconds = c.execute([self.checker, out / 'contract.json', '--out', out / 'proof'],
                                    out / 'report.json', (0, 1, 3))
        if kind == 'cpu':
            c.RUNNER.validate_report(report, None)
            if any(q.get('backend') not in ('finite_bv', 'structural_kernel')
                   for q in report['obligations']):
                raise RuntimeError('unsupported proof backend')
        else:
            c.validate_scoped(report)
            expected_examples = {(target, name, example['expect'])
                for target, spec in doc['specs'].items() for name, example in spec['examples'].items()}
            actual_examples = {(e.get('target'), e.get('example'), e.get('expect'))
                               for e in report['examples']}
            if actual_examples != expected_examples:
                raise RuntimeError('missing or unexpected memory contract example results')
        self._checked_import(machine, source, kind, count)
        if hashlib.sha256(self.checker.read_bytes()).hexdigest() != self.checker_hash:
            raise ValueError('checker changed during proof')
        if json.loads((out / 'contract.json').read_text()) != doc:
            raise ValueError('proof document changed during solving')
        metadata = {'rule': RULE, 'kind': kind, 'capacity': count, 'interface': INTERFACE,
                    'source_sha256': hashlib.sha256(source.encode()).hexdigest(),
                    'machine_sha256': digest(machine), 'document_sha256': digest(doc),
                    'checker_sha256': self.checker_hash, 'import_artifacts': provenance, 'dependencies': [],
                    'seconds': seconds, 'report_sha256': digest(report),
                    'cpu_port_contract': port_contract,
                    'cpu_port_document_sha256': digest(c.cpu_contract(machine)) if kind == 'cpu' else None}
        handle = object()
        self._proofs[handle] = copy.deepcopy(metadata)
        c.RUNNER.write_json(out / 'verified-session-record.json', metadata)
        return handle, report

    def compose(self, cpu_proof, memory_proof, cpu, memory, cpu_source, memory_source, count,
                connections=None):
        expected_connections = {'imem_address': 'cpu.imem_address', 'dmem_address': 'cpu.dmem_address',
                                'cpu.imem_response': 'memory.imem_response',
                                'cpu.dmem_response': 'memory.dmem_response'}
        if connections is not None and connections != expected_connections:
            raise ValueError('connection graph differs from proved wiring')
        wiring(cpu)
        if cpu_proof is memory_proof:
            raise ValueError('cyclic or duplicate proof dependency')
        records = []
        for handle, kind, machine, source, capacity in (
                (cpu_proof, 'cpu', cpu, cpu_source, None),
                (memory_proof, 'memory', memory, memory_source, count)):
            try:
                record = self._proofs[handle]
            except (KeyError, TypeError):
                raise ValueError('no verified proof handle in this session') from None
            provenance = self._checked_import(machine, source, kind, capacity)
            expected = {'import_artifacts': provenance, 'kind': kind, 'capacity': capacity, 'interface': INTERFACE,
                        'machine_sha256': digest(machine),
                        'source_sha256': hashlib.sha256(source.encode()).hexdigest(),
                        'checker_sha256': hashlib.sha256(self.checker.read_bytes()).hexdigest()}
            if any(record[k] != v for k, v in expected.items()) or record['dependencies']:
                raise ValueError('stale, mismatched, or cyclic proof dependency')
            if kind == 'cpu' and (record.get('cpu_port_contract') is None
                    or record.get('cpu_port_document_sha256') != digest(c.cpu_contract(machine))):
                raise ValueError('missing or stale CPU port-contract prerequisite')
            doc = abstract_composition(machine) if kind == 'cpu' else c.memory_contract(count, machine)
            if record['document_sha256'] != digest(doc):
                raise ValueError('contract changed since proof')
            records.append(copy.deepcopy(record))
        return {'status': 'checked_immutable_memory_composition', 'rule': RULE,
                'capacity': count, 'interface': INTERFACE, 'connections': expected_connections,
                'dependencies': records, 'concrete_memory_expanded_in_cpu_proof': False,
                'saved_records_are_certificates': False,
                'claim': 'Imported connected CPU refines the ISA over the imported immutable memory snapshot',
                'trusted_rule': 'Instantiate the universally quantified immutable-array CPU theorem with the total zero-padded lookup of concrete frozen cells; independently checked reset capture, preservation and both read equations establish this substitution at every step'}


def check_abstract_negative(cpu, fault, out, checker):
    """Real finite witness required; no expected-failure labels count as proof."""
    out = Path(out)
    out.mkdir(parents=True, exist_ok=True)
    doc = abstract_composition(cpu, fault if fault in c.COMPOSITION_FAULTS else None)
    c.RUNNER.write_json(out / 'contract.json', doc)
    report, seconds = c.execute([checker, out / 'contract.json', '--out', out / 'proof'],
                                out / 'report.json', (0, 1, 3))
    c.RUNNER.validate_report(report, fault)
    if any(q.get('backend') not in ('finite_bv', 'structural_kernel') for q in report['obligations']):
        raise RuntimeError('unsupported negative-control backend')
    return {'fault': fault, 'status': report['status'], 'seconds': seconds,
            'document_sha256': digest(doc), 'obligations': report['obligations']}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--sizes', type=int, nargs='+', choices=[4, 16, 64], default=[4, 16, 64])
    parser.add_argument('--negative-controls', action='store_true',
                        help='Check abstract wiring and ISA-relevant imported CPU mutations')
    parser.add_argument('--frontend', type=Path, default=ROOT / 'conformance/veryl-proof/target/debug/veryl-proof-frontend')
    parser.add_argument('--lifter', type=Path, default=ROOT / '../target/release/lydite-sir-lift')
    parser.add_argument('--checker', type=Path, default=ROOT / '../target/release/lydite')
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=False)
    os.environ['LYDITE_SOLVER'] = 'finite'
    os.environ['LYDITE_FINITE_SEARCH_HINT'] = 'query'
    tripwire = args.out.resolve() / 'z3-tripwire'
    tripwire.write_text('#!/bin/sh\nprintf invoked > "' + str(args.out.resolve() / 'z3-invoked.txt') + '"\nexit 97\n')
    tripwire.chmod(0o755)
    os.environ['Z3_BIN'] = str(tripwire)
    tested_paths = [Path(__file__), Path(c.__file__), Path(c.registers.__file__),
        ROOT / 'conformance/veryl-symbolic/run.py', ROOT / 'examples/build_pipeline.py',
        ROOT / 'conformance/veryl-symbolic/pipeline_memory_ports.veryl',
        args.frontend, args.lifter, args.checker, *ROOT.glob('crates/*/src/**/*.rs')]
    tested_files = {str(p.resolve()): hashlib.sha256(p.read_bytes()).hexdigest() for p in tested_paths}
    c.RUNNER.write_json(args.out / 'tested-files.json', tested_files)
    session = ProofSession(args.checker)
    cpu = session.import_component(c.source(), 'cpu', None, args.out / 'cpu-import', args.frontend, args.lifter)
    cpu_proof, cpu_report = session.prove('cpu', cpu, c.source(), args.out / 'cpu-abstract')
    results = []
    for count in args.sizes:
        source = c.memory_source(count)
        memory = session.import_component(source, 'memory', count, args.out / f'memory-{count}-import', args.frontend, args.lifter)
        memory_proof, _ = session.prove('memory', memory, source, args.out / f'memory-{count}-contract', count)
        result = session.compose(cpu_proof, memory_proof, cpu, memory, c.source(), source, count)
        results.append(result)
        c.RUNNER.write_json(args.out / f'composition-{count}.json', result)
        print(json.dumps({'capacity': count, 'status': result['status']}), flush=True)
    negatives = []
    if args.negative_controls:
        for fault in c.COMPOSITION_FAULTS:
            negatives.append(check_abstract_negative(cpu, fault, args.out / ('wiring-bad-' + fault), args.checker))
        for fault in c.CPU_FAULTS:
            imported = session.import_component(c.source(fault), 'cpu', None,
                args.out / ('cpu-bad-' + fault + '-import'), args.frontend, args.lifter)
            if fault == 'load_enable':
                negatives.append(c.check_document(c.cpu_contract(imported),
                    args.out / ('cpu-port-bad-' + fault), args.checker, fault))
            else:
                negatives.append(check_abstract_negative(imported, fault, args.out / ('cpu-bad-' + fault), args.checker))
    if (args.out / 'z3-invoked.txt').exists():
        raise RuntimeError('external solver tripwire fired')
    for path, expected in tested_files.items():
        if hashlib.sha256(Path(path).read_bytes()).hexdigest() != expected:
            raise RuntimeError('tested source or binary changed during gate: ' + path)
    c.RUNNER.write_json(args.out / 'summary.json', {'cpu_isa_proof_runs': 1, 'capacities': args.sizes,
        'cpu_document_sha256': digest(abstract_composition(cpu)), 'compositions': results,
        'cpu_obligations': cpu_report['obligations'], 'negative_controls': negatives,
        'cpu_scalar_state_bits': sum(1 if ty == 'bool' else ty['bv'] for ty in c.cpu_state().values()),
        'abstract_memory_count': 2, 'concrete_backing_cells_in_cpu_proof': 0,
        'external_solver_invoked': False})


if __name__ == '__main__':
    main()
