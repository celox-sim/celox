"""Capacity-independent RV32I refinement over disjoint checked byte memory.

The global proof is separate from instruction execution lemmas and concrete RAM
contracts. Building this document is not evidence that its obligations passed.
"""
import copy
from pathlib import Path
import sys
ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT))
from audit.veryl_scaling import rv32i_pipeline as p
from audit.veryl_scaling import rv32i_spec as a
from audit.veryl_scaling import cpu_memory_contract as c
from audit.veryl_scaling.cpu_memory_reuse import expand

b, eq, it, land, lor, neg = a.b, a.eq, a.it, a.land, a.lor, a.neg
RAM_BASE = 0x1000
ARRAYS = {'rom': {'mem': [7, 32]}, 'data': {'mem': [7, 32]}, 'limit': {'bv': 7}}


def aligned(address):
    # Map checks use all32 address bits before this seven-bit backing selector.
    # limit is BV7, so mapped offsets are at most127 complete words. RAM_BASE is
    # 512-byte aligned; its low address bits are the same as offset address bits.
    return a.ex(8, 2, address)
def span(limit): return ['shl', ['zext', 25, limit], b(32, 2)]
def rom_mapped(address, limit): return ['ult', address, span(limit)]
def ram_mapped(address, limit):
    return land(neg(['ult', address, b(32, RAM_BASE)]),
                ['ult', address, ['add', b(32, RAM_BASE), span(limit)]])
def instruction(rom, address, limit):
    return it(rom_mapped(address, limit), ['read', rom, aligned(address)], b(32, 0))
def data_word(rom, data, address, limit):
    return it(rom_mapped(address, limit), ['read', rom, aligned(address)],
              it(ram_mapped(address, limit), ['read', data, aligned(address)], b(32, 0)))
def data_fault(address, write, limit):
    return neg(it(write, ram_mapped(address, limit), lor(rom_mapped(address, limit), ram_mapped(address, limit))))
def byte_mask(mask):
    return a.cat(*[it(eq(a.ex(i, i, mask), b(1, 1)), b(8, 255), b(8, 0)) for i in reversed(range(4))])
def update(data, address, value, mask, enable, limit):
    address_key = aligned(address)
    bits = byte_mask(mask)
    merged = ['bor', ['band', ['read', data, address_key], ['bxor', bits, b(32, 0xffffffff)]],
              ['band', value, bits]]
    return it(land(enable, ram_mapped(address, limit)), ['write', data, address_key, merged], data)


def action(ir, pc, regs, rom, data, limit, checked_address=None):
    d = a.decode(ir)
    rs1, rs2 = a.reg_read(d['rs1'], regs), a.reg_read(d['rs2'], regs)
    preliminary = a.build_execute(ir, pc, rs1, rs2)
    address = preliminary['dmem_address']
    return a.build_execute(ir, pc, rs1, rs2, neg(rom_mapped(pc, limit)),
        data_word(rom, data, address if checked_address is None else checked_address, limit),
        data_fault(address, preliminary['dmem_write'], limit))


