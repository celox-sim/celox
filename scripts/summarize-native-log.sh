#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -eq 0 ]; then
    echo "usage: $0 LOG..." >&2
    exit 2
fi

awk_kv='
function kv(name,    i, a) {
    for (i = 1; i <= NF; i++) {
        split($i, a, "=")
        if (a[1] == name) {
            return a[2]
        }
    }
    return ""
}
'

echo "## native timing"
awk "$awk_kv"'
/\[native-timing\] compile_units start/ {
    label = kv("label")
}
/\[native-timing\] emit_chained (mir_opt|regalloc|post_regalloc_peephole|emit)/ {
    stage = $3
    print label "\t" stage "\tmir_insts=" kv("mir_insts") "\tvregs=" kv("vregs") "\tspill_frame=" kv("spill_frame") "\tbytes=" kv("bytes") "\telapsed=" kv("elapsed")
}
' "$@"

echo
echo "## native MIR stats"
awk "$awk_kv"'
/\[native-mir-stats\]/ {
    total = 0
    for (i = 1; i <= NF; i++) {
        split($i, a, "=")
        if (a[1] != "label" && a[1] != "stage" && a[2] ~ /^[0-9]+$/) {
            total += a[2]
        }
    }
    print kv("label") "\t" kv("stage") "\ttotal=" total "\timm=" kv("imm") "\tload_stack=" kv("load_stack") "\tstore_stack=" kv("store_stack") "\talu=" kv("alu") "\talu_imm=" kv("alu_imm") "\tselect=" kv("select")
}
' "$@"

echo
echo "## regalloc stats"
awk "$awk_kv"'
/\[regalloc-stats\]/ {
    print kv("label") "\tspill_frame=" kv("spill_frame") "\tdelta_insts=" kv("delta_insts") "\tdelta_load_stack=" kv("delta_load_stack") "\tdelta_store_stack=" kv("delta_store_stack") "\tdelta_load_imm=" kv("delta_load_imm")
}
' "$@"

echo
echo "## regalloc trace top"
awk "$awk_kv"'
/\[regalloc-trace\].*event=/ {
    key = kv("label") "\t" kv("event") "\t" kv("reason") "\t" kv("kind") "\t" kv("def") "\t" kv("next")
    count[key] += kv("count")
}
END {
    for (key in count) {
        print count[key] "\t" key
    }
}
' "$@" | sort -nr | head -40

echo
echo "## post-regalloc immediate uses"
awk "$awk_kv"'
/\[native-timing\] compile_units start/ {
    label = kv("label")
}
/\[native-timing\] emit_chained mir_opt/ {
    if (kv("label") != "") {
        label = kv("label")
    }
}
/\[post-regalloc-imm-stats\] total_load_imm=/ {
    total[label] += kv("total_load_imm")
}
/\[post-regalloc-imm-stats\] rank=/ {
    key = label "\t" kv("next")
    count[key] += kv("count")
}
END {
    for (label in total) {
        print total[label] "\t" label "\ttotal"
    }
    for (key in count) {
        print count[key] "\t" key
    }
}
' "$@" | sort -nr
