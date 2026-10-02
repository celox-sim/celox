#!/usr/bin/env python3
"""Finite-only ISA retirement / read-only memory interface contracts.

A fixed six-bit address space isolates backing-capacity costs. Unmapped reads
return zero. The CPU contract has arbitrary responses and NO memory words.
Concrete composition is a separate, capacity-dependent architectural proof.
"""
import argparse
import copy
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT))

from audit.veryl_scaling import cpu_registers as registers
from audit.veryl_scaling.cpu_registers import b, eq, it, add, ex, land, lor, neg, implies

RUNNER = registers.runner
GPRS, RB, WIDTH, IW, AB = 8, 3, 32, 41, 6
PORTS = {'imem_address': {'bv': AB}, 'dmem_address': {'bv': AB},
         'imem_valid': 'bool', 'dmem_valid': 'bool', 'commit': 'bool'}
CPU_INPUTS = {'rst': 'bool', 'stall': 'bool', 'imem_response': {'bv': IW},
              'dmem_response': {'bv': WIDTH}}
CPU_FAULTS = {
    'fetch_address': ('imem_address = fetch_pc;', "imem_address = fetch_pc + 6'd1;"),
    'load_address': ('dmem_address = x_ir[5:0];', 'dmem_address = d_ir[5:0];'),
    'high_address_drop': ('imem_address = fetch_pc;', "imem_address = fetch_pc & 6'd3;"),
    'fetch_response': ('fetched = imem_response;', "fetched = imem_response ^ 41'd1;"),
    'load_response': ('result = dmem_response;', "result = dmem_response ^ 32'd1;"),
    'stale_load': ('result = dmem_response;', 'result = w_result;'),
    'load_enable': ("dmem_valid = !stall && x_valid && x_op == 3'd3;", "dmem_valid = !stall && d_valid && d_op == 3'd3;"),
    'no_flush': ('x_valid = d_valid && !hazard && !taken;', 'x_valid = d_valid && !hazard;'),
    'no_interlock': ("&& x_match && x_op == 3'd3;", "&& x_match && 1'b0;"),
    'no_forward': ("if x_match && x_op != 3'd3 { operand = result; }", "if 1'b0 { operand = result; }"),
}
COMPOSITION_FAULTS = ('load_address_association', 'load_response_corruption', 'fetch_address_high_drop')
MEMORY_FAULTS = ('wrong_address', 'high_address_drop', 'wrong_value', 'recapture', 'wrong_reset')


def conj(items):
    items = list(items)
    return registers.balanced_and(items) if items else True


def zero(ty):
    return False if ty == 'bool' else b(ty['bv'], 0)


def op(ir): return ex(40, 38, ir)
def rd(ir): return ex(37, 35, ir)
def rs(ir): return ex(34, 32, ir)
def imm(ir): return ex(31, 0, ir)
def address(ir): return ex(5, 0, ir)
def isop(ir, code): return eq(op(ir), b(3, code))
def writes(ir): return eq(ex(40, 40, ir), b(1, 0))
def uses(ir): return lor(lor(isop(ir, 1), isop(ir, 2)), isop(ir, 4))


def regread(selector, values):
    result = values[-1]
    for index in reversed(range(GPRS - 1)):
        result = it(eq(selector, b(RB, index)), values[index], result)
    return result


def isa_value(ir, regs, load):
    value = regread(rs(ir), regs)
    return it(isop(ir, 0), imm(ir), it(isop(ir, 1), add(value, imm(ir)),
              it(isop(ir, 2), ['bxor', value, imm(ir)], load)))


def isa_pc(ir, pc, regs):
    return it(land(isop(ir, 4), eq(regread(rs(ir), regs), b(WIDTH, 0))),
              address(ir), add(pc, b(AB, 1)))


def lookup(kind, count, addr, prefix='s.'):
    value = b(IW if kind == 'rom' else WIDTH, 0)
    for index in reversed(range(count)):
        value = it(eq(addr, b(AB, index)), prefix + kind + str(index), value)
    return value


