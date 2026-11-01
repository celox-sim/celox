# Verification

<script setup>
import VerificationResults from '../.vitepress/components/VerificationResults.vue'
</script>

## SystemVerilog: sv-tests

<VerificationResults />

The daily extended verification run measures the pinned upstream **imported** corpus in both two-state and four-state modes. It runs separately
from PR CI. Main tests remain in the existing CI; core-design generators are outside this measurement.
The official upstream runner determines pass, fail and skip verdicts.

Skipped cases are excluded from the executed denominator. Groups and state modes
are reported separately. Parsing and elaboration passes do not establish simulation
correctness, and negative tests can pass because of an unsupported construct.
These percentages measure test outcomes, not language-standard coverage.

The page starts with a checked-in local measurement and loads newer complete daily
results when available. Collection failures retain artifacts but do not replace the
last complete public result. Daily artifacts contain per-case verdicts, execution
modes, diagnostic classifications and official logs; they are retained for 14 days.

Expand each group to see selected execution modes and failure categories.
Diagnostic categories describe observed errors, rather than proven root causes.
The initial local snapshot uses the `heliodor-dev` optimized build; scheduled CI
uses `release`. Upstream timeouts apply in both configurations, so build settings
and machine load can affect timeout results.
