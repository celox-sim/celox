#!/usr/bin/env python3
"""Import and verify the separate RV32I v2.1 D/X/W fixture.

Legacy custom-ISA proof interfaces and fixtures remain unchanged.
"""
import argparse
import copy
import hashlib
import os
import json
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT))
from audit.veryl_scaling import cpu_memory_contract as c

BV32 = {'bv': 32}
INPUTS = {'rst': 'bool', 'stall': 'bool', 'imem_response': BV32,
          'imem_fault': 'bool', 'dmem_response': BV32, 'dmem_fault': 'bool'}
PORTS = {**{k: BV32 for k in ('imem_address', 'dmem_address', 'write_address',
                             'write_data', 'trap_pc', 'trap_value')},
         **{k: 'bool' for k in ('imem_valid', 'dmem_valid', 'dmem_write', 'write_enable',
                               'commit', 'retire', 'trap_valid')},
         'write_mask': {'bv': 4}, 'trap_cause': {'bv': 4}}

INTERNALS = {
    'alu_result': ('result', BV32), 'next_pc': ('next_pc', BV32),
    'fault': ('fault', 'bool'), 'cause': ('cause', {'bv': 4}), 'tval': ('tval', BV32),
    'address': ('address', BV32), 'store_data': ('store_data', BV32),
    'store_mask': ('store_mask', {'bv': 4}), 'writes': ('writes', 'bool'),
    'legal': ('legal', 'bool'), 'taken': ('taken', 'bool'),
}


def cpu_state():
    state = {k: BV32 for k in ('pc', 'fetch_pc', *[f'r{i}' for i in range(32)])}
    state['halted'] = 'bool'
    for stage in ('d', 'x', 'w'):
        state.update({f'{stage}_valid': 'bool', f'{stage}_pc': BV32, f'{stage}_ir': BV32})
    state.update({k: 'bool' for k in ('d_fetch_fault', 'x_fetch_fault', 'w_writes',
                                     'w_store', 'w_fault')})
    state.update({k: BV32 for k in ('x_operand1', 'x_operand2', 'w_result', 'w_next_pc',
                                   'w_address', 'w_data', 'w_tval')})
    state.update({'w_mask': {'bv': 4}, 'w_cause': {'bv': 4}})
    return state


def source():
    return (ROOT / 'conformance/veryl-symbolic/rv32i_pipeline.veryl').read_text()


def compile_machine(out, frontend=None, lifter=None, text=None):
    frontend = frontend or ROOT / 'conformance/veryl-proof/target/debug/veryl-proof-frontend'
    lifter = lifter or ROOT / 'target/release/hwverify-sir-lift'
    text = source() if text is None else text
    out = Path(out)
    out.mkdir(parents=True, exist_ok=False)
    (out / 'source.veryl').write_text(text)
    c.RUNNER.write_json(out / 'design.json', {'top': 'RV32IPipeline', 'four_state': False,
        'sources': [{'path': 'source.veryl', 'text': text}]})
    compiled, _ = c.execute([frontend, out / 'design.json'], out / 'compiled.json')
    if compiled.get('allowed_diagnostics') != []:
        raise RuntimeError('RV32I frontend diagnostics require a waiver')
    state = cpu_state()
    inputs = {'rst_n': {'name': 'rst', 'type': 'bool', 'expr': ['not', 'i.rst']},
              **{k: {'type': v} for k, v in INPUTS.items() if k != 'rst'}}
    config = {'event': 'clk', 'inputs': inputs, 'state': {k: {'type': v} for k, v in state.items()}}
    machine = {'state': state}
    for mode, reset in [('normal', False), ('reset', True)]:
        config['overrides'] = {'rst': reset}
        config['outputs'] = {} if reset else {**{k: {'signal': k, 'type': v} for k, v in PORTS.items()},
            **{'proof_' + k: {'signal': signal, 'type': ty} for k, (signal, ty) in INTERNALS.items()}}
        c.RUNNER.write_json(out / (mode + '-bindings.json'), config)
        lifted, _ = c.execute([lifter, out / 'compiled.json', out / (mode + '-bindings.json')]
                             + (['--inline'] if reset else []), out / (mode + '-lift.json'))
        if reset:
            if any(c.RUNNER.references(v, 's.') or c.RUNNER.references(v, 'w.') for v in lifted['next'].values()):
                raise RuntimeError('RV32I reset depends on arbitrary prestate')
            machine['reset'] = lifted['next']
        else:
            for part in ('next', 'wires'):
                machine[part] = lifted[part]
            machine['outputs'] = {k: lifted['outputs'][k] for k in PORTS}
            c.RUNNER.write_json(out / 'internal.json', {k: lifted['outputs']['proof_' + k] for k in INTERNALS})
    c.RUNNER.write_json(out / 'machine.json', machine)
    return machine



