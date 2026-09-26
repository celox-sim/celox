# Adapter-free arithmetic reproduction

`integer_edges.sv` reproduces two differences without Veryl or the Rust/VPI
verification adapters. Inputs come from plusargs to preserve runtime evaluation.

```sh
iverilog -g2012 -gstrict-expr-width -s Top -o /tmp/integer_edges.vvp integer_edges.sv
vvp /tmp/integer_edges.vvp +a=8000000000000000 +b=ffffffffffffffff +x=ff +z=01
verilator --binary --timing -Wno-fatal --top-module Top --Mdir /tmp/integer_edges-obj integer_edges.sv
/tmp/integer_edges-obj/VTop +a=8000000000000000 +b=ffffffffffffffff +x=ff +z=01
```

Observed on 2026-09-26:

| Simulator | Signed division | Port sum |
| --- | --- | --- |
| Verilator 5.052 | `0000000000000000` | `100` |
| Icarus 13.0 | `8000000000000000` | `000` |

These are recorded observations, not replacement expected values for the suite.

The [expanded mismatch review](../../MISMATCH_REVIEW.md) includes controlled
probes for all 11 distinct assertion disagreements in `mismatches/`.
