"""Celox command adapter for the pinned sv-tests BaseRunner.

Metadata, mode selection, timeouts and result judgement belong to sv-tests.
"""

import json
import os
import subprocess
from pathlib import Path

from BaseRunner import BaseRunner


def known_missing_include(path, params):
    """Keep one reviewed malformed upstream fixture as a tool failure, not a skip."""
    issues = json.loads((Path(__file__).resolve().parents[1] / "input-issues.json").read_text())
    case = os.environ.get("CELOX_SV_TESTS_CASE")
    issue = issues["cases"].get(case)
    if not issue or params["should_fail"] != "0":
        return False
    root = Path(os.environ["TESTS_DIR"]).resolve().parent
    if path != root / issue["missing_include"]:
        return False
    revision = subprocess.check_output(["git", "-C", str(root), "rev-parse", "HEAD"], text=True).strip()
    return revision == issues["revision"]


class celox(BaseRunner):
    def __init__(self):
        super().__init__(
            name="celox",
            executable=os.environ["CELOX_SV_TESTS_DRIVER"],
            supported_features={"preprocessing", "parsing", "elaboration", "simulation"},
        )
        self.url = "https://github.com/celox-sim/celox"

    def prepare_run_cb(self, tmp_dir, params):
        self.cmd = [
            self.executable,
            "--mode", params["mode"],
            "--state-mode", os.environ["CELOX_SV_STATE_MODE"],
        ]
        if params["mode"] == "simulation":
            for variant in params["simvariants"]:
                self.cmd += ["--simvariant", variant]
        if params["top_module"]:
            self.cmd += ["--top", params["top_module"]]
        for define in params["defines"]:
            self.cmd += ["--define", define]
        for directory in params["incdirs"]:
            path = Path(directory).resolve()
            if not path.is_dir() and not known_missing_include(path, params):
                raise FileNotFoundError(f"include directory is unavailable: {path}")
            self.cmd += ["--include", str(path)]
        self.cmd += ["--"]
        for source in params["files"]:
            path = Path(source).resolve()
            if not path.is_file():
                raise FileNotFoundError(f"source file is unavailable: {path}")
            self.cmd.append(str(path))
