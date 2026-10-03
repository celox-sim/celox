# Pinned FPGA timing preparation (not CPU timing results)

Workspace-local preparation completed 2026-10-02. No CPU RTL or repository dependency files were changed. No CPU synthesis, placement, board pin assignment, bitstream, or programming was performed. The SV CPU artifact is a mutable-source snapshot for syntax testing only.

## Tools

Official release: https://github.com/YosysHQ/oss-cad-suite-build/releases/tag/2026-10-02

Archive: `oss-cad-suite-linux-x64-20261002.tgz`, 746395724 bytes.
SHA-256: `d5002d4af8de694302754edc1e26787ae4d731f6318d007a43223a5e4b7f0d85`.
GitHub's release API digest was compared using sha256sum; verification passed. `release.json` retains the metadata. `logs/download.log` and `logs/checksum.log` retain installation evidence. The archive and extracted distribution are retained.

- Yosys: `0.69+185`, official binary reports git `fb1a2fdae-dirty`, Clang 21.1.8. The dirty label is upstream distribution provenance; this preparation did not modify that binary. The exact release archive and installed binary hashes identify it, rather than claiming a pristine source commit.
- nextpnr-ecp5: `nextpnr-0.11.1-47-ge2fe86b3`.
- Trellis data: bundled in the same hash-pinned distribution; individual database and timing files are hashed in `provenance-files.sha256.json`. No separate Trellis source commit is asserted.
- ECP5 exploration target: `--45k --package CABGA381 --speed 6`, corresponding to LFE5U-45F-6BG381I logic/package/speed assumptions. The industrial suffix and board electrical behavior are not independently qualified by this tool smoke.

## Isolated environment

Source `timing-prep/env.sh`. It exports only `FPGA_YOSYS`, `FPGA_NEXTPNR`, and `VERYL_TIMING_EMIT`; it does not change PATH, HOME, Rust variables, or solver discovery in the caller. Use quoted executable variables. Do not source the suite's broad environment script into proof processes. The official suite contains Z3 and other solvers; none was selected or invoked for this work. The nextpnr launcher isolates HOME to `tool-home` within its child process because the official GUI-enabled launcher otherwise attempts read-only home directory writes. Those initial warnings are corrected in current launcher/logs.

## Veryl emission

`emitter` is an independent utility with its own Cargo manifest, lockfile and target directory. It uses the existing proof frontend's exact patched analyzer and vendored metadata paths, plus registry Veryl emitter/parser 0.21.0. Emitter, parser and base analyzer provenance each report upstream commit `2e32d53f056eef323225a91079af92574d18f726`. Emitter crate SHA-256 is `f2870a8d50639ed33bfea64cc934b2e834f1a80809b7ee76c76c8a6dd8d6e244` (Cargo.lock). Proof analyzer original crate and patch hashes remain in the repository's `conformance/veryl-proof/dependencies.json`. Current analyzer/metadata Rust sources are also captured by the local provenance hash manifest.

Build from timing-prep:

    source ../recovered/tools/env.sh
    CARGO_TARGET_DIR="$PWD/emitter-target" cargo build --locked --offline --manifest-path emitter/Cargo.toml

The initial build fetched missing official registry crates. The locked offline rebuild passed. Initial frontend lockfile seeded dependency versions; adding emitter necessarily generated a separate final lockfile. No proof lockfile changed.

Emit current source (use fresh output after source proof is complete):

    source env.sh
    "$VERYL_TIMING_EMIT" ../hwverify-git-publish/conformance/veryl-symbolic/rv32i_pipeline.veryl /tmp/rv32i_pipeline.sv

The utility parses, runs analyzer passes and drains diagnostics, rejects any analyzer errors, then uses upstream Emitter (not a manual RTL translation). It omits the project module prefix and strips comments. The proof frontend allows special FF function diagnostics; this utility deliberately does not relax those diagnostics. Current CPU source emitted without those exceptions. This is not proof that the emitter preserves formal frontend semantics: emitted-vs-source equivalence remains future validation.

## Verification

`./smoke.sh` passed:

- Yosys and nextpnr version calls
- Upstream Veryl emission of `smoke/rv32i_pipeline.snapshot.veryl` (hash in `logs/rv32i-source.sha256`)
- Yosys SystemVerilog parse/hierarchy check, top RV32IPipeline
- A separate 8-bit tool-health pipeline was synthesized and run through nextpnr on 45k/CABGA381/speed6, seed 1, 100 MHz exploration constraint, out-of-context mode. This only demonstrates tool/database usability and does not establish CPU timing
- Locked offline emitter rebuild

`logs/nextpnr-help.log` and `logs/yosys-help.log` preserve actual installed help.
The negative SDC probe produced exit 125 and `Unsupported SDC command 'set_input_delay'`, retained in `logs/sdc-io-delay-negative.*`.

## Constraints and next phase limits

100 MHz is an exploration assumption, not a user-selected board requirement or guaranteed frequency. The raw core's large debug port bank must not be placed as a physical top. Add a separately reviewed registered harness, or obtain a genuine board pin map and interface budget. Do not invent pin locations. nextpnr out-of-context mode explicitly disables IO insertion and global promotion/routing, and is experimental; results from it cannot qualify a normal clock network or board IO. No final timing result should rely on this smoke configuration.

The installed SDC parser rejects set_input_delay, so simply adding generic input/output SDC budgets is not a complete supported solution. Register-to-register timing in a declared harness must be separated from unqualified external IO timing. Do not use --timing-allow-fail to assert success. Require timing reports, clock constraints, absence of unexpected unconstrained paths, and multi-seed physical runs for future CPU evaluation. Instruction cycle latency requires separate formal properties and explicit stall/memory assumptions; MHz alone does not prove instruction latency. No ecppack or device programming is needed for this phase.
