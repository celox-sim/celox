#!/usr/bin/env python3
"""Checked writable-memory CPU refinement; separate from immutable legacy gates."""
import copy
from pathlib import Path
import sys
ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT))
from audit.veryl_scaling import cpu_memory_contract as c
from audit.veryl_scaling import cpu_memory_reuse as old
from audit.veryl_scaling.cpu_memory_contract import b, eq, it, add, ex, land, lor, neg, implies

PORTS = {**c.PORTS, 'write_enable': 'bool', 'write_address': {'bv': 6}, 'write_data': {'bv': 32}}
RULE = 'writable-total-memory-step-simulation-v1'
INTERFACE = {**old.INTERFACE, 'writes': True, 'nonreset': 'mapped-W-retirement-write',
             'read_during_write': 'new-data', 'unmapped_write': 'ignore',
             'configuration': 'immutable-bv7-backing-limit-instantiated-to-capacity'}
CPU_FAULTS = {**c.CPU_FAULTS,
    'store_data': ('write_data = w_result;', "write_data = w_result ^ 32'd1;"),
    'store_address': ('write_address = w_ir[5:0];', 'write_address = x_ir[5:0];'),
    'store_missing': ("w_ir[40:38] == 3'd5;", "w_ir[40:38] == 3'd6;"),
    'store_early': ("w_valid && w_ir[40:38] == 3'd5;", "x_valid && x_ir[40:38] == 3'd5;"),
    'store_stall': ("write_enable = rst_n && !stall &&", "write_enable = rst_n &&"),
    'store_operand': ("if x_op == 3'd5 { result = x_operand; }", "if x_op == 3'd5 { result = x_ir[31:0]; }"),
    'store_interlock': (" || d_op == 3'd5)", ")"),
}
COMPOSITION_FAULTS = (*c.COMPOSITION_FAULTS, 'store_address_wiring', 'store_data_wiring', 'store_enable_wiring', 'missing_bypass')

def uses(ir): return lor(c.uses(ir), c.isop(ir, 5))
def source(fault=None):
    text = (ROOT / 'conformance/veryl-symbolic/pipeline_memory_ports_store.veryl').read_text()
    if fault:
        before, after = CPU_FAULTS[fault]
        if text.count(before) != 1: raise ValueError('mutation site not unique: ' + fault)
        text = text.replace(before, after)
    return text

def mapped(addr, limit): return ['ult', ['concat', b(1, 0), addr], limit]
def read_data(array, addr, limit): return it(mapped(addr, limit), ['read', array, addr], b(32, 0))
def update_data(array, addr, value, enable, limit):
    return it(land(enable, mapped(addr, limit)), ['write', array, addr, value], array)

def wiring(cpu):
    if set(cpu) != {'state', 'reset', 'next', 'wires', 'outputs'} or cpu['state'] != c.cpu_state() or set(cpu['outputs']) != set(PORTS):
        raise ValueError('CPU interface mismatch')
    for name in cpu['wires']: old.expand('w.' + name, cpu['wires'])
    requests = {k: old.expand(cpu['outputs'][k], cpu['wires']) for k in ('imem_address', 'dmem_address', 'write_enable', 'write_address', 'write_data')}
    for name, value in requests.items():
        if c.RUNNER.references(value, 'i.') and name != 'write_enable':
            raise ValueError('request depends on response/input')
        if name == 'write_enable':
            refs = str(value)
            if 'i.imem_response' in refs or 'i.dmem_response' in refs:
                raise ValueError('write enable response feedback')
    return requests