def cpu_contract(raw):
    """Independent ISA retirement and precise-trap contract, arbitrary port replies.

    The read-only ghost samples historical X replies for W evaluation. Architectural
    operands are read from the architectural register file, never DUT payloads.
    """
    from audit.veryl_scaling import rv32i_spec as isa
    b, eq, it, land, lor, neg = isa.b, isa.eq, isa.it, isa.land, isa.lor, isa.neg
    conj, implies = c.conj, c.implies
    active = land(neg('i.stall'), neg('s.halted'))
    trapping_w = land('s.w_valid', 's.w_fault')
    advance = land(active, neg(trapping_w))
    machine = c.with_observers(raw,
        {'obs_' + k: ({'bv': 1}, it(v, b(1, 1), b(1, 0))) if PORTS[k] == 'bool' else (PORTS[k], v)
         for k, v in raw['outputs'].items()},
        {'obs_load': (BV32, it(advance, 'i.dmem_response', 's.obs_load')),
         'obs_data_fault': ('bool', it(advance, 'i.dmem_fault', 's.obs_data_fault')),
         'obs_fetch_fault': ('bool', it(advance, 's.x_fetch_fault', 's.obs_fetch_fault'))})
    regs = [f's.r{i}' for i in range(32)]
    wd, xd, dd = [isa.decode(f's.{stage}_ir') for stage in ('w', 'x', 'd')]
    w = isa.build_execute('s.w_ir', 's.w_pc', isa.reg_read(wd['rs1'], regs),
                          isa.reg_read(wd['rs2'], regs), 's.obs_fetch_fault', 's.obs_load', 's.obs_data_fault')
    pending = [b(32, 0)] + [it(land('s.w_valid', neg('s.w_fault'), 's.w_writes', eq(wd['rd'], b(5, i))),
                                 's.w_result', regs[i]) for i in range(1, 32)]
    x = isa.build_execute('s.x_ir', 's.x_pc', 's.x_operand1',
                          's.x_operand2', 's.x_fetch_fault', 'i.dmem_response', 'i.dmem_fault')
    xpc = it('s.w_valid', 's.w_next_pc', 's.pc')
    dpc = ['add', xpc, it('s.x_valid', b(32, 4), b(32, 0))]
    fpc = ['add', dpc, it('s.d_valid', b(32, 4), b(32, 0))]
    valid_w = conj([eq('s.w_pc', 's.pc'), eq('s.w_fault', w['trap']),
        eq('s.w_cause', w['cause']), eq('s.w_tval', w['trap_value']),
        eq('s.w_next_pc', w['next_pc']), eq('s.w_writes', w['rd_write']),
        implies(w['rd_write'], eq('s.w_result', w['rd_value'])),
        eq('s.w_store', land(w['dmem_write'], neg(w['trap']))),
        implies(lor(wd['load'], wd['store']), eq('s.w_address', w['dmem_address'])),
        implies('s.w_store', conj([eq('s.w_address', w['dmem_address']),
            eq('s.w_data', w['store_data']), eq('s.w_mask', w['store_mask'])]))])
    invariant = conj([eq('s.r0', b(32, 0)),
        *[eq(isa.ex(1, 0, 's.' + key), b(2, 0)) for key in ('pc', 'fetch_pc', 'd_pc', 'x_pc', 'w_pc', 'w_next_pc')],
        implies('s.halted', conj([neg('s.d_valid'), neg('s.x_valid'), neg('s.w_valid')])),
        implies(trapping_w, conj([neg('s.d_valid'), neg('s.x_valid'), neg('s.w_writes'), neg('s.w_store')])),
        implies(land('s.w_valid', 's.w_writes'), neg(eq(wd['rd'], b(5, 0)))),
        implies('s.w_valid', valid_w),
        implies('s.x_valid', conj([eq('s.x_pc', xpc),
            implies(xd['uses1'], eq('s.x_operand1', isa.reg_read(xd['rs1'], pending))),
            implies(xd['uses2'], eq('s.x_operand2', isa.reg_read(xd['rs2'], pending)))])),
        implies('s.d_valid', eq('s.d_pc', dpc)),
        implies(land(neg('s.halted'), neg(trapping_w)), eq('s.fetch_pc', fpc)),
        *[eq('o.' + k, 's.' + k) for k in ('pc', 'halted', *[f'r{i}' for i in range(32)])]])
    retire = land(active, 's.w_valid')
    commit = land(retire, neg(w['trap']))
    trap = land(retire, w['trap'])
    expectations = [eq('n.pc', it(retire, w['next_pc'], 's.pc')),
                    eq('n.halted', lor('s.halted', trap))]
    expectations.extend(eq(f'n.r{i}', it(land(commit, w['rd_write'], eq(wd['rd'], b(5, i))),
                            w['rd_value'], regs[i])) for i in range(32))
    # A conservative opcode-only source-use interlock may also stall illegal D
    # encodings; they still retire one illegal-instruction trap, never a NOP.
    dop = isa.ex(6, 0, 's.d_ir')
    uses1 = lor(*(eq(dop, b(7, n)) for n in (0x13, 0x33, 0x67, 3, 0x23, 0x63)))
    uses2 = lor(*(eq(dop, b(7, n)) for n in (0x33, 0x23, 0x63)))
    hazard = land('s.d_valid', 's.x_valid', x['rd_write'], xd['load'],
                  lor(land(uses1, eq(xd['rd'], dd['rs1'])), land(uses2, eq(xd['rd'], dd['rs2']))))
    # Redirects include a taken branch even when its target equals pc+4.
    branch_taken = lor(land(xd['beq'], eq('s.x_operand1', 's.x_operand2')),
        land(xd['bne'], neg(eq('s.x_operand1', 's.x_operand2'))),
        land(xd['blt'], ['slt', 's.x_operand1', 's.x_operand2']),
        land(xd['bge'], neg(['slt', 's.x_operand1', 's.x_operand2'])),
        land(xd['bltu'], ['ult', 's.x_operand1', 's.x_operand2']),
        land(xd['bgeu'], neg(['ult', 's.x_operand1', 's.x_operand2'])))
    redirect = land('s.x_valid', neg(x['trap']), lor(xd['jal'], xd['jalr'], branch_taken))
    kill = lor(redirect, land('s.x_valid', x['trap']))
    fetched = land(neg(hazard), neg(kill))
    transfer = {'w_valid': 's.x_valid', 'w_pc': 's.x_pc', 'w_ir': 's.x_ir',
        'x_valid': land('s.d_valid', neg(hazard), neg(kill)),
        'x_pc': 's.d_pc', 'x_ir': 's.d_ir', 'x_fetch_fault': 's.d_fetch_fault',
        'd_valid': it(kill, False, it(hazard, 's.d_valid', True)),
        'd_pc': it(fetched, 's.fetch_pc', 's.d_pc'),
        'd_ir': it(fetched, 'i.imem_response', 's.d_ir'),
        'd_fetch_fault': it(fetched, 'i.imem_fault', 's.d_fetch_fault'),
        'fetch_pc': it(redirect, x['next_pc'], it(fetched, ['add', 's.fetch_pc', b(32, 4)], 's.fetch_pc'))}
    for field, value in transfer.items():
        if field in ('w_valid', 'x_valid', 'd_valid'):
            expected = it(active, it(trapping_w, False, value), 's.' + field)
        else:
            expected = it(advance, value, 's.' + field)
        expectations.append(eq('n.' + field, expected))
    expectations += [implies(lor('i.stall', 's.halted'), eq('n.' + k, 's.' + k)) for k in cpu_state()]
    requests = {'commit': commit, 'retire': retire, 'trap_valid': trap,
        'trap_pc': 's.w_pc', 'trap_cause': 's.w_cause', 'trap_value': 's.w_tval',
        'imem_address': 's.fetch_pc',
        'imem_valid': land(active, neg(trapping_w), fetched),
        'dmem_valid': land(active, neg(trapping_w), 's.x_valid', x['dmem_valid']),
        'write_enable': land(commit, w['dmem_write'])}
    for name, value in requests.items():
        expectations.append(eq('n.obs_' + name, it(value, b(1, 1), b(1, 0)) if PORTS[name] == 'bool' else value))
    # Address/data pins are meaningful only for an accepted request.
    expectations.append(implies(land(active, neg(trapping_w), 's.x_valid', x['dmem_valid']),
        conj([eq('n.obs_dmem_address', x['dmem_address']),
              eq('n.obs_dmem_write', it(x['dmem_write'], b(1, 1), b(1, 0)))])))
    expectations.append(implies(land(commit, w['dmem_write']), conj([
        eq('n.obs_write_address', w['dmem_address']), eq('n.obs_write_data', w['store_data']),
        eq('n.obs_write_mask', w['store_mask'])])))
    state = machine['state']
    initial = conj(eq('s.' + k, c.zero(v)) for k, v in state.items())
    outputs = {k: state[k] for k in ('pc', 'halted', *[f'r{i}' for i in range(32)])}
    return c.scoped_document('RV32I precise ISA retirement and byte-memory ports', state, INPUTS,
                            outputs, initial, invariant, conj(expectations), machine)


