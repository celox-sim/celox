#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -eq 0 ]; then
    echo "usage: $0 NATIVE_DUMP_LOG..." >&2
    exit 2
fi

echo "## opcode counts"
awk '
function opcode(body,    parts, op_parts) {
    if (body ~ /^v[0-9]+ = /) {
        split(body, parts, " = ")
        split(parts[2], op_parts, " ")
    } else {
        split(body, op_parts, " ")
    }
    split(op_parts[1], op_parts, ".")
    return op_parts[1]
}
/^  [0-9]+: / {
    body = $0
    sub(/^  [0-9]+: /, "", body)
    op = opcode(body)
    count[op]++
}
END {
    for (op in count) {
        print count[op] "\t" op
    }
}
' "$@" | sort -nr

echo
echo "## imm first-use opcode"
awk '
function opcode(body,    parts, op_parts) {
    if (body ~ /^v[0-9]+ = /) {
        split(body, parts, " = ")
        split(parts[2], op_parts, " ")
    } else {
        split(body, op_parts, " ")
    }
    split(op_parts[1], op_parts, ".")
    return op_parts[1]
}
function dst_reg(body,    parts) {
    if (body ~ /^v[0-9]+ = /) {
        split(body, parts, " ")
        return parts[1]
    }
    return ""
}
function note_uses(body, op, dst,    cleaned, n, i, tok) {
    cleaned = body
    gsub(/[][,+]/, " ", cleaned)
    n = split(cleaned, tokens, /[[:space:]]+/)
    for (i = 1; i <= n; i++) {
        tok = tokens[i]
        if (tok == dst) {
            continue
        }
        if (tok in imm_pending) {
            first_use[op]++
            delete imm_pending[tok]
        }
    }
}
/^  [0-9]+: / {
    body = $0
    sub(/^  [0-9]+: /, "", body)
    op = opcode(body)
    dst = dst_reg(body)
    note_uses(body, op, dst)
    if (op == "imm" && dst != "") {
        imm_pending[dst] = 1
        imm_total++
    }
}
END {
    for (op in first_use) {
        used += first_use[op]
        print first_use[op] "\t" op
    }
    print (imm_total - used) "\tunused_or_not_in_dump"
}
' "$@" | sort -nr
