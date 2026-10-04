# Original verification baseline

These immutable reports cover the original 646 cases before the signed-bound
and X/Z expectation fixes and before dynamic output cases became rejection tests.
They retain all 9 Verilator and 6 Icarus disagreements discussed in
[the expectation review](../../MISMATCH_REVIEW.md).

- Verilator: 456 passed, 9 mismatch, 55 emission errors, 19 compile errors, 107 unsupported.
- Icarus: 448 passed, 6 mismatch, 60 emission errors, 130 compile errors, 2 runtime errors.

The [current reports](../README.md) include the fixes and the added shift regression.
