"""Migration-printer tests; set HWVERIFY_BIN for parser/validator round trips."""

import copy
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

from json_to_hwv import expression, print_document, scoped_expression


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


def scoped_reference_specification():
    doc = minimal_scoped_specification()
    doc['specs']['spec'].update({
        'inputs': {'enable': 'bool'}, 'outputs': {'visible': 'bool'},
        'state': {'hidden': 'bool'}, 'init': ['not', 's.hidden'],
        'invariant': ['and', ['eq', 'o.visible', 's.hidden'],
                      ['eq', 'visible', 'o.visible']],
        'operations': {'advance': ['and', ['eq', 'n.hidden', 'enable'],
                                  ['and', ['eq', 'no.visible', 'n.hidden'],
                                   ['and', ['eq', 'i.enable', 'enable'], True]]]},
    })
    return doc


def scoped_shared_name_specification():
    doc = minimal_scoped_specification()
    doc['specs']['spec'].update({
        'inputs': {'shared_in': 'bool'}, 'outputs': {'shared_out': 'bool'},
        'state': {'shared_in': 'bool', 'shared_out': 'bool'},
        'init': ['and', ['not', 's.shared_in'], ['not', 's.shared_out']],
        'invariant': ['eq', 'shared_out', 's.shared_out'],
        'operations': {'advance': ['and', ['eq', 'n.shared_in', 'shared_in'],
                                  ['and', ['eq', 'n.shared_out', 'i.shared_in'],
                                   ['and', ['eq', 'no.shared_out', 'n.shared_out'],
                                    ['eq', 'o.shared_out', 's.shared_out']]]]},
    })
    return doc


