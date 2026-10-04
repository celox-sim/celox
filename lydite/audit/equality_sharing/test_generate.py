"""Independent concrete sanity tests; universal proof comes only from checker."""
import hashlib
import itertools
import random
import unittest

from audit.equality_sharing.generate import alpha_rename, cases, positive_cases


def evaluate(expr, env, mask):
    if isinstance(expr, str): return env[expr]
    if isinstance(expr, bool): return expr
    op, *args = expr
    if op == "bv": return args[1] & ((1 << args[0]) - 1)
    if op == "ite": return evaluate(args[1 if evaluate(args[0], env, mask) else 2], env, mask)
    values = [evaluate(a, env, mask) for a in args]
    if op == "not": return not values[0]
    a, b = values
    if op == "eq": return a == b
    if op == "and": return a and b
    if op == "implies": return not a or b
    if op == "add": return (a + b) & mask
    if op == "sub": return (a - b) & mask
    if op == "mul": return (a * b) & mask
    if op == "bxor": return a ^ b
    if op == "shl": return (a << b) & mask if b < mask.bit_length() else 0
    if op == "lshr": return a >> b if b < mask.bit_length() else 0
    raise AssertionError(op)


def reset(doc, inputs, mask):
    return {side: {key: evaluate(expr, inputs, mask) for key, expr in doc[side]["reset"].items()}
            for side in ("spec", "impl")}


def transition(doc, state, inputs, mask):
    return {side: {key: evaluate(expr, {**inputs, **{"s." + k: v for k, v in state[side].items()}}, mask)
                   for key, expr in doc[side]["next"].items()} for side in ("spec", "impl")}


def related(doc, state, mask):
    env = {side + "." + key: val for side, fields in state.items() for key, val in fields.items()}
    return evaluate(doc["binding"], env, mask)


def inputs(doc, rng, mask):
    values = {"i." + k: (bool(rng.getrandbits(1)) if s == "bool" else rng.randrange(mask + 1))
              for k, s in doc["inputs"].items()}
    values["i.shift"] = rng.randrange(mask.bit_length() + 2)
    return values


def related_state(doc, rng, mask, route, active):
    common = rng.randrange(mask + 1)
    other = (common + 1) & mask
    state = {side: {key: (bool(rng.getrandbits(1)) if sort == "bool" else rng.randrange(mask + 1))
                    for key, sort in doc[side]["state"].items()} for side in ("spec", "impl")}
    state["impl"]["out"] = state["spec"]["out"]
    for side in ("spec", "impl"):
        if "route" in state[side]:
            state[side]["route"], state[side]["active"] = route, active
    guarded = "route" in state["spec"]
    enabled = route and (active if doc["name"].startswith("nested_guard") else True)
    target = other if guarded and not enabled else common
    if "product" in doc["name"]:
        for side in ("spec", "impl"):
            a = rng.randrange(mask + 1) | 1
            state[side]["a"], state[side]["b"] = a, common * pow(a, -1, mask + 1) & mask
    else:
        state["spec"]["b"] = (common - state["spec"]["a"]) & mask
        if "transitive" in doc["name"]:
            state["impl"]["b"] = common ^ state["impl"]["a"]
            state["impl"]["d"] = (common - state["impl"]["c"]) & mask
        else:
            state["impl"]["b"] = (target - state["impl"]["a"]) & mask
    return state


class Fixtures(unittest.TestCase):
    def test_deterministic_unhinted_and_no_input_restrictions(self):
        docs = list(cases())
        self.assertEqual(docs, list(cases()))
        self.assertEqual(len(docs), 25)
        self.assertEqual(len({d["name"] for d in docs}), 25)
        for doc in docs:
            self.assertNotIn("proof_programs", doc)
            self.assertNotIn("program_contract", doc)
            self.assertNotIn('"i.', __import__('json').dumps(doc["binding"]))

    def test_reset_and_arbitrary_related_state_preservation(self):
        for width in (4, 24, 32, 64):
            mask = (1 << width) - 1
            rng = random.Random(2319 + width)
            for doc in positive_cases(width):
                for route, active in itertools.product((False, True), repeat=2):
                    for _ in range(48):
                        inp = inputs(doc, rng, mask)
                        init = reset(doc, inp, mask)
                        self.assertTrue(related(doc, init, mask), doc["name"])
                        state = related_state(doc, rng, mask, route, active)
                        self.assertTrue(related(doc, state, mask), doc["name"])
                        self.assertTrue(related(doc, transition(doc, state, inp, mask), mask), doc["name"])

    def test_all_mutants_have_reset_reachable_witness(self):
        mask = (1 << 32) - 1
        for doc in list(cases())[8:]:
            inp = {"i." + key: (False if sort == "bool" else 0) for key, sort in doc["inputs"].items()}
            inp["i.factor"] = 1
            init = reset(doc, inp, mask)
            self.assertTrue(related(doc, init, mask), doc["name"])
            self.assertFalse(related(doc, transition(doc, init, inp, mask), mask), doc["name"])

    def test_alpha_rename_preserves_inputs_reset_and_transition(self):
        mask, seed = (1 << 24) - 1, 9413
        rng = random.Random(seed)
        for doc in positive_cases(24):
            renamed = alpha_rename(doc, seed)
            self.assertEqual(renamed, alpha_rename(doc, seed))
            self.assertEqual(sorted(map(str, doc["inputs"].values())), sorted(map(str, renamed["inputs"].values())))
            key = lambda kind, name: kind + "_" + hashlib.sha256((str(seed) + "/" + name).encode()).hexdigest()[:12]
            for _ in range(24):
                inp = inputs(doc, rng, mask)
                state = related_state(doc, rng, mask, False, True)
                new_inp = {"i." + key("input", k[2:]): v for k, v in inp.items()}
                new_state = {side: {key("state", k): v for k, v in fields.items()} for side, fields in state.items()}
                self.assertEqual(related(doc, state, mask), related(renamed, new_state, mask))
                actual = transition(renamed, new_state, new_inp, mask)
                expected = {side: {key("state", k): v for k, v in fields.items()} for side, fields in transition(doc, state, inp, mask).items()}
                self.assertEqual(expected, actual)


if __name__ == "__main__": unittest.main()
