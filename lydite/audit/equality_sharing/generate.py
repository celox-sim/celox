"""Unhinted state-encoding transport cases for checked equality sharing.

All live inputs range over their full sorts. The compound equalities are the
state relation being inductively checked, not assumptions on sampled inputs.
"""
import argparse
import copy
import hashlib
import json
from pathlib import Path


def op(name, *args):
    return [name, *args]


def word(width, value):
    return ["bv", width, value]


def conjunction(terms):
    result = terms[-1]
    for item in reversed(terms[:-1]):
        result = op("and", item, result)
    return result


def expression(kind, prefix, pair=("a", "b")):
    return op(kind, prefix + pair[0], prefix + pair[1])


def document(name, width, kind="add", wrapper="mul", reverse=False,
             transitive=False, guarded=False, nested_guard=False):
    sort = {"bv": width}
    zero, one = word(width, 0), word(width, 1)
    inputs = {"rst": "bool", "seed": sort, "factor": sort, "bias": sort,
              "shift": sort, "pick": "bool", "enable": "bool", "fallback": sort}
    sstate = {"a": sort, "b": sort, "out": sort}
    istate = {"a": sort, "b": sort, "out": sort}
    sreset = {"a": "i.seed", "b": one if kind == "mul" else zero, "out": zero}
    ireset = {"a": one if kind == "mul" else zero,
              "b": "i.seed", "out": zero}
    snext = {key: "s." + key for key in sstate}
    inext = {key: "s." + key for key in istate}
    lhs, rhs = expression(kind, "s."), expression(kind, "s.")
    relation_left = expression(kind, "spec.")
    relation_right = expression(kind, "impl.")
    edge = op("eq", relation_right, relation_left) if reverse else op("eq", relation_left, relation_right)
    facts = [op("eq", "spec.out", "impl.out")]
    if transitive:
        # spec add encoding equals impl xor encoding equals impl second add encoding.
        # The next output compares spec add against impl second add, requiring both edges.
        istate.update({"c": sort, "d": sort})
        ireset.update({"a": "i.seed", "b": zero, "c": "i.seed", "d": zero})
        inext.update({"c": "s.c", "d": "s.d"})
        middle = expression("bxor", "impl.")
        endpoint = expression("add", "impl.", ("c", "d"))
        edges = [(relation_left, middle), (middle, endpoint)]
        facts.extend(op("eq", b, a) if reverse else op("eq", a, b) for a, b in edges)
        rhs = expression("add", "s.", ("c", "d"))
    elif guarded:
        for key, initial in (("route", "i.pick"), ("active", "i.enable")):
            sstate[key] = istate[key] = "bool"
            sreset[key] = ireset[key] = initial
            snext[key] = inext[key] = "s." + key
            facts.append(op("eq", "spec." + key, "impl." + key))
        if nested_guard:
            facts.append(op("implies", "spec.route", op("implies", "spec.active", edge)))
        else:
            facts.append(op("implies", "spec.route", edge))
    else:
        facts.append(edge)
    def wrap(value):
        if wrapper == "mul":
            return op("mul", value, "i.factor")
        if wrapper == "mac_shift":
            return op("lshr", op("add", op("mul", value, "i.factor"), "i.bias"), "i.shift")
        if wrapper == "shift":
            return op("shl", value, "i.shift")
        raise ValueError(wrapper)
    sout, iout = wrap(lhs), wrap(rhs)
    if guarded:
        guard = op("and", "s.route", "s.active") if nested_guard else "s.route"
        sout = op("ite", guard, sout, "i.fallback")
        iout = op("ite", guard, iout, "i.fallback")
    if guarded:
        initial_guard = op("and", "i.pick", "i.enable") if nested_guard else "i.pick"
        ireset["a"] = op("ite", initial_guard, zero, one)
    snext["out"], inext["out"] = sout, iout
    return {"version": 2, "name": name, "inputs": inputs, "reset_input": "rst",
            "spec": {"state": sstate, "reset": sreset, "next": snext, "outputs": {"allowed": True}},
            "impl": {"state": istate, "reset": ireset, "next": inext, "outputs": {"retire": True}},
            "binding": conjunction(facts), "commit": "retire", "can_step": "allowed",
            "progress": {"enabled": True, "rank": word(1, 0)}}


def positive_cases(width=32):
    yield document("compound_sum_transport", width)
    yield document("compound_product_transport", width, kind="mul")
    yield document("transitive_encoded_transport", width, transitive=True)
    yield document("reversed_transitive_mac_shift", width, transitive=True, reverse=True, wrapper="mac_shift")
    yield document("guarded_compound_transport", width, guarded=True)
    yield document("nested_guard_compound_shift", width, guarded=True, nested_guard=True, wrapper="shift")
    yield document("nested_guard_compound_mac", width, guarded=True, nested_guard=True, wrapper="mac_shift")
    reuse = document("branched_compound_reuse", width)
    value = expression("add", "s.")
    alternate = op("add", value, "i.bias")
    reuse["spec"]["next"]["out"] = op("mul", op("ite", "i.pick", value, alternate), "i.factor")
    reuse["impl"]["next"]["out"] = op("ite", "i.pick", op("mul", value, "i.factor"), op("mul", alternate, "i.factor"))
    for side in ("spec", "impl"):
        reuse[side]["next"]["out"] = op("add", reuse[side]["next"]["out"], "i.fallback")
    yield reuse


def cases(width=32):
    originals = list(positive_cases(width))
    yield from originals
    for base in originals:
        bad = copy.deepcopy(base)
        bad["name"] += "_wrong_result"
        bad["impl"]["next"]["out"] = op("add", bad["impl"]["next"]["out"], word(width, 1))
        yield bad
        complement = copy.deepcopy(base)
        complement["name"] += "_complement_wrong"
        original = complement["impl"]["next"]["out"]
        # Reachable current-input complement, independent of the encoded state guards.
        complement["impl"]["next"]["out"] = op("ite", "i.pick", original, op("add", original, word(width, 1)))
        yield complement
    # A genuinely inactive compound equality must never be imported globally.
    escaped = document("guarded_fact_escape_wrong", width, guarded=True)
    escaped["spec"]["next"]["out"] = op("mul", expression("add", "s."), "i.factor")
    escaped["impl"]["next"]["out"] = op("mul", expression("add", "s."), "i.factor")
    yield escaped


def alpha_rename(doc, seed):
    doc = copy.deepcopy(doc)
    fresh = lambda kind, key: kind + "_" + hashlib.sha256((str(seed) + "/" + key).encode()).hexdigest()[:12]
    imap = {key: fresh("input", key) for key in doc["inputs"]}
    smap = {key: fresh("state", key) for side in ("spec", "impl") for key in doc[side]["state"]}
    refs = {"i." + key: "i." + value for key, value in imap.items()}
    refs.update({prefix + "." + key: prefix + "." + val
                 for prefix in ("s", "spec", "impl") for key, val in smap.items()})
    def rewrite(value):
        if isinstance(value, str): return refs.get(value, value)
        if isinstance(value, list): return [rewrite(x) for x in value]
        if isinstance(value, dict): return {key: rewrite(val) for key, val in value.items()}
        return value
    doc = rewrite(doc)
    doc["inputs"] = {imap[key]: val for key, val in reversed(list(doc["inputs"].items()))}
    doc["reset_input"] = imap[doc["reset_input"]]
    for side in ("spec", "impl"):
        for field in ("state", "reset", "next"):
            doc[side][field] = {smap[key]: val for key, val in reversed(list(doc[side][field].items()))}
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


if __name__ == "__main__": main()