def quantified_specification(version):
    if version == 3:
        doc = minimal_specification()
        doc['inputs'] = {'amount': {'bv': 4}}
        doc['observations'] = {'count': {'bv': 4}}
        doc['operations'] = {'advance': {}}
        target = doc['components']['component']
        target['steps'] = {'advance': ['eq', 'no.count', 'i.amount']}
        output = 'o.count'
    else:
        doc = minimal_scoped_specification()
        target = doc['specs']['spec']
        target['inputs'] = {'amount': {'bv': 4}}
        target['outputs'] = {'count': {'bv': 4}}
        target['operations'] = {'advance': ['eq', 'no.count', 'i.amount']}
        output = 'count'
    target['examples'] = {
        'quantified': {
            'expect': 'forall',
            'quantifiers': [
                {'kind': 'forall', 'variables': {'a': {'bv': 4}}},
                {'kind': 'exists', 'variables': {'b': {'bv': 4}, 'flag': 'bool'}},
                {'kind': 'forall', 'variables': {'c': {'bv': 4}}},
            ],
            'initial': {'count': ['bv', 4, 0]},
            'trace': [
                {'operation': 'advance', 'inputs': {'amount': 'q.a'}, 'observe': {},
                 'ensure': ['eq', output, 'q.a']},
                {'operation': 'advance', 'inputs': {'amount': ['add', 'q.a', 'q.b']},
                 'observe': {'count': 'q.c'},
                 'ensure': ['or', ['not', 'q.flag'], ['eq', 'o.count', 'q.c']]},
            ],
        },
    }
    if version == 4:
        target['examples']['quantified']['trace'].append({
            'actions': ['advance'], 'inputs': {}, 'observe': {}, 'ensure': True,
        })
    return doc


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
            "  operation add {\n    expect eq(value', add(value, amount));\n  }",
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
        self.assertIn('  operation actions {\n    expect true;\n  }', source)
        self.assertNotIn('state ', source)

    def test_scoped_prime_references_and_single_expect_preserve_tree(self):
        doc = scoped_reference_specification()
        original = copy.deepcopy(doc)
        for infix in (False, True):
            with self.subTest(infix=infix):
                source = print_document(doc, infix)
                self.assertIn('  operation advance {\n    expect ', source)
                self.assertEqual(source.count('    expect '), 1)
                for reference in ("hidden'", "visible'", 'o.visible', 'i.enable'):
                    self.assertIn(reference, source)
                for reference in ('s.hidden', 'n.hidden', 'no.visible'):
                    self.assertNotIn(reference, source)
        self.assertEqual(doc, original)

    def test_scoped_shared_names_keep_explicit_references(self):
        doc = scoped_shared_name_specification()
        original = copy.deepcopy(doc)
        source = print_document(doc)
        self.assertIn('  invariant eq(o.shared_out, s.shared_out);', source)
        self.assertIn('eq(n.shared_in, i.shared_in)', source)
        self.assertIn('eq(n.shared_out, i.shared_in)', source)
        self.assertIn('eq(no.shared_out, n.shared_out)', source)
        self.assertIn('eq(o.shared_out, s.shared_out)', source)
        self.assertNotIn("'", source)
        self.assertEqual(doc, original)

    def test_scoped_expression_rewrites_only_expression_positions(self):
        scope = {'state': {'add': {'bv': 8}}, 'inputs': {}, 'outputs': {}}
        value = ['eq', 'n.add', ['add', 's.add', ['bv', 8, -1]]]
        self.assertEqual(scoped_expression(value, scope),
                         "eq(add', add(add, bv(8, -1)))")
        # The printer leaves non-expression positions for the validator, even
        # when their invalid string values happen to look like references.
        for value, expected in (
            (['bv', 's.add', 'n.add'], 'bv(s.add, n.add)'),
            (['zext', 's.add', 'n.add'], "zext(s.add, add')"),
            (['sext', 's.add', 'n.add'], "sext(s.add, add')"),
            (['extract', 's.add', 'n.add', 's.add'], 'extract(s.add, n.add, add)'),
            (['const_mem', 's.add', 'n.add'], "const_mem(s.add, add')"),
        ):
            with self.subTest(value=value):
                self.assertEqual(scoped_expression(value, scope), expected)

    def test_scoped_sugar_does_not_change_legacy_documents(self):
        doc = minimal_specification()
        component = doc['components']['component']
        component['state'] = {'hidden': 'bool'}
        component['init'] = ['not', 's.hidden']
        component['steps']['operation'] = ['eq', 'n.hidden', 's.hidden']
        source = print_document(doc)
        self.assertIn('  init not(s.hidden);', source)
        self.assertIn('    operation = eq(n.hidden, s.hidden);', source)
        self.assertNotIn("hidden'", source)
        self.assertNotIn('expect ', source)

    def test_quantified_example_clauses_and_invocations(self):
        for version in (3, 4):
            with self.subTest(version=version):
                doc = quantified_specification(version)
                original = copy.deepcopy(doc)
                source = print_document(doc)
                self.assertIn('    forall a: bv<4>;\n    exists {\n'
                              '      b: bv<4>;\n      flag: bool;\n    }\n'
                              '    forall c: bv<4>;\n    execution forall;', source)
                output = 'o.count' if version == 3 else 'count'
                self.assertIn(f'      advance(amount: q.a) => eq({output}, q.a);', source)
                self.assertIn('      advance {\n        inputs {\n'
                              '          amount = add(q.a, q.b);', source)
                self.assertIn('        observe {\n          count = q.c;\n        }\n'
                              '        ensure or(not(q.flag), eq(o.count, q.c));', source)
                if version == 4:
                    self.assertIn('      actions(advance) {\n        inputs {\n        }\n'
                                  '        observe {\n        }\n        ensure true;', source)
                self.assertEqual(doc, original)

    def test_new_execution_modes_and_legacy_expectations(self):
        doc = minimal_specification()
        target = doc['components']['component']
        for mode in ('exists', 'not_exists', 'forall', 'positive', 'negative'):
            target['examples'] = {'empty': {'expect': mode, 'initial': {}, 'trace': []}}
            keyword = 'execution' if mode in ('exists', 'not_exists', 'forall') else 'expect'
            with self.subTest(mode=mode):
                self.assertIn(f'    {keyword} {mode};', print_document(doc))

    def test_optional_ensure_preserves_false_and_legacy_frame_shapes(self):
        doc = minimal_specification()
        target = doc['components']['component']
        target['examples'] = {'frames': {
            'expect': 'positive', 'initial': {}, 'trace': [
                {'operation': 'operation', 'inputs': {}, 'observe': {}, 'ensure': False},
                {'operation': 'operation', 'inputs': {}, 'observe': {}},
            ],
        }}
        source = print_document(doc)
        self.assertIn('      operation() => false;', source)
        self.assertIn('      operation {\n        inputs {\n        }\n'
                      '        observe {\n        }\n      }', source)
        self.assertEqual(source.count('=>'), 1)
        self.assertNotIn('execution ', source)

    def test_empty_quantifier_array_has_explicit_marker(self):
        doc = minimal_specification()
        doc['components']['component']['examples'] = {'empty': {
            'expect': 'exists', 'quantifiers': [],
            'initial': {}, 'trace': [],
        }}
        self.assertIn('    quantifiers {}\n    execution exists;', print_document(doc))

    def test_operation_named_actions_uses_unambiguous_block(self):
        doc = minimal_scoped_specification()
        doc['specs']['spec']['examples'] = {'contextual': {
            'expect': 'exists', 'initial': {}, 'trace': [{
                'operation': 'actions', 'inputs': {}, 'observe': {}, 'ensure': True,
            }],
        }}
        source = print_document(doc)
        self.assertIn('      actions {\n        inputs {\n        }\n'
                      '        observe {\n        }\n        ensure true;', source)
        self.assertNotIn('actions() =>', source)

    def test_invalid_quantifier_array_or_empty_group_rejected(self):
        for invalid in ({}, 'forall', [{'kind': 'forall', 'variables': {}}]):
            doc = minimal_specification()
            doc['components']['component']['examples'] = {'empty': {
                'expect': 'exists', 'quantifiers': invalid, 'initial': {}, 'trace': [],
            }}
            with self.subTest(invalid=invalid), self.assertRaisesRegex(ValueError, 'quantifier'):
                print_document(doc)

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

    def test_v4_prime_state_output_and_nested_single_expect_round_trip(self):
        for infix in (False, True):
            with self.subTest(infix=infix):
                self.assert_round_trip(scoped_reference_specification(), infix)

    def test_v4_shared_names_qualify_legacy_bare_ports(self):
        doc = scoped_shared_name_specification()
        qualified = copy.deepcopy(doc)
        spec = qualified['specs']['spec']
        spec['invariant'][1] = 'o.shared_out'
        spec['operations']['advance'][1][2] = 'i.shared_in'
        # Bare JSON ports and explicit i./o. ports elaborate identically, but
        # ambiguous source names require qualification. Only these two strings
        # change in the canonical AST; every relation and state reference stays.
        for infix in (False, True):
            with self.subTest(infix=infix):
                self.assert_source_document(print_document(doc, infix), qualified)
                self.assert_round_trip(qualified, infix)

    def test_quantified_v3_v4_ordered_groups_and_trace_predicates(self):
        for version in (3, 4):
            for mode in ('exists', 'not_exists', 'forall'):
                for infix in (False, True):
                    with self.subTest(version=version, mode=mode, infix=infix):
                        doc = quantified_specification(version)
                        target = (doc['components']['component'] if version == 3
                                  else doc['specs']['spec'])
                        target['examples']['quantified']['expect'] = mode
                        self.assert_round_trip(doc, infix)

    def test_execution_modes_without_quantifiers_and_optional_ensure(self):
        for version in (3, 4):
            for mode in ('exists', 'not_exists', 'forall', 'positive', 'negative'):
                for ensure in (False, True):
                    with self.subTest(version=version, mode=mode, ensure=ensure):
                        doc = (minimal_specification() if version == 3
                               else minimal_scoped_specification())
                        target = (doc['components']['component'] if version == 3
                                  else doc['specs']['spec'])
                        target['examples'] = {'frames': {
                            'expect': mode, 'initial': {}, 'trace': [{
                                'operation': 'operation' if version == 3 else 'actions',
                                'inputs': {}, 'observe': {}, 'ensure': ensure,
                            }],
                        }}
                        self.assert_round_trip(doc)

    def test_empty_quantifier_array_round_trip(self):
        for version in (3, 4):
            for mode in ('exists', 'not_exists', 'forall'):
                with self.subTest(version=version, mode=mode):
                    doc = (minimal_specification() if version == 3
                           else minimal_scoped_specification())
                    target = (doc['components']['component'] if version == 3
                              else doc['specs']['spec'])
                    target['examples'] = {'empty': {
                        'expect': mode, 'quantifiers': [], 'initial': {}, 'trace': [],
                    }}
                    self.assert_round_trip(doc)

    def test_contextual_trace_operation_names_round_trip(self):
        for version in (3, 4):
            for operation in ('use', 'actions', 'bool', 'bv', 'mem', 'design',
                              'specification', 'forall', 'exists', 'expect',
                              'ensure', 'execution', 'any', 'all'):
                with self.subTest(version=version, operation=operation):
                    doc = (minimal_specification() if version == 3
                           else minimal_scoped_specification())
                    target = (doc['components']['component'] if version == 3
                              else doc['specs']['spec'])
                    if version == 3:
                        doc['operations'] = {operation: {}}
                        target['steps'] = {operation: True}
                    else:
                        target['operations'] = {operation: True}
                    target['examples'] = {'contextual': {
                        'expect': 'exists', 'initial': {}, 'trace': [{
                            'operation': operation, 'inputs': {}, 'observe': {}, 'ensure': True,
                        }],
                    }}
                    self.assert_round_trip(doc)

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
