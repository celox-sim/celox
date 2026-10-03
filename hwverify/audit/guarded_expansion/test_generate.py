"""Independent finite arithmetic checks; never proof authority."""
import hashlib
import json
import random
import unittest
from audit.equality_sharing.test_generate import related, reset, transition
from audit.guarded_expansion.generate import FAMILIES, POSITIVES, alpha_rename, cases, document


class Fixtures(unittest.TestCase):
    def test_unhinted_deterministic_full_domains(self):
        docs = list(cases())
        self.assertEqual(docs, list(cases()))
        self.assertEqual(len(docs), 16)
        for d in docs:
            self.assertNotIn('proof_programs', d)
            self.assertNotIn('program_contract', d)
            self.assertNotIn('"i.', json.dumps(d['binding']))
            self.assertEqual(d['inputs']['factor'], {'bv': 32})
            self.assertEqual(d['inputs']['pick'], 'bool')

    def test_reset_and_arbitrary_related_states(self):
        for width in (4, 24, 32, 64):
            mask = (1 << width)-1
            rng = random.Random(width+9841)
            for family in FAMILIES:
                d = document(family, width)
                for active in (False, True):
                    for _ in range(96):
                        inp = {'i.'+k: bool(rng.getrandbits(1)) if t == 'bool' else rng.randrange(mask+1)
                               for k, t in d['inputs'].items()}
                        self.assertTrue(related(d, reset(d, inp, mask), mask))
                        st = {k: rng.randrange(mask+1) for k in d['impl']['state']}
                        st['active'] = active
                        st['shift'] = rng.randrange(width+2)
                        total = (st['a']+st['b']) & mask
                        st['d'] = st['c'] ^ total
                        encoded = (total*st['factor']) & mask
                        if family == FAMILIES[1]:
                            encoded = ((encoded+st['bias']) & mask) >> st['shift']
                        spec = {'active': active, 'out': st['out'],
                                'encoded': encoded if active else rng.randrange(mask+1)}
                        state = {'impl': st, 'spec': spec}
                        self.assertTrue(related(d, state, mask))
                        self.assertTrue(related(d, transition(d, state, inp, mask), mask))

    def test_all_mutants_have_reset_reachable_witnesses(self):
        for width in (4, 24, 32, 64):
            mask = (1 << width)-1
            for d in cases(width):
                if d['name'] in POSITIVES:
                    continue
                inp = {'i.'+k: False if t == 'bool' else 0 for k, t in d['inputs'].items()}
                inp['i.inactive'] = 1
                state = reset(d, inp, mask)
                self.assertTrue(related(d, state, mask))
                self.assertFalse(related(d, transition(d, state, inp, mask), mask), d['name'])

    def test_reversal_changes_only_output_equality_orientation(self):
        docs = list(cases())
        for normal, mirror in zip(docs[::2], docs[1::2]):
            self.assertEqual(mirror['binding'][1], ['eq', 'impl.out', 'spec.out'])
            mirror['binding'][1] = ['eq', 'spec.out', 'impl.out']
            mirror['name'] = normal['name']
            self.assertEqual(normal, mirror)

    def test_alpha_equivalence(self):
        seed, mask, rng = 25109, (1 << 24)-1, random.Random(25109)
        key = lambda kind, name: kind+'_'+hashlib.sha256((str(seed)+'/'+name).encode()).hexdigest()[:12]
        for d in cases(24):
            new = alpha_rename(d, seed)
            self.assertEqual(new, alpha_rename(d, seed))
            for _ in range(32):
                inp = {'i.'+k: bool(rng.getrandbits(1)) if t == 'bool' else rng.randrange(mask+1)
                       for k, t in d['inputs'].items()}
                renamed = {'i.'+key('input', k[2:]): v for k, v in inp.items()}
                state, nstate = reset(d, inp, mask), reset(new, renamed, mask)
                rekey = lambda s: {side: {key('state', k): v for k, v in fields.items()} for side, fields in s.items()}
                self.assertEqual(rekey(state), nstate)
                self.assertEqual(rekey(transition(d, state, inp, mask)), transition(new, nstate, renamed, mask))
                self.assertEqual(related(d, state, mask), related(new, nstate, mask))

if __name__ == '__main__':
    unittest.main()
