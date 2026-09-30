"""Migration-printer tests; set HWVERIFY_BIN for parser/validator round trips."""

import copy
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

from json_to_hwv import expression, print_document


ROOT = Path(__file__).resolve().parents[1]


def fixture(name):
    return json.loads((ROOT / 'examples' / f'{name}.json').read_text())


def minimal_specification():
    return {
        'version': 3,
        'kind': 'specification',
        'name': 'Empty optional collections',
        'inputs': {},
        'observations': {},
        'operations': {'operation': {}},
        'components': {
            'component': {
                'state': {}, 'init': True, 'invariant': True,
                'steps': {'operation': True}, 'examples': {},
            },
        },
        'compositions': {},
    }


def minimal_scoped_specification():
    return {
        'version': 4, 'kind': 'specification', 'name': 'Empty scoped ports',
        'specs': {
            'spec': {
                'inputs': {}, 'outputs': {}, 'state': {},
                'init': True, 'invariant': True,
                'operations': {'actions': True}, 'examples': {},
            },
        },
        'compositions': {},
    }


class PrinterTests(unittest.TestCase):
    def test_design_header_and_individual_types(self):
        doc = fixture('array_sum')
        source = print_document(doc)
        self.assertEqual(source.splitlines()[0], f'design {json.dumps(doc["name"])}')
        self.assertIn('input rst: bool;', source)
        self.assertIn('  state pc: bv<8>;', source)
        self.assertIn('  parameter ', source)
        self.assertNotIn('inputs {', source)
        self.assertNotIn('state {', source)
        self.assertNotIn('parameters {', source)

    def test_specification_named_declarations_and_mapping(self):
        doc = fixture('budgeted_counter')
        original = copy.deepcopy(doc)
        source = print_document(doc)
        self.assertEqual(source.splitlines()[0], f'specification {json.dumps(doc["name"])}')
        for text in (
            'observation count: bv<4>;', 'operation add {}',
            'component Counter {', 'composition Budgeted {',
            '  example spend_all {', '    bind Counter {',
            '    observation count = s.count;', '  operations {',
            '      add {', '        inputs {', '        observe {',
        ):
            self.assertIn(text, source)
        for text in ('components {', 'compositions {', 'examples {', 'states {'):
            self.assertNotIn(text, source)
        self.assertEqual(doc, original)

    def test_empty_collections_are_omitted_but_required_steps_remain(self):
        source = print_document(minimal_specification())
        self.assertIn('operation operation {}', source)
        self.assertIn('component component {', source)
        self.assertIn('  steps {\n    operation = true;\n  }', source)
        for text in ('input ', 'observation ', 'state ', 'example ', 'composition '):
            self.assertNotIn(text, source)

    def test_unicode_and_escaped_header_name(self):
        doc = minimal_specification()
        doc['name'] = '名前 "quoted"\\path\nline'
        header = print_document(doc).splitlines()[0]
        self.assertEqual(header, 'specification ' + json.dumps(doc['name'], ensure_ascii=False))

    def test_expression_tree_grouping_and_literal_values(self):
        value = ['sub', ['bv', 8, -1], ['add', ['bv', 8, 256], ['bv', 8, 2]]]
        self.assertEqual(expression(value), 'sub(bv(8, -1), add(bv(8, 256), bv(8, 2)))')
        self.assertEqual(expression(value, infix=True), '(bv(8, -1) - (bv(8, 256) + bv(8, 2)))')

    def test_unsupported_version_kind_and_operation_payload_rejected(self):
        with self.assertRaisesRegex(ValueError, 'versions 2, 3 and 4'):
            print_document({'version': 5})
        for version in (3, 4):
            with self.subTest(version=version), self.assertRaisesRegex(ValueError, 'kind'):
                print_document({'version': version, 'kind': 'design'})
        doc = minimal_specification()
        doc['operations']['operation'] = {'unexpected': True}
        with self.assertRaisesRegex(ValueError, 'empty object'):
            print_document(doc)

    def test_scoped_signatures_groups_actions_and_bindings(self):
        doc = fixture('scoped_dual_operator')
        original = copy.deepcopy(doc)
        source = print_document(doc)
        for text in (
            'spec Counter(input amount: bv<4>, output count: bv<4>) {',
            '  operation add = eq(n.value, add(s.value, amount));',
            '  use left: Counter(amount: left_amount, count: left_count);',
            '  operation left = actions(left.add);',
            '  operation right = actions(right.add);',
            '      left {', '      actions(right) {',
            '      actions(left, right) {', '      actions() {',
            'implementation {\n  composition Pair;\n  input rst: bool;',
            '    bind left {', '    output left_count = s.left;',
        ):
            self.assertIn(text, source)
        self.assertNotIn('observation ', source)
        self.assertEqual(doc, original)

    def test_scoped_nested_paths_and_omitted_group_shorthand(self):
        source = print_document(fixture('scoped_independent_counters'))
        self.assertIn('    bind pair.left {', source)
        self.assertIn('    bind pair.right {', source)
        self.assertNotIn('= actions(', source)

    def test_scoped_empty_signature(self):
        source = print_document(minimal_scoped_specification())
        self.assertIn('spec spec() {', source)
        self.assertIn('  operation actions = true;', source)
        self.assertNotIn('state ', source)

    def test_scoped_frame_choice_must_be_unambiguous(self):
        for selectors in ({}, {'operation': 'left', 'actions': ['left']}):
            doc = fixture('scoped_dual_operator')
            frame = doc['compositions']['Pair']['examples']['left_right_both_neither']['trace'][0]
            frame.pop('operation')
            frame.update(selectors)
            with self.subTest(selectors=selectors), self.assertRaisesRegex(ValueError, 'exactly one'):
                print_document(doc)

    def test_scoped_action_arrays_and_nonempty_groups(self):
        for invalid in ({}, {'left': []}, {'left': 'left.add'}, {'left': [7]}):
            doc = fixture('scoped_dual_operator')
            doc['compositions']['Pair']['operations'] = invalid
            with self.subTest(invalid=invalid), self.assertRaisesRegex(ValueError, 'nonempty'):
                print_document(doc)
        doc = fixture('scoped_dual_operator')
        doc['compositions']['Pair']['examples']['left_right_both_neither']['trace'][1]['actions'] = 'right'
        with self.assertRaisesRegex(ValueError, 'array of names'):
            print_document(doc)