def execution_contract(raw, instruction=None):
    """Universal X-stage semantic lemma; not the global pipeline refinement.

    Each instruction predicate covers every operand, immediate, PC and memory
    response/fault. The illegal class covers all remaining 32-bit words. Guards
    select proof cases; they are not runtime assumptions or NOP translations.
    """
    from audit.veryl_scaling import rv32i_spec as isa
    b, eq, it, land, neg = isa.b, isa.eq, isa.it, isa.land, isa.neg
    x = isa.build_execute('s.x_ir', 's.x_pc', 's.x_operand1', 's.x_operand2',
                          's.x_fetch_fault', 'i.dmem_response', 'i.dmem_fault')
    decoded = isa.decode('s.x_ir')
    cases = [k for k in decoded if k not in ('rd', 'rs1', 'rs2', 'legal', 'load', 'store', 'branch', 'uses1', 'uses2')]
    if instruction is not None and instruction not in (*cases, 'illegal'):
        raise ValueError('unknown instruction proof case: ' + instruction)
    case = True if instruction is None else neg(decoded['legal']) if instruction == 'illegal' else decoded[instruction]
    active = land(neg('i.stall'), neg('s.halted'), neg(land('s.w_valid', 's.w_fault')), 's.x_valid')
    # A combinational lemma quantifies the entire original prestate as inputs.
    # It observes ONLY these actual imported next/output expressions, avoiding
    # irrelevant register-file next-state cones in the execution-only query.
    selected = ('w_valid', 'w_pc', 'w_ir', 'w_fault', 'w_cause', 'w_tval',
                'w_next_pc', 'w_writes', 'w_result', 'w_store', 'w_address', 'w_data', 'w_mask')
    renames = {'s.' + k: 'i.pre_' + k for k in raw['state']}
    observed_ports = {k: PORTS[k] for k in ('dmem_valid', 'dmem_address', 'dmem_write')}
    state = {**{k: raw['state'][k] for k in selected},
             **{'obs_' + k: {'bv': 1} if v == 'bool' else v for k, v in observed_ports.items()}}
    machine = {'state': state, 'reset': {k: c.zero(v) for k, v in state.items()},
               'next': {k: c.rename(raw['next'][k], renames) for k in selected},
               'wires': c.rename(raw['wires'], renames)}
    for k in observed_ports:
        value = c.rename(raw['outputs'][k], renames)
        machine['next']['obs_' + k] = it(value, b(1, 1), b(1, 0)) if PORTS[k] == 'bool' else value
    live = set()
    def visit(value):
        if isinstance(value, str) and value.startswith('w.'):
            key = value[2:]
            if key not in live:
                live.add(key)
                visit(machine['wires'][key])
        elif isinstance(value, list):
            for item in value[1:]: visit(item)
    for value in machine['next'].values(): visit(value)
    machine['wires'] = {k: v for k, v in machine['wires'].items() if k in live}
    expectations = [eq('n.w_valid', True), eq('n.w_pc', 's.x_pc'), eq('n.w_ir', 's.x_ir'),
        eq('n.w_fault', x['trap']), eq('n.w_cause', x['cause']), eq('n.w_tval', x['trap_value']),
        eq('n.w_next_pc', x['next_pc']), eq('n.w_writes', x['rd_write']),
        c.implies(x['rd_write'], eq('n.w_result', x['rd_value'])),
        eq('n.w_store', land(x['dmem_write'], neg(x['trap']))),
        c.implies(land(x['dmem_write'], neg(x['trap'])), c.conj([
            eq('n.w_address', x['dmem_address']), eq('n.w_data', x['store_data']), eq('n.w_mask', x['store_mask'])])),
        eq('n.obs_dmem_valid', it(x['dmem_valid'], b(1, 1), b(1, 0))),
        c.implies(x['dmem_valid'], c.conj([eq('n.obs_dmem_address', x['dmem_address']),
            eq('n.obs_dmem_write', it(x['dmem_write'], b(1, 1), b(1, 0)))]))]
    initial = c.conj(eq('s.' + k, c.zero(v)) for k, v in state.items())
    inputs = {**INPUTS, **{'pre_' + k: v for k, v in raw['state'].items()}}
    operation = c.rename(c.implies(land(active, case), c.conj(expectations)), renames)
    return c.scoped_document('RV32I X execution: ' + (instruction or 'all encodings'), state, inputs,
        {}, initial, True, operation, machine)



def internal_contract(raw, internal, instruction=None, fields=None):
    """Exact, guarded combinational equations in the ORIGINAL imported DAG."""
    from audit.veryl_scaling import rv32i_spec as isa
    x = isa.build_execute('s.x_ir', 's.x_pc', 's.x_operand1', 's.x_operand2',
                         's.x_fetch_fault', 'i.dmem_response', 'i.dmem_fault')
    d = isa.decode('s.x_ir')
    terms = {
        'alu_result': (x['rd_write'], x['rd_value']), 'next_pc': (True, x['next_pc']),
        'fault': (True, x['trap']), 'cause': (True, x['cause']), 'tval': (True, x['trap_value']),
        'address': (isa.lor(d['load'], d['store']), x['dmem_address']),
        'store_data': (d['store'], x['store_data']), 'store_mask': (isa.land(d['store'], isa.neg(x['trap'])), x['store_mask']),
        'writes': (True, x['writes']), 'legal': (True, d['legal']),
        'taken': (d['legal'], x['redirect'])}
    case = True if instruction is None else isa.neg(d['legal']) if instruction == 'illegal' else d[instruction]
    rename = {'s.' + k: 'i.pre_' + k for k in raw['state']}
    fields = tuple(INTERNALS) if fields is None else tuple(fields)
    if not fields or any(k not in INTERNALS for k in fields):
        raise ValueError('unknown or empty internal equation set')
    state = {k: INTERNALS[k][1] for k in fields}
    machine = {'state': state, 'reset': {k: c.zero(ty) for k, ty in state.items()},
        'next': c.rename({k: internal[k] for k in fields}, rename), 'wires': c.rename(raw['wires'], rename)}
    live = set()
    def visit(value):
        if isinstance(value, str) and value.startswith('w.'):
            key = value[2:]
            if key not in live:
                live.add(key)
                visit(machine['wires'][key])
        elif isinstance(value, list):
            for item in value[1:]: visit(item)
    for value in machine['next'].values(): visit(value)
    machine['wires'] = {k: v for k, v in machine['wires'].items() if k in live}
    operation = c.conj(c.implies(isa.land(case, guard), isa.eq('n.' + name, value))
                      for name, (guard, value) in terms.items() if name in fields)
    initial = c.conj(isa.eq('s.' + k, c.zero(ty)) for k, ty in state.items())
    inputs = {**INPUTS, **{'pre_' + k: ty for k, ty in raw['state'].items()}}
    return c.scoped_document('RV32I guarded internal equation: ' + (instruction or 'all encodings'),
        state, inputs, {}, initial, True, c.rename(operation, rename), machine), terms