def cpu_state():
    state = {'pc': {'bv': AB}, **{f'r{i}': {'bv': WIDTH} for i in range(GPRS)},
             'fetch_pc': {'bv': AB}}
    for stage in ('d', 'x', 'w'):
        state.update({stage + '_valid': 'bool', stage + '_pc': {'bv': AB},
                      stage + '_ir': {'bv': IW}})
    state.update({'x_operand': {'bv': WIDTH}, 'w_result': {'bv': WIDTH},
                  'w_next_pc': {'bv': AB}})
    return state


def source(fault=None):
    text = (ROOT / 'conformance/veryl-symbolic/pipeline_memory_ports.veryl').read_text()
    if fault:
        old, new = CPU_FAULTS[fault]
        if text.count(old) != 1:
            raise ValueError('mutation site is not unique: ' + fault)
        text = text.replace(old, new)
    return text


def memory_source(count, fault=None):
    if count not in (4, 16, 64):
        raise ValueError('supported backing capacities: 4, 16, 64')
    declarations, resets, reads = [], [], []
    for kind, width, port in [('rom', IW, 'imem'), ('data', WIDTH, 'dmem')]:
        for i in range(count):
            declarations += [f'seed_{kind}{i}: input bit<{width}>,', f'{kind}{i}: output bit<{width}>,']
            resets += [f'{kind}{i} = seed_{kind}{i};']
        selector = port + '_address'
        if fault == 'wrong_address' and kind == 'data': selector = 'imem_address'
        if fault == 'high_address_drop' and kind == 'data': selector += " & 6'd3"
        cases = [f"6'd{i}: {port}_response = {kind}{i};" for i in range(count)]
        cases += [f"default: {port}_response = '0;"]
        reads += ['case ' + selector + ' { ' + ' '.join(cases) + ' }']
    if fault == 'wrong_value':
        reads += ["dmem_response = dmem_response ^ 32'd1;"]
    reset = ' '.join(resets)
    if fault == 'wrong_reset': reset = reset.replace('data0 = seed_data0;', 'data0 = seed_data1;')
    extra = 'else { data0 = seed_data0; }' if fault == 'recapture' else ''
    return '''// Concrete immutable memory: capture seeds on reset; total combinational reads.
module ReadMemory (
    clk: input clock, rst: input bit,
    imem_address: input bit<6>, dmem_address: input bit<6>,
    imem_response: output bit<41>, dmem_response: output bit<32>,
    ''' + '\n    '.join(declarations) + '''
) {
    always_comb { ''' + ' '.join(reads) + ''' }
    always_ff (clk) { if rst { ''' + reset + ' } ' + extra + ''' }
}
'''


def immutable(count):
    return {**{f'rom{i}': {'bv': IW} for i in range(count)},
            **{f'data{i}': {'bv': WIDTH} for i in range(count)}}


def scoped_document(name, state, inputs, outputs, initial, invariant, operation, implementation,
                    examples=None):
    """One always-active tick contract; no environmental assumptions are inserted."""
    leaf = {'inputs': inputs, 'outputs': outputs, 'state': state,
            'init': initial, 'invariant': invariant, 'operations': {'tick': operation},
            'examples': examples or {}}
    implementation = copy.deepcopy(implementation)
    implementation.pop('outputs', None)
    implementation.update(inputs=inputs, reset_input='rst', composition='System', operations={'tick': True},
        binding={'states': {'contract': {k: 's.' + k for k in state}},
                 'outputs': {k: 's.' + k for k in outputs}})
    return {'version': 4, 'kind': 'specification', 'name': name, 'specs': {'Contract': leaf},
            'compositions': {'System': {'inputs': inputs, 'outputs': outputs,
                'instances': {'contract': {'target': 'Contract',
                    'connections': {k: k for k in list(inputs) + list(outputs)}}}, 'examples': {}}},
            'implementation': implementation}


