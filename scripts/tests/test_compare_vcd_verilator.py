"""Guard against accepting unlike VCD workloads as a valid comparison."""

import importlib.util
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


if __name__ == "__main__":
    unittest.main()