def control_contract(raw, fields=None):
    """Independent two-source forwarding, interlock, squash and retirement gating.

    This combinational transition lemma treats the prestate as arbitrary inputs.
    Forwarding from W uses its payload by the port protocol; X forwarding uses
    the independently decoded ISA value, not the DUT ALU result.
    """
    from audit.veryl_scaling import rv32i_spec as a
    b, eq, it, land, lor, neg = a.b, a.eq, a.it, a.land, a.lor, a.neg
    d, x, w = [a.decode('s.' + stage + '_ir') for stage in ('d', 'x', 'w')]
    execute = a.build_execute('s.x_ir', 's.x_pc', 's.x_operand1', 's.x_operand2',
                             's.x_fetch_fault', 'i.dmem_response', 'i.dmem_fault')
    dop = a.ex(6, 0, 's.d_ir')
    uses1 = lor(*(eq(dop, b(7, code)) for code in (0x13, 0x33, 0x67, 3, 0x23, 0x63)))
    uses2 = lor(*(eq(dop, b(7, code)) for code in (0x33, 0x23, 0x63)))
    hazard = land('s.d_valid', 's.x_valid', execute['rd_write'], x['load'],
        lor(land(uses1, eq(x['rd'], d['rs1'])), land(uses2, eq(x['rd'], d['rs2']))))
    redirect = land('s.x_valid', execute['redirect'], neg(execute['trap']))
    kill = lor(redirect, land('s.x_valid', execute['trap']))
    active = land(neg('i.stall'), neg('s.halted'))
    wtrap = land('s.w_valid', 's.w_fault')
    advance = land(active, neg(wtrap))
    expected = {}
    for number in (1, 2):
        selector = d['rs' + str(number)]
        # Unlike an ISA register read, this lemma includes arbitrary physical r0
        # prestate. The independently proved architectural invariant fixes r0=0.
        value = a.choose([(eq(selector, b(5, i)), 's.r' + str(i)) for i in range(31)], 's.r31')
        value = it(land('s.w_valid', 's.w_writes', neg('s.w_fault'), eq(w['rd'], selector)), 's.w_result', value)
        value = it(land('s.x_valid', execute['rd_write'], neg(x['load']), eq(x['rd'], selector)), execute['rd_value'], value)
        field = 'x_operand' + str(number)
        expected[field] = it(advance, value, 's.' + field)
    expected.update({
        'w_valid': it(active, it(wtrap, False, 's.x_valid'), 's.w_valid'),
        'x_valid': it(active, it(wtrap, False, land('s.d_valid', neg(hazard), neg(kill))), 's.x_valid'),
        'd_valid': it(active, it(wtrap, False, it(kill, False, it(hazard, 's.d_valid', True))), 's.d_valid'),
        'fetch_pc': it(advance, it(redirect, execute['next_pc'],
            it(land(neg(hazard), neg(kill)), ['add', 's.fetch_pc', b(32, 4)], 's.fetch_pc')), 's.fetch_pc')})
    ports = {'write_enable': land(active, 's.w_valid', neg('s.w_fault'), 's.w_store'),
        'commit': land(active, 's.w_valid', neg('s.w_fault')),
        'trap_valid': land(active, 's.w_valid', 's.w_fault'),
        'retire': land(active, 's.w_valid'),
        'imem_valid': land(advance, neg(hazard), neg(kill))}
    if fields is not None:
        selected = set(fields)
        if not selected or not selected <= (set(expected) | set(ports)):
            raise ValueError('unknown or empty control proof projection')
        expected = {k: value for k, value in expected.items() if k in selected}
        ports = {k: value for k, value in ports.items() if k in selected}
    renames = {'s.' + k: 'i.pre_' + k for k in raw['state']}
    state = {**{k: raw['state'][k] for k in expected}, **{'obs_' + k: 'bool' for k in ports}}
    nxt = {**{k: raw['next'][k] for k in expected}, **{'obs_' + k: raw['outputs'][k] for k in ports}}
    machine = {'state': state, 'reset': {k: c.zero(ty) for k, ty in state.items()},
               'next': c.rename(nxt, renames), 'wires': c.rename(raw['wires'], renames)}
    live = set()
    def visit(value):
        if isinstance(value, str) and value.startswith('w.'):
            key = value[2:]
            if key not in live:
                live.add(key)
                visit(machine['wires'][key])
        elif isinstance(value, list):
            for part in value[1:]: visit(part)
    for value in machine['next'].values(): visit(value)
    machine['wires'] = {k: value for k, value in machine['wires'].items() if k in live}
    operation = c.conj([*[eq('n.' + k, value) for k, value in expected.items()],
                        *[eq('n.obs_' + k, value) for k, value in ports.items()]])
    inputs = {**INPUTS, **{'pre_' + k: ty for k, ty in raw['state'].items()}}
    initial = c.conj(eq('s.' + k, c.zero(ty)) for k, ty in state.items())
    return c.scoped_document('RV32I forwarding, interlock and precise retirement gates', state, inputs,
                            {}, initial, True, c.rename(operation, renames), machine)

def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(',', ':')).encode()).hexdigest()


