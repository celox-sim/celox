"""Guard against accepting unlike VCD workloads as a valid comparison."""

import importlib.util
import copy
from pathlib import Path
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "compare-vcd-verilator.py"
SPEC = importlib.util.spec_from_file_location("compare_vcd_verilator", SCRIPT)
BENCH = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BENCH)

WAVEFORM = '''$timescale 1 ns $end
$scope module Top $end
$var wire 1 ! clk $end
$var wire 1 " rst $end
$var wire 1 # en0 $end
$var wire 64 $ q0 [63:0] $end
$upscope $end
$enddefinitions $end
#0
0!
1"
1#
b10 $
#2
0!
#3
1!
b11 $
'''


class WaveformComparisonTests(unittest.TestCase):
    def validate(self, text):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "wave.vcd"
            path.write_text(text)
            return BENCH.validate_waveform(
                path, BENCH.expected_waveform(1, 1, "dense", False)
            )

    def test_equivalent_padding_and_redundant_records(self):
        short = self.validate(WAVEFORM)
        padded = self.validate(
            WAVEFORM.replace("b10 $", "b00010 $").replace("b11 $", "b00011 $")
        )
        no_empty_step = self.validate(WAVEFORM.replace("#2\n0!\n", ""))
        self.assertEqual(short["normalized_sha256"], padded["normalized_sha256"])
        self.assertEqual(short["normalized_sha256"], no_empty_step["normalized_sha256"])

    def test_mismatches_are_rejected(self):
        for before, after in (
            ("b10 $", "b1 $"),              # wrong initialization
            ("#3\n1!", "#3"),              # missing clock edge
            ("#3", "#4"),                  # wrong sample time
            ("b11 $", "bxx $"),            # unknown value in two-state run
            ("wire 64", "wire 63"),         # missing a traced bit
            ("q0 [63:0]", "other [63:0]"),  # wrong signal set
            ("1 ns", "1 ps"),              # unlike time scales
            ("$upscope", '$var wire 1 % clk $end\n$upscope'),
        ):
            with self.subTest(before=before, after=after):
                with self.assertRaises(ValueError):
                    self.validate(WAVEFORM.replace(before, after))

    def test_missing_header_is_rejected(self):
        with self.assertRaises(ValueError):
            self.validate("")

    def test_cached_fixture_must_match_without_overwriting_it(self):
        with tempfile.TemporaryDirectory() as temporary:
            folder = Path(temporary)
            BENCH.generate_fixture(folder, 3, False)
            original = (folder / "Top.sv").read_bytes()
            BENCH.generate_fixture(folder, 3, False, verify_only=True)
            for signals, full_width in ((4, False), (3, True)):
                with self.subTest(signals=signals, full_width=full_width):
                    with self.assertRaises(ValueError):
                        BENCH.generate_fixture(folder, signals, full_width, verify_only=True)
                    self.assertEqual(original, (folder / "Top.sv").read_bytes())

    def test_baseline_can_be_the_cached_binary_being_replaced(self):
        with tempfile.TemporaryDirectory() as temporary:
            folder = Path(temporary)
            current = folder / "celox-vcd"
            candidate = folder / "candidate"
            current.write_bytes(b"before")
            candidate.write_bytes(b"after")
            binaries = BENCH.cache_celox_binaries(folder, candidate, current)
            self.assertEqual(binaries["celox"].read_bytes(), b"after")
            self.assertEqual(binaries["baseline_celox"].read_bytes(), b"before")
            # Inputs may alias both destinations when swapping the two builds.
            swapped = BENCH.cache_celox_binaries(
                folder, binaries["baseline_celox"], binaries["celox"]
            )
            self.assertEqual(swapped["celox"].read_bytes(), b"before")
            self.assertEqual(swapped["baseline_celox"].read_bytes(), b"after")


class BuildReuseTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        binary = Path(self.temporary.name) / "celox-vcd"
        binary.write_bytes(b"cached executable")
        self.binary = binary
        self.manifest = {
            "options": {"signals": 256, "celox": None, "baseline_celox": None, "steps": 100},
            "source_sha256": {str(path.relative_to(BENCH.ROOT)): BENCH.digest(path)
                              for path in (BENCH.HARNESS, BENCH.RUST_BENCH, SCRIPT)},
            "rustc": "rustc version 1", "cxx": "g++ version 1", "verilator": "Verilator version 1",
            "git_head": "revision", "git_status": "", "git_diff_sha256": "clean",
            "environment": {"RUST_MIN_STACK": "67108864"},
            "build_commands": [["cargo", "bench", "--locked"]],
            "builds": [{"command": ["cargo", "bench", "--locked"], "elapsed_ns": 1}],
            "binaries": {str(binary): BENCH.digest(binary)},
        }

    def test_unchanged_builds_allow_different_measurement_options(self):
        current = copy.deepcopy(self.manifest)
        current["options"].update(steps=1000, repeats=7, cpu=1, cases=["idle"])
        BENCH.validate_reused_builds(self.manifest, current)

    def test_changed_recorded_build_inputs_are_rejected(self):
        for field in ("rustc", "cxx", "verilator", "git_head", "git_status", "git_diff_sha256"):
            with self.subTest(field=field):
                current = copy.deepcopy(self.manifest)
                current[field] += " changed"
                with self.assertRaisesRegex(ValueError, "omit --reuse-builds"):
                    BENCH.validate_reused_builds(self.manifest, current)
        for source in self.manifest["source_sha256"]:
            with self.subTest(source=source):
                current = copy.deepcopy(self.manifest)
                current["source_sha256"][source] = "changed"
                with self.assertRaisesRegex(ValueError, "omit --reuse-builds"):
                    BENCH.validate_reused_builds(self.manifest, current)
        for field, replacement in (("build_commands", [["cargo", "bench", "--release"]]),
                                   ("environment", {"RUSTFLAGS": "-C target-cpu=native"})):
            with self.subTest(field=field):
                current = copy.deepcopy(self.manifest)
                current[field] = replacement
                with self.assertRaisesRegex(ValueError, "omit --reuse-builds"):
                    BENCH.validate_reused_builds(self.manifest, current)

    def test_changed_build_options_are_rejected(self):
        for option, value in (("signals", 512), ("celox", "/another/binary"),
                              ("baseline_celox", "/another/baseline")):
            with self.subTest(option=option):
                current = copy.deepcopy(self.manifest)
                current["options"][option] = value
                with self.assertRaisesRegex(ValueError, "omit --reuse-builds"):
                    BENCH.validate_reused_builds(self.manifest, current)

    def test_incomplete_or_changed_build_history_is_rejected(self):
        for history in ([], [{"command": ["cargo", "bench", "--different-flags"]}]):
            with self.subTest(history=history):
                previous = copy.deepcopy(self.manifest)
                previous["builds"] = history
                with self.assertRaisesRegex(ValueError, "omit --reuse-builds"):
                    BENCH.validate_reused_builds(previous, self.manifest)

    def test_changed_cached_binary_is_rejected(self):
        self.binary.write_bytes(b"different executable")
        with self.assertRaisesRegex(ValueError, "omit --reuse-builds"):
            BENCH.validate_reused_builds(self.manifest, self.manifest)

    def test_old_manifests_and_missing_binaries_are_rejected(self):
        previous = copy.deepcopy(self.manifest)
        del previous["build_commands"]
        with self.assertRaisesRegex(ValueError, "omit --reuse-builds"):
            BENCH.validate_reused_builds(previous, self.manifest)
        self.binary.unlink()
        with self.assertRaisesRegex(ValueError, "omit --reuse-builds"):
            BENCH.validate_reused_builds(self.manifest, self.manifest)

    def test_compiler_environment_is_recorded_without_credentials(self):
        environment = {"RUSTFLAGS": "-C target-cpu=native", "CXXFLAGS": "-O2",
                       "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS": "-C opt-level=2",
                       "CARGO_PROFILE_BENCH_LTO": "false", "RUST_MIN_STACK": "67108864",
                       "CARGO_REGISTRY_TOKEN": "private", "UNRELATED_SECRET": "private"}
        actual = BENCH.build_environment(environment)
        self.assertEqual(actual, {key: value for key, value in environment.items() if value != "private"})


if __name__ == "__main__":
    unittest.main()
