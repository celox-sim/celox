"""Source-level structural contracts: native parsing, graph and witness controls."""
import copy
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'conformance/celox-replay'))
from project import FRONTEND, invoke, write
from structure import extract

NATIVE = '''specification "Structural separation"
input rst: bool;
input a: bool;
observation y: bool;
operation tick {}
component Isolation {
  init true; invariant true; steps { tick = true; }
  structure { no_comb_path isolated { from i.a; to o.y; } }
}
composition Inner { members compose(Isolation); }
composition Checked { members compose(Inner); }
implementation {
 composition Checked; reset_input rst;
 state q: bool; reset { q = false; } next { q = i.a; }
 operations { tick = true; }
 endpoints { i.a = a; o.y = y; }
 binding { bind Isolation {} observation y = s.q; }
}
'''

def checker(graph=None, source=NATIVE, document=None):
    request = {'version': 1, **({'document': document} if document is not None else {'source': source})}
    if graph is not None: request['artifact'] = graph
    return invoke([ROOT / 'target/release/hwverify-structure'], request)

def graph(source, top='Top'):
    design = {'top': top, 'four_state': False, 'sources': [{'path': 'rtl.veryl', 'text': source}]}
    with tempfile.TemporaryDirectory() as temp:
        p = Path(temp) / 'design.json'; write(p, design)
        compiled = invoke([FRONTEND, p])
    return extract(design, compiled), design, compiled

