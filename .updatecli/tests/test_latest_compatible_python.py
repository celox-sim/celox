from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path

SCRIPT = Path(__file__).parents[1] / "scripts" / "latest_compatible_python.py"
SPEC = importlib.util.spec_from_file_location("latest_compatible_python", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class LatestCompatiblePythonTests(unittest.TestCase):
    def test_selects_latest_shared_stable_linux_x64_minor(self) -> None:
        cocotb_release = {
            "urls": [
                {
                    "filename": (
                        "cocotb-2.1.0-cp313-cp313-"
                        "manylinux_2_17_x86_64.whl"
                    ),
                    "packagetype": "bdist_wheel",
                },
                {
                    "filename": (
                        "cocotb-2.1.0-cp314-cp314-"
                        "manylinux_2_17_x86_64.whl"
                    ),
                    "packagetype": "bdist_wheel",
                },
                {
                    "filename": "cocotb-2.1.0-cp315-cp315-win_amd64.whl",
                    "packagetype": "bdist_wheel",
                },
            ]
        }
        python_manifest = [
            {
                "version": "3.15.0-beta.1",
                "stable": False,
                "files": [{"platform": "linux", "arch": "x64"}],
            },
            {
                "version": "3.14.4",
                "stable": True,
                "files": [{"platform": "linux", "arch": "x64"}],
            },
            {
                "version": "3.13.12",
                "stable": True,
                "files": [{"platform": "linux", "arch": "x64"}],
            },
        ]

        self.assertEqual(
            MODULE.latest_compatible_minor(cocotb_release, python_manifest),
            (3, 14),
        )

    def test_ignores_source_archives_and_non_linux_wheels(self) -> None:
        cocotb_release = {
            "urls": [
                {
                    "filename": "cocotb-2.1.0.tar.gz",
                    "packagetype": "sdist",
                },
                {
                    "filename": "cocotb-2.1.0-cp314-cp314-win_amd64.whl",
                    "packagetype": "bdist_wheel",
                },
            ]
        }

        self.assertEqual(MODULE.cocotb_linux_x64_minors(cocotb_release), set())

    def test_fails_when_no_compatible_release_exists(self) -> None:
        cocotb_release = {
            "urls": [
                {
                    "filename": (
                        "cocotb-2.1.0-cp314-cp314-"
                        "manylinux_2_17_x86_64.whl"
                    ),
                    "packagetype": "bdist_wheel",
                }
            ]
        }
        python_manifest = [
            {
                "version": "3.13.12",
                "stable": True,
                "files": [{"platform": "linux", "arch": "x64"}],
            }
        ]

        with self.assertRaisesRegex(RuntimeError, "no compatible stable CPython"):
            MODULE.latest_compatible_minor(cocotb_release, python_manifest)


if __name__ == "__main__":
    unittest.main()
