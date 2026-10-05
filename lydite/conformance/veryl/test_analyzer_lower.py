"""Adversarial source -> real analyzer -> finite checks for the added constructs.

Synthetic expected values are independently computed in Python. They are never
used by the analyzer/transition lowering, and every positive has a negative twin.
The upstream corpus still exclusively uses executed original Rust expectations.
"""
import copy
import hashlib
import itertools
import json
import os
from pathlib import Path
import tempfile
import unittest
from analyzer_lower import Lower, Unsupported
from check_extracted import analyze, run, LYDITE
from catalog import oracle_independence


def synthetic(source, samples, read_kind='get', scalar_width=None, scalar_type=None, comparison='eq'):
    actions = []
    count = 0
    for inputs, expected in samples:
        actions.extend({'action': 'write', 'signal': key, 'payload': str(value), 'mask': '0', 'instances': []} for key, value in inputs.items())
        actions.append({'action': 'eval_comb'})
        for key, value in expected.items():
            assertion = {'comparison': comparison, 'read_kind': read_kind,
                         'projection': 'scalar_low_bits' if read_kind == 'get_as' else 'payload',
                         'mask_constraint': None}
            if scalar_width:
                assertion.update(scalar_width=scalar_width, scalar_type=scalar_type)
            actions.append({'action': 'read', 'signal': key, 'payload': str(value), 'mask': '0', 'instances': [], 'assertion': assertion})
            count += 1
    return {'case': 'synthetic', 'status': 'extracted', 'design': {'top': 'Top', 'four_state': False,
            'sources': [{'text': source, 'sha256': hashlib.sha256(source.encode()).hexdigest()}]},
            'actions': actions, 'assertion_count': count}