def with_observers(machine, observations, ghost_next=None, ghost_reset=None):
    """Read-only deterministic harness: samples ACTUAL imported expressions.

    No DUT expression may read a harness field. No monitor feeds back to the DUT.
    Every sampled output is updated even on stall, so port errors cannot hide.
    """
    machine = copy.deepcopy(machine)
    if 'obs_' in json.dumps(machine):
        raise ValueError('DUT illegally depends on observer namespace')
    for field, (ty, expr) in observations.items():
        machine['state'][field] = ty
        machine['next'][field] = expr
        machine['reset'][field] = zero(ty)
    for field, (ty, expr) in (ghost_next or {}).items():
        machine['state'][field] = ty
        machine['next'][field] = expr
        machine['reset'][field] = (ghost_reset or {}).get(field, zero(ty))
    return machine


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
            implies(writes('s.w_ir'), eq('s.w_result', isa_value('s.w_ir', regs, 's.obs_load')))])),
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
                'dmem_valid': land(neg('i.stall'), 's.x_valid', isop('s.x_ir', 3)), 'commit': retire}
    expectations += [eq('n.obs_' + k, it(v, b(1, 1), b(1, 0)) if PORTS[k] == 'bool' else v)
                     for k, v in requests.items()]
    initial = conj(eq('s.' + k, zero(v)) for k, v in state.items())
    outputs = {k: state[k] for k in ['pc', *[f'r{i}' for i in range(GPRS)]]}
    return scoped_document('CPU ISA retirement and port provenance (arbitrary responses)',
        state, CPU_INPUTS, outputs, initial, invariant, conj(expectations), machine, cpu_examples())


def cpu_examples():
    def insn(opcode, dest=0, src=0, value=0):
        return b(IW, (opcode << 38) | (dest << 35) | (src << 32) | value)
    # Each incoming response is unconstrained except this example's explicit
    # trace. At tick four, the first fetched instruction retires.
    trace = [{'operation': 'tick', 'inputs': {'rst': False, 'stall': False,
              'imem_response': insn(0, value=7) if i == 0 else insn(7),
              'dmem_response': b(WIDTH, 99)}, 'observe': {}} for i in range(4)]
    trace[-1]['observe'] = {'pc': b(AB, 1), 'r0': b(WIDTH, 7)}
    bad = copy.deepcopy(trace)
    bad[-1]['observe']['r0'] = b(WIDTH, 8)
    load = [{'operation': 'tick', 'inputs': {'rst': False, 'stall': False,
              'imem_response': insn(3, dest=1, value=63) if i == 0 else insn(7),
              'dmem_response': b(WIDTH, 55 if i == 2 else 99)}, 'observe': {}}
            for i in range(4)]
    load[-1]['observe'] = {'r1': b(WIDTH, 55)}
    wrong_cycle = copy.deepcopy(load)
    wrong_cycle[-1]['observe']['r1'] = b(WIDTH, 99)
    return {'retires_fetched_movi': {'expect': 'positive', 'initial': {}, 'trace': trace},
            'rejects_wrong_retired_value': {'expect': 'negative', 'initial': {}, 'trace': bad},
            'retires_historical_load': {'expect': 'positive', 'initial': {}, 'trace': load},
            'rejects_current_cycle_load': {'expect': 'negative', 'initial': {}, 'trace': wrong_cycle}}


