# Source this file. Only named tool paths are exported; PATH and solver lookup stay unchanged.
_TIMING_PREP_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export FPGA_YOSYS="$_TIMING_PREP_ROOT/oss-cad-suite/bin/yosys"
export FPGA_NEXTPNR="$_TIMING_PREP_ROOT/bin/nextpnr-ecp5"
export VERYL_TIMING_EMIT="$_TIMING_PREP_ROOT/emitter-target/debug/veryl-timing-emit"
unset _TIMING_PREP_ROOT
