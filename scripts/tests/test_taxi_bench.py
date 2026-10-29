import copy
import importlib.util
import os
import sys
from unittest.mock import patch
from pathlib import Path
import tempfile
import time
from types import ModuleType, SimpleNamespace
import unittest

spec = importlib.util.spec_from_file_location('taxi_bench', Path(__file__).resolve().parents[1] / 'run-taxi-bench.py')
taxi = importlib.util.module_from_spec(spec)
spec.loader.exec_module(taxi)

clock_spec = importlib.util.spec_from_file_location('taxi_bench_clock', Path(__file__).resolve().parents[1] / 'taxi_bench_clock.py')
clock = importlib.util.module_from_spec(clock_spec)
clock_spec.loader.exec_module(clock)


class TaxiResultsTests(unittest.TestCase):
    def parse(self, text, expected=1):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'result.xml'
            path.write_text(text)
            return taxi.read_results(path, expected)

    def test_results_require_complete_unique_passing_finite_tests(self):
        test = '<testcase classname="tb" name="frame" time="0.5" sim_time_ns="100"/>'
        good = '<testsuites><testsuite>' + test + '</testsuite></testsuites>'
        self.assertEqual(self.parse(good), {'body_seconds': .5, 'simulation_ns': {'tb/frame': 100}})
        bad = [good.replace(test, ''), good.replace(test, test * 2),
               good.replace('time="0.5"', 'time="NaN"'), good.replace('time="0.5"', 'time="-1"'),
               good.replace('sim_time_ns="100"', 'sim_time_ns="inf"'),
               good.replace('sim_time_ns="100"', 'sim_time_ns="0"'),
               good.replace('name="frame"', 'name=""'), good.replace(' sim_time_ns="100"', '')]
        bad += [good.replace('/>', f'><{tag}/></testcase>') for tag in ['failure', 'error', 'skipped']]
        for text in bad:
            with self.subTest(text=text), self.assertRaises((ValueError, KeyError)):
                self.parse(text)
        with self.assertRaises(ValueError):
            self.parse(good.replace(test, test * 2), expected=2)

    def test_pair_requires_same_test_names_and_simulation_time(self):
        pair = {e: {'simulation_ns': {'tb/frame': 100.0}} for e in taxi.ENGINES}
        taxi.check_pair(pair)
        for times in [{'tb/other': 100}, {'tb/frame': 101}, {}]:
            bad = copy.deepcopy(pair)
            bad['verilator']['simulation_ns'] = times
            with self.assertRaises(ValueError):
                taxi.check_pair(bad)

    def test_monotonic_hook_is_local_and_ignores_wall_clock_reversal(self):
        regression = SimpleNamespace(time=time)
        package = ModuleType('cocotb')
        package.__version__ = '2.0.1'
        package.regression = regression
        with patch.dict(sys.modules, cocotb=package), patch.object(time, 'time', side_effect=[20, 10]), patch.object(time, 'perf_counter', side_effect=[2, 3]):
            original = time.time
            clock.configure([])
            begin = regression.time.time()
            self.assertEqual(regression.time.time() - begin, 1)
            self.assertIs(time.time, original)
            self.assertEqual(time.time(), 20)
            self.assertEqual(time.time(), 10)
        package.__version__ = 'other'
        with patch.dict(sys.modules, cocotb=package), self.assertRaises(RuntimeError):
            clock.configure([])

    def test_source_verification_rejects_wrong_revision_modified_and_extra_files(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for args in [('init', '--quiet'), ('config', 'user.name', 'fixture'),
                         ('config', 'user.email', 'fixture@example.invalid')]:
                taxi.git(root, *args)
            source = root / 'top.sv'
            source.write_text('module top; endmodule')
            taxi.git(root, 'add', '.')
            taxi.git(root, '-c', 'commit.gpgsign=false', 'commit', '--quiet', '-m', 'fixture')
            pinned = taxi.git(root, 'rev-parse', 'HEAD')
            with patch.dict(taxi.MANIFEST, revision=pinned):
                taxi.verify_source(root)
                source.write_text('module changed; endmodule')
                with self.assertRaises(ValueError):
                    taxi.verify_source(root)
                taxi.git(root, 'checkout', '--', 'top.sv')
                (root / 'untracked_helper.py').write_text('')
                with self.assertRaises(ValueError):
                    taxi.verify_source(root)
            with patch.dict(taxi.MANIFEST, revision='0' * 40), self.assertRaises(ValueError):
                taxi.verify_source(root)

    def test_process_failure_and_timeout_keep_raw_logs(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with self.assertRaises(RuntimeError):
                taxi.execute([sys.executable, '-c', 'print("failed", flush=True); exit(7)'],
                             root, dict(os.environ), root / 'failure.log', 5)
            self.assertIn('failed', (root / 'failure.log').read_text())
            with self.assertRaises(TimeoutError):
                taxi.execute([sys.executable, '-c', 'import time; time.sleep(10)'],
                             root, dict(os.environ), root / 'timeout.log', .05)
            self.assertTrue((root / 'timeout.command.json').exists())

    def test_all_pinned_configurations_share_sources_parameters_and_timescale(self):
        self.assertEqual(sum(c['tests'] for c in taxi.MANIFEST['cases'].values()), 472)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for name, case in taxi.MANIFEST['cases'].items():
                output = root / name
                output.mkdir()
                commands, _, _ = taxi.build_commands(case, root, Path('celox'), output, root / 'libs', root / 'share', 'verilator')
                native, reference = commands['celox'][0], commands['verilator'][0]
                native_sources = [p for p in native if str(p).endswith('.sv')]
                reference_sources = [p for p in reference if str(p).endswith('.sv')]
                self.assertEqual(native_sources, reference_sources)
                self.assertIn('--two-state', native)
                self.assertEqual(native[native.index('-O') + 1], 'O1')
                for key, value in case['parameters'].items():
                    self.assertIn(f'{key}={value}', native)
                    self.assertIn(f'-G{key}={value}', reference)
                if name == 'rx32':
                    for args in [native, reference]:
                        self.assertEqual(args[args.index('--timescale') + 1], '1ns/1fs')


if __name__ == '__main__':
    unittest.main()