def abstract_composition(cpu, fault=None):
    requests = wiring(cpu)
    if fault == 'load_address_association': requests['dmem_address'] = requests['imem_address']
    elif fault == 'fetch_address_high_drop': requests['imem_address'] = ['band', requests['imem_address'], b(6, 3)]
    elif fault == 'store_address_wiring': requests['write_address'] = requests['dmem_address']
    elif fault == 'store_data_wiring': requests['write_data'] = ['bxor', requests['write_data'], b(32, 1)]
    elif fault == 'store_enable_wiring': requests['write_enable'] = False
    elif fault not in (None, 'load_response_corruption', 'missing_bypass'): raise ValueError('unknown fault')
    arrays = {'rom': {'mem': [6, 41]}, 'data': {'mem': [6, 32]}, 'limit': {'bv': 7}}
    inputs = {'rst': 'bool', 'stall': 'bool', **{'seed_' + k: ty for k, ty in arrays.items()}}
    updated = update_data('s.data', requests['write_address'], requests['write_data'], requests['write_enable'], 's.limit')
    load = read_data('s.data' if fault == 'missing_bypass' else updated, requests['dmem_address'], 's.limit')
    if fault == 'load_response_corruption': load = ['bxor', load, b(32, 1)]
    machine = c.rename(copy.deepcopy(cpu), {'i.imem_response': ['read', 's.rom', requests['imem_address']], 'i.dmem_response': load})
    machine['state'].update(arrays)
    machine['reset'].update({k: 'i.seed_' + k for k in arrays})
    machine['next'].update(rom='s.rom', data=updated, limit='s.limit')
    machine['outputs'] = {'commit': machine['outputs']['commit']}
    arch = {'pc': {'bv': 6}, **{f'r{i}': {'bv': 32} for i in range(8)}, **arrays}
    regs = [f's.r{i}' for i in range(8)]
    ir = 'w.ir'
    spec = {'state': arch, 'reset': {'pc': b(6, 0), **{f'r{i}': b(32, 0) for i in range(8)}, **{k: 'i.seed_' + k for k in arrays}},
      'wires': {'ir': ['read', 's.rom', 's.pc'], 'result': c.isa_value(ir, regs, read_data('s.data', c.address(ir), 's.limit'))},
      'next': {'pc': c.isa_pc(ir, 's.pc', regs), **{f'r{i}': it(land(c.writes(ir), eq(c.rd(ir), b(3, i))), 'w.result', f's.r{i}') for i in range(8)},
               'rom': 's.rom', 'limit': 's.limit', 'data': update_data('s.data', c.address(ir), c.regread(c.rs(ir), regs), c.isop(ir, 5), 's.limit')},
      'outputs': {'can_step': True}}
    a = [f'impl.r{i}' for i in range(8)]
    pending = [it(land('impl.w_valid', c.writes('impl.w_ir'), eq(c.rd('impl.w_ir'), b(3,i))), 'impl.w_result', a[i]) for i in range(8)]
    px = it('impl.w_valid', 'impl.w_next_pc', 'impl.pc')
    pd = add(px, it('impl.x_valid', b(6,1), b(6,0)))
    pf = add(pd, it('impl.d_valid', b(6,1), b(6,0)))
    rel = [eq('spec.'+k, 'impl.'+k) for k in arch]
    rel += [implies('impl.w_valid', c.conj([
      eq('impl.w_pc','impl.pc'), eq('impl.w_ir',['read','impl.rom','impl.w_pc']),
      eq('impl.w_next_pc', c.isa_pc('impl.w_ir','impl.w_pc',a)),
      implies(c.writes('impl.w_ir'), eq('impl.w_result',c.isa_value('impl.w_ir',a,read_data('impl.data',c.address('impl.w_ir'),'impl.limit')))),
      implies(c.isop('impl.w_ir',5), eq('impl.w_result',c.regread(c.rs('impl.w_ir'),a)))])),
      implies('impl.x_valid',c.conj([eq('impl.x_pc',px),eq('impl.x_ir',['read','impl.rom','impl.x_pc']),implies(uses('impl.x_ir'),eq('impl.x_operand',c.regread(c.rs('impl.x_ir'),pending)))])),
      implies('impl.d_valid',land(eq('impl.d_pc',pd),eq('impl.d_ir',['read','impl.rom','impl.d_pc']))),eq('impl.fetch_pc',pf)]
    return {'version':2,'name':'writable_memory_8gpr_cpu_ISA_capacity_independent','inputs':inputs,'reset_input':'rst','spec':spec,'impl':machine,'binding':c.conj(rel),'commit':'commit','can_step':'can_step','hold_when':'i.stall',
      'progress':{'enabled':neg('i.stall'),'rank':it('impl.w_valid',b(2,0),it('impl.x_valid',b(2,1),it('impl.d_valid',b(2,2),b(2,3))))}}

from audit.veryl_scaling.cpu_memory_contract import (with_observers, WIDTH, GPRS, RB, AB, cpu_state, writes, rd, rs, regread, isa_pc, isa_value, isop, address, conj, zero, scoped_document, CPU_INPUTS)

