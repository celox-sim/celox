# Verification

<script setup>
import VerificationResults from '../.vitepress/components/VerificationResults.vue'
</script>

Celox results for the pinned [sv-tests](https://github.com/chipsalliance/sv-tests)
corpus. Switch between **Main tests**, **Cores**, and **Imported**, then search
by language feature or test-suite tag. Only Celox results are displayed.
The layout follows the [upstream dashboard](https://chipsalliance.github.io/sv-tests-results/).

<VerificationResults />

## Reading the results

**Main tests** come from the current checkout's reviewed `expected.tsv` on master.
They are a baseline assembled from reviewed verdicts, rather than a fresh full run.
The source link identifies a commit containing that baseline; it is not a single
simulator build or measurement date.
Main baseline changes appear on the next website deployment.

**Cores** and **Imported** are measured by the official upstream runner in a
separate daily/manual workflow, outside PR gates. The page starts with checked-in
measurements and loads newer complete daily results when available. Each group
retains its own source revision and measurement time. Collection failures do not
replace the last complete public result. Raw results and logs are retained as
workflow artifacts for 14 days.

Skipped cases are excluded from the executed denominator. A dash means there
are no executed cases, not a zero-percent score. Some core fixtures exclude Celox
through upstream's `compatible-runners` metadata; those remain skipped and visible.
The pinned CVA6 testbench has two unavailable include paths. Its failure is
reported as an upstream input issue, not language support.

Tags overlap: a case can contribute to several tag rows but counts only once in
the overall totals. Two-state and four-state results remain separate. Parsing
and elaboration passes do not establish simulation correctness. Negative tests
can pass because of an unsupported construct. Some imported cases check process
success without validating their printed checks, so an official pass can still
contain `Failed` output. These scores measure official test outcomes, not coverage
of the language standard. Expand the execution details to inspect modes and diagnostics.

Local measurements use `heliodor-dev`; daily CI uses `release`. Upstream timeouts
apply in both configurations, so build settings and machine load can affect results.