def abstract_composition(cpu):
    if cpu['state'] != p.cpu_state() or set(cpu['outputs']) != set(p.PORTS):
        raise ValueError('RV32I CPU interface mismatch')
    requests = {k: expand(cpu['outputs'][k], cpu['wires']) for k in
                ('imem_address', 'dmem_address', 'dmem_write', 'write_enable',
                 'write_address', 'write_data', 'write_mask')}
    for name, value in requests.items():
        if any(c.RUNNER.references(value, 'i.' + key) for key in
               ('imem_response', 'dmem_response', 'imem_fault', 'dmem_fault')):
            raise ValueError('memory request/response combinational cycle: ' + name)
    updated = update('s.data', requests['write_address'], requests['write_data'], requests['write_mask'],
                     requests['write_enable'], 's.limit')
    replacements = {'i.imem_response': instruction('s.rom', requests['imem_address'], 's.limit'),
        'i.imem_fault': neg(rom_mapped(requests['imem_address'], 's.limit')),
        'i.dmem_response': data_word('s.rom', updated, requests['dmem_address'], 's.limit'),
        'i.dmem_fault': data_fault(requests['dmem_address'], requests['dmem_write'], 's.limit')}
    machine = c.rename(copy.deepcopy(cpu), replacements)
    machine['state'].update(ARRAYS)
    machine['reset'].update({k: 'i.seed_' + k for k in ARRAYS})
    machine['next'].update(rom='s.rom', data=updated, limit='s.limit')
    machine['outputs'] = {'retire': machine['outputs']['retire']}
    arch = {'pc': {'bv': 32}, 'halted': 'bool', **{f'r{i}': {'bv': 32} for i in range(32)}, **ARRAYS}
    regs = [f's.r{i}' for i in range(32)]
    ir = instruction('s.rom', 's.pc', 's.limit')
    execute = action(ir, 's.pc', regs, 's.rom', 's.data', 's.limit')
    spec = {'state': arch,
        'reset': {'pc': b(32, 0), 'halted': False, **{f'r{i}': b(32, 0) for i in range(32)},
                  **{k: 'i.seed_' + k for k in ARRAYS}},
        'next': {'pc': execute['next_pc'], 'halted': execute['trap'],
                 **{f'r{i}': it(land(execute['rd_write'], eq(execute['rd'], b(5, i))), execute['rd_value'], regs[i]) for i in range(32)},
                 'rom': 's.rom', 'limit': 's.limit',
                 'data': update('s.data', execute['dmem_address'], execute['store_data'], execute['store_mask'],
                                land(execute['dmem_write'], neg(execute['trap'])), 's.limit')},
        'outputs': {'can_step': neg('s.halted')}}
    regs = [f'impl.r{i}' for i in range(32)]
    wd, xd = a.decode('impl.w_ir'), a.decode('impl.x_ir')
    w = action('impl.w_ir', 'impl.w_pc', regs, 'impl.rom', 'impl.data', 'impl.limit', 'impl.w_address')
    pending = [b(32, 0)] + [it(land('impl.w_valid', neg('impl.w_fault'), 'impl.w_writes', eq(wd['rd'], b(5, i))),
                               'impl.w_result', regs[i]) for i in range(1, 32)]
    xpc = it('impl.w_valid', 'impl.w_next_pc', 'impl.pc')
    dpc = ['add', xpc, it('impl.x_valid', b(32, 4), b(32, 0))]
    fpc = ['add', dpc, it('impl.d_valid', b(32, 4), b(32, 0))]
    fault_w = land('impl.w_valid', 'impl.w_fault')
    w_relation = c.conj([eq('impl.w_pc', 'impl.pc'),
        eq('impl.w_ir', instruction('impl.rom', 'impl.w_pc', 'impl.limit')),
        eq('impl.w_fault', w['trap']), eq('impl.w_cause', w['cause']), eq('impl.w_tval', w['trap_value']),
        eq('impl.w_next_pc', w['next_pc']), eq('impl.w_writes', w['rd_write']),
        c.implies(w['rd_write'], eq('impl.w_result', w['rd_value'])),
        eq('impl.w_store', land(w['dmem_write'], neg(w['trap']))),
        # This independent address equality permits the read's index to use the
        # captured address above; the architectural reference remains unchanged.
        c.implies(lor(wd['load'], wd['store']), eq('impl.w_address', w['dmem_address'])),
        c.implies('impl.w_store', c.conj([eq('impl.w_address', w['dmem_address']),
            eq('impl.w_data', w['store_data']), eq('impl.w_mask', w['store_mask'])]))])
    relation = [eq('spec.' + key, 'impl.' + key) for key in arch]
    relation.extend([eq('impl.r0', b(32, 0)),
        *[eq(a.ex(1, 0, 'impl.' + key), b(2, 0)) for key in ('pc', 'fetch_pc', 'd_pc', 'x_pc', 'w_pc', 'w_next_pc')],
        c.implies('impl.halted', c.conj([neg('impl.d_valid'), neg('impl.x_valid'), neg('impl.w_valid')])),
        c.implies(fault_w, c.conj([neg('impl.d_valid'), neg('impl.x_valid'), neg('impl.w_writes'), neg('impl.w_store')])),
        c.implies(land('impl.w_valid', 'impl.w_writes'), neg(eq(wd['rd'], b(5, 0)))),
        c.implies('impl.w_valid', w_relation),
        c.implies('impl.x_valid', c.conj([eq('impl.x_pc', xpc),
            eq('impl.x_ir', instruction('impl.rom', 'impl.x_pc', 'impl.limit')),
            eq('impl.x_fetch_fault', neg(rom_mapped('impl.x_pc', 'impl.limit'))),
            c.implies(xd['uses1'], eq('impl.x_operand1', a.reg_read(xd['rs1'], pending))),
            c.implies(xd['uses2'], eq('impl.x_operand2', a.reg_read(xd['rs2'], pending)))])),
        c.implies('impl.d_valid', c.conj([eq('impl.d_pc', dpc),
            eq('impl.d_ir', instruction('impl.rom', 'impl.d_pc', 'impl.limit')),
            eq('impl.d_fetch_fault', neg(rom_mapped('impl.d_pc', 'impl.limit')))])),
        c.implies(land(neg('impl.halted'), neg(fault_w)), eq('impl.fetch_pc', fpc))])
    return {'version': 2, 'name': 'RV32I precise retirement over checked disjoint byte memory',
        'inputs': {'rst': 'bool', 'stall': 'bool', **{'seed_' + k: ty for k, ty in ARRAYS.items()}},
        'reset_input': 'rst', 'spec': spec, 'impl': machine, 'binding': c.conj(relation),
        'commit': 'retire', 'can_step': 'can_step', 'hold_when': 'i.stall',
        'progress': {'enabled': land(neg('i.stall'), neg('impl.halted')),
                     'rank': it('impl.w_valid', b(2, 0), it('impl.x_valid', b(2, 1), it('impl.d_valid', b(2, 2), b(2, 3))))}}


