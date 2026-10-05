#!/usr/bin/env python3
"""Independent writable Harvard-memory contract and actual Veryl import.

A write is accepted on a non-reset edge exactly when write_enable is asserted.
The CPU owns stall/retirement qualification. Mapped same-address reads explicitly
forward that write, giving the composition W-before-X ordering. ROM is immutable.
"""
from audit.veryl_scaling import cpu_memory_contract as c
from audit.veryl_scaling.cpu_registers import b, eq, it, land, neg

MEMORY_FAULTS = ('wrong_write_address', 'wrong_write_data', 'ungated_write',
    'ungated_cell_write', 'disabled_write', 'missing_bypass', 'wrong_bypass_address', 'off_by_one_mapping',
    'other_cell_change', 'rom_change', 'recapture', 'reset_write_priority', 'wrong_reset')


def immutable(count):
    """Legacy API name for the complete memory state shape, not a mutability claim."""
    if count not in (4, 16, 64):
        raise ValueError('supported backing capacities: 4, 16, 64')
    return c.immutable(count)


def memory_source(count, fault=None):
    immutable(count)
    if fault is not None and fault not in MEMORY_FAULTS:
        raise ValueError('unknown writable memory fault: ' + str(fault))
    declarations, resets, reads, updates = [], [], [], []
    write_address = "write_address + 6'd1" if fault == 'wrong_write_address' else 'write_address'
    write_data = "write_data ^ 32'd1" if fault == 'wrong_write_data' else 'write_data'
    enabled = "1'b1" if fault == 'ungated_write' else "1'b0" if fault == 'disabled_write' else 'write_enable'
    for kind, width, port in [('rom', c.IW, 'imem'), ('data', c.WIDTH, 'dmem')]:
        for i in range(count):
            declarations += [f'seed_{kind}{i}: input bit<{width}>,', f'{kind}{i}: output bit<{width}>,']
            resets.append(f'{kind}{i} = seed_{kind}{i};')
        reads += [f"case {port}_address {{ " + ' '.join(
            f"6'd{i}: {port}_response = {kind}{i};" for i in range(count)) +
            f" default: {port}_response = '0; }}"]
    for i in range(count):
        selected = (i + 1) % 64 if fault == 'off_by_one_mapping' else i
        cell_enabled = "1'b1" if fault == 'ungated_cell_write' and i == 0 else enabled
        updates.append(f"if {cell_enabled} && ({write_address}) == 6'd{selected} {{ data{i} = {write_data}; }}")
    if fault == 'other_cell_change': updates.append("if !write_enable { data0 = data0 ^ 32'd1; }")
    if fault == 'rom_change': updates.append("if write_enable { rom0 = rom0 ^ 41'd1; }")
    if fault == 'recapture': updates.append('if !write_enable { data0 = seed_data0; }')
    mapped = "1'b1" if count == 64 else f"(write_address & 6'd{64-count}) == 6'd0"
    comparison = 'imem_address' if fault == 'wrong_bypass_address' else 'dmem_address'
    if fault != 'missing_bypass':
        reads.append(f'if !rst && write_enable && {mapped} && write_address == {comparison} {{ dmem_response = write_data; }}')
    if fault == 'wrong_reset': resets[0] = 'rom0 = seed_rom1;'
    if fault == 'reset_write_priority': resets.append('if write_enable { data0 = write_data; }')
    return '''// Reset-captured ROM and writable data; W-before-X write-through.
module ReadMemory (
    clk: input clock, rst: input bit,
    imem_address: input bit<6>, dmem_address: input bit<6>,
    write_enable: input bit, write_address: input bit<6>, write_data: input bit<32>,
    imem_response: output bit<41>, dmem_response: output bit<32>,
    ''' + '\n    '.join(declarations) + '''
) {
    always_comb { ''' + ' '.join(reads) + ''' }
    always_ff (clk) { if rst { ''' + ' '.join(resets) + ''' }
        else { ''' + ' '.join(updates) + ''' } }
}
'''


def memory_contract(count, machine):
    cells = immutable(count)
    observations = {'obs_' + k: ({'bv': c.IW if k == 'imem_response' else c.WIDTH}, machine['outputs'][k])
                    for k in ('imem_response', 'dmem_response')}
    # Reference cells use only specified input semantics, never DUT next/wires.
    def expected_cell(k):
        return it(land('i.write_enable', eq('i.write_address', b(c.AB, int(k[4:])))),
                  'i.write_data', 's.obs_model_' + k) if k.startswith('data') else 's.obs_model_' + k
    ghosts = {'obs_model_' + k: (ty, expected_cell(k)) for k, ty in cells.items()}
    machine = c.with_observers(machine, observations, ghosts,
                              {'obs_model_' + k: 'i.seed_' + k for k in cells})
    state = machine['state']
    inputs = {'rst': 'bool', 'imem_address': {'bv': c.AB}, 'dmem_address': {'bv': c.AB},
              'write_enable': 'bool', 'write_address': {'bv': c.AB}, 'write_data': {'bv': c.WIDTH},
              **{'seed_' + k: ty for k, ty in cells.items()}}
    invariant = c.conj([*[eq('s.' + k, 's.obs_model_' + k) for k in cells],
                       *[eq('o.' + k, 's.' + k) for k in observations]])
    initial = c.conj(eq('s.' + k, c.zero(ty)) for k, (ty, _) in observations.items())
    # The reference predicate is arithmetic, independently of the RTL's mask.
    mapped = True if count == 64 else ['ult', 'i.write_address', b(c.AB, count)]
    response = it(land('i.write_enable', mapped, eq('i.write_address', 'i.dmem_address')),
                  'i.write_data', c.lookup('data', count, 'i.dmem_address', 's.obs_model_'))
    step = c.conj([*[eq('n.' + k, expected_cell(k)) for k in cells],
                   *[eq('n.obs_model_' + k, expected_cell(k)) for k in cells],
                   eq('n.obs_imem_response', c.lookup('rom', count, 'i.imem_address', 's.obs_model_')),
                   eq('n.obs_dmem_response', response)])
    return c.scoped_document(f'Writable memory {count} backing words per port', state, inputs,
        {k: state[k] for k in observations}, initial, invariant, step, machine, memory_examples(count))


