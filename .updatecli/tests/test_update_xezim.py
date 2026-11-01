from __future__ import annotations

from contextlib import redirect_stdout
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).parents[1] / 'scripts/update_xezim.py'
SPEC = importlib.util.spec_from_file_location('update_xezim', SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class UpdateXezimTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        (self.root / 'nix/xezim').mkdir(parents=True)
        (self.root / 'flake.nix').write_text('xezim.url = "github:aionhw/xezim/0.11.3";\nother.url = "unchanged";\n')
        self.write_locks('0.11.3')

    def write_locks(self, tag):
        (self.root / 'flake.lock').write_text(json.dumps({'nodes': {'xezim': {
            'original': {'ref': tag}, 'locked': {'rev': 'a' * 40, 'narHash': 'sha256-test'},
        }, 'other': {'original': 'unchanged'}}}))
        (self.root / 'nix/xezim/Cargo.lock').write_text(
            f'[[package]]\nname = "xezim"\nversion = "{tag.removeprefix("v")}"\n')

    def contents(self):
        return {name: (self.root / name).read_bytes() for name in MODULE.FILES}

    def regenerate(self, command, **kwargs):
        self.assertEqual(kwargs['cwd'], self.root)
        if command[:3] == ('nix', 'flake', 'update'):
            self.assertIn('github:aionhw/xezim/0.11.4', (self.root / 'flake.nix').read_text())
            self.write_locks('0.11.4')
        elif command[:2] == ('nix', 'build'):
            return subprocess.CompletedProcess(command, 0, stdout='/nix/store/xezim-0.11.4\n')
        elif command[-1] == '-V':
            return subprocess.CompletedProcess(command, 0, stdout='xezim version 0.11.4\ngit unknown\n')
        return subprocess.CompletedProcess(command, 0)

    def test_updates_all_three_files_and_validates_before_reporting_change(self):
        before = self.contents()
        with patch.object(MODULE.subprocess, 'run', side_effect=self.regenerate) as run, redirect_stdout(io.StringIO()) as output:
            self.assertTrue(MODULE.update(self.root, '0.11.4'))
        after = self.contents()
        self.assertTrue(all(after[name] != before[name] for name in MODULE.FILES))
        commands = [call.args[0] for call in run.call_args_list]
        self.assertEqual(commands[:2], [('nix', 'flake', 'update', 'xezim'), ('bash', 'nix/xezim/update-lock.sh')])
        self.assertIn(('nix', 'flake', 'check', '--no-build', '--all-systems', '--no-update-lock-file'), commands)
        self.assertIn('.#xezim', commands[-2])
        self.assertEqual(commands[-1], ('/nix/store/xezim-0.11.4/bin/xezim', '-V'))
        self.assertIn('Nix build/version verified', output.getvalue())
        self.assertIn('other.url = "unchanged"', after['flake.nix'].decode())

    def test_unchanged_release_does_not_resolve_dependencies_or_open_patch_churn(self):
        with patch.object(MODULE.subprocess, 'run') as run, redirect_stdout(io.StringIO()) as output:
            self.assertFalse(MODULE.update(self.root, '0.11.3'))
        run.assert_not_called()
        self.assertEqual(output.getvalue(), '')

    def test_preview_reports_the_complete_change_without_mutation_or_build(self):
        before = self.contents()
        with patch.object(MODULE.subprocess, 'run') as run, redirect_stdout(io.StringIO()) as output:
            self.assertTrue(MODULE.update(self.root, '0.11.4', dry_run=True))
        run.assert_not_called()
        self.assertEqual(self.contents(), before)
        self.assertIn('flake.lock and Cargo.lock', output.getvalue())

    def test_failed_lock_generation_or_build_restores_all_files(self):
        for failing in [('bash', 'nix/xezim/update-lock.sh'), ('nix', 'build')]:
            before = self.contents()
            def fail(command, **kwargs):
                if command[:len(failing)] == failing:
                    raise subprocess.CalledProcessError(1, command)
                return self.regenerate(command, **kwargs)
            with self.subTest(failing=failing), patch.object(MODULE.subprocess, 'run', side_effect=fail), redirect_stdout(io.StringIO()) as output:
                with self.assertRaises(subprocess.CalledProcessError):
                    MODULE.update(self.root, '0.11.4')
            self.assertEqual(self.contents(), before)
            self.assertEqual(output.getvalue(), '')

    def test_wrong_built_version_and_incomplete_locks_cannot_report_success(self):
        before = self.contents()
        for incorrect in ['locks', 'version']:
            def broken(command, **kwargs):
                result = self.regenerate(command, **kwargs)
                if incorrect == 'locks' and command[:2] == ('bash', 'nix/xezim/update-lock.sh'):
                    self.write_locks('0.11.3')
                if incorrect == 'version' and command[-1] == '-V':
                    result.stdout = 'xezim version 0.11.3\n'
                return result
            with self.subTest(incorrect=incorrect), patch.object(MODULE.subprocess, 'run', side_effect=broken):
                with self.assertRaises(ValueError):
                    MODULE.update(self.root, '0.11.4')
            self.assertEqual(self.contents(), before)

    def test_rejects_branches_commits_prereleases_and_shell_input(self):
        for tag in ['main', 'a' * 40, '0.12.0-rc.1', '0.12.0; echo bad', '0.12.0\n']:
            with self.subTest(tag=tag), self.assertRaises(ValueError):
                MODULE.update(self.root, tag)


if __name__ == '__main__':
    unittest.main()