def memory_contract(count, machine):
    observations = {'obs_' + k: ({'bv': IW if k == 'imem_response' else WIDTH}, v)
                    for k, v in machine['outputs'].items()}
    ghosts = {'obs_frozen_' + k: (ty, 's.obs_frozen_' + k) for k, ty in immutable(count).items()}
    resets = {'obs_frozen_' + k: 'i.seed_' + k for k in immutable(count)}
    machine = with_observers(machine, observations, ghosts, resets)
    state = machine['state']
    inputs = {'rst': 'bool', 'imem_address': {'bv': AB}, 'dmem_address': {'bv': AB},
              **{'seed_' + k: ty for k, ty in immutable(count).items()}}
    inv = conj([*[eq('s.' + k, 's.obs_frozen_' + k) for k in immutable(count)],
                eq('o.obs_imem_response', 's.obs_imem_response'),
                eq('o.obs_dmem_response', 's.obs_dmem_response')])
    # Reset capture is checked by equality to independently reset observer cells.
    initial = conj([eq('s.obs_imem_response', b(IW, 0)), eq('s.obs_dmem_response', b(WIDTH, 0))])
    step = conj([*[eq('n.' + k, 's.' + k) for k in state if not k.endswith('_response')],
        eq('n.obs_imem_response', lookup('rom', count, 'i.imem_address', 's.obs_frozen_')),
        eq('n.obs_dmem_response', lookup('data', count, 'i.dmem_address', 's.obs_frozen_'))])
    outputs = {k: state[k] for k in observations}
    return scoped_document(f'Immutable memory {count} backing words per port', state, inputs,
        outputs, initial, inv, step, machine, memory_examples(count))


def memory_examples(count):
    # Finite examples complement universal binding: same-address reads are
    # consistent even if seed inputs change; all unmapped addresses return zero.
    trace = [{'operation': 'tick', 'inputs': {'imem_address': b(AB, 0), 'dmem_address': b(AB, 0)},
              'observe': {'obs_dmem_response': b(WIDTH, 7)}} for _ in range(2)]
    for i, step in enumerate(trace):
        step['inputs'].update({'seed_data0': b(WIDTH, 100 + i), 'seed_rom0': b(IW, 200 + i)})
    bad = copy.deepcopy(trace)
    bad[-1]['observe']['obs_dmem_response'] = b(WIDTH, 8)
    examples = {'consistent_repeat': {'expect': 'positive', 'initial': {}, 'trace': trace},
        'reject_inconsistent_repeat': {'expect': 'negative', 'initial': {}, 'trace': bad}}
    if count < 64:
        for value, expected in [(0, 'positive'), (1, 'negative')]:
            examples['unmapped_' + expected] = {'expect': expected, 'initial': {}, 'trace': [
                {'operation': 'tick', 'inputs': {'imem_address': b(AB, 63), 'dmem_address': b(AB, 63)},
                 'observe': {'obs_imem_response': b(IW, value), 'obs_dmem_response': b(WIDTH, value)}}]}
    return examples


def rename(expr, mapping):
    if isinstance(expr, str): return mapping.get(expr, expr)
    if isinstance(expr, list): return [rename(x, mapping) for x in expr]
    if isinstance(expr, dict): return {k: rename(v, mapping) for k, v in expr.items()}
    return expr


