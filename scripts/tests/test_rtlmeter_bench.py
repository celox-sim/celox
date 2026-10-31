import importlib.util
import tempfile
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location('rtlmeter', Path(__file__).parents[1] / 'run-rtlmeter-bench.py')
bench = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bench)


class RtlmeterTests(unittest.TestCase):
    def test_success_requires_program_output_and_clock_count(self):
        outcome = {'exitCode': 0, 'timedOut': False}
        text = 'Hello Friend!\nTEST PASSED\n'
        self.assertEqual(bench.accept(text, '5392\n', outcome, 'Hello Friend!'), 5392)
        for output, count, result in [
            ('TEST PASSED\n', '5392', outcome),
            (text + 'TEST PASSED\n', '5392', outcome),
            (text + 'TEST FAILED\n', '5392', outcome),
            (text + '%Error: failed assertion\n', '5392', outcome),
            (text, '0', outcome), (text, '', outcome),
            (text, '5392', {'exitCode': 1, 'timedOut': False}),
            (text, '5392', {'exitCode': 124, 'timedOut': True}),
        ]:
            with self.subTest(output=output, count=count, outcome=result), self.assertRaises(ValueError):
                bench.accept(output, count, result, 'Hello Friend!')

    def test_verilator_times_build_and_run_in_both_modes(self):
        spec = {'top': 'tb', 'includes': [Path('/source/rtl')], 'defines': {'CLOCK': 'tb.clk'}}
        binaries = {engine: engine for engine in bench.ENGINES}
        for state in bench.STATES:
            commands = bench.commands('verilator', state, spec, Path('/source/files.f'), Path('/build'), binaries, ['+limit=1'])
            self.assertEqual(len(commands), 2)
            self.assertIn('--fourstate' if state == 'four-state' else '--no-fourstate', commands[0])
            self.assertEqual(commands[1], [Path('/build/sim'), '+limit=1'])
            self.assertIn('-DCLOCK=tb.clk', commands[0])
        with self.assertRaisesRegex(ValueError, 'no global two-state'):
            bench.commands('xezim', 'two-state', spec, Path('/source/files.f'), Path('/build'), binaries, [])

    def test_cold_build_directory_cannot_be_reused(self):
        with tempfile.TemporaryDirectory() as directory:
            with bench.build_directory(Path(directory), 'sample') as first:
                (Path(first) / 'stale.o').write_text('previous artifact')
                with bench.build_directory(Path(directory), 'sample') as second:
                    self.assertNotEqual(first, second)
                    self.assertEqual(list(Path(second).iterdir()), [])
            self.assertFalse(Path(first).exists())
            self.assertFalse(Path(second).exists())

    def test_external_build_cleanup_also_runs_on_failure(self):
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaises(RuntimeError):
                with bench.build_directory(Path(directory), 'failed') as build:
                    (Path(build) / 'partial.o').write_text('incomplete')
                    raise RuntimeError('compiler failed')
            self.assertFalse(Path(build).exists())

    def test_no_inherited_simulator_settings_or_persistent_cache(self):
        from unittest.mock import patch
        with patch.dict('os.environ', {'XEZIM_INIT_ZERO': '1', 'CELOX_THREADS': '32', 'VERILATOR_ROOT': '/wrong'}):
            env = bench.environment()
        self.assertNotIn('XEZIM_INIT_ZERO', env)
        self.assertNotIn('CELOX_THREADS', env)
        self.assertNotIn('VERILATOR_ROOT', env)
        self.assertEqual(env['XEZIM_NO_CACHE'], '1')
        self.assertEqual(env['XEZIM_NO_NATIVE_CACHE'], '1')

    def test_timeout_and_failed_build_never_run_model(self):
        from unittest.mock import patch
        with patch.object(bench, 'execute', return_value={'exitCode': 124, 'timedOut': True, 'elapsedNs': 100}) as execute:
            result = bench.run_commands([['compiler'], ['model']], Path('/cwd'), Path('/logs'), 1, 0)
        self.assertTrue(result['timedOut'])
        self.assertEqual(execute.call_count, 1)


if __name__ == '__main__':
    unittest.main()