class ProofSession:
    """Fresh imported, normalized CPU theorem plus independent concrete EEI proofs.

    The composition rule instantiates the abstract limit and the two total
    seven-bit backing arrays, with full byte-address map/permission checks kept
    outside their selectors. JSON verdicts never create a reusable handle.
    """
    RULE = 'rv32i-disjoint-byte-memory-step-simulation-v1'

    def __init__(self, out, checker=None, frontend=None, lifter=None, conjunctive_lemmas=False):
        import hashlib
        from pathlib import Path
        from audit.veryl_scaling import rv32i_memory as memory
        self.out = Path(out)
        self.out.mkdir(parents=True, exist_ok=False)
        self.normalization = p.NormalizationSession(self.out / 'normalization', checker, frontend, lifter)
        self.checker = self.normalization.checker
        self.conjunctive_lemmas = bool(conjunctive_lemmas)
        self._proofs = {}
        self._memories = {}
        self._files = {str(path.resolve()): hashlib.sha256(path.read_bytes()).hexdigest()
                       for path in (Path(__file__), Path(memory.__file__))}
        self.cpu = None
        self._normalization_record = None

    def _check(self):
        import hashlib
        from pathlib import Path
        self.normalization._check()
        for path, value in self._files.items():
            if hashlib.sha256(Path(path).read_bytes()).hexdigest() != value:
                raise ValueError('refinement/composition rule changed during session')

    def _prove_document(self, doc, label, scoped=False):
        import json
        out = self.out / label
        out.mkdir(exist_ok=False)
        (out / 'contract.json').write_text(json.dumps(doc, separators=(',', ':')))
        import os
        decompose = self.conjunctive_lemmas and label.startswith('cpu-')
        previous = os.environ.get('HWVERIFY_CONJUNCTIVE_LEMMAS')
        os.environ['HWVERIFY_CONJUNCTIVE_LEMMAS'] = '1' if decompose else '0'
        try:
            report, seconds = c.execute([self.checker, out / 'contract.json', '--out', out / 'proof'],
                                        out / 'report.json', (0, 1, 3))
        finally:
            if previous is None: os.environ.pop('HWVERIFY_CONJUNCTIVE_LEMMAS', None)
            else: os.environ['HWVERIFY_CONJUNCTIVE_LEMMAS'] = previous
        if scoped:
            if doc['specs']['Contract'].get('examples'):
                c.validate_scoped(report)
            else:
                p.validate_lemma(report, allow_conjunctive=decompose)
        else:
            c.RUNNER.validate_report(report, None)
            for query in report['obligations']:
                if query.get('backend') == 'conjunctive_lemmas' and decompose:
                    p.validate_conjunctive_obligation(query)
                elif query.get('backend') not in ('finite_bv', 'structural_kernel'):
                    raise ValueError('unsupported CPU proof backend')
        self._check()
        if json.loads((out / 'contract.json').read_text()) != doc:
            raise ValueError('contract changed during solving')
        return {'document_sha256': p.digest(doc), 'report_sha256': p.digest(report),
                'seconds': seconds, 'status': report['status'], 'conjunctive_lemmas_enabled': decompose}

    def prove_cpu(self):
        self._check()
        if self.cpu is not None:
            raise ValueError('CPU already normalized in this session')
        import os
        previous = os.environ.get('HWVERIFY_CONJUNCTIVE_LEMMAS')
        os.environ['HWVERIFY_CONJUNCTIVE_LEMMAS'] = '0'
        try:
            cpu, normalization = self.normalization.run()
        finally:
            if previous is None: os.environ.pop('HWVERIFY_CONJUNCTIVE_LEMMAS', None)
            else: os.environ['HWVERIFY_CONJUNCTIVE_LEMMAS'] = previous
        self.cpu = cpu
        self._normalization_record = normalization
        local = self._prove_document(p.cpu_contract(cpu), 'cpu-port-contract', True)
        theorem = self._prove_document(abstract_composition(cpu), 'cpu-abstract')
        self._check()
        handle = object()
        self._proofs[handle] = {'kind': 'cpu', 'machine_sha256': p.digest(cpu),
            'local': local, 'abstract': theorem, 'normalization_sha256': p.digest(normalization),
            'rule_files': copy.deepcopy(self._files)}
        return handle

    def prove_memory(self, count):
        import hashlib
        import json
        from pathlib import Path
        from audit.veryl_scaling import rv32i_memory as memory
        self._check()
        out = self.out / ('memory-' + str(count) + '-import')
        raw = memory.compile_machine(count, out, self.normalization.frontend, self.normalization.lifter)
        if (out / 'source.veryl').read_text() != memory.memory_source(count):
            raise ValueError('memory source changed during import')
        artifacts = {str(path.resolve()): hashlib.sha256(path.read_bytes()).hexdigest()
                     for path in out.iterdir() if path.is_file()}
        contract = memory.memory_contract(count, raw)
        theorem = self._prove_document(contract, 'memory-' + str(count) + '-proof', True)
        self._check()
        for path, value in artifacts.items():
            if hashlib.sha256(Path(path).read_bytes()).hexdigest() != value:
                raise ValueError('memory artifact changed during proving')
        handle = object()
        self._memories[handle] = raw
        self._proofs[handle] = {'kind': 'memory', 'capacity': count, 'machine_sha256': p.digest(raw),
            'contract': theorem, 'artifacts': artifacts, 'rule_files': copy.deepcopy(self._files)}
        (self.out / ('memory-' + str(count) + '-session-record.json')).write_text(json.dumps(self._proofs[handle], indent=2))
        return handle

    def compose(self, cpu_handle, memory_handle, count, connections=None):
        import hashlib
        from pathlib import Path
        from audit.veryl_scaling import rv32i_memory as memory
        self._check()
        try:
            cpu_proof = self._proofs[cpu_handle]
            memory_proof = self._proofs[memory_handle]
            concrete = self._memories[memory_handle]
        except (KeyError, TypeError):
            raise ValueError('composition requires current-session verified handles') from None
        if cpu_handle is memory_handle or cpu_proof['kind'] != 'cpu' or memory_proof['kind'] != 'memory':
            raise ValueError('cyclic or mismatched composition dependency')
        if (memory_proof['capacity'] != count or count not in (4, 16, 64)
                or cpu_proof['machine_sha256'] != p.digest(self.cpu)
                or memory_proof['machine_sha256'] != p.digest(concrete)
                or cpu_proof['normalization_sha256'] != p.digest(self._normalization_record)
                or any(r['rule_files'] != self._files for r in (cpu_proof, memory_proof))):
            raise ValueError('stale composition dependencies')
        for path, value in memory_proof['artifacts'].items():
            if hashlib.sha256(Path(path).read_bytes()).hexdigest() != value:
                raise ValueError('stale concrete memory import')
        if (cpu_proof['local']['document_sha256'] != p.digest(p.cpu_contract(self.cpu))
                or cpu_proof['abstract']['document_sha256'] != p.digest(abstract_composition(self.cpu))
                or memory_proof['contract']['document_sha256'] != p.digest(memory.memory_contract(count, concrete))):
            raise ValueError('contract changed after proof')
        expected = {'cpu.rst': 'system.rst', 'memory.rst': 'system.rst', 'cpu.stall': 'system.stall',
                    **{'memory.' + k: 'cpu.' + k for k in memory.memory_inputs(count)
                       if k != 'rst' and not k.startswith('seed_')},
                    **{'cpu.' + k: 'memory.' + k for k in memory.PORTS}}
        if connections is not None and connections != expected:
            raise ValueError('connection graph differs from proved byte-memory interface')
        for name, ty in memory.memory_inputs(count).items():
            if name != 'rst' and not name.startswith('seed_') and p.PORTS.get(name) != ty:
                raise ValueError('CPU-to-memory port type mismatch: ' + name)
        for name, ty in memory.PORTS.items():
            if p.INPUTS.get(name) != ty:
                raise ValueError('memory-to-CPU port type mismatch: ' + name)
        if concrete['state'] != memory.memory_state(count) or set(concrete['outputs']) != set(memory.PORTS):
            raise ValueError('concrete memory interface mismatch')
        def total_array(kind, seeds=False):
            array = ['const_mem', 7, b(32, 0)]
            for index in range(count):
                value = 'memory.' + ('seed_' if seeds else '') + kind + str(index)
                array = ['write', array, b(7, index), value]
            return array
        instantiation = {'array_sort': {'mem': [7, 32]}, 'limit': b(7, count),
            'rom': total_array('rom'), 'data': total_array('data'),
            'seed_rom': total_array('rom', True), 'seed_data': total_array('data', True),
            'seed_limit': b(7, count), 'unmapped_cells': 'zero',
            'selector': 'effective byte address bits[8:2], only after all32-bit region/permission checks'}
        return {'status': 'checked_rv32i_memory_composition', 'rule': self.RULE, 'capacity': count,
            'backing_limit': count, 'rom_base': 0, 'ram_base': RAM_BASE, 'connections': expected,
            'abstract_instantiation': instantiation,
            'dependencies': copy.deepcopy([cpu_proof, memory_proof]),
            'saved_records_are_certificates': False,
            'claim': 'Imported RV32I pipeline refines the independent ISA over the imported disjoint byte memory',
            'trusted_rule': 'Instantiate limit=count and each abstract7bit array with concrete indexed cells, zero elsewhere. Checked seed reset, immutable ROM/map, exact full-address permissions, per-byte accepted W update, frame preservation and W-before-X merged reads preserve the abstract memory step. A common synchronous reset and the exact typed connection graph preserve the independently proved CPU microstep and terminal-trap-aware progress.'}


