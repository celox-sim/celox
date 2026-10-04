# Usage and development

Run commands from the repository root. Use Rust 1.98.1 and Python 3.12, with
locked Cargo dependencies. Z3 5.1.0 is the recorded compatibility-test version;
set `Z3_BIN` for tests using the external-solver route. See the
[trust boundary](trust.md) before interpreting a successful result.

## Inputs and outputs

```sh
cargo build --release --locked
# External solver route:
target/release/hwverify-rs examples/cpu.json --z3 /path/to/z3 --out /tmp/new-cpu
# Validate only; --emit-json also implies --check:
target/release/hwverify-rs examples/memory_increment.hwv --check --out /tmp/new-check
target/release/hwverify-rs examples/array_sum.hwv --emit-json /tmp/array-sum.json --out /tmp/new-lowering
# Finite-only route, with source-authored proposals:
HWVERIFY_SOLVER=finite target/release/hwverify-rs audit/lemma_candidates/counter.hwv --out /tmp/new-counter
# Attach a native lemma module to an existing design:
HWVERIFY_SOLVER=finite target/release/hwverify-rs design.json --lemmas lemmas.hwv --out /tmp/new-proof
```

Replace illustrative `design.json`/`lemmas.hwv` with your files. Use fresh output
directories: the CLI defaults to `results` and writes reports/query evidence.
`.hwv` is detected by extension; `--format hwv|json` overrides detection. The
[native lemma guide](lemmas.md) includes complete examples and failure meanings.

| Exit | Meaning |
|---|---|
| 0 | Verification succeeded, or validation-only succeeded when requested |
| 1 | Counterexample |
| 2 | Invalid input or tool error |
| 3 | Unknown or inadequate contract |

`--check` success does not establish a theorem. Read `report.json` and the
individual obligations; do not convert Unknown, a failed nonvacuity check, or
an unfinished child into success. `.smt2`, `.out` and diagnostic JSON record the
actual queries and attempts.

`HWVERIFY_SOLVER=finite` selects the bounded Bool/BV solver with no Z3 fallback.
`--finite-search-hint query|sat|unsat` (or `HWVERIFY_FINITE_SEARCH_HINT`) affects
search order only. The default `query` follows the obligation's expected result.
A hint does not assert that result. Quantified examples have a separate solver
route and are not general finite-backend quantifier support.