def architectural_composition(count, cpu, memory, fault=None):
    """Wire actual imported modules, then prove the original ISA relation.

    This is intentionally a full concrete proof, NOT proof-certificate reuse.
    No local contract conclusion is assumed by this checker invocation.
    """
    doc = registers.model(GPRS)
    old_memory = immutable(4)
    new_memory = immutable(count)
    def rewrite(value):
        if isinstance(value, list):
            # The architectural model's original exhaustive four-word mux.
            if (len(value) == 4 and value[0] == 'ite' and
                isinstance(value[2], str) and value[2] in
                ('s.rom0', 's.data0', 'impl.rom0', 'impl.data0')):
                prefix, kind = value[2].rsplit('.', 1)
                kind = kind[:-1]
                return lookup(kind, count, rewrite(value[1][1]), prefix + '.')
            if value[:1] == ['bv'] and value[1] == 2:
                return b(AB, value[2])
            if value[:3] == ['extract', 1, 0]:
                return address(rewrite(value[3]))
            return [rewrite(x) for x in value]
        if isinstance(value, dict):
            if value == {'bv': 2}: return {'bv': AB}
            return {k: rewrite(v) for k, v in value.items()}
        return value
    doc = rewrite(doc)
    for part in ('state', 'reset', 'next'):
        for k in old_memory: doc['spec'][part].pop(k)
    doc['spec']['state'].update(new_memory)
    doc['spec']['reset'].update({k: 'i.seed_' + k for k in new_memory})
    doc['spec']['next'].update({k: 's.' + k for k in new_memory})
    # Existing equality leaves cover first four words; append all extra words.
    doc['binding'] = conj([doc['binding'], *[eq('spec.' + k, 'impl.' + k)
        for k in new_memory if k not in old_memory]])
    doc['inputs'] = {'rst': 'bool', 'stall': 'bool',
                     **{'seed_' + k: ty for k, ty in new_memory.items()}}
    cpu = copy.deepcopy(cpu)
    memory = copy.deepcopy(memory)
    cpu_wire_map = {'w.' + k: 'w.cpu_' + k for k in cpu['wires']}
    memory_wire_map = {'w.' + k: 'w.mem_' + k for k in memory['wires']}
    # Break no cycles: request addresses are combinational functions of CPU
    # state only. Refuse a future RTL change that introduces response feedback.
    def expand(expr, wires, active=None):
        active = set() if active is None else active
        if isinstance(expr, str) and expr.startswith('w.'):
            name = expr[2:]
            if name in active: raise ValueError('wire cycle')
            return expand(wires[name], wires, active | {name})
        if isinstance(expr, list): return [expand(x, wires, active) for x in expr]
        return expr
    for k in ('imem_address', 'dmem_address'):
        expanded = expand(cpu['outputs'][k], cpu['wires'])
        if RUNNER.references(expanded, 'i.'):
            raise ValueError('request addresses must be state-only; response loop is unsupported')
    request_map = {'i.' + k: rename(cpu['outputs'][k], cpu_wire_map)
                   for k in ('imem_address', 'dmem_address')}
    if fault == 'load_address_association':
        request_map['i.dmem_address'] = request_map['i.imem_address']
    elif fault == 'fetch_address_high_drop':
        request_map['i.imem_address'] = ['band', request_map['i.imem_address'], b(AB, 3)]
    elif fault not in (None, 'load_response_corruption'):
        raise ValueError('unknown composition fault')
    memory_map = {**memory_wire_map, **request_map}
    response_map = {'i.' + k: rename(memory['outputs'][k], memory_map)
                    for k in ('imem_response', 'dmem_response')}
    if fault == 'load_response_corruption':
        response_map['i.dmem_response'] = ['bxor', response_map['i.dmem_response'], b(WIDTH, 1)]
    cpu_map = {**cpu_wire_map, **response_map}
    machine = {'state': {**cpu['state'], **memory['state']},
               'reset': {**rename(cpu['reset'], cpu_map), **rename(memory['reset'], memory_map)},
               'next': {**rename(cpu['next'], cpu_map), **rename(memory['next'], memory_map)},
               'outputs': {'commit': rename(cpu['outputs']['commit'], cpu_map)},
               'wires': {**{'cpu_' + k: rename(v, cpu_map) for k, v in cpu['wires'].items()},
                         **{'mem_' + k: rename(v, memory_map) for k, v in memory['wires'].items()}}}
    if any('i.' + k in json.dumps(machine) for k in
           ('imem_address', 'dmem_address', 'imem_response', 'dmem_response')):
        raise ValueError('composition has unconnected interface ports')
    doc['impl'] = machine
    doc['name'] = f'composed_8gpr_cpu_{count}_backing_words_fixed64_address_space'
    return doc


def execute(args, output, allowed=(0,)):
    start = time.monotonic()
    result = subprocess.run([str(x) for x in args], capture_output=True, text=True, timeout=180,
                            env={**os.environ, 'HWVERIFY_SOLVER': 'finite'})
    output.write_text(result.stdout)
    output.with_suffix(output.suffix + '.stderr').write_text(result.stderr)
    if result.returncode not in allowed:
        raise RuntimeError(f'{args[0]} exit {result.returncode}: {result.stderr[:1500]} {result.stdout[:500]}')
    data = json.loads(result.stdout)
    return data, time.monotonic() - start