def main():
    import argparse
    import json
    import os
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--sizes', type=int, nargs='+', choices=(4, 16, 64), default=(4, 16, 64))
    parser.add_argument('--negative-controls', action='store_true')
    parser.add_argument('--conjunctive-lemmas', action='store_true', help='Opt in to checked post-conjunct lemmas after monolithic Unknown; per-lemma limits unchanged, aggregate cost reported')
    parser.add_argument('--checker', type=Path)
    parser.add_argument('--frontend', type=Path)
    parser.add_argument('--lifter', type=Path)
    args = parser.parse_args()
    os.environ['HWVERIFY_SOLVER'] = 'finite'
    session = ProofSession(args.out, args.checker, args.frontend, args.lifter, args.conjunctive_lemmas)
    tripwire = session.out / 'z3-tripwire'
    marker = session.out / 'z3-invoked.txt'
    tripwire.write_text('#!/bin/sh\nprintf invoked > "' + str(marker.resolve()) + '"\nexit 97\n')
    tripwire.chmod(0o755)
    os.environ['Z3_BIN'] = str(tripwire.resolve())
    try:
        cpu_handle = session.prove_cpu()
        compositions = []
        for count in args.sizes:
            memory_handle = session.prove_memory(count)
            compositions.append(session.compose(cpu_handle, memory_handle, count))
        negative_controls = {}
        if args.negative_controls:
            from audit.veryl_scaling import rv32i_memory as memory
            negative_controls['cpu_execution'] = p.check_execution_mutations(
                session.out / 'cpu-negative-controls', session.checker,
                session.normalization.frontend, session.normalization.lifter)
            negative_controls['cpu_pipeline'] = p.check_pipeline_mutations(
                session.out / 'pipeline-negative-controls', session.checker,
                session.normalization.frontend, session.normalization.lifter)
            negative_controls['memory'] = memory.check_mutations(
                session.out, session.checker, session.normalization.frontend, session.normalization.lifter)
        session._check()
        if marker.exists():
            raise RuntimeError('external solver invoked')
        result = {'status': 'passed', 'instruction_set': 'RV32I-v2.1',
            'instruction_count': 40, 'normalization_lemmas': 49,
            'compositions': compositions, 'negative_controls': negative_controls, 'external_solver_invoked': False}
        (session.out / 'summary.json').write_text(json.dumps(result, indent=2))
        print(json.dumps({'status': result['status'], 'capacities': list(args.sizes),
                          'normalization_lemmas': 49}))
    except Exception as exc:
        (session.out / 'summary.json').write_text(json.dumps({'status': 'failed', 'error': str(exc),
            'external_solver_invoked': marker.exists()}, indent=2))
        raise


if __name__ == '__main__':
    main()
