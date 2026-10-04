#!/usr/bin/env python3
"""Finite RV32I byte-memory EEI; independent contract and actual RTL import.

ROM [0,4*N) is executable and data-readable, never writable. RAM
[0x1000,0x1000+4*N) is data-readable/writable, never executable; N=4/16/64.
Full unsigned 32-bit bounds are checked before indexing. Responses are aligned
little-endian words; natural alignment is CPU-owned. Faults reflect the immutable
map and dmem_write permission selection, independently of request validity.
An X store permission check remains valid at W; X requests never mutate RAM.
W atomically replaces selected lanes with W-before-X read-through. Invalid W
writes do nothing. Reset seed capture wins; seeds otherwise have no effect.
"""
from pathlib import Path
from audit.veryl_scaling import cpu_memory_contract as c
from audit.veryl_scaling.rv32i_spec import b, eq, it, ex, land, lor, neg, cat

RAM_BASE = 0x1000

BV32 = {'bv': 32}
PORTS = {'imem_response': BV32, 'dmem_response': BV32,
         'imem_fault': 'bool', 'dmem_fault': 'bool'}
MEMORY_FAULTS = ('high_address_alias', 'wrong_read_word', 'wrong_write_address',
    'wrong_write_data', 'ignore_mask', 'reverse_lanes', 'missing_bypass',
    'wrong_bypass_address', 'ungated_write', 'disabled_write', 'other_cell_change',
    'rom_change', 'recapture', 'reset_write_priority', 'wrong_reset',
    'missing_imem_fault', 'missing_dmem_fault', 'x_store_side_effect')


def memory_state(count):
    if count not in (4, 16, 64):
        raise ValueError('supported backing capacities: 4, 16, 64')
    return {f'{kind}{i}': BV32 for kind in ('rom', 'data') for i in range(count)}


def memory_inputs(count):
    return {'rst': 'bool', 'imem_address': BV32, 'dmem_address': BV32,
        'imem_valid': 'bool', 'dmem_valid': 'bool', 'dmem_write': 'bool',
        'write_enable': 'bool', 'write_address': BV32, 'write_data': BV32,
        'write_mask': {'bv': 4}, **{'seed_' + k: ty for k, ty in memory_state(count).items()}}