def validate_lemma(report, allow_conjunctive=False):
    """No examples are required for a universally quantified expression equality."""
    binding = report.get('implementation_binding', {})
    queries = binding.get('obligations', [])
    expected = {'binding_reset_nonempty': 'sat', 'binding_reset_establishes_product': 'unsat',
                'binding_product_preservation': 'unsat'}
    if (report.get('status') != 'binding_verified_no_examples' or binding.get('status') != 'verified'
            or len(queries) != len(expected) or {q.get('name') for q in queries} != set(expected)):
        raise ValueError('incomplete or unsuccessful guarded equality proof')
    for query in queries:
        wanted = expected[query['name']]
        if query.get('backend') == 'conjunctive_lemmas':
            if not allow_conjunctive or wanted != 'unsat':
                raise ValueError('conjunctive proof not enabled for this obligation')
            validate_conjunctive_obligation(query)
        elif (query.get('status') != 'passed' or query.get('solver_result') != wanted
                or query.get('backend') not in ('finite_bv', 'structural_kernel')
                or query.get('z3_seconds', 0) != 0):
            raise ValueError('invalid guarded equality verdict/backend')
        if wanted == 'sat' and query.get('finite', {}).get('original_formula_validated') is not True:
            raise ValueError('unvalidated nonemptiness witness')



def primitive_proof_queries(query):
    """All actual solver invocations; derived parents are not extra queries."""
    if isinstance(query.get('original_attempt'), dict):
        yield from primitive_proof_queries(query['original_attempt'])
    for key in ('children', 'conjunctive_children'):
        for child in query.get(key, []):
            yield from primitive_proof_queries(child)
    if query.get('backend') not in ('conjunctive_lemmas', 'checked_proof_bundle', 'checked_congruence'):
        yield query


def validate_proof_bundle(query):
    coverage, cost = query.get('coverage', {}), query.get('cost', {})
    children, graph = query.get('children', []), query.get('proof_graph', [])
    if (query.get('backend') != 'checked_proof_bundle' or query.get('status') != 'passed'
            or query.get('solver_result') != 'unsat' or query.get('logical_expectation') != 'unsat'
            or query.get('z3_seconds') != 0 or coverage.get('rule') != 'fresh-acyclic-sequent-bundle-v1'
            or coverage.get('complete') is not True or coverage.get('fresh_handles_only') is not True
            or coverage.get('saved_reports_are_authority') is not False
            or query.get('original_source_validation', {}).get('complete') is not True
            or query.get('original_source_validation', {}).get('includes_derived_context') is not True
            or not children or len(children) > 512 or len(graph) > 512):
        raise ValueError('incomplete, stale, or unproved sequent bundle')
    if (cost.get('mode') not in ('shared_query', 'independent_lemmas')
            or cost.get('per_query_work_limit') != 100_000_000
            or cost.get('per_query_clause_limit') != 1_000_000
            or cost.get('per_query_timeout_ms') != 10_000
            or cost.get('whole_bundle_is_one_query') is not (cost.get('mode') == 'shared_query')):
        raise ValueError('invalid sequent budget mode')
    for index, child in enumerate(children):
        if (child.get('name') != query['name'] + '_query_' + str(index).zfill(4)
                or child.get('backend') not in ('finite_bv', 'structural_kernel')
                or child.get('z3_seconds') != 0 or child.get('logical_expectation') != 'unsat'
                or type(child.get('emission_work')) is not int or child['emission_work'] < 0):
            raise ValueError('missing, reordered, or external sequent query')
        if child.get('solver_result') == 'sat':
            if child.get('status') != 'counterexample' or child.get('finite', {}).get('original_formula_validated') is not True:
                raise ValueError('unvalidated auxiliary counterexample')
        elif child.get('solver_result') != 'unsat' or child.get('status') != 'passed':
            raise ValueError('unproved sequent leaf')
        if child['backend'] == 'finite_bv':
            for key, limit in (('work', 100_000_000), ('clauses', 1_000_000)):
                value = child.get('finite', {}).get(key)
                if type(value) is not int or not 0 <= value <= limit:
                    raise ValueError('sequent query exceeds unchanged finite limits')
    work = sum(c.get('finite', {}).get('work', 0) + c.get('emission_work', 0) for c in children)
    clauses = sum(c.get('finite', {}).get('clauses', 0) for c in children)
    if (type(cost.get('work_including_validation_and_derived_steps')) is not int
            or cost['work_including_validation_and_derived_steps'] < work
            or type(cost.get('allocated_clauses')) is not int or cost.get('allocated_clauses') != clauses):
        raise ValueError('sequent aggregate cost mismatch')
    if cost['mode'] == 'shared_query' and (cost['work_including_validation_and_derived_steps'] > 100_000_000
            or clauses > 1_000_000 or query.get('seconds', float('inf')) >= 10
            or any(c['backend'] != 'finite_bv' for c in children)):
        raise ValueError('shared sequent bundle exceeds its single-query budget')
    attempt = query.get('original_attempt')
    if attempt is not None and (cost['mode'] != 'independent_lemmas'
            or attempt.get('status') != 'unknown' or attempt.get('solver_result') != 'unknown'
            or attempt.get('backend') != 'finite_bv' or attempt.get('z3_seconds') != 0):
        raise ValueError('invalid original sequent attempt')
    root = query.get('root')
    if root is None:
        replay = query.get('original_recheck', {})
        if (replay not in children or replay.get('status') != 'passed' or replay.get('solver_result') != 'unsat'
                or replay.get('proof_label') != 'original counterexample replay'):
            raise ValueError('missing original-query proof after rejected cut')
        return
    if type(root) is not int or not 0 <= root < len(graph) or coverage.get('exact_original_sequent') is not True:
        raise ValueError('missing exact original sequent root')
    arities = {'universal-typed-simultaneous-instantiation': 1, 'guard-preserving-projection': 1,
        'exact-antecedent-modus-ponens': 2, 'guarded-equality-with-original-fallback': 1,
        'exhaustive-guard-complement': 2}
    used_queries = set()
    for index, node in enumerate(graph):
        deps = node.get('dependencies')
        if node.get('id') != index or not isinstance(deps, list) or any(type(d) is not int or not 0 <= d < index for d in deps):
            raise ValueError('cyclic, missing, or reordered sequent dependency')
        if node.get('statement') != query['name'] + '_statement_' + str(index).zfill(4) + '.smt2':
            raise ValueError('stale sequent statement identity')
        rule = node.get('rule')
        if rule == 'fresh-solver-unsat':
            q = node.get('query_index')
            if deps or type(q) is not int or not 0 <= q < len(children) or q in used_queries or children[q].get('solver_result') != 'unsat':
                raise ValueError('missing fresh solver authority')
            used_queries.add(q)
        elif rule == 'exact-congruence-with-original-premise-retained':
            if not 2 <= len(deps) <= 33:
                raise ValueError('missing congruence dependencies')
        elif rule not in arities or len(deps) != arities[rule]:
            raise ValueError('unknown or incomplete sequent inference')