def cpu_contract(machine):
    observed = {'obs_' + k: ({'bv': 1}, it(machine['outputs'][k], b(1, 1), b(1, 0)))
                if v == 'bool' else (v, machine['outputs'][k]) for k, v in PORTS.items()}
    machine = with_observers(machine, observed, {'obs_load': ({'bv': WIDTH},
        it('i.stall', 's.obs_load', 'i.dmem_response'))})
    state = machine['state']
    regs = [f's.r{i}' for i in range(GPRS)]
    pending = [it(land('s.w_valid', writes('s.w_ir'), eq(rd('s.w_ir'), b(RB, i))),
                  's.w_result', regs[i]) for i in range(GPRS)]
    pc_x = it('s.w_valid', 's.w_next_pc', 's.pc')
    pc_d = add(pc_x, it('s.x_valid', b(AB, 1), b(AB, 0)))
    pc_f = add(pc_d, it('s.d_valid', b(AB, 1), b(AB, 0)))
    invariant = conj([
        implies('s.w_valid', conj([eq('s.w_pc', 's.pc'),
            eq('s.w_next_pc', isa_pc('s.w_ir', 's.w_pc', regs)),
            implies(writes('s.w_ir'), eq('s.w_result', isa_value('s.w_ir', regs, 's.obs_load'))),
            implies(isop('s.w_ir', 5), eq('s.w_result', regread(rs('s.w_ir'), regs)))])),
        implies('s.x_valid', conj([eq('s.x_pc', pc_x),
            implies(uses('s.x_ir'), eq('s.x_operand', regread(rs('s.x_ir'), pending)))])),
        implies('s.d_valid', eq('s.d_pc', pc_d)), eq('s.fetch_pc', pc_f),
        *[eq('o.' + k, 's.' + k) for k in ['pc', *[f'r{i}' for i in range(GPRS)]]]])
    hazard = land('s.d_valid', uses('s.d_ir'), 's.x_valid', writes('s.x_ir'),
                  eq(rd('s.x_ir'), rs('s.d_ir')), isop('s.x_ir', 3))
    taken = land('s.x_valid', isop('s.x_ir', 4),
                 eq('s.x_operand', b(WIDTH, 0)))
    retire = land(neg('i.stall'), 's.w_valid')
    next_pc = it(retire, isa_pc('s.w_ir', 's.pc', regs), 's.pc')
    expectations = [eq('n.pc', next_pc)]
    for i in range(GPRS):
        expectations.append(eq(f'n.r{i}', it(land(retire, writes('s.w_ir'), eq(rd('s.w_ir'), b(RB, i))),
            isa_value('s.w_ir', regs, 's.obs_load'), f's.r{i}')))
    # Instruction provenance / control, not a duplicated operand/result pipeline.
    transfer = {'w_valid': 's.x_valid', 'w_pc': 's.x_pc', 'w_ir': 's.x_ir',
        'x_valid': land('s.d_valid', neg(hazard), neg(taken)),
        'x_pc': 's.d_pc', 'x_ir': 's.d_ir',
        'd_valid': it(taken, False, it(hazard, 's.d_valid', True)),
        'd_pc': it(hazard, 's.d_pc', 's.fetch_pc'),
        'd_ir': it(hazard, 's.d_ir', 'i.imem_response'),
        'fetch_pc': it(taken, address('s.x_ir'), it(hazard, 's.fetch_pc', add('s.fetch_pc', b(AB, 1)))),
        'obs_load': 'i.dmem_response'}
    for field, value in transfer.items():
        expectations.append(eq('n.' + field, it('i.stall', 's.' + field, value)))
    # Actual DUT architectural and payload registers must freeze on stall. The
    # sample monitors intentionally do not, and are not hardware state.
    expectations += [implies('i.stall', eq('n.' + k, 's.' + k)) for k in cpu_state()]
    requests = {'imem_address': 's.fetch_pc', 'dmem_address': address('s.x_ir'),
                'imem_valid': land(neg('i.stall'), neg(hazard), neg(taken)),
                'dmem_valid': land(neg('i.stall'), 's.x_valid', isop('s.x_ir', 3)), 'commit': retire,
                'write_enable': land(retire, isop('s.w_ir', 5)),
                'write_address': address('s.w_ir'), 'write_data': 's.w_result'}
    expectations += [eq('n.obs_' + k, it(v, b(1, 1), b(1, 0)) if PORTS[k] == 'bool' else v)
                     for k, v in requests.items()]
    initial = conj(eq('s.' + k, zero(v)) for k, v in state.items())
    outputs = {k: state[k] for k in ['pc', *[f'r{i}' for i in range(GPRS)]]}
    return scoped_document('CPU ISA retirement and port provenance (arbitrary responses)',
        state, CPU_INPUTS, outputs, initial, invariant, conj(expectations), machine, c.cpu_examples())