def memory_source(count, fault=None):
    cells = memory_state(count)
    if fault is not None and fault not in MEMORY_FAULTS:
        raise ValueError('unknown RV32I memory fault: ' + str(fault))
    declarations = [f'seed_{k}: input bit<32>, {k}: output bit<32>,' for k in cells]
    variables = []
    reset = [f'{k} = seed_{k};' for k in cells]
    reads, updates = [], []
    for port in ('imem', 'dmem'):
        addr = port + '_address'
        if fault == 'high_address_alias': addr = f"({addr} & 32'h1fff)"
        rom_valid = f"{addr} <: 32'd{4*count}"
        ram_valid = f"({addr} >= 32'd4096 && {addr} <: 32'd{4096+4*count})"
        valid = rom_valid if port == 'imem' else f"({ram_valid} || (!dmem_write && {rom_valid}))"
        bad = "1'b0" if fault == 'missing_' + port + '_fault' else f'!({valid})'
        reads += [f'{port}_fault = {bad};', f"{port}_response = '0;", f'{port}_index = {addr} >> 2;']
        for kind, base, region in ([('rom', 0, rom_valid)] if port == 'imem' else [('rom', 0, rom_valid), ('data', 4096, ram_valid)]):
            cases = ' '.join(f"32'd{base//4+i}: {port}_response = {kind}{(i+1)%count if fault == 'wrong_read_word' else i};" for i in range(count))
            reads += [f'if {region} {{ case {port}_index {{ {cases} default: {port}_response = 0; }} }}']
    reads += ["lane_mask = '0;"]
    for lane in range(4):
        pick = 3-lane if fault == 'reverse_lanes' else lane
        condition = "1'b1" if fault == 'ignore_mask' else f"write_mask[{pick}] == 1'b1"
        reads += [f"if {condition} {{ lane_mask = lane_mask | 32'h{255 << (8*lane):08x}; }}"]
    enabled = "1'b1" if fault == 'ungated_write' else "1'b0" if fault == 'disabled_write' else 'write_enable'
    wa = "(write_address + 32'd4)" if fault == 'wrong_write_address' else 'write_address'
    if fault == 'high_address_alias': wa = "(write_address & 32'h1fff)"
    wd = "(write_data ^ 32'd1)" if fault == 'wrong_write_data' else 'write_data'
    reads += [f'write_value = {wd};']
    for i in range(count):
        selected = f"({wa} >= 32'd4096 && {wa} <: 32'd{4096+4*count}) && (({wa} >> 2) == 32'd{1024+i})"
        lanes = []
        for lane in range(4):
            pick = 3-lane if fault == 'reverse_lanes' else lane
            condition = "1'b1" if fault == 'ignore_mask' else f"write_mask[{pick}] == 1'b1"
            variables.append(f'var byte_{i}_{lane}: bit<8>;')
            reads.append(f'byte_{i}_{lane} = data{i}[{8*lane+7}:{8*lane}]; if {condition} {{ byte_{i}_{lane} = write_value[{8*lane+7}:{8*lane}]; }}')
            lanes.append(f'byte_{i}_{lane}')
        updates += [f'if {enabled} && {selected} {{ data{i} = {{' + ', '.join(reversed(lanes)) + '}; }']
    compare = 'imem_address' if fault == 'wrong_bypass_address' else 'dmem_address'
    if fault != 'missing_bypass':
        reads += [f"if !rst && {enabled} && {wa} >= 32'd4096 && {wa} <: 32'd{4096+4*count} && dmem_address >= 32'd4096 && dmem_address <: 32'd{4096+4*count} && ({wa} >> 2) == ({compare} >> 2) {{ dmem_response = (dmem_response & ~lane_mask) | ({wd} & lane_mask); }}"]
    if fault == 'other_cell_change': updates += ["if !write_enable { data0 = data0 ^ 32'd1; }"]
    if fault == 'rom_change': updates += ["if write_enable { rom0 = rom0 ^ 32'd1; }"]
    if fault == 'recapture': updates += ['if !write_enable { data0 = seed_data0; }']
    if fault == 'x_store_side_effect': updates += ["if dmem_valid && dmem_write { data0 = 32'd123; }"]
    if fault == 'wrong_reset': reset[0] = 'rom0 = seed_rom1;'
    if fault == 'reset_write_priority': reset += ['if write_enable { data0 = write_data; }']
    return '''// Full byte-address checks precede word selection; immutable ROM/RAM map.
module RV32IMemory (
    clk: input clock, rst: input bit,
    imem_address: input bit<32>, imem_valid: input bit,
    dmem_address: input bit<32>, dmem_valid: input bit, dmem_write: input bit,
    write_enable: input bit, write_address: input bit<32>,
    write_data: input bit<32>, write_mask: input bit<4>,
    imem_response: output bit<32>, imem_fault: output bit,
    dmem_response: output bit<32>, dmem_fault: output bit,
    ''' + '\n    '.join(declarations) + '''
) {
    var lane_mask: bit<32>; var write_value: bit<32>;
    var imem_index: bit<32>; var dmem_index: bit<32>;
    ''' + ' '.join(variables) + '''
    always_comb { ''' + ' '.join(reads) + ''' }
    always_ff (clk) { if rst { ''' + ' '.join(reset) + ''' }
        else { ''' + ' '.join(updates) + ''' } }
}
'''


def mapped(count, address, base=0):
    return land(neg(['ult', address, b(32, base)]), ['ult', address, b(32, base+4*count)])


def lookup(count, kind, address, prefix='s.'):
    # Exact full-width aligned-address equality, independently written from the
    # RTL's bounded shift/case decoder; no high address bit is discarded.
    value = b(32, 0)
    base = RAM_BASE if kind == 'data' else 0
    for i in reversed(range(count)):
        match = eq(['band', address, b(32, 0xfffffffc)], b(32, base+4*i))
        value = it(match, prefix + kind + str(i), value)
    return value


def merged(old):
    # Oracle constructs a word from independently chosen bytes, not RTL masks.
    return cat(*(it(eq(ex(i, i, 'i.write_mask'), b(1, 1)),
                   ['band', ex(7, 0, ['lshr', 'i.write_data', b(32, 8*i)]), b(8, 255)], ex(8*i+7, 8*i, old))
                 for i in reversed(range(4))))