def validate_conjunctive_obligation(query):
    """Accept only the fresh checker's complete, strictly checked conjunction."""
    coverage, children, cost = query.get('coverage', {}), query.get('children', []), query.get('cost', {})
    expected_names = [query['name'] + '_lemma_' + format(i, '04d') for i in range(len(children))]
    if (query.get('status') != 'passed' or query.get('solver_result') != 'unsat'
            or not children or len(children) > 4096
            or coverage.get('rule') != 'exact-conjunction-introduction-v1'
            or coverage.get('ordered_names') != expected_names
            or [child.get('name') for child in children] != expected_names
            or coverage.get('complete') is not True or coverage.get('same_full_precondition') is not True
            or coverage.get('postconditions_used_as_assumptions') is not False
            or query.get('original_source_validation', {}).get('includes_derived_context') is not True
            or query.get('original_attempt', {}).get('status') != 'unknown'
            or query.get('original_attempt', {}).get('solver_result') != 'unknown'
            or query.get('original_attempt', {}).get('backend') != 'finite_bv'
            or query.get('original_attempt', {}).get('z3_seconds', 0) != 0
            or query.get('z3_seconds', 0) != 0):
        raise ValueError('incomplete/stale/failed conjunction coverage')
    for child in children:
        if child.get('backend') == 'checked_proof_bundle':
            validate_proof_bundle(child)
            continue
        if (child.get('status') != 'passed' or child.get('solver_result') != 'unsat'
                or child.get('backend') not in ('finite_bv', 'structural_kernel')
                or child.get('z3_seconds', 0) != 0 or child.get('logical_expectation') != 'unsat'):
            raise ValueError('conjunction has unknown/counterexample/external child')
        if child.get('backend') == 'finite_bv':
            finite = child.get('finite', {})
            for name, limit in (('work', 100_000_000), ('clauses', 1_000_000)):
                value = finite.get(name)
                if type(value) is not int or not 0 <= value <= limit:
                    raise ValueError('successful conjunction child exceeds budget or has invalid cost')
    if (cost.get('per_lemma_work_limit') != 100_000_000 or cost.get('per_lemma_clause_limit') != 1_000_000
            or cost.get('per_lemma_timeout_ms') != 10_000
            or type(cost.get('total_child_work')) is not int or cost.get('total_child_work') < 0
            or cost.get('total_child_work') != sum(leaf.get('finite', {}).get('work', 0) for child in children for leaf in primitive_proof_queries(child))
            or cost.get('original_monolithic_result') != 'unknown'
            or cost.get('whole_bundle_budget_is_not_a_single_query_budget') is not True):
        raise ValueError('conjunction budget/cost accounting mismatch')