@unittest.skipUnless(os.environ.get('HWVERIFY_BIN'), 'set HWVERIFY_BIN to a freshly built CLI')
class ParserRoundTripTests(unittest.TestCase):
    def assert_round_trip(self, doc, infix=False):
        self.assert_source_document(print_document(doc, infix), doc)

    def assert_source_document(self, source_text, doc):
        with tempfile.TemporaryDirectory(prefix='hwverify-printer-') as directory:
            source = Path(directory) / 'input.hwv'
            canonical = Path(directory) / 'canonical.json'
            output = Path(directory) / 'results'
            source.write_text(source_text, encoding='utf-8')
            result = subprocess.run(
                [os.environ['HWVERIFY_BIN'], str(source), '--emit-json', str(canonical), '--out', str(output)],
                capture_output=True, text=True, check=False,
            )
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertEqual(json.loads(canonical.read_text()), doc)

    def test_v2_v3_examples_call_and_infix(self):
        for name in ('array_sum', 'auto_array_sum', 'split_counter', 'budgeted_counter'):
            for infix in (False, True):
                with self.subTest(name=name, infix=infix):
                    self.assert_round_trip(fixture(name), infix)

    def test_contextual_names_and_empty_maps(self):
        self.assert_round_trip(minimal_specification())

    def test_empty_trace_and_repeated_operation_order(self):
        doc = minimal_specification()
        doc['components']['component']['examples'] = {
            'empty': {'expect': 'positive', 'initial': {}, 'trace': []},
            'repeated': {
                'expect': 'positive', 'initial': {},
                'trace': [{'operation': 'operation', 'inputs': {}, 'observe': {}}] * 2,
            },
        }
        self.assert_round_trip(doc)

    def test_empty_state_and_observation_bindings(self):
        doc = minimal_specification()
        doc['inputs'] = {'rst': 'bool'}
        doc['compositions'] = {'composition': {'members': ['component'], 'examples': {}}}
        doc['implementation'] = {
            'composition': 'composition', 'reset_input': 'rst',
            'state': {}, 'reset': {}, 'next': {}, 'operations': {'operation': False},
            'binding': {'states': {'component': {}}, 'observations': {}},
        }
        self.assert_round_trip(doc)

    def test_v4_fixtures_call_and_infix(self):
        for name in ('scoped_budgeted_counter', 'scoped_independent_counters',
                     'scoped_contradictory_outputs', 'scoped_dual_operator'):
            for infix in (False, True):
                with self.subTest(name=name, infix=infix):
                    self.assert_round_trip(fixture(name), infix)

    def test_v4_handwritten_sources_match_json(self):
        for name in ('scoped_budgeted_counter', 'scoped_independent_counters',
                     'scoped_contradictory_outputs', 'scoped_dual_operator'):
            with self.subTest(name=name):
                source_text = (ROOT / 'examples' / f'{name}.hwv').read_text()
                self.assert_source_document(source_text, fixture(name))

    def test_v4_empty_ports_and_action_occurrences(self):
        doc = minimal_scoped_specification()
        doc['specs']['spec']['examples'] = {
            'empty': {'expect': 'positive', 'initial': {}, 'trace': []},
            'occurrences': {
                'expect': 'positive', 'initial': {}, 'trace': [
                    {'operation': 'actions', 'inputs': {}, 'observe': {}},
                    {'actions': ['actions'], 'inputs': {}, 'observe': {}},
                    {'actions': [], 'inputs': {}, 'observe': {}},
                ],
            },
        }
        self.assert_round_trip(doc)

    def test_v4_nested_exported_groups_and_shared_leaf_action(self):
        doc = fixture('scoped_dual_operator')
        doc.pop('implementation')
        pair = doc['compositions']['Pair']
        pair['operations']['together'] = ['left.add', 'right.add', 'left.add']
        doc['compositions']['Nested'] = {
            'inputs': copy.deepcopy(pair['inputs']), 'outputs': copy.deepcopy(pair['outputs']),
            'instances': {'pair': {'target': 'Pair', 'connections': {
                port: port for port in [*pair['inputs'], *pair['outputs']]
            }}},
            'operations': {'left': ['pair.left'], 'both': ['pair.together', 'pair.left']},
            'examples': {},
        }
        self.assert_round_trip(doc)

    def test_v4_empty_state_and_output_bindings(self):
        doc = minimal_scoped_specification()
        doc['compositions'] = {
            'composition': {
                'inputs': {}, 'outputs': {},
                'instances': {'spec': {'target': 'spec', 'connections': {}}},
                'operations': {'actions': ['spec.actions']}, 'examples': {},
            },
        }
        doc['implementation'] = {
            'composition': 'composition', 'inputs': {'rst': 'bool'}, 'reset_input': 'rst',
            'state': {}, 'reset': {}, 'next': {}, 'operations': {'actions': False},
            'binding': {'states': {'spec': {}}, 'outputs': {}},
        }
        self.assert_round_trip(doc)


if __name__ == '__main__':
    unittest.main()