def compile_machine(text, kind, count, out, frontend, lifter):
    out.mkdir(parents=True, exist_ok=False)
    (out / 'source.veryl').write_text(text)
    top = 'PipelineCPU' if kind == 'cpu' else 'ReadMemory'
    RUNNER.write_json(out / 'design.json', {'top': top, 'four_state': False,
        'sources': [{'path': 'source.veryl', 'text': text}]})
    compiled, _ = execute([frontend, out / 'design.json'], out / 'compiled.json')
    if compiled.get('allowed_diagnostics') != []:
        raise RuntimeError('frontend diagnostics require a waiver')
    if kind == 'cpu':
        state = cpu_state()
        inputs = {'rst_n': {'name': 'rst', 'type': 'bool', 'expr': ['not', 'i.rst']},
                  **{k: {'type': v} for k, v in CPU_INPUTS.items() if k != 'rst'}}
        outputs = PORTS
    else:
        state = immutable(count)
        inputs = {'rst': {'type': 'bool'}, 'imem_address': {'type': {'bv': AB}},
                  'dmem_address': {'type': {'bv': AB}},
                  **{'seed_' + k: {'type': v} for k, v in state.items()}}
        outputs = {'imem_response': {'bv': IW}, 'dmem_response': {'bv': WIDTH}}
    config = {'event': 'clk', 'inputs': inputs,
              'state': {k: {'type': v} for k, v in state.items()},
              'outputs': {k: {'signal': k, 'type': v} for k, v in outputs.items()}}
    machine = {'state': state}
    for mode, reset in [('normal', False), ('reset', True)]:
        config['overrides'] = {'rst': reset}
        config['outputs'] = {} if reset else {k: {'signal': k, 'type': v} for k, v in outputs.items()}
        RUNNER.write_json(out / (mode + '-bindings.json'), config)
        lifted, _ = execute([lifter, out / 'compiled.json', out / (mode + '-bindings.json')]
            + (['--inline'] if reset else []), out / (mode + '-lift.json'))
        if reset:
            if any(RUNNER.references(v, 's.') or RUNNER.references(v, 'w.') for v in lifted['next'].values()):
                raise RuntimeError('DUT reset depends on arbitrary prestate')
            machine['reset'] = lifted['next']
        else:
            for part in ('next', 'wires', 'outputs'): machine[part] = lifted[part]
    RUNNER.write_json(out / 'machine.json', machine)
    return machine


def validate_scoped(report, fault=None):
    binding = report.get('implementation_binding') or {}
    obligations = binding.get('obligations', [])
    expected_names = {'binding_reset_nonempty', 'binding_reset_establishes_product',
                      'binding_product_preservation'}
    if len(obligations) != 3 or {q['name'] for q in obligations} != expected_names:
        raise RuntimeError('missing, duplicate, or unexpected contract obligations')
    bad_name = ('binding_reset_establishes_product' if fault == 'wrong_reset'
                else 'binding_product_preservation') if fault else None
    for q in obligations:
        wanted = 'sat' if q['name'] in ('binding_reset_nonempty', bad_name) else 'unsat'
        if q.get('solver_result') != wanted:
            raise RuntimeError(f"{q['name']}: expected {wanted}, got {q.get('solver_result')}")
        if q.get('backend') not in ('finite_bv', 'structural_kernel'):
            raise RuntimeError('external or unsupported solver backend')
        if wanted == 'sat' and q.get('finite', {}).get('original_formula_validated') is not True:
            raise RuntimeError('SAT witness lacks original-query validation')
        expected_status = 'counterexample' if q['name'] == bad_name else 'passed'
        if q.get('status') != expected_status:
            raise RuntimeError('contradictory contract obligation status')
    if report.get('status') != ('implementation_binding_failed' if fault else 'spec_examples_and_binding_verified'):
        raise RuntimeError('contradictory top-level contract status')
    if binding.get('status') != ('failed' if fault else 'verified'):
        raise RuntimeError('contradictory binding status')
    examples = report.get('examples', [])
    if not examples or any(e.get('status') != 'passed' for e in examples):
        raise RuntimeError('contract examples missing or failed')
    labels = [(e.get('target'), e.get('example')) for e in examples]
    if len(set(labels)) != len(labels) or {e.get('expect') for e in examples} != {'positive', 'negative'}:
        raise RuntimeError('missing polarity or duplicate contract examples')
    for example in examples:
        q = example.get('evidence', {})
        if example.get('expect') not in ('positive', 'negative'):
            raise RuntimeError('example expectation is missing or unsupported')
        wanted = 'sat' if example['expect'] == 'positive' else 'unsat'
        if q.get('solver_result') != wanted or q.get('backend') not in ('finite_bv', 'structural_kernel'):
            raise RuntimeError('example has missing/contradictory verdict or external solver')
        if wanted == 'sat' and q.get('finite', {}).get('original_formula_validated') is not True:
            raise RuntimeError('example SAT witness not validated')
    return obligations


