#!/usr/bin/env python3
"""Fail-closed typed Veryl analyzer IR -> bounded two-state trace relations.

This module never executes HDL, reads expected fixtures, or invokes a solver.
The assertion operand is exclusively the independently captured Rust predicate.
"""
import copy


class Unsupported(ValueError):
    def __init__(self, code, detail):
        super().__init__(detail)
        self.code = code


def require(condition, code, detail):
    if not condition:
        raise Unsupported(code, detail)


def bv(width, number):
    require(1 <= width <= 64, 'width', f'bit-vector width {width} outside 1..64')
    return ['bv', width, int(number) & ((1 << width) - 1)]


def allof(items):
    items = list(items)
    if not items:
        return True
    if len(items) == 1:
        return items[0]
    middle = len(items) // 2
    return ['and', allof(items[:middle]), allof(items[middle:])]


def resize(expr, old, new, signed=False):
    require(1 <= old <= 64 and 1 <= new <= 64, 'width', f'resize {old} to {new} outside 1..64')
    if old == new:
        return expr
    return ['sext' if signed else 'zext', new - old, expr] if new > old else ['extract', new - 1, 0, expr]


def truth(expr, width):
    return ['ne', expr, bv(width, 0)]


class Lower:
    def __init__(self, ir, mutation=None):
        require(ir['schema'] == 'veryl-analyzer-probe-v1', 'schema', 'unrecognized analyzer schema')
        require(ir['metadata_defaults'] == {'clock_type': 'posedge', 'reset_type': 'async_low'}, 'metadata', 'unsupported clock/reset defaults')
        require(len(ir['modules']) == 1, 'hierarchy', 'hierarchy/multiple modules outside subset')
        self.m = ir['modules'][0]
        self.mutation = mutation
        self.v = {v['id']: v for v in self.m['variables']}
        self.byname = {v['path']: v for v in self.v.values()}
        require(len(self.v) == len(self.byname), 'duplicate_path', 'scoped variables share an exported path')
        for v in self.v.values():
            require(not v['type']['array_shape'], 'array', 'unpacked arrays outside subset')
            require(1 <= v['type']['width'] <= 64, 'width', f"signal {v['path']} width {v['type']['width']} outside 1..64")
            require(v['kind'] in ('input', 'output', 'var'), 'variable_kind', f"variable kind {v['kind']} outside subset")
        self.ins = {k: v for k, v in self.v.items() if v['kind'] == 'input'}
        self.reg = {k: v for k, v in self.v.items() if v['kind'] != 'input'}
        self.comb = [d for d in self.m['declarations'] if d['kind'] == 'comb']
        self.ffs = [d for d in self.m['declarations'] if d['kind'] == 'ff']
        require(all(d['kind'] in ('comb', 'ff', 'null') for d in self.m['declarations']), 'declaration', 'unsupported declaration')
        require(len(self.comb) <= 1 and len(self.ffs) <= 1, 'multi_process', 'multi-process scheduling outside subset')
        self.resets = {f['reset']['id']: f['reset']['comptime']['type']['kind'] for f in self.ffs if f['reset']}
        self.clocks = {f['clock']['id']: f['clock']['comptime']['type']['kind'] for f in self.ffs}
        require(all(t in ('Clock', 'ClockPosedge') for t in self.clocks.values()), 'clock_edge', 'negedge outside subset')

    def width(self, identifier):
        return self.v[identifier]['type']['width']

    @staticmethod
    def literal(expr):
        require(expr['kind'] == 'value' and expr['comptime']['is_const'], 'dynamic_select', 'select bounds must be elaborated numeric literals')
        value = expr['value']
        require(int(value['mask_xz']) == 0, 'four_state', 'X/Z literal outside two-state subset')
        number = int(value['payload'])
        require(not value['signed'] or not (number >> (value['width'] - 1)), 'select_bounds', 'negative select bound')
        return number

    def expr(self, e, env):
        context = e['comptime']['expr_context']
        width, signed, kind = context['width'], context['signed'], e['kind']
        if kind == 'value' and width == 0:
            require(e['comptime']['is_const'] and e['comptime']['evaluated'], 'untyped_value', 'unevaluated literal')
            width, signed = e['value']['width'], e['value']['signed']
        require(1 <= width <= 64, 'width', f'expression width {width} outside 1..64')
        if kind == 'variable':
            identifier = e['id']
            require(identifier not in self.clocks, 'clock_data', 'clock data read outside subset')
            value, old_width = env[identifier], self.width(identifier)
            select = e.get('select')
            if select:
                require(len(self.v[identifier]['type']['packed_shape']) <= 1 and len(select['indices']) == 1, 'packed_select', 'multi-dimensional packed select outside subset')
                first = self.literal(select['indices'][0])
                high = low = first
                if select['range']:
                    op, bound = select['range']['op'], self.literal(select['range']['bound'])
                    if op == 'Colon':
                        high, low = first, bound
                    elif op == 'PlusColon':
                        high, low = first + bound - 1, first
                    elif op == 'MinusColon':
                        high, low = first, first - bound + 1
                    elif op == 'Step':
                        high, low = first * bound + bound - 1, first * bound
                    else:
                        raise Unsupported('select_operator', f'unsupported select operator {op}')
                require(0 <= low <= high < old_width, 'select_bounds', 'out-of-range or empty select')
                value, old_width = ['extract', high, low, value], high - low + 1
            return resize(value, old_width, width, signed), width, signed
        if kind == 'value':
            value = e['value']
            require(int(value['mask_xz']) == 0, 'four_state', 'X/Z literal outside two-state subset')
            return resize(bv(value['width'], value['payload']), value['width'], width, signed), width, signed
        if kind == 'cast':
            value, old_width, _ = self.expr(e['arg'], env)
            target = e['comptime']['type']
            require(target['kind'] in ('Bit', 'Logic') and not target['array_shape'], 'cast_type', 'only scalar bit/logic casts are supported')
            value = resize(value, old_width, target['width'], e['arg']['comptime']['type']['signed'])
            # An explicit cast is a width boundary. The analyzer may carry the
            # outer 1-bit comparison context on it; that cannot shrink its type.
            width = max(width, target['width'])
            return resize(value, target['width'], width, signed), width, signed
        if kind == 'unary':
            value, old_width, _ = self.expr(e['arg'], env)
            op = e['op']
            if op in ('Add', 'Sub', 'BitNot'):
                value = resize(value, old_width, width, signed)
                if op == 'Sub':
                    value = ['sub', bv(width, 0), value]
                elif op == 'BitNot':
                    value = ['bnot', value]
                return value, width, signed
            if op == 'LogicNot':
                predicate = ['eq', value, bv(old_width, 0)]
            elif op in ('BitAnd', 'BitNand'):
                predicate = ['eq', value, bv(old_width, (1 << old_width) - 1)]
            elif op in ('BitOr', 'BitNor'):
                predicate = truth(value, old_width)
            elif op in ('BitXor', 'BitXnor'):
                predicate = ['eq', ['extract', 0, 0, value], bv(1, 1)]
                for bit in range(1, old_width):
                    predicate = ['xor', predicate, ['eq', ['extract', bit, bit, value], bv(1, 1)]]
            else:
                raise Unsupported('unary_operator', f'unsupported unary operator {op}')
            if op in ('BitNand', 'BitNor', 'BitXnor'):
                predicate = ['not', predicate]
            return ['ite', predicate, bv(width, 1), bv(width, 0)], width, False
        if kind == 'ternary':
            cond, cond_width, _ = self.expr(e['cond'], env)
            yes, yes_width, _ = self.expr(e['true_side'], env)
            no, no_width, _ = self.expr(e['false_side'], env)
            return ['ite', truth(cond, cond_width), resize(yes, yes_width, width, signed), resize(no, no_width, width, signed)], width, signed
        if kind == 'binary':
            a, aw, asg = self.expr(e['lhs'], env)
            b, bw, bsg = self.expr(e['rhs'], env)
            op = e['op']
            if op in ('Eq', 'Ne', 'Less', 'LessEq', 'Greater', 'GreaterEq'):
                operand_width = max(aw, bw)
                a = resize(a, aw, operand_width, asg and bsg)
                b = resize(b, bw, operand_width, asg and bsg)
                if op in ('Eq', 'Ne'):
                    predicate = ['eq' if op == 'Eq' else 'ne', a, b]
                else:
                    compare = ('s' if asg and bsg else 'u') + ('le' if op.endswith('Eq') else 'lt')
                    predicate = [compare, b, a] if op.startswith('Greater') else [compare, a, b]
                return ['ite', predicate, bv(width, 1), bv(width, 0)], width, False
            if op in ('LogicAnd', 'LogicOr'):
                return ['ite', ['and' if op == 'LogicAnd' else 'or', truth(a, aw), truth(b, bw)], bv(width, 1), bv(width, 0)], width, False
            if op in ('LogicShiftR', 'LogicShiftL', 'ArithShiftR', 'ArithShiftL'):
                a = resize(a, aw, width, signed)
                # Saturate before resizing: a wide count must never wrap to zero.
                if (1 << bw) > width:
                    count = ['ite', ['ult', b, bv(bw, width)], resize(b, bw, width), bv(width, width)]
                else:
                    count = resize(b, bw, width)
                if op == 'ArithShiftR' and signed and self.mutation != 'logical_instead_of_arithmetic':
                    negative = ['eq', ['extract', width - 1, width - 1, a], bv(1, 1)]
                    value = ['ite', negative, ['bnot', ['lshr', ['bnot', a], count]], ['lshr', a, count]]
                else:
                    value = ['lshr' if op in ('ArithShiftR', 'LogicShiftR') else 'shl', a, count]
                return value, width, signed
            names = {'BitAnd': 'band', 'BitOr': 'bor', 'BitXor': 'bxor', 'BitXnor': 'bxor', 'Add': 'add', 'Sub': 'sub', 'Mul': 'mul'}
            require(op in names, 'binary_operator', f'unsupported binary operator {op}')
            if self.mutation == 'self_sized_subtraction' and op == 'Sub':
                inner_width = e['lhs']['comptime']['type']['width']
                value = [names[op], resize(a, aw, inner_width), resize(b, bw, inner_width)]
                return resize(value, inner_width, width, signed), width, signed
            value = [names[op], resize(a, aw, width, signed), resize(b, bw, width, signed)]
            if op == 'BitXnor':
                value = ['bnot', value]
            return value, width, signed
        raise Unsupported('expression', f'unsupported expression {kind}')

    def statements(self, statements, read_env, next_env, blocking, reset=None):
        for statement in statements:
            kind = statement['kind']
            if kind == 'null':
                continue
            if kind == 'assign':
                require(len(statement['dst']) == 1, 'lhs_concatenation', 'concatenated assignment destination outside subset')
                identifier = statement['dst'][0]['id']
                require(identifier in self.reg, 'assignment_destination', 'assignment to non-state variable')
                value, width, signed = self.expr(statement['expr'], read_env)
                require(statement['width'] == self.width(identifier), 'partial_assignment', 'partial assignment outside subset')
                next_env[identifier] = resize(value, width, self.width(identifier), signed)
                if blocking:
                    read_env[identifier] = next_env[identifier]
            elif kind in ('if', 'if_reset'):
                if kind == 'if':
                    condition, width, _ = self.expr(statement['cond'], read_env)
                    condition = truth(condition, width)
                else:
                    require(reset is not None, 'reset_context', 'if_reset outside FF process')
                    identifier, reset_kind = reset['id'], reset['comptime']['type']['kind']
                    require(reset_kind in ('Reset', 'ResetAsyncLow', 'ResetAsyncHigh', 'ResetSyncLow', 'ResetSyncHigh'), 'reset_type', 'unsupported reset type')
                    active = 0 if reset_kind in ('Reset', 'ResetAsyncLow', 'ResetSyncLow') else 1
                    if self.mutation == 'wrong_reset_polarity':
                        active = 1 - active
                    condition = ['eq', read_env[identifier], bv(1, active)]
                yes, no = copy.deepcopy(next_env), copy.deepcopy(next_env)
                self.statements(statement['true_side'], dict(read_env), yes, blocking, reset)
                self.statements(statement['false_side'], dict(read_env), no, blocking, reset)
                for key in next_env:
                    if yes[key] != no[key]:
                        next_env[key] = ['ite', condition, yes[key], no[key]]
                if blocking:
                    read_env.update(next_env)
            else:
                raise Unsupported('statement', f'unsupported statement {kind}')

    def operation(self, event=None):
        env = {k: 'i.' + v['path'] for k, v in self.ins.items()}
        env.update({k: 's.' + v['path'] for k, v in self.reg.items()})
        next_env = {k: env[k] for k in self.reg}
        if event:
            matches = [f for f in self.ffs if self.v[f['clock']['id']]['path'] == event]
            require(len(matches) == 1, 'event', 'unsupported named event')
            ff = matches[0]
            self.statements(ff['statements'], dict(env), next_env, self.mutation == 'blocking_ff', ff['reset'])
            env.update(next_env)
        for comb in self.comb:
            self.statements(comb['statements'], env, next_env, True)
        return allof([['eq', 'n.' + v['path'], next_env[k]] for k, v in self.reg.items()])

    def build(self, row):
        require(row.get('status') == 'extracted', 'incomplete_capture', 'only complete extracted traces are accepted')
        require(row['design']['four_state'] is False, 'four_state', 'four-state design outside two-state subset')
        require(row['design']['top'] == self.m['name'], 'top_module', 'top module mismatch')
        ins = {v['path']: {'bv': self.width(k)} for k, v in self.ins.items()}
        state = {v['path']: {'bv': self.width(k)} for k, v in self.reg.items()}
        outs = {v['path']: {'bv': self.width(k)} for k, v in self.reg.items() if v['kind'] == 'output'}
        operations = {'eval_comb': self.operation()}
        for identifier in self.clocks:
            name = self.v[identifier]['path']
            operations['tick_' + name] = self.operation(name)
        trace, inputs = [], {name: 0 for name in ins}
        settled_inputs, dirty = dict(inputs), False
        count = 0
        for action in row['actions']:
            require(not action.get('instances'), 'hierarchical_observation', 'hierarchical observations outside subset')
            kind = action['action']
            if kind == 'write':
                require(action['signal'] in ins, 'write_target', 'write to non-input signal')
                require(int(action['mask']) == 0, 'four_state', 'masked input write outside two-state subset')
                width = ins[action['signal']]['bv']
                inputs[action['signal']] = int(action['payload']) & ((1 << width) - 1)
                dirty = True
            elif kind in ('eval_comb', 'tick'):
                require(not (dirty and kind == 'tick' and self.comb), 'unsettled_tick', 'tick with pending writes and combinational process requires pre-event scheduling')
                for identifier, reset_kind in self.resets.items():
                    name = self.v[identifier]['path']
                    old, new = settled_inputs[name], inputs[name]
                    active = 0 if reset_kind in ('Reset', 'ResetAsyncLow') else 1
                    if reset_kind in ('Reset', 'ResetAsyncLow', 'ResetAsyncHigh'):
                        require(not (old != new and new == active), 'async_reset_edge', 'asynchronous reset assertion edge outside event model')
                settled_inputs, dirty = dict(inputs), False
                operation = 'eval_comb' if kind == 'eval_comb' else 'tick_' + action['event']
                require(operation in operations, 'event', f'unsupported event {operation}')
                trace.append({'operation': operation, 'inputs': {name: bv(ins[name]['bv'], value) for name, value in inputs.items()}, 'observe': {}})
            elif kind == 'read':
                require(not dirty, 'implicit_settle', 'read with pending writes outside event model')
                require(bool(trace), 'initial_read', 'initial direct read before an explicit operation outside subset')
                require(action['signal'] in outs, 'observation_target', 'only output observations are supported')
                claim = action.get('assertion', {})
                require(claim.get('comparison') in ('eq', 'ne'), 'assertion_comparison', 'unsupported captured comparison')
                require(claim.get('mask_constraint') is None and int(action['mask']) == 0, 'four_state_observation', 'mask assertions outside two-state subset')
                source_width = outs[action['signal']]['bv']
                value, width = 'o.' + action['signal'], source_width
                if claim.get('read_kind') == 'get_as' and claim.get('projection') == 'scalar_low_bits':
                    width = claim['scalar_width']
                    scalar_types = {'bool': 1, 'u8': 8, 'i8': 8, 'u16': 16, 'i16': 16, 'u32': 32, 'i32': 32, 'u64': 64, 'i64': 64, 'usize': 64, 'isize': 64}
                    require(scalar_types.get(claim.get('scalar_type')) == width, 'scalar_type', 'scalar type/width metadata mismatch or unsupported scalar')
                    require(1 <= width <= 64, 'scalar_width', 'scalar projection wider than 64 bits')
                    # Scalar::from_bits zero fills bytes, even for signed Rust types.
                    value = resize(value, source_width, width, False)
                else:
                    require(claim.get('read_kind') == 'get' and claim.get('projection') == 'payload', 'assertion_projection', 'unsupported observation projection')
                expected = int(action['payload'])
                require(expected >= 0, 'expected_value', 'captured payload cannot be negative')
                if expected >= 1 << width:
                    predicate = claim['comparison'] == 'ne'
                else:
                    predicate = [claim['comparison'], value, bv(width, expected)]
                trace[-1]['ensure'] = allof([trace[-1].get('ensure', True), predicate])
                count += 1
            else:
                raise Unsupported('action', f'unsupported action {kind}')
        require(count > 0 and count == row['assertion_count'], 'assertion_count', 'empty or incomplete assertion schedule')
        example = {'quantifiers': [], 'expect': 'forall', 'initial': {}, 'trace': trace}
        spec = {'inputs': ins, 'outputs': outs, 'state': state,
                'init': allof([['eq', 's.' + name, bv(t['bv'], 0)] for name, t in state.items()]),
                'invariant': allof([['eq', 'o.' + name, 's.' + name] for name in outs]),
                'operations': operations, 'examples': {'original_assertions': example}}
        return {'version': 4, 'kind': 'specification', 'name': row['case'], 'specs': {'Top': spec}, 'compositions': {}}