def memory_contract(count, raw):
    cells = memory_state(count)
    # Reference expressions are authored from the map and byte-write rules,
    # never recovered from imported next/output/wire expressions. Ghosts retain
    # the exact reset-seed relation; normal checks cover arbitrary prestate.
    def expected(k):
        old = 's.' + k
        if k.startswith('rom'): return old
        i = int(k[4:])
        base = RAM_BASE
        # Association is intentional: equivalent Boolean guards must share
        # structurally instead of repeating a 32-bit address proof per cell.
        selected = ['and', 'i.write_enable', ['and', ['ule', b(32, RAM_BASE), 'i.write_address'],
                        ['and', ['ult', 'i.write_address', b(32, RAM_BASE+4*count)],
                        eq(['lshr', 'i.write_address', b(32, 2)], b(32, base//4+i))]]]
        return it(selected, merged(old), old)
    observations = {'obs_' + k: (({'bv': 1}, it(raw['outputs'][k], b(1, 1), b(1, 0)))
        if ty == 'bool' else (ty, raw['outputs'][k])) for k, ty in PORTS.items()}
    machine = c.with_observers(raw, observations,
        {'obs_model_' + k: (ty, expected(k)) for k, ty in cells.items()},
        {'obs_model_' + k: 'i.seed_' + k for k in cells})
    old_read = it(mapped(count, 'i.dmem_address'), lookup(count, 'rom', 'i.dmem_address'), lookup(count, 'data', 'i.dmem_address'))
    # Word equality expressed by retaining all address bits except byte offset.
    same_word = eq(['band', 'i.write_address', b(32, 0xfffffffc)],
                   ['band', 'i.dmem_address', b(32, 0xfffffffc)])
    read = it(land('i.write_enable', mapped(count, 'i.write_address', RAM_BASE),
                   mapped(count, 'i.dmem_address', RAM_BASE), same_word), merged(old_read), old_read)
    expectations = {**{k: expected(k) for k in cells},
        **{'obs_model_' + k: expected(k) for k in cells},
        'obs_imem_response': lookup(count, 'rom', 'i.imem_address'),
        'obs_dmem_response': read,
        'obs_imem_fault': it(neg(mapped(count, 'i.imem_address')), b(1, 1), b(1, 0)),
        'obs_dmem_fault': it(neg(lor(mapped(count, 'i.dmem_address', RAM_BASE), land(neg('i.dmem_write'), mapped(count, 'i.dmem_address')))), b(1, 1), b(1, 0))}
    invariant = c.conj([*[eq('s.'+k, 's.obs_model_'+k) for k in cells],
                       *[eq('o.'+k, 's.'+k) for k in observations]])
    initial = c.conj(eq('s.'+k, c.zero(ty)) for k, (ty, _) in observations.items())
    return c.scoped_document(f'RV32I byte-memory {count} words per port',
        machine['state'], memory_inputs(count), {k: ty for k, (ty, _) in observations.items()},
        initial, invariant, c.conj(eq('n.'+k, v) for k, v in expectations.items()), machine, memory_examples())


def compile_machine(count, out, frontend=None, lifter=None, fault=None):
    frontend = frontend or c.ROOT / 'conformance/veryl-proof/target/debug/veryl-proof-frontend'
    lifter = lifter or c.ROOT / '../target/release/lydite-celox-lift'
    out = Path(out)
    out.mkdir(parents=True, exist_ok=False)
    text = memory_source(count, fault)
    (out/'source.veryl').write_text(text)
    c.RUNNER.write_json(out/'design.json', {'top': 'RV32IMemory', 'four_state': False,
        'sources': [{'path': 'source.veryl', 'text': text}]})
    compiled, _ = c.execute([frontend, out/'design.json'], out/'compiled.json')
    if compiled.get('allowed_diagnostics') != []:
        raise RuntimeError('RV32I memory frontend diagnostics require a waiver')
    state = memory_state(count)
    config = {'event': 'clk', 'inputs': {k: {'type': v} for k,v in memory_inputs(count).items()},
              'state': {k: {'type': v} for k,v in state.items()}}
    machine = {'state': state}
    for mode, reset in [('normal', False), ('reset', True)]:
        config['overrides'] = {'rst': reset}
        config['outputs'] = {} if reset else {k: {'signal': k, 'type': v} for k,v in PORTS.items()}
        c.RUNNER.write_json(out/(mode+'-bindings.json'), config)
        lifted, _ = c.execute([lifter, out/'compiled.json', out/(mode+'-bindings.json')]
            + (['--inline'] if reset else []), out/(mode+'-lift.json'))
        if reset:
            if any(c.RUNNER.references(v, 's.') or c.RUNNER.references(v, 'w.') for v in lifted['next'].values()):
                raise RuntimeError('memory reset depends on arbitrary prestate')
            machine['reset'] = lifted['next']
        else:
            for part in ('next', 'wires', 'outputs'): machine[part] = lifted[part]
    c.RUNNER.write_json(out/'machine.json', machine)
    return machine


def memory_examples():
    """Finite specification examples complement, never replace, universal binding."""
    import copy
    trace = [
        {'operation': 'tick', 'inputs': {'write_enable': True,
            'write_address': b(32, RAM_BASE), 'write_data': b(32, 0x11223344),
            'write_mask': b(4, 15), 'dmem_address': b(32, RAM_BASE)},
         'observe': {'obs_dmem_response': b(32, 0x11223344), 'obs_dmem_fault': b(1, 0)}},
        {'operation': 'tick', 'inputs': {'write_enable': True,
            'write_address': b(32, RAM_BASE+1), 'write_data': b(32, 0xaabbccdd),
            'write_mask': b(4, 2), 'dmem_address': b(32, RAM_BASE+3)},
         'observe': {'obs_dmem_response': b(32, 0x1122cc44)}},
        {'operation': 'tick', 'inputs': {'write_enable': False, 'dmem_write': True,
            'dmem_address': b(32, 0), 'imem_address': b(32, RAM_BASE)},
         'observe': {'obs_dmem_fault': b(1, 1), 'obs_imem_fault': b(1, 1)}}]
    wrong = copy.deepcopy(trace)
    wrong[1]['observe']['obs_dmem_response'] = b(32, 0xaabbccdd)
    return {'masked_write_and_permissions': {'expect': 'positive', 'initial': {}, 'trace': trace},
            'reject_unmasked_overwrite': {'expect': 'negative', 'initial': {}, 'trace': wrong}}


def check_mutations(out, checker, frontend=None, lifter=None):
    """Run every actual source mutant; return diagnostics, never proof handles.

    Existing session roots are supported, but all mutation destinations must be
    absent. Each expected failing obligation must be finite SAT with an original-
    formula-replayed witness. Reset and preservation obligations, example
    polarities, and all remaining verdicts are checked strictly by validate_scoped.
    """
    import hashlib
    import json
    out = Path(out)
    checker = Path(checker).resolve()
    frontend = Path(frontend or c.ROOT / 'conformance/veryl-proof/target/debug/veryl-proof-frontend').resolve()
    lifter = Path(lifter or c.ROOT / '../target/release/lydite-celox-lift').resolve()
    summary_path = out / 'memory-mutation-summary.json'
    destinations = [summary_path] + [out / f'memory-4-bad-{fault}-{kind}'
        for fault in MEMORY_FAULTS for kind in ('import', 'proof')]
    for path in destinations:
        if path.exists():
            raise FileExistsError('refusing to overwrite mutation evidence: ' + str(path))
    if len(MEMORY_FAULTS) != 18 or len(set(MEMORY_FAULTS)) != 18:
        raise RuntimeError('expected exactly eighteen distinct source mutations')
    def sha(path):
        return hashlib.sha256(Path(path).read_bytes()).hexdigest()
    tools = {name: {'path': str(path), 'sha256': sha(path)} for name, path in
             (('checker', checker), ('frontend', frontend), ('lifter', lifter))}
    out.mkdir(parents=True, exist_ok=True)
    results = []
    for fault in MEMORY_FAULTS:
        imported = out / f'memory-4-bad-{fault}-import'
        raw = compile_machine(4, imported, frontend, lifter, fault)
        folder = out / f'memory-4-bad-{fault}-proof'
        folder.mkdir(exist_ok=False)
        reset_fault = fault in ('wrong_reset', 'reset_write_priority')
        checked = c.check_document(memory_contract(4, raw), folder, checker,
                                   'wrong_reset' if reset_fault else fault)
        report = json.loads((folder / 'report.json').read_text())
        name = 'binding_reset_establishes_product' if reset_fault else 'binding_product_preservation'
        obligation = next(q for q in report['implementation_binding']['obligations'] if q['name'] == name)
        if (obligation.get('backend') != 'finite_bv' or obligation.get('solver_result') != 'sat'
                or obligation.get('status') != 'counterexample'
                or obligation.get('finite', {}).get('original_formula_validated') is not True):
            raise RuntimeError('mutation lacks a finite replay-validated counterexample: ' + fault)
        proof_root = (folder / 'proof').resolve()
        evidence = (proof_root / obligation['evidence']).resolve()
        witness = (evidence.parent / obligation['finite_diagnostics']).resolve()
        for path in (evidence, witness):
            if proof_root not in path.parents or not path.is_file():
                raise RuntimeError('missing or out-of-scope mutation evidence: ' + str(path))
        diagnostics = json.loads(witness.read_text())
        if diagnostics != obligation['finite']:
            raise RuntimeError('mutation witness sidecar differs from reported evidence: ' + fault)
        results.append({'fault': fault, 'status': 'validated_original_formula_counterexample',
            'obligation': name, 'solver_result': 'sat', 'backend': 'finite_bv',
            'original_formula_validated': True, 'seconds': checked['seconds'],
            'source_sha256': sha(imported / 'source.veryl'),
            'machine_sha256': sha(imported / 'machine.json'),
            'document_sha256': sha(folder / 'contract.json'),
            'report_sha256': sha(folder / 'report.json'),
            'obligation_sha256': sha(evidence), 'witness_sha256': sha(witness),
            'obligation_path': str(evidence.relative_to(out.resolve())),
            'witness_path': str(witness.relative_to(out.resolve())), 'tools': tools})
    for tool in tools.values():
        if sha(tool['path']) != tool['sha256']:
            raise RuntimeError('tool binary changed during memory mutation verification')
    with summary_path.open('x') as output:
        json.dump(results, output, indent=2)
    return results