class TypedLoweringTests(unittest.TestCase):
    def check(self, row, status='passed'):
        with tempfile.TemporaryDirectory(prefix='veryl-lowering-test-') as directory:
            destination = Path(directory)
            ir, document = analyze(row, destination)
            oracle_independence(row, ir, document)
            result = run(document, LYDITE, destination / 'normal')
            self.assertEqual(result['status'], status, json.dumps(result))
            self.assertTrue(result['cases'][0]['feasible'])
            if status == 'passed':
                wrong = copy.deepcopy(row)
                for action in wrong['actions']:
                    if action['action'] == 'read':
                        action['assertion']['comparison'] = 'ne' if action['assertion']['comparison'] == 'eq' else 'eq'
                        break
                result = run(Lower(ir).build(wrong), LYDITE, destination / 'negative')
                self.assertEqual(result['status'], 'failed')
                self.assertTrue(result['cases'][0]['feasible'])
            return ir, document

    def test_unary_reductions_exhaustive_three_bits(self):
        source = '''module Top(a: input logic<3>, inv: output logic<3>, neg: output logic<3>, plus: output logic<3>,
          red_and: output logic, red_nand: output logic, red_or: output logic, red_nor: output logic,
          red_xor: output logic, red_xnor: output logic) {
          always_comb { inv = ~a; neg = -a; plus = +a; red_and = &a; red_nand = ~&a;
          red_or = |a; red_nor = ~|a; red_xor = ^a; red_xnor = ~^a; }
        }'''
        self.check(synthetic(source, [({'a': a}, {'inv': a ^ 7, 'neg': (-a) & 7, 'plus': a,
          'red_and': int(a == 7), 'red_nand': int(a != 7), 'red_or': int(a != 0), 'red_nor': int(a == 0),
          'red_xor': a.bit_count() % 2, 'red_xnor': 1 - a.bit_count() % 2}) for a in range(8)]))

    def test_signed_comparison_cast_width_boundary(self):
        source = '''module Top(a: input logic<8>, b: input logic<8>, lt: output logic, ge: output logic) {
            always_comb { lt = (a as i8) <: (b as i8); ge = (a as i8) >= (b as i8); }
        }'''
        signed = lambda value: value if value < 128 else value - 256
        self.check(synthetic(source, [({'a': a, 'b': b}, {'lt': int(signed(a) < signed(b)), 'ge': int(signed(a) >= signed(b))})
            for a, b in itertools.product([0, 1, 127, 128, 254, 255], repeat=2)]))

    def test_mixed_signed_comparison_and_extension(self):
        source = '''module Top(a: input signed logic<3>, b: input logic<5>, lt: output logic, ge: output logic) {
            always_comb { lt = a <: b; ge = a >= b; }
        }'''
        self.check(synthetic(source, [({'a': a, 'b': b}, {'lt': int(a < b), 'ge': int(a >= b)})
            for a, b in itertools.product(range(8), [0, 3, 7, 8, 31])]))

    def test_ternary_arm_widths_and_unselected_values(self):
        source = '''module Top(sel: input logic, a: input logic<3>, b: input logic<5>, y: output logic<5>) {
            assign y = if sel ? a : b;
        }'''
        self.check(synthetic(source, [({'sel': sel, 'a': a, 'b': b}, {'y': a if sel else b})
            for sel, a, b in itertools.product(range(2), [0, 7], [0, 16, 31])]))

    def test_blocking_conditionals_merge_then_continue(self):
        source = '''module Top(sel: input logic, a: input logic<3>, b: input logic<3>, y: output logic<3>, z: output logic<3>) {
            always_comb { y = a; if sel { y = b; } z = y + 3'd1; }
        }'''
        self.check(synthetic(source, [({'sel': sel, 'a': a, 'b': b}, {'y': b if sel else a, 'z': ((b if sel else a) + 1) & 7})
            for sel, a, b in itertools.product(range(2), [0, 7], [0, 6])]))

    def test_dynamic_shifts_saturate_wide_counts(self):
        source = '''module Top(a: input signed logic<3>, count: input logic<8>, logical: output logic<3>, arithmetic: output logic<3>, left: output logic<3>) {
            always_comb { logical = a >> count; arithmetic = a >>> count; left = a << count; }
        }'''
        signed = lambda a: a if a < 4 else a - 8
        self.check(synthetic(source, [({'a': a, 'count': shift}, {'logical': a >> shift, 'arithmetic': (signed(a) >> shift) & 7, 'left': (a << shift) & 7})
            for a, shift in itertools.product([0, 3, 4, 7], [0, 1, 2, 3, 8, 255])]))

    def test_multiplication_and_binary_xnor(self):
        source = '''module Top(a: input logic<3>, b: input logic<3>, product: output logic<5>, o_xnor: output logic<3>) {
            always_comb { product = a * b; o_xnor = a ~^ b; }
        }'''
        self.check(synthetic(source, [({'a': a, 'b': b}, {'product': (a * b) & 31, 'o_xnor': (a ^ b) ^ 7})
            for a, b in itertools.product([0, 1, 3, 7], repeat=2)]))

    def test_scalar_signed_reads_zero_fill_not_sign_extend(self):
        source = 'module Top(a: input signed logic<3>, y: output signed logic<3>) { assign y = a; }'
        self.check(synthetic(source, [({'a': 7}, {'y': 7})], 'get_as', 8, 'i8'))
        self.check(synthetic(source, [({'a': 7}, {'y': 255})], 'get_as', 8, 'i8'), 'failed')

    def test_scalar_bool_uses_only_low_bit(self):
        source = 'module Top(a: input logic<3>, y: output logic<3>) { assign y = a; }'
        self.check(synthetic(source, [({'a': a}, {'y': a & 1}) for a in range(8)], 'get_as', 1, 'bool'))

    def test_scalar_narrow_truncation_and_inequality(self):
        source = 'module Top(a: input logic<9>, y: output logic<9>) { assign y = a; }'
        self.check(synthetic(source, [({'a': 511}, {'y': 255})], 'get_as', 8, 'u8'))
        self.check(synthetic(source, [({'a': 511}, {'y': 0})], 'get_as', 8, 'u8', 'ne'))

    def test_out_of_range_expected_is_false_not_truncated(self):
        source = 'module Top(a: input logic<3>, y: output logic<3>) { assign y = a; }'
        self.check(synthetic(source, [({'a': 1}, {'y': 9})]), 'failed')
        self.check(synthetic(source, [({'a': 1}, {'y': 9})], comparison='ne'))

    def test_static_read_select_variants(self):
        source = '''module Top(a: input logic<8>, o_bit: output logic, range: output logic<3>, up: output logic<3>, down: output logic<3>) {
            always_comb { o_bit = a[7]; range = a[5:3]; up = a[3+:3]; down = a[5-:3]; }
        }'''
        self.check(synthetic(source, [({'a': a}, {'o_bit': a >> 7, 'range': (a >> 3) & 7, 'up': (a >> 3) & 7, 'down': (a >> 3) & 7}) for a in [0, 8, 40, 128, 255]]))

    def test_logic_operations(self):
        source = '''module Top(a: input logic, b: input logic, y: output logic, z: output logic, n: output logic) {
          always_comb { y = a && b; z = a || b; n = !a; }
        }'''
        self.check(synthetic(source, [({'a': a, 'b': b}, {'y': a & b, 'z': a | b, 'n': 1 - a}) for a, b in itertools.product(range(2), repeat=2)]))

    def test_four_state_is_not_silently_approximated(self):
        row = synthetic('module Top(a: input logic, y: output logic) { assign y = a; }', [({'a': 0}, {'y': 0})])
        row['design']['four_state'] = True
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaisesRegex(Unsupported, 'four-state'):
                analyze(row, Path(directory))


if __name__ == '__main__':
    unittest.main()