import argparse
import hashlib
import json
import os
from audit.veryl_scaling import cpu_writable_memory as wm
digest = old.digest
expand = old.expand

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
        machine = wm.compile_machine(source, kind, count, Path(out), Path(frontend), Path(lifter))
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
                    or machine['state'] != wm.immutable(count)):
                raise ValueError('memory interface mismatch')
            for key in machine['wires']:
                expand('w.' + key, machine['wires'])
            doc = wm.memory_contract(count, machine)
        else:
            raise ValueError('unknown proof kind')
        out = Path(out)
        out.mkdir(parents=True, exist_ok=True)
        port_contract = None
        if kind == 'cpu':
            port_contract = c.check_document(cpu_contract(machine), out / 'port-contract', self.checker)
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
                    'cpu_port_document_sha256': digest(cpu_contract(machine)) if kind == 'cpu' else None}
        handle = object()
        self._proofs[handle] = copy.deepcopy(metadata)
        c.RUNNER.write_json(out / 'verified-session-record.json', metadata)
        return handle, report

    def compose(self, cpu_proof, memory_proof, cpu, memory, cpu_source, memory_source, count,
                connections=None):
        expected_connections = {'cpu.rst': 'system.rst', 'memory.rst': 'system.rst',
                                'cpu.stall': 'system.stall', 'imem_address': 'cpu.imem_address', 'dmem_address': 'cpu.dmem_address',
                                'cpu.imem_response': 'memory.imem_response',
                                'cpu.dmem_response': 'memory.dmem_response',
                                'write_enable': 'cpu.write_enable', 'write_address': 'cpu.write_address',
                                'write_data': 'cpu.write_data'}
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
                    or record.get('cpu_port_document_sha256') != digest(cpu_contract(machine))):
                raise ValueError('missing or stale CPU port-contract prerequisite')
            doc = abstract_composition(machine) if kind == 'cpu' else wm.memory_contract(count, machine)
            if record['document_sha256'] != digest(doc):
                raise ValueError('contract changed since proof')
            records.append(copy.deepcopy(record))
        return {'status': 'checked_writable_memory_composition', 'rule': RULE,
                'capacity': count, 'abstract_backing_limit': count, 'interface': INTERFACE, 'connections': expected_connections,
                'dependencies': records, 'concrete_memory_expanded_in_cpu_proof': False,
                'saved_records_are_certificates': False,
                'claim': 'Imported connected CPU refines the STORE ISA over the imported writable memory',
                'trusted_rule': 'Instantiate immutable abstract limit with concrete capacity and ROM/data with zero-padded concrete cells. Independently checked reset, mapped write update, unchanged-cell frame, immutable ROM and write-through read equations establish a step-preserving simulation; unmapped writes are ignored and reads are zero'}


def validate_abstract_negative(report, fault):
    if fault != 'store_stall':
        return c.RUNNER.validate_report(report, fault)
    expected_failures = {'microstep_refinement', 'hold_contract'}
    obligations = report.get('obligations', [])
    if len(obligations) != len(c.RUNNER.EXPECTED_RESULTS) or {o.get('name') for o in obligations} != set(c.RUNNER.EXPECTED_RESULTS):
        raise RuntimeError('missing or duplicate obligations')
    if report.get('status') != 'counterexample' or report.get('engine_summary', {}).get('z3_queries') != 0:
        raise RuntimeError('unexpected result or external solver')
    for q in obligations:
        bad = q['name'] in expected_failures
        expected = 'sat' if bad else c.RUNNER.EXPECTED_RESULTS[q['name']]
        if q.get('solver_result') != expected or q.get('status') != ('counterexample' if bad else 'passed'):
            raise RuntimeError('unexpected stall-store obligation result: ' + q['name'])
        if expected == 'sat' and q.get('finite', {}).get('original_formula_validated') is not True:
            raise RuntimeError('SAT witness lacks original formula validation')