def compile_machine(text, kind, count, out, frontend, lifter):
    """Compile/lift real source, with independent reset and normal imports."""
    if kind not in ('cpu', 'memory'):
        raise ValueError('expected cpu or memory import kind')
    out.mkdir(parents=True, exist_ok=False)
    (out / 'source.veryl').write_text(text)
    c.RUNNER.write_json(out / 'design.json', {'top': 'PipelineCPU' if kind == 'cpu' else 'ReadMemory',
        'four_state': False, 'sources': [{'path': 'source.veryl', 'text': text}]})
    compiled, _ = c.execute([frontend, out / 'design.json'], out / 'compiled.json')
    if compiled.get('allowed_diagnostics') != []:
        raise RuntimeError('frontend diagnostics require a waiver')
    if kind == 'cpu':
        from audit.veryl_scaling.cpu_store_reuse import PORTS
        state, ports = c.cpu_state(), PORTS
        inputs = {'rst_n': {'name': 'rst', 'type': 'bool', 'expr': ['not', 'i.rst']},
                  **{k: {'type': v} for k, v in c.CPU_INPUTS.items() if k != 'rst'}}
    else:
        state = immutable(count)
        ports = {'imem_response': {'bv': c.IW}, 'dmem_response': {'bv': c.WIDTH}}
        inputs = {k: {'type': v} for k, v in {
            'rst': 'bool', 'imem_address': {'bv': c.AB}, 'dmem_address': {'bv': c.AB},
            'write_enable': 'bool', 'write_address': {'bv': c.AB}, 'write_data': {'bv': c.WIDTH},
            **{'seed_' + k: ty for k, ty in state.items()}}.items()}
    config = {'event': 'clk', 'inputs': inputs, 'state': {k: {'type': v} for k, v in state.items()}}
    machine = {'state': state}
    for mode, reset in [('normal', False), ('reset', True)]:
        config['overrides'] = {'rst': reset}
        config['outputs'] = {} if reset else {k: {'signal': k, 'type': v} for k, v in ports.items()}
        c.RUNNER.write_json(out / (mode + '-bindings.json'), config)
        lifted, _ = c.execute([lifter, out / 'compiled.json', out / (mode + '-bindings.json')]
            + (['--inline'] if reset else []), out / (mode + '-lift.json'))
        if reset:
            if any(c.RUNNER.references(v, 's.') or c.RUNNER.references(v, 'w.') for v in lifted['next'].values()):
                raise RuntimeError('DUT reset depends on arbitrary prestate')
            machine['reset'] = lifted['next']
        else:
            for part in ('next', 'wires', 'outputs'): machine[part] = lifted[part]
    c.RUNNER.write_json(out / 'machine.json', machine)
    return machine


def memory_examples(count):
    """Write-through and durable writes; no assumptions about initial seeds."""
    import copy
    trace = [
        {'operation': 'tick', 'inputs': {'rst': False, 'write_enable': True,
            'write_address': b(c.AB, count - 1), 'write_data': b(c.WIDTH, 0x12345678),
            'dmem_address': b(c.AB, count - 1)},
         'observe': {'obs_dmem_response': b(c.WIDTH, 0x12345678)}},
        {'operation': 'tick', 'inputs': {'rst': False, 'write_enable': False,
            'write_address': b(c.AB, count - 1), 'write_data': b(c.WIDTH, 99),
            'dmem_address': b(c.AB, count - 1), 'seed_data0': b(c.WIDTH, 99)},
         'observe': {'obs_dmem_response': b(c.WIDTH, 0x12345678)}}]
    bad = copy.deepcopy(trace)
    bad[-1]['observe']['obs_dmem_response'] = b(c.WIDTH, 99)
    examples = {'write_through_and_hold': {'expect': 'positive', 'initial': {}, 'trace': trace},
                'reject_disabled_write': {'expect': 'negative', 'initial': {}, 'trace': bad}}
    if count < 64:
        for value, expected in [(0, 'positive'), (1, 'negative')]:
            examples['unmapped_write_' + expected] = {'expect': expected, 'initial': {}, 'trace': [
                {'operation': 'tick', 'inputs': {'write_enable': True, 'write_address': b(c.AB, 63),
                    'write_data': b(c.WIDTH, 1), 'dmem_address': b(c.AB, 63)},
                 'observe': {'obs_dmem_response': b(c.WIDTH, value)}}]}
    return examples
