"""Independent finite concrete sanity checks; not universal proof authority."""
import itertools
import hashlib
import random
import unittest

from audit.automatic_proof.generate import alpha_rename, cases, positive_cases


def evaluate(expr, env, mask):
    if isinstance(expr, str):
        return env[expr]
    if isinstance(expr, bool):
        return expr
    op, *args = expr
    if op == "bv":
        return args[1] & ((1 << args[0]) - 1)
    if op == "ite":
        return evaluate(args[1 if evaluate(args[0], env, mask) else 2], env, mask)
    values = [evaluate(x, env, mask) for x in args]
    a, b = values
    if op == "add":
        return (a + b) & mask
    if op == "mul":
        return (a * b) & mask
    if op == "bxor":
        return a ^ b
    if op == "shl":
        return ((a << b) & mask) if b <= mask.bit_length() else 0
    if op == "lshr":
        return (a >> b) if b <= mask.bit_length() else 0
    if op == "and":
        return a and b
    raise AssertionError(op)


class GeneratedFixtures(unittest.TestCase):
    def test_deterministic_unhinted_and_distinct(self):
        docs = list(cases(32))
        self.assertEqual(docs, list(cases(32)))
        self.assertEqual(len(docs), 18)
        self.assertEqual(len({d["name"] for d in docs}), 18)
        for d in docs:
            self.assertNotIn("proof_programs", d)
            self.assertNotEqual(d["spec"]["next"], d["impl"]["next"])

    def test_small_exhaustive_controls_and_datapath_edges(self):
        for width in (4, 24, 32, 64):
            mask = (1 << width) - 1
            rng = random.Random(9261 + width)
            edges = (0, 1, mask, 1 << (width - 1))
            vectors = list(itertools.product(edges, repeat=4))
            vectors += [tuple(rng.randrange(mask + 1) for _ in range(4)) for _ in range(32)]
            for a, b, c, d in vectors:
                for pick, enable in itertools.product((False, True), repeat=2):
                    env = {"i.a": a, "i.b": b, "i.c": c, "i.d": d,
                           "i.bias": rng.randrange(mask + 1), "i.shift": rng.randrange(width + 1),
                           "i.pick": pick, "i.enable": enable}
                    for doc in positive_cases(width):
                        self.assertEqual(evaluate(doc["spec"]["next"]["out"], env, mask),
                                         evaluate(doc["impl"]["next"]["out"], env, mask), doc["name"])

    def test_mutations_have_reachable_complement_witnesses(self):
        env = {"i." + k: 0 for k in ("a", "b", "c", "d", "bias", "shift")}
        env.update({"i.pick": False, "i.enable": False})
        for doc in list(cases(32))[6:]:
            self.assertNotEqual(evaluate(doc["spec"]["next"]["out"], env, (1 << 32) - 1),
                                evaluate(doc["impl"]["next"]["out"], env, (1 << 32) - 1))

    def test_alpha_rename_preserves_datapath_and_domains(self):
        seed = 7301
        source = next(positive_cases(24))
        renamed = alpha_rename(source, seed)
        self.assertEqual(renamed, alpha_rename(source, seed))
        self.assertEqual(sorted(map(str, source["inputs"].values())),
                         sorted(map(str, renamed["inputs"].values())))
        self.assertTrue(set(source["inputs"]).isdisjoint(renamed["inputs"]))
        rng = random.Random(seed)
        mask = (1 << 24) - 1
        for _ in range(64):
            env = {"i." + key: (bool(rng.getrandbits(1)) if sort == "bool" else rng.randrange(mask + 1))
                   for key, sort in source["inputs"].items()}
            alternate = {"i.in_" + hashlib.sha256((str(seed) + "/" + key[2:]).encode()).hexdigest()[:12]: value
                         for key, value in env.items()}
            for side in ("spec", "impl"):
                self.assertEqual(evaluate(source[side]["next"]["out"], env, mask),
                                 evaluate(next(iter(renamed[side]["next"].values())), alternate, mask))


if __name__ == "__main__":
    unittest.main()
