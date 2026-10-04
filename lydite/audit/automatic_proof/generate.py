"""Deterministic, ISA-independent automatic-proof evaluation documents.

These model equivalent combinational datapath implementations behind one
registered output. They contain no proof metadata, assumptions about sampled
values, term identifiers, or external-solver results.
"""
import argparse
import copy
import hashlib
import json
from pathlib import Path


def binary(op, a, b):
    return [op, a, b]


def mux(g, a, b):
    return ["ite", g, a, b]


def word(width, value):
    return ["bv", width, value]


def document(name, width, left, right, inputs):
    zero = word(width, 0)
    return {
        "version": 2, "name": name,
        "inputs": {"rst": "bool", **inputs}, "reset_input": "rst",
        "spec": {"state": {"out": {"bv": width}}, "reset": {"out": zero},
                 "next": {"out": left}, "outputs": {"allowed": True}},
        "impl": {"state": {"out": {"bv": width}}, "reset": {"out": zero},
                 "next": {"out": right}, "outputs": {"retire": True}},
        "binding": ["eq", "spec.out", "impl.out"],
        "commit": "retire", "can_step": "allowed",
        "progress": {"enabled": True, "rank": word(1, 0)},
    }


def positive_cases(width=32):
    values = {key: {"bv": width} for key in ("a", "b", "c", "d", "bias", "shift")}
    inputs = {**values, "pick": "bool", "enable": "bool"}
    a, b, c, d, bias, shift = ["i." + k for k in values]
    g, h = "i.pick", "i.enable"
    mul = lambda x, y: binary("mul", x, y)
    add = lambda x, y: binary("add", x, y)
    xor = lambda x, y: binary("bxor", x, y)
    product = mul(mux(g, a, b), mux(g, c, d))
    selected_product = mux(g, mul(a, c), mul(b, d))
    yield document("wrapped_multiplier", width, add(product, bias),
                   add(selected_product, bias), inputs)
    # A second independent guard changes the retiming tree and needs four leaves.
    double = mul(mux(g, a, b), mux(h, c, d))
    selected_double = mux(g, mux(h, mul(a, c), mul(a, d)),
                          mux(h, mul(b, c), mul(b, d)))
    yield document("double_guard_mac", width, add(double, bias),
                   add(selected_double, bias), inputs)
    # Unlike the multiplier family this is a noncommutative barrel shifter.
    shifted = binary("shl", mux(g, a, b), mux(g, c, d))
    selected_shift = mux(g, binary("shl", a, c), binary("shl", b, d))
    yield document("wrapped_barrel_shift", width, xor(shifted, bias),
                   xor(selected_shift, bias), inputs)
    # Two nested operation levels, with the mux below both output operators.
    nested = binary("lshr", add(product, bias), shift)
    selected_nested = binary("lshr", add(selected_product, bias), shift)
    yield document("postscaled_multiplier", width, nested, selected_nested, inputs)
    # Guard is an arbitrary Boolean expression, not a direct control variable.
    guard = ["and", g, h]
    gated = mul(mux(guard, a, word(width, 0)), b)
    selected_gated = mux(guard, mul(a, b), word(width, 0))
    yield document("operand_isolation", width, xor(gated, bias),
                   xor(selected_gated, bias), inputs)
    # Four distinct variable-shift cones; this exercises guard coverage without
    # multiplication and differs structurally from the single-guard family.
    shifted_twice = binary("shl", mux(g, a, b), mux(h, c, d))
    chosen_shift = mux(g, mux(h, binary("shl", a, c), binary("shl", a, d)),
                       mux(h, binary("shl", b, c), binary("shl", b, d)))
    yield document("double_guard_barrel_shift", width, xor(shifted_twice, bias),
                   xor(chosen_shift, bias), inputs)


def cases(width=32):
    originals = list(positive_cases(width))
    yield from originals
    # Mutations affect a reachable data result, not an auxiliary proof proposal.
    for base in originals:
        bad = copy.deepcopy(base)
        bad["name"] += "_wrong_result"
        bad["impl"]["next"]["out"] = binary("add", bad["impl"]["next"]["out"], word(width, 1))
        yield bad
        missed = copy.deepcopy(base)
        missed["name"] += "_complement_wrong"
        original = missed["impl"]["next"]["out"]
        missed["impl"]["next"]["out"] = mux("i.pick", original,
                                                        binary("add", original, word(width, 1)))
        yield missed


def alpha_rename(doc, seed):
    """Rename every signal without changing sorts, input domains, or equations."""
    doc = copy.deepcopy(doc)
    fresh = lambda kind, key: kind + "_" + hashlib.sha256((str(seed) + "/" + key).encode()).hexdigest()[:12]
    inputs = {key: fresh("in", key) for key in doc["inputs"]}
    state = fresh("state", "out")
    allowed, retire = fresh("pin", "allowed"), fresh("pin", "retire")
    references = {"i." + key: "i." + value for key, value in inputs.items()}
    references.update({prefix + ".out": prefix + "." + state for prefix in ("s", "spec", "impl")})
    def rewrite(value):
        if isinstance(value, str):
            return references.get(value, value)
        if isinstance(value, list):
            return [rewrite(x) for x in value]
        if isinstance(value, dict):
            return {key: rewrite(val) for key, val in value.items()}
        return value
    doc = rewrite(doc)
    doc["inputs"] = {inputs[key]: val for key, val in reversed(list(doc["inputs"].items()))}
    doc["reset_input"] = inputs[doc["reset_input"]]
    for side in ("spec", "impl"):
        for key in ("state", "reset", "next"):
            doc[side][key] = {state: doc[side][key]["out"]}
    doc["spec"]["outputs"] = {allowed: True}
    doc["impl"]["outputs"] = {retire: True}
    doc["can_step"], doc["commit"] = allowed, retire
    doc["name"] += "__alpha_" + str(seed)
    return doc


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--width", type=int, default=32, choices=range(4, 65))
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    for case in cases(args.width):
        (args.out / (case["name"] + ".json")).write_text(json.dumps(case, indent=2) + "\n")


if __name__ == "__main__":
    main()
