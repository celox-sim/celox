"""Celox command adapter for the pinned sv-tests BaseRunner.

Metadata, mode selection, timeouts and result judgement belong to sv-tests.
"""

import os
from pathlib import Path

from BaseRunner import BaseRunner


class celox(BaseRunner):
    def __init__(self):
        super().__init__(
            name="celox",
            executable=os.environ["CELOX_SV_TESTS_DRIVER"],
            supported_features={"preprocessing", "parsing", "elaboration", "simulation"},
        )
        self.url = "https://github.com/celox-sim/celox"

    def prepare_run_cb(self, tmp_dir, params):
        if params["simvariants"]:
            raise NotImplementedError("Celox does not support simulation variant plusargs")
        self.cmd = [
            self.executable,
            "--mode", params["mode"],
            "--state-mode", os.environ["CELOX_SV_STATE_MODE"],
        ]
        if params["top_module"]:
            self.cmd += ["--top", params["top_module"]]
        for define in params["defines"]:
            self.cmd += ["--define", define]
        for directory in params["incdirs"]:
            path = Path(directory).resolve()
            if not path.is_dir():
                raise FileNotFoundError(f"include directory is unavailable: {path}")
            self.cmd += ["--include", str(path)]
        self.cmd += ["--"]
        for source in params["files"]:
            path = Path(source).resolve()
            if not path.is_file():
                raise FileNotFoundError(f"source file is unavailable: {path}")
            self.cmd.append(str(path))
