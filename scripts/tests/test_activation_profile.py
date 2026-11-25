import contextlib
import importlib.util
import io
import json
import tempfile
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location('activation_profile', Path(__file__).parents[1] / 'activation-profile.py')
tool = importlib.util.module_from_spec(spec)
spec.loader.exec_module(tool)
spec = importlib.util.spec_from_file_location('rtlmeter_profile', Path(__file__).parents[1] / 'rtlmeter-profile.py')
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)


def counter(kind, measure, count, subject=''):
    return {'kind': kind, 'subject': subject, 'measure': measure, 'count': count}


def profile(quiet, dirty):
    return {
        'version': 1,
        'counters': [
            counter('slot', 'runs', 2000),
            counter('domain_judge', 'judged', 2000, 'simple'),
            counter('domain_judge', 'quiet', quiet, 'simple'),
            counter('domain', 'eval_apply', 1000, 'tb.clk'),
            counter('settle', 'entered', 3000),
            counter('settle', 'dirty', dirty),
            counter('settle_call', 'requests', 3000, 'generate/slot_end.rs:120'),
        ],
        'settle_partitions': {'settles': 10, 'runs': [10, 4], 'changed': [10, 1]},
    }


class ActivationProfileTests(unittest.TestCase):
    def run_tool(self, *args):
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            tool.main(list(args))
        return out.getvalue()

    def write(self, directory, name, data, cycles=None):
        path = Path(directory) / name
        path.mkdir()
        (path / 'activation.json').write_text(json.dumps(data))
        if cycles:
            (path / '_rtlmeter_cycles.txt').write_text(f'{cycles}\n')
        return str(path / 'activation.json')

    def test_show_normalizes_per_clock_and_reports_quiet_judgments(self):
        with tempfile.TemporaryDirectory() as directory:
            path = self.write(directory, 'a', profile(1100, 1500), cycles=1000)
            text = self.run_tool('show', path)
        self.assertIn('per RTL clock = /1000 (rtlmeter)', text)
        self.assertIn('55.0% of the simple domain judgments found no edge (1.100/clock)', text)
        self.assertIn('the settle ran in 50.0% of its 3.000 entries per clock', text)
        self.assertIn('partition runs/clock', text)
        self.assertRegex(text, r'domain_judge\s+quiet\s+1100\s+1\.100\s+55\.0%\s+simple')

    def test_show_without_cycles_uses_the_most_frequent_domain(self):
        with tempfile.TemporaryDirectory() as directory:
            path = self.write(directory, 'a', profile(1100, 1500))
            text = self.run_tool('show', path)
        self.assertIn('/1000 (runs of domain tb.clk)', text)

    def test_diff_lists_the_largest_changes_per_clock_first(self):
        with tempfile.TemporaryDirectory() as directory:
            before = self.write(directory, 'a', profile(1100, 1500))
            after = self.write(directory, 'b', profile(100, 1400))
            text = self.run_tool('diff', before, after, '--cycles', '1000')
        rows = [line for line in text.splitlines() if line.startswith(('domain_judge', 'settle '))]
        self.assertTrue(rows[0].startswith('domain_judge'), rows)
        self.assertRegex(rows[0], r'quiet\s+1\.100\s+0\.100\s+-1\.000')
        self.assertRegex(rows[1], r'dirty\s+1\.500\s+1\.400\s+-0\.100')

    def test_native_functions_and_roles_from_perf_map_symbols(self):
        self.assertEqual(runner.function_of('eval_comb.eval_comb.bb12'), 'eval_comb')
        self.assertEqual(runner.function_of('scheduler_callee[3] domain a.b.clk eval_only.scheduler_callee[3].bb7'),
                         'scheduler_callee[3] domain a.b.clk eval_only')
        self.assertEqual(runner.function_of('lane[0].task[1].lane[0].task[1].prologue'), 'lane[0].task[1]')
        self.assertEqual(runner.role_of('scheduler_callee[3] domain a.b.clk eval_only'), 'domain a.b.clk eval_only')
        table = {('domain', 'a.b.clk', 'eval_only'): 10, ('settle', '', 'dirty'): 4,
                 ('kernel', '7: `always`', 'runs'): 3}
        self.assertEqual(runner.activations_of('domain a.b.clk eval_only', table), 10)
        self.assertEqual(runner.activations_of('eval_comb_chunk[2]', table), 4)
        self.assertEqual(runner.activations_of('kernel 7: `always`', table), 3)
        self.assertIsNone(runner.activations_of('scheduler: domain waves', table))

    def test_attribution_scales_samples_to_the_counted_execute_phase(self):
        with tempfile.TemporaryDirectory() as directory:
            out = Path(directory)
            (out / 'profiled').mkdir()
            (out / 'profiled' / 'activation.json').write_text(json.dumps(profile(1100, 1500)))
            summary = {'profiled': {'cycles': 1000}, 'sampled': {'execute': {'instructions': 40000}},
                       'attribution': {'functions': {'scheduler_callee[0] domain tb.clk eval_apply': 3000,
                                                     'scheduler: domain waves': 900},
                                       'host': {'memcpy': 100}, 'blocks': {}}}
            rows = {row['role']: row for row in runner.attribution_rows(summary, out)}
        self.assertAlmostEqual(rows['domain tb.clk eval_apply']['perClock'], 30.0)
        self.assertAlmostEqual(rows['domain tb.clk eval_apply']['perActivation'], 30.0)
        self.assertAlmostEqual(rows['scheduler: domain waves']['perClock'], 9.0)
        self.assertAlmostEqual(rows['(host code)']['perClock'], 1.0)

    def test_rejects_unknown_versions(self):
        with tempfile.TemporaryDirectory() as directory:
            path = self.write(directory, 'a', {'version': 2})
            with self.assertRaises(SystemExit):
                self.run_tool('show', path)


if __name__ == '__main__':
    unittest.main()