## Required checks

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
Z3_BIN=/path/to/z3 cargo test --release --workspace --locked
cargo build --release --locked
HWVERIFY_BIN="$PWD/target/release/hwverify-rs" python3 -m unittest discover -s scripts -p 'test_json_to_hwv.py'
python3 scripts/check_docs.py
```

Use the release profile for the complete solver suite: some debug-profile tests
need a larger test-thread stack. Keep proof limits unchanged. The Markdown check
covers local links and fragments in all tracked Markdown, including the sealed
historical records retained unchanged.

The mandatory hosted gates are defined in
[the workflow](../.github/workflows/veryl-fv.yml). They include the original Veryl
corpus, symbolic RTL refinement, native lemma diagnostics, selected RV32I, fixed
automatic-proof controls and original-formula mutants. For their exact commands,
dependency preparation and evidence locations, use the [audit index](../audit/README.md).
Do not replace these gates with an old report or a reduced smoke run.

## Source layout

`crates/ir` validates typed models; `crates/syntax` uses the parol grammar and
source spans; `crates/solver` owns finite/Z3 solving and checked proof rules;
`crates/verify` builds refinement/specification obligations; `crates/cli` provides
the command line. [SIR lifting](../crates/sir/README.md) connects imported RTL.
`examples/` and `audit/` contain generators, independent interpreters and tests.

`python3 scripts/json_to_hwv.py INPUT.json OUTPUT.hwv` is a migration printer.
Its output still passes the same semantic validation as handwritten source.
Keep generated runs outside tracked evidence directories. Publish new release
archives as release assets only when needed; ordinary source and test changes
belong in Git. Existing sealed evidence is indexed separately and is never proof
authority for a new run.

## Editor and language server

The initial editor integration supports Linux/macOS, Python 3.10+ and VS Code
1.100+. Build the isolated Rust worker and install the pinned client dependencies:

```sh
cargo build --release --locked --bin hwverify-editor
cd editor/vscode
npm ci --ignore-scripts
cd ../..
code --extensionDevelopmentPath="$PWD/editor/vscode" "$PWD"
```

In the opened Extension Development Host, set these **User** settings to absolute
paths, then reload that window. The extension runs only in a trusted workspace.

```json
{
  "hwverify.python": "python3",
  "hwverify.server": "/absolute/path/to/hwverify/editor/server.py",
  "hwverify.worker": "/absolute/path/to/hwverify/target/release/hwverify-editor"
}
```

Open [`counter.hwv`](../audit/lemma_candidates/counter.hwv). Name, type and width
errors appear as you edit; these debounced checks do not run a solver. Definition,
completion and hover use the current unsaved buffer. Click **Check target** above
`counter_step`, or use **hwverify: Check Target or Lemma** in the command palette.
Leave the branch field empty for this example. **Check prefix through step**
checks the program through that lemma, including earlier instructions; it does
not certify the unexecuted remainder. The progress notification can cancel the
request. Results appear in the **hwverify proof results** output channel, with
source diagnostics and lemma hovers showing validity, guard use, declared
context/dependencies and source-variable counterexamples.

For a standalone `lemmas` file, use **hwverify: Associate Lemma Sidecar with Model**
to select the underlying `.hwv` design or canonical JSON model. Association is
session-local and must be selected again after reopening the document. Unsaved
associated models take precedence over disk. Disk changes, closing an unsaved
model, changing dependencies, and replacing the worker invalidate stored status.
Navigation across a sidecar/model boundary supports `.hwv` state/input declarations;
JSON models support validation and proof execution but not cross-file definition
locations. Navigation is conservative lexical assistance during incomplete edits;
ambiguous definitions are omitted. Rust parsing and validation decide validity.

Proof execution is explicit, finite-only and uses the normal budgets. It checks
one matching target or a program prefix, not the whole design or its global
obligations. A target with multiple matching decomposition branches requires an
explicit zero-based index; successful checking of one branch does not prove the
other branches. Targets must match an exact current query RHS (which can differ
from the unsplit next-state expression after guarded decomposition).
Specifications receive editing diagnostics. A native `responses` block offers
**Check implementation responses and safety**, using the full specification checker
with source-linked obligations. Native lemma execution still requires a design. Worker concurrency is limited to two; edit analysis has a 20-second wall limit
and explicit proof execution a 120-second wall limit. Exceeding either limit
retains no verdict. LSP messages and individual worker requests are limited to
64 MiB; larger disk models use streamed, hash-checked private snapshots.
There is no persistent proof cache: status is bound
to the complete document, model bytes, version and worker hash, and every proof
request recreates all handles. Unknown, cancellation, stale results and saved
reports never supply a trusted handle. See [lemma status meanings](lemmas.md#editor-proof-status).

Other LSP clients can launch `python3 editor/server.py --worker /absolute/path/to/hwverify-editor`.
The server uses stdio framing, UTF-16 positions and incremental text synchronization.
`workspace/executeCommand` accepts `hwverify.setBase` with
`[{"uri":"file:///lemmas.hwv","baseUri":"file:///model.json"}]` (null clears it), or
`hwverify.prove` with `[{"uri":"file:///design.hwv","program":"counter_step"}]` and
optional `step` and `branch`. Standard `$/cancelRequest`, shutdown and exit are
supported. Returned identities and reports are diagnostic data, not proof APIs.
Run the real-worker protocol regressions with `python3 -m unittest editor.test_server -v`.


### Extension Host integration tests

The required editor CI gate also uses Microsoft's official
[`@vscode/test-electron` runner](https://code.visualstudio.com/api/working-with-extensions/testing-extension#advanced-setup-your-own-runner),
pinned to 2.5.2, with VS Code 1.100.3. The runner creates a disposable workspace
from the real counter example and an isolated user profile configured for the
real Python server and Rust worker. Tests run through VS Code's Extension Host
and language-provider APIs: activation, unsaved Unicode/CRLF edits, diagnostics and
clearing, definition/completion/hover, CodeLens registration, the checked proof
command/result, actual bounded Unknown, cancellation, stale-result rejection and
per-target witness attribution.
They do not simulate mouse/keyboard interaction or assert rendered pixels.

```sh
cargo build --release --locked --bin hwverify-editor
npm ci --prefix editor/vscode --ignore-scripts
# Linux, with Xvfb and Electron's GTK/NSS/GBM/audio dependencies installed:
xvfb-run -a npm test --prefix editor/vscode
# On a desktop with a display, the same official runner can be launched directly:
npm test --prefix editor/vscode
```

The first run downloads the pinned VS Code build from Microsoft's distribution
service. Missing display dependencies or a failed download cause a failing test;
CI never silently skips this gate. Test stages are logged, and failures dump the
isolated Extension Host and language-client logs (including server stderr).
Transport failure stops the language client; it does not silently restart to
make a failing test pass. Reload the window after fixing a connection failure.
Fast Python protocol, pinned JavaScript JSON-RPC integration and client-wiring
tests remain separate. The runner uses its standard isolated test-host flags, including its
own Electron sandbox flags; these do not change the production extension's
workspace-trust requirement.

Extension consumers can invoke `hwverify.prove` with explicit `branch: 0` (or
`branch: null` for a unique match) to avoid the branch-selection dialog and receive
the diagnostic result. The activated extension also exposes
`checkProof(options, cancellationToken)` for clients providing their own UI; it
uses the same live language client and checked backend. `onProofStarted(listener)`
reports the real worker startup with its request identity and document version;
tests interrupt in that notification callback rather than guessing a startup
delay. The underlying `hwverify/proofStarted` notification is lifecycle data only,
not a solver verdict. Checked hovers include the exact proof request identity.
Neither entry point
accepts saved reports as proof authority.