class NormalizationSession:
    """Sealed exact-equation rewrite ledger; saved reports cannot mint handles.

    Every equation is proved in the original imported wire environment. New
    expressions mention only original prestate/inputs, never other equations or
    proof handles. A guard is retained in the exact-root replacement. Rewriting
    downstream uses of several already-equal roots is equality congruence.
    """
    GLOBAL_FIELDS = ('next_pc', 'fault', 'cause', 'address', 'store_data', 'writes', 'legal', 'taken')
    CASE_FIELDS = ('alu_result', 'tval', 'store_mask')

    def __init__(self, out, checker=None, frontend=None, lifter=None):
        self.out = Path(out)
        self.out.mkdir(parents=True, exist_ok=False)
        self.checker = Path(checker or ROOT / 'target/release/hwverify-rs').resolve()
        self.frontend = Path(frontend or ROOT / 'conformance/veryl-proof/target/debug/veryl-proof-frontend').resolve()
        self.lifter = Path(lifter or ROOT / 'target/release/hwverify-sir-lift').resolve()
        self._handles = {}
        self._source = source()
        before = {str(path.resolve()): hashlib.sha256(path.read_bytes()).hexdigest()
                  for path in (self.checker, self.frontend, self.lifter, Path(__file__),
                      Path(c.__file__), ROOT / 'audit/veryl_scaling/rv32i_spec.py',
                      ROOT / 'conformance/veryl-symbolic/rv32i_pipeline.veryl')}
        self._machine = compile_machine(self.out / 'import', self.frontend, self.lifter, self._source)
        if source() != self._source or (self.out / 'import/source.veryl').read_text() != self._source:
            raise ValueError('source changed while importing')
        design = json.loads((self.out / 'import/design.json').read_text())
        if design.get('sources') != [{'path': 'source.veryl', 'text': self._source}]:
            raise ValueError('import design source does not match captured source')
        for path, fingerprint in before.items():
            if hashlib.sha256(Path(path).read_bytes()).hexdigest() != fingerprint:
                raise ValueError('source or tool changed while importing: ' + path)
        self._internal = json.loads((self.out / 'import/internal.json').read_text())
        self._original_hash = digest(self._machine)
        self._internal_hash = digest(self._internal)
        paths = [self.checker, self.frontend, self.lifter, Path(__file__),
                 Path(c.__file__), ROOT / 'audit/veryl_scaling/rv32i_spec.py',
                 ROOT / 'conformance/veryl-symbolic/rv32i_pipeline.veryl',
                 *[p for p in (self.out / 'import').iterdir() if p.is_file()]]
        self._files = {str(p.resolve()): hashlib.sha256(p.read_bytes()).hexdigest() for p in paths}
        from audit.veryl_scaling import rv32i_spec as isa
        self._cases = tuple(k for k in isa.decode('s.x_ir') if k not in
            ('rd', 'rs1', 'rs2', 'legal', 'load', 'store', 'branch', 'uses1', 'uses2')) + ('illegal',)
        if len(self._cases) != 41 or len(set(self._cases)) != 41:
            raise ValueError('instruction partition incomplete')
        self._terms = internal_contract(self._machine, self._internal)[1]
        if any(c.RUNNER.references(value, prefix) for pair in self._terms.values()
               for value in pair for prefix in ('w.', 'n.', 'o.', 'impl.', 'spec.')):
            raise ValueError('canonical equations depend on derived or poststate values')
        self._terms_hash = digest(self._terms)

    def _check(self):
        if digest(self._machine) != self._original_hash or digest(self._internal) != self._internal_hash:
            raise ValueError('imported machine or internal root changed')
        if digest(self._terms) != self._terms_hash:
            raise ValueError('guarded equality changed')
        for path, value in self._files.items():
            if hashlib.sha256(Path(path).read_bytes()).hexdigest() != value:
                raise ValueError('proof/import/source tool changed during session: ' + path)

    def prove(self, fields, instruction=None):
        self._check()
        fields = tuple(fields)
        if fields == self.CASE_FIELDS and instruction in self._cases:
            label = 'case-' + instruction
        elif len(fields) == 1 and fields[0] in self.GLOBAL_FIELDS and instruction is None:
            label = 'field-' + fields[0]
        else:
            raise ValueError('unrecognized guarded-equation proof partition')
        out = self.out / label
        out.mkdir(exist_ok=False)
        doc, terms = internal_contract(self._machine, self._internal, instruction, fields)
        (out / 'contract.json').write_text(json.dumps(doc, separators=(',', ':')))
        report, seconds = c.execute([self.checker, out / 'contract.json', '--out', out / 'proof'],
                                    out / 'report.json', (0, 1, 3))
        validate_lemma(report)
        self._check()
        if json.loads((out / 'contract.json').read_text()) != doc or digest(terms) != self._terms_hash:
            raise ValueError('proof artifact or equation changed during solving')
        handle = object()
        self._handles[handle] = {'label': label, 'fields': fields, 'instruction': instruction,
            'source_artifacts': copy.deepcopy(self._files), 'machine_sha256': self._original_hash,
            'internal_sha256': self._internal_hash, 'terms_sha256': self._terms_hash,
            'document_sha256': digest(doc), 'report_sha256': digest(report), 'seconds': seconds}
        (out / 'session-record.json').write_text(json.dumps(self._handles[handle], indent=2))
        return handle

    def normalize(self, handles):
        self._check()
        records = []
        for handle in handles:
            try: record = self._handles[handle]
            except (KeyError, TypeError):
                raise ValueError('no guarded-equation handle in this session') from None
            records.append(record)
        labels = [r['label'] for r in records]
        expected = {'field-' + field for field in self.GLOBAL_FIELDS} | {'case-' + case for case in self._cases}
        if len(labels) != len(set(labels)) or set(labels) != expected:
            raise ValueError('missing, duplicated or unexpected normalization lemma')
        for record in records:
            if (record['source_artifacts'] != self._files or record['machine_sha256'] != self._original_hash
                    or record['internal_sha256'] != self._internal_hash or record['terms_sha256'] != self._terms_hash):
                raise ValueError('stale guarded-equation provenance')
            doc, _ = internal_contract(self._machine, self._internal, record['instruction'], record['fields'])
            if digest(doc) != record['document_sha256']:
                raise ValueError('guarded-equation document changed')
        # The 40 legal predicates OR their explicit complement form a tautology.
        # CASE_FIELDS retain their individual guards after case union. No lemma
        # that was proved under rd_write/store conditions becomes unconditional.
        from audit.veryl_scaling import rv32i_spec as isa
        d = isa.decode('s.x_ir')
        legal_cases = isa.lor(*(d[k] for k in self._cases if k != 'illegal'))
        if d['legal'] != legal_cases:
            raise ValueError('instruction partition does not match exact legality predicate')
        machine = copy.deepcopy(self._machine)
        roots = [v for v in self._internal.values()]
        if len(set(roots)) != len(roots) or any(not isinstance(v, str) or not v.startswith('w.') for v in roots):
            raise ValueError('normalization requires distinct exact wire roots')
        interned = {}
        def share(value):
            if not isinstance(value, list): return value
            expr = [value[0], *[share(item) for item in value[1:]]]
            if value[0] == 'bv': return expr
            key = json.dumps(expr, separators=(',', ':'))
            if key not in interned:
                name = 'rv32_canonical_' + str(len(interned))
                if name in machine['wires']: raise ValueError('normalization wire collision')
                machine['wires'][name] = expr
                interned[key] = 'w.' + name
            return interned[key]
        for name, root in self._internal.items():
            guard, value = self._terms[name]
            old = copy.deepcopy(machine['wires'][root[2:]])
            machine['wires'][root[2:]] = share(isa.it(guard, value, old))
        record = {'rule': 'exact-guarded-internal-expression-rewrite-v1',
            'original_machine_sha256': self._original_hash, 'normalized_machine_sha256': digest(machine),
            'guards_preserved': True, 'instruction_partition': list(self._cases),
            'dependencies': copy.deepcopy(records), 'saved_records_are_certificates': False}
        (self.out / 'normalized-machine.json').write_text(json.dumps(machine, separators=(',', ':')))
        (self.out / 'normalization-record.json').write_text(json.dumps(record, indent=2))
        return machine, record

    def run(self):
        handles = [self.prove((field,)) for field in self.GLOBAL_FIELDS]
        handles += [self.prove(self.CASE_FIELDS, case) for case in self._cases]
        return self.normalize(handles)

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    machine = compile_machine(args.out)
    print(json.dumps({'state_fields': len(machine['state']), 'ports': len(machine['outputs']),
                      'source': str(args.out / 'source.veryl')}))



CPU_MUTATIONS = {
    'wrong_add': ("3'd0: result = x_operand1 + rhs;", "3'd0: result = x_operand1 + rhs + 32'd1;", 'addi', ('alu_result',)),
    'wrong_sub': ("result = x_operand1 - rhs;", "result = x_operand1 + rhs;", 'sub', ('alu_result',)),
    'wrong_shift_bits': ('shift = rhs[4:0];', 'shift = rhs[5:1];', 'sll', ('alu_result',)),
    'writable_x0_decode': ("&& x_rd != 5'd0;", "&& 1'b1;", 'addi', ('writes',)),
    'jalr_clears_bit1': ("target = (x_operand1 + imm_i) & 32'hfffffffe;", "target = (x_operand1 + imm_i) & 32'hfffffffc;", 'jalr', ('next_pc', 'fault')),
    'wrong_byte_lane': ("store_mask = 4'd1 << address[1:0];", "store_mask = 4'd1;", 'sb', ('store_mask',)),
    'unsigned_load_sign': ("if f3 == 3'd0 && shifted_load[7]", "if shifted_load[7]", 'lbu', ('alu_result',)),
    'load_x0_ignores_access_fault': ("else if (is_load || is_store) && dmem_fault", "else if (is_load || is_store) && dmem_fault && x_rd != 5'd0", 'lw', ('fault',)),
    'untaken_branch_fault': ("else if taken && target[1:0] != 2'd0", "else if (taken || is_branch) && target[1:0] != 2'd0", 'beq', ('fault',)),
    'accept_mul_encoding': ("7'h33: legal = f7 == 7'h00 ||", "7'h33: legal = f7 == 7'h01 || f7 == 7'h00 ||", 'illegal', ('legal',)),
}