def check_document(doc, out, checker, fault=None):
    out.mkdir(parents=True, exist_ok=True)
    RUNNER.write_json(out / 'contract.json', doc)
    report, seconds = execute([checker, out / 'contract.json', '--out', out / 'proof'],
                               out / 'report.json', (0, 1, 3))
    if doc['version'] == 4:
        obligations = validate_scoped(report, fault)
        expected_examples = {(target, name, example['expect'])
            for target, spec in doc['specs'].items() for name, example in spec['examples'].items()}
        actual_examples = {(e.get('target'), e.get('example'), e.get('expect')) for e in report['examples']}
        if actual_examples != expected_examples:
            raise RuntimeError('missing or unexpected contract example results')
        key = 'binding_product_preservation'
    else:
        RUNNER.validate_report(report, fault)
        obligations = report['obligations']
        key = 'microstep_refinement'
    query = next(q for q in obligations if q['name'] == key)
    result = {'status': report['status'], 'correct_outcome': True, 'seconds': seconds,
              'fault': fault, 'document_sha256': hashlib.sha256((out / 'contract.json').read_bytes()).hexdigest(),
              'state_bits': sum(1 if ty == 'bool' else ty['bv'] for ty in
                    (doc['implementation']['state'] if doc['version'] == 4 else doc['impl']['state']).values()),
              'query': query, 'obligations': [{k: q.get(k) for k in
                  ('name', 'status', 'solver_result', 'backend')} for q in obligations]}
    RUNNER.write_json(out / 'summary.json', result)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--out', required=True, type=Path)
    parser.add_argument('--sizes', nargs='+', type=int, default=[4, 16, 64], choices=[4, 16, 64])
    parser.add_argument('--cpu-faults', nargs='*', default=list(CPU_FAULTS), choices=list(CPU_FAULTS))
    parser.add_argument('--memory-faults', nargs='*', default=list(MEMORY_FAULTS), choices=list(MEMORY_FAULTS))
    parser.add_argument('--composition-faults', nargs='*', default=list(COMPOSITION_FAULTS), choices=list(COMPOSITION_FAULTS))
    parser.add_argument('--skip-composition', action='store_true')
    parser.add_argument('--require-success', action='store_true')
    parser.add_argument('--frontend', type=Path, default=ROOT / 'conformance/veryl-proof/target/debug/veryl-proof-frontend')
    parser.add_argument('--lifter', type=Path, default=ROOT / 'target/release/hwverify-sir-lift')
    parser.add_argument('--checker', type=Path, default=ROOT / 'target/release/hwverify-rs')
    args = parser.parse_args()
    args.out = args.out.resolve()
    args.out.mkdir(parents=True, exist_ok=False)
    results, cpu, memories = [], None, {}
    # Never use a real external solver even if the caller forgot its CI tripwire.
    tripwire = args.out / 'z3-tripwire'
    tripwire.write_text('#!/bin/sh\nprintf invoked > "' + str(args.out / 'z3-invoked.txt') + '"\nexit 97\n')
    tripwire.chmod(0o755)
    os.environ['Z3_BIN'] = str(tripwire)
    os.environ['HWVERIFY_SOLVER'] = 'finite'
    os.environ['HWVERIFY_FINITE_SEARCH_HINT'] = 'query'
    files = [Path(__file__), ROOT / 'conformance/veryl-symbolic/pipeline_memory_ports.veryl',
             ROOT / 'audit/veryl_scaling/cpu_registers.py', ROOT / 'conformance/veryl-symbolic/run.py',
             ROOT / 'examples/build_pipeline.py', args.frontend, args.lifter, args.checker]
    RUNNER.write_json(args.out / 'tested-files.json', {str(p.resolve()): hashlib.sha256(p.read_bytes()).hexdigest() for p in files})
    def run(kind, capacity, fault, thunk):
        try:
            result = thunk()
        except (RuntimeError, ValueError, KeyError, subprocess.TimeoutExpired) as error:
            result = {'correct_outcome': False, 'status': 'error', 'error': str(error)}
        result.update(kind=kind, capacity=capacity, fault=fault)
        results.append(result)
        RUNNER.write_json(args.out / 'summary.json', results)
        print(json.dumps({k: result.get(k) for k in ('kind', 'capacity', 'fault', 'status', 'correct_outcome', 'error')}), flush=True)
    for fault in [None, *args.cpu_faults]:
        out = args.out / ('cpu' + ('_bad_' + fault if fault else ''))
        def cpu_case():
            nonlocal cpu
            machine = compile_machine(source(fault), 'cpu', None, out, args.frontend, args.lifter)
            if not fault: cpu = machine
            return check_document(cpu_contract(machine), out, args.checker, fault)
        run('cpu_contract', None, fault, cpu_case)
    for count in args.sizes:
        for fault in [None, *args.memory_faults]:
            out = args.out / (f'memory_{count}' + ('_bad_' + fault if fault else ''))
            def memory_case():
                machine = compile_machine(memory_source(count, fault), 'memory', count, out, args.frontend, args.lifter)
                if not fault: memories[count] = machine
                return check_document(memory_contract(count, machine), out, args.checker, fault)
            run('memory_contract', count, fault, memory_case)
        if not args.skip_composition:
            for fault in [None, *args.composition_faults]:
                out = args.out / (f'composed_{count}' + ('_bad_' + fault if fault else ''))
                run('concrete_ISA_composition', count, fault,
                    lambda: check_document(architectural_composition(count, cpu, memories[count], fault), out, args.checker, fault))
    cpu_result = next((r for r in results if r['kind'] == 'cpu_contract' and r['fault'] is None), {})
    RUNNER.write_json(args.out / 'capacity-summary.json', {
        'cpu_contract_runs': 1, 'cpu_proof_has_no_capacity_parameter': True,
        'cpu_document_sha256': cpu_result.get('document_sha256'),
        'cpu_hardware_state_bits': sum(1 if ty == 'bool' else ty['bv'] for ty in cpu_state().values()),
        'cpu_observer_bits': 47, 'address_bits': AB, 'registers': GPRS,
        'capacities': args.sizes, 'full_ISA_proof_reused_component_certificates': False,
        'limits': 'unchanged finite100M work/1M allocated clauses/10 seconds per query',
        'external_solver_invoked': (args.out / 'z3-invoked.txt').exists()})
    if (args.out / 'z3-invoked.txt').exists():
        raise SystemExit('external solver tripwire fired')
    if args.require_success and any(not r['correct_outcome'] for r in results):
        raise SystemExit('memory contract gate failed; unknown/error is not success')


if __name__ == '__main__':
    main()