class StructuralContracts(unittest.TestCase):
    def test_registered_boundary_and_native_source_locations(self):
        g, _, _ = graph('module Top (clk: input clock, a: input bit, y: output bit) { always_ff (clk) { y = a; } }')
        result = checker(g)
        self.assertEqual(result['status'], 'verified')
        self.assertTrue(g['sequential_cuts'])
        self.assertEqual(result['obligations'][0]['name'], 'Isolation.isolated')
        self.assertIn('source_location', result['obligations'][0])
        self.assertEqual(checker()['status'], 'unbound')

    def test_data_control_and_cancelled_expression_paths(self):
        for body, kind in [('assign y = a;', 'data'), ('assign y = a ^ a;', 'data'),
                           ('always_comb { if a { y = 1; } else { y = 0; } }', 'control')]:
            g, _, _ = graph('module Top (a: input bit, y: output bit) { ' + body + ' }')
            result = checker(g)
            self.assertEqual(result['status'], 'violated')
            witness = result['obligations'][0]['witness']
            self.assertEqual((witness[0]['from'], witness[-1]['to']), ('a', 'y'))
            self.assertEqual(witness[0]['kind'], kind)
            self.assertEqual(witness[0]['location']['file'], 'rtl.veryl')

    def test_alias_and_hierarchy_paths_and_cuts(self):
        alias = 'module Top (a: input bit, y: output bit) { var bridge: bit; assign bridge = a; assign y = bridge; }'
        g, _, _ = graph(alias); self.assertEqual(len(checker(g)['obligations'][0]['witness']), 2)
        src = 'module Child (a: input bit, y: output bit) { assign y = a; } module Top (a: input bit, y: output bit) { inst child: Child (a: a, y: y); }'
        g, _, _ = graph(src); result = checker(g)
        self.assertEqual(result['status'], 'violated')
        self.assertEqual(len(result['obligations'][0]['witness']), 3)
        mapped = checker(g, NATIVE.replace('o.y = y;', 'o.y = child.y;'))
        self.assertEqual(mapped['status'], 'violated')
        self.assertEqual(mapped['obligations'][0]['witness'][-1]['to'], 'child.y')
        src = 'module Child (clk: input clock, a: input bit, y: output bit) { always_ff (clk) { y = a; } } module Top (clk: input clock, a: input bit, y: output bit) { inst child: Child (clk: clk, a: a, y: y); }'
        g, _, _ = graph(src); self.assertEqual(checker(g)['status'], 'verified')

    def test_named_ports_share_connection_aliases_without_reversing_assignments(self):
        src = 'module Child (a: input bit, y: output bit) { assign y = a; } module Top (a: input bit, y: output bit, z: output bit) { inst child: Child (a: a, y: z); assign y = a; }'
        g, _, _ = graph(src)
        child_input = NATIVE.replace('i.a = a;', 'i.a = child.a;')
        result = checker(g, child_input)
        self.assertEqual(result['status'], 'violated')
        witness = result['obligations'][0]['witness']
        self.assertEqual([(e['from'], e['to']) for e in witness], [('child.a', 'a'), ('a', 'y')])
        self.assertEqual(witness[0]['kind'], 'connection')
        self.assertIn('location', witness[0])
        outputs = NATIVE.replace('observation y: bool;', 'observation y: bool; observation z: bool;').replace('from i.a;', 'from o.z;').replace('i.a = a; o.y = y;', 'o.z = child.y; o.y = z;').replace('observation y = s.q;', 'observation y = s.q; observation z = s.q;')
        self.assertEqual(checker(g, outputs)['status'], 'violated')
        reverse = outputs.replace('o.z = child.y; o.y = z;', 'o.z = z; o.y = child.y;')
        self.assertEqual(checker(g, reverse)['status'], 'violated')
        # A shared source does not make separate assignment destinations aliases.
        separate = outputs.replace('o.z = child.y; o.y = z;', 'o.z = z; o.y = y;')
        self.assertEqual(checker(g, separate)['status'], 'verified')
        registered = 'module Child (a: input bit, y: output bit) { assign y = a; } module Top (clk: input clock, a: input bit, y: output bit, z: output bit) { inst child: Child (a: a, y: z); always_ff (clk) { y = a; } }'
        cut, _, _ = graph(registered)
        self.assertEqual(checker(cut, child_input)['status'], 'verified')

    def test_ff_module_state_cuts_and_local_temporaries_fail_closed(self):
        module_state = 'module Top (clk: input clock, a: input bit, y: output bit) { var temp: bit; always_ff (clk) { temp = a; y = temp; } }'
        g, _, _ = graph(module_state)
        self.assertEqual(checker(g)['status'], 'verified')
        self.assertEqual([(e['from'], e['to']) for e in g['sequential_cuts']], [('a', 'temp'), ('temp', 'y')])
        for local in ['var temp: bit; temp = a;', 'let temp: bit = a;']:
            src = 'module Top (clk: input clock, a: input bit, y: output bit) { always_ff (clk) { ' + local + ' y = temp; } }'
            g, _, _ = graph(src)
            self.assertEqual(checker(g)['status'], 'unsupported')

    def test_coverage_unknown_construct_and_endpoint_fail_closed(self):
        src = 'module Top (a: input bit, y: output bit) { assign y = a; }'
        g, design, compiled = graph(src)
        missing = copy.deepcopy(compiled); missing['signals'].pop()
        self.assertEqual(checker(extract(design, missing))['status'], 'unsupported')
        incomplete = copy.deepcopy(g); incomplete['coverage'] = []
        self.assertEqual(checker(incomplete)['status'], 'unsupported')
        unbound = checker(g, NATIVE.replace('o.y = y;', 'o.y = absent;'))
        self.assertEqual(unbound['status'], 'unbound')
        # The compiler can lower concatenation; the source graph subset cannot.
        g, _, _ = graph('module Top (a: input bit, y: output bit) { assign y = {a}; }')
        self.assertEqual(checker(g)['status'], 'unsupported')
        with self.assertRaises(RuntimeError): checker(g, NATIVE.replace('from i.a;', 'from i.missing;'))
        with self.assertRaises(RuntimeError): checker(g, NATIVE.replace('o.y = y;', 'o.y = !y;'))

    def test_missing_artifact_blocks_native_overall_success(self):
        with tempfile.TemporaryDirectory() as temp:
            p = Path(temp) / 'contract.hwv'; p.write_text(NATIVE)
            result = invoke([ROOT / 'target/release/hwverify-rs', p, '--out', Path(temp) / 'report'], allowed=(0, 1, 2, 3))
            self.assertEqual(result['status'], 'unknown')
            self.assertEqual(result['structural']['status'], 'unbound')

    def test_public_project_cli_and_editor_unbound_diagnostic(self):
        with tempfile.TemporaryDirectory(prefix='structural project ') as temp:
            root = Path(temp)
            (root / 'contract.hwv').write_text(NATIVE)
            (root / 'rtl.veryl').write_text('module Top (clk: input clock, a: input bit, y: output bit) { always_ff (clk) { y = a; } }')
            write(root / 'project.json', {'version': 1, 'top': 'Top', 'sources': ['rtl.veryl'], 'specification': 'contract.hwv'})
            result = invoke([sys.executable, ROOT / 'conformance/celox-replay/structure_project.py', root / 'project.json', '--out', root / 'checked'])
            self.assertEqual(result['status'], 'verified')
            self.assertEqual(result['obligations'][0]['source_location']['uri'], str(root / 'contract.hwv'))
            native = invoke([ROOT / 'target/release/hwverify-rs', root / 'contract.hwv', '--structural-artifact', root / 'checked/structural-graph.json', '--out', root / 'native'])
            self.assertEqual(native['status'], 'binding_verified_no_examples')
            self.assertEqual(native['structural']['status'], 'verified')
            bad_graph, _, _ = graph('module Top (a: input bit, y: output bit) { assign y = a; }')
            write(root / 'bad-graph.json', bad_graph)
            failed = invoke([ROOT / 'target/release/hwverify-rs', root / 'contract.hwv', '--structural-artifact', root / 'bad-graph.json', '--out', root / 'native-fail'], allowed=(1,))
            self.assertEqual(failed['status'], 'structural_contract_failed')
            bad_graph['coverage'] = []; write(root / 'incomplete.json', bad_graph)
            unknown = invoke([ROOT / 'target/release/hwverify-rs', root / 'contract.hwv', '--structural-artifact', root / 'incomplete.json', '--out', root / 'native-unknown'], allowed=(3,))
            self.assertEqual(unknown['status'], 'unknown')
            worker = invoke([ROOT / 'target/release/hwverify-editor'], {'uri': 'contract.hwv', 'text': NATIVE, 'operation': 'prove', 'program': 'responses', 'out': str(root / 'editor')})
            self.assertEqual(worker['verification']['status'], 'unknown')
            diagnostic = next(d for d in worker['diagnostics'] if d['code'] == 'structural_unbound')
            self.assertEqual(diagnostic['severity'], 1)
            self.assertGreater(diagnostic['span']['line'], 0)
            with self.assertRaises(RuntimeError):
                invoke([ROOT / 'target/release/hwverify-replay'], {'version': 1, 'source': NATIVE, 'mode': 'search', 'goal': 'safety', 'depth': 2})

if __name__ == '__main__': unittest.main()