def check_abstract_negative(cpu, fault, out, checker):
    """Real finite witness required; no expected-failure labels count as proof."""
    out = Path(out)
    out.mkdir(parents=True, exist_ok=True)
    doc = abstract_composition(cpu, fault if fault in COMPOSITION_FAULTS else None)
    c.RUNNER.write_json(out / 'contract.json', doc)
    report, seconds = c.execute([checker, out / 'contract.json', '--out', out / 'proof'],
                                out / 'report.json', (0, 1, 3))
    validate_abstract_negative(report, fault)
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
    tested_paths = [Path(__file__), Path(c.__file__), Path(wm.__file__), Path(old.__file__), Path(c.registers.__file__),
        ROOT / 'conformance/veryl-symbolic/run.py', ROOT / 'examples/build_pipeline.py',
        ROOT / 'conformance/veryl-symbolic/pipeline_memory_ports_store.veryl',
        args.frontend, args.lifter, args.checker, *ROOT.glob('crates/*/src/**/*.rs')]
    tested_files = {str(p.resolve()): hashlib.sha256(p.read_bytes()).hexdigest() for p in tested_paths}
    c.RUNNER.write_json(args.out / 'tested-files.json', tested_files)
    session = ProofSession(args.checker)
    cpu = session.import_component(source(), 'cpu', None, args.out / 'cpu-import', args.frontend, args.lifter)
    cpu_proof, cpu_report = session.prove('cpu', cpu, source(), args.out / 'cpu-abstract')
    results = []
    for count in args.sizes:
        mem_source = wm.memory_source(count)
        memory = session.import_component(mem_source, 'memory', count, args.out / f'memory-{count}-import', args.frontend, args.lifter)
        memory_proof, _ = session.prove('memory', memory, mem_source, args.out / f'memory-{count}-contract', count)
        result = session.compose(cpu_proof, memory_proof, cpu, memory, source(), mem_source, count)
        results.append(result)
        c.RUNNER.write_json(args.out / f'composition-{count}.json', result)
        print(json.dumps({'capacity': count, 'status': result['status']}), flush=True)
    negatives = []
    if args.negative_controls:
        for fault in COMPOSITION_FAULTS:
            negatives.append(check_abstract_negative(cpu, fault, args.out / ('wiring-bad-' + fault), args.checker))
        for fault in CPU_FAULTS:
            imported = session.import_component(source(fault), 'cpu', None,
                args.out / ('cpu-bad-' + fault + '-import'), args.frontend, args.lifter)
            if fault == 'load_enable':
                negatives.append(c.check_document(cpu_contract(imported),
                    args.out / ('cpu-port-bad-' + fault), args.checker, fault))
            else:
                negatives.append(check_abstract_negative(imported, fault, args.out / ('cpu-bad-' + fault), args.checker))
        for count in args.sizes:
            for fault in wm.MEMORY_FAULTS:
                imported = session.import_component(wm.memory_source(count, fault), 'memory', count,
                    args.out / f'memory-{count}-bad-{fault}-import', args.frontend, args.lifter)
                reset_fault = 'wrong_reset' if fault in ('wrong_reset', 'reset_write_priority') else fault
                result = c.check_document(wm.memory_contract(count, imported),
                    args.out / f'memory-{count}-bad-{fault}', args.checker, reset_fault)
                negatives.append({'capacity': count, 'fault': fault, 'memory_contract': result})
    if (args.out / 'z3-invoked.txt').exists():
        raise RuntimeError('external solver tripwire fired')
    for path, expected in tested_files.items():
        if hashlib.sha256(Path(path).read_bytes()).hexdigest() != expected:
            raise RuntimeError('tested source or binary changed during gate: ' + path)
    c.RUNNER.write_json(args.out / 'summary.json', {'cpu_isa_proof_runs': 1, 'capacities': args.sizes,
        'cpu_document_sha256': digest(abstract_composition(cpu)), 'compositions': results,
        'cpu_obligations': cpu_report['obligations'], 'negative_controls': negatives,
        'cpu_scalar_state_bits': sum(1 if ty == 'bool' else ty['bv'] for ty in c.cpu_state().values()),
        'abstract_memory_count': 2, 'immutable_backing_limit_bits': 7, 'concrete_backing_cells_in_cpu_proof': 0,
        'external_solver_invoked': False})


if __name__ == '__main__':
    main()
