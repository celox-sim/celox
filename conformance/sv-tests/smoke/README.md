# CPU execution smoke tests

These tests execute programs on the pinned cores. They are separate from the
upstream sv-tests core verdicts, which currently select elaboration. Results do
not update `expected.tsv` or the public conformance snapshots.

## fx68k

Use the fx68k sources and microcode ROMs from the checkout prepared by
`conformance/sv-tests/prepare_cores.py`. At the pinned sv-tests revision, fx68k is
`0602ee4627b10f301298f2673d826cdd6baa9327`. The upstream RTL and ROM files are
used unchanged.

The testbench supplies reset, alternating phase enables, a zero-wait-state bus,
and reset vectors for a tiny ROM program. The program writes `0x1234` to byte
address `0x1000`. An unexpected write, byte lane or timeout is fatal; successful
execution prints `WRITE 001000 1234` and finishes.

Run from the repository root, with a prepared checkout at `work/sv-tests-upstream`:

```sh
cargo build --locked -p celox-vpi --features systemverilog --bin celox
repo_root="$PWD"
rtl_root="$repo_root/work/sv-tests-upstream/third_party/cores/fx68k"
smoke_root="$repo_root/target/fx68k-smoke"
mkdir -p "$smoke_root"
cp "$rtl_root/microrom.mem" "$rtl_root/nanorom.mem" "$smoke_root/"
(
  cd "$smoke_root"
  "$repo_root/target/debug/celox" run --top Top \
    --diagnostic inferred_latch=warn \
    "$rtl_root/fx68k.sv" "$rtl_root/fx68kAlu.sv" "$rtl_root/uaddrPla.sv" \
    "$repo_root/conformance/sv-tests/smoke/fx68k.sv"
)
```

Compare the same testbench with Verilator (the locked environment uses 5.052):

```sh
verilator --binary --timing -j 2 --top-module Top \
  -Wno-fatal -Wno-BLKANDNBLK --no-assert-case \
  --Mdir "$smoke_root/reference" \
  "$rtl_root/fx68k.sv" "$rtl_root/fx68kAlu.sv" "$rtl_root/uaddrPla.sv" \
  "$repo_root/conformance/sv-tests/smoke/fx68k.sv"
(cd "$smoke_root" && ./reference/VTop)
```

Both executions must exit successfully and print the same write signature.
This checks reset, instruction fetch, instruction execution and an external word
write. It is not an ISA conformance test or a performance measurement.

`inferred_latch=warn` enables the existing retained-assignment implementation for
the upstream CCR table's one-hot `unique case` without a default. The ordinary
row decoder's complete case after a local assignment must compile without a
latch diagnostic in two-state mode. Global diagnostic defaults are unchanged.

Verilator's aggregate blocking/nonblocking diagnostic is disabled because the
upstream design drives independent members of an unpacked structure from
different processes. Its unique-case assertions are disabled because unused
ALU control values violate those checks during startup. The testbench's explicit
`$fatal` checks stay enabled. These settings are part of the smoke-test
configuration, not claims that every upstream assertion passes.

For a negative control, repeat the Celox command with `-G ROM_VALUE=48879`
(`0xbeef`). The CPU must write that changed ROM operand and the testbench must
fail with `unexpected CPU write`. A compile error or timeout does not satisfy
this check. The Verilator equivalent is `-GROM_VALUE=48879` with a separate
`--Mdir`, followed by the same expected failing execution.