PIPELINE_MUTATIONS = {
    'no_rs1_x_forward': ('if x_valid && writes && !is_load && !fault && x_rd == d_rs1 { operand1 = result; }', "if 1'b0 { operand1 = result; }"),
    'no_rs2_x_forward': ('if x_valid && writes && !is_load && !fault && x_rd == d_rs2 { operand2 = result; }', "if 1'b0 { operand2 = result; }"),
    'no_rs1_w_forward': ('if w_valid && w_writes && !w_fault && w_rd == d_rs1 { operand1 = w_result; }', "if 1'b0 { operand1 = w_result; }"),
    'no_rs2_w_forward': ('if w_valid && w_writes && !w_fault && w_rd == d_rs2 { operand2 = w_result; }', "if 1'b0 { operand2 = w_result; }"),
    'no_interlock': ('hazard = d_valid && x_valid && writes && is_load && !fault', "hazard = 1'b0 && d_valid && x_valid && writes && is_load && !fault"),
    'no_younger_squash': ('x_valid = d_valid && !hazard && !redirect && !(x_valid && fault);', 'x_valid = d_valid && !hazard;'),
    'ungated_store': ('write_enable = commit && w_store;', 'write_enable = w_store;'),
    'commit_during_stall': ('commit = rst_n && !stall && !halted && w_valid && !w_fault;', 'commit = rst_n && !halted && w_valid && !w_fault;'),
    'trap_during_stall': ('trap_valid = rst_n && !stall && !halted && w_valid && w_fault;', 'trap_valid = rst_n && !halted && w_valid && w_fault;'),
}

def validate_negative_lemma(report):
    binding = report.get('implementation_binding', {})
    queries = binding.get('obligations', [])
    expected = {'binding_reset_nonempty': 'sat', 'binding_reset_establishes_product': 'unsat',
                'binding_product_preservation': 'sat'}
    if (report.get('status') != 'implementation_binding_failed' or binding.get('status') != 'failed'
            or len(queries) != 3 or {q.get('name') for q in queries} != set(expected)):
        raise ValueError('mutation did not yield the required original-formula counterexample')
    for query in queries:
        if query.get('solver_result') != expected[query['name']] or query.get('backend') not in ('finite_bv', 'structural_kernel'):
            raise ValueError('mutation has missing/unknown/unsupported obligation')
        status = 'counterexample' if query['name'] == 'binding_product_preservation' else 'passed'
        if query.get('status') != status:
            raise ValueError('mutation verdict/status mismatch')
        if expected[query['name']] == 'sat' and query.get('finite', {}).get('original_formula_validated') is not True:
            raise ValueError('mutation SAT witness was not replayed on the original formula')



def _mutation_fingerprints(checker, frontend, lifter):
    paths = [Path(checker), Path(frontend or ROOT / 'conformance/veryl-proof/target/debug/veryl-proof-frontend'),
             Path(lifter or ROOT / 'target/release/hwverify-sir-lift'), Path(__file__), Path(c.__file__),
             ROOT / 'audit/veryl_scaling/rv32i_spec.py', ROOT / 'conformance/veryl-symbolic/rv32i_pipeline.veryl']
    return {str(path.resolve()): hashlib.sha256(path.read_bytes()).hexdigest() for path in paths}


def _check_mutation_fingerprints(fingerprints):
    for path, expected in fingerprints.items():
        if hashlib.sha256(Path(path).read_bytes()).hexdigest() != expected:
            raise ValueError('mutation source/reference/tool changed during audit: ' + path)

def check_execution_mutations(out, checker, frontend=None, lifter=None):
    """Compile real source faults and demand validated finite counterexamples."""
    out = Path(out)
    out.mkdir(parents=True, exist_ok=False)
    results = []
    fingerprints = _mutation_fingerprints(checker, frontend, lifter)
    for name, (before, after, instruction, fields) in CPU_MUTATIONS.items():
        _check_mutation_fingerprints(fingerprints)
        text = source()
        if text.count(before) != 1:
            raise ValueError('mutation site is not unique: ' + name)
        imported = out / (name + '-import')
        raw = compile_machine(imported, frontend, lifter, text.replace(before, after))
        internal = json.loads((imported / 'internal.json').read_text())
        doc, _ = internal_contract(raw, internal, instruction, fields)
        folder = out / name
        folder.mkdir()
        (folder / 'contract.json').write_text(json.dumps(doc, separators=(',', ':')))
        report, seconds = c.execute([checker, folder / 'contract.json', '--out', folder / 'proof'],
                                    folder / 'report.json', (0, 1, 3))
        validate_negative_lemma(report)
        _check_mutation_fingerprints(fingerprints)
        results.append({'fault': name, 'source_and_tool_sha256': fingerprints,
                        'mutated_source_sha256': hashlib.sha256((imported / 'source.veryl').read_bytes()).hexdigest(), 'instruction': instruction, 'fields': list(fields),
                        'document_sha256': digest(doc), 'report_sha256': digest(report), 'seconds': seconds,
                        'status': 'validated_original_formula_counterexample'})
    (out / 'summary.json').write_text(json.dumps(results, indent=2))
    return results


def check_pipeline_mutations(out, checker, frontend=None, lifter=None):
    out = Path(out)
    out.mkdir(parents=True, exist_ok=False)
    results = []
    fingerprints = _mutation_fingerprints(checker, frontend, lifter)
    for name, (before, after) in PIPELINE_MUTATIONS.items():
        _check_mutation_fingerprints(fingerprints)
        text = source()
        if text.count(before) != 1:
            raise ValueError('pipeline mutation site is not unique: ' + name)
        imported = out / (name + '-import')
        raw = compile_machine(imported, frontend, lifter, text.replace(before, after))
        field = ('x_operand1' if 'rs1' in name else 'x_operand2' if 'rs2' in name else
                 'write_enable' if name == 'ungated_store' else 'commit' if name == 'commit_during_stall' else
                 'trap_valid' if name == 'trap_during_stall' else 'x_valid')
        doc = control_contract(raw, (field,))
        folder = out / name
        folder.mkdir()
        (folder / 'contract.json').write_text(json.dumps(doc, separators=(',', ':')))
        report, seconds = c.execute([checker, folder / 'contract.json', '--out', folder / 'proof', '--finite-search-hint', 'sat'],
                                    folder / 'report.json', (0, 1, 3))
        validate_negative_lemma(report)
        _check_mutation_fingerprints(fingerprints)
        results.append({'fault': name, 'source_and_tool_sha256': fingerprints,
                        'mutated_source_sha256': hashlib.sha256((imported / 'source.veryl').read_bytes()).hexdigest(), 'document_sha256': digest(doc), 'report_sha256': digest(report),
                        'seconds': seconds, 'status': 'validated_original_formula_counterexample'})
    (out / 'summary.json').write_text(json.dumps(results, indent=2))
    return results


if __name__ == '__main__':
    main()
