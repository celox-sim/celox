"""Concrete checks are sanity tests, never universal proof authority."""
import hashlib
import itertools
import json
import random
import unittest
from audit.equality_sharing.test_generate import evaluate, related, reset, transition
from audit.word_frontier.generate import FAMILIES, alpha_rename, cases, document, read_bank


class Fixtures(unittest.TestCase):
    def test_deterministic_unhinted_full_input_domains(self):
        docs=list(cases())
        self.assertEqual(docs,list(cases()))
        self.assertEqual(len(docs),6)
        for d in docs:
            self.assertNotIn('proof_programs',d)
            self.assertNotIn('program_contract',d)
            self.assertNotIn('"i.',json.dumps(d['binding']))
            self.assertEqual(d['inputs']['factor'],{'bv':32})
            self.assertEqual(d['inputs']['pick'],'bool')

    def test_reset_and_arbitrary_related_state_preservation(self):
        for width in (4,24,32,64):
            mask=(1<<width)-1;rng=random.Random(517+width)
            for family in FAMILIES:
                doc=document(family,width)
                for route,s0,s1,s2 in itertools.product((False,True),repeat=4):
                    for _ in range(12):
                        inp={'i.'+k:bool(rng.getrandbits(1)) if sort=='bool' else rng.randrange(mask+1)
                             for k,sort in doc['inputs'].items()}
                        inp['i.shift']=rng.randrange(width+2)
                        self.assertTrue(related(doc,reset(doc,inp,mask),mask))
                        ins={k:bool(rng.getrandbits(1)) if sort=='bool' else rng.randrange(mask+1)
                             for k,sort in doc['impl']['state'].items()}
                        ins.update(route=route,select0=s0,select1=s1,select2=s2)
                        value=evaluate(read_bank('s.',family),{'s.'+k:v for k,v in ins.items()},mask)
                        spec={'route':route,'out':ins['out'],'value':value if route else rng.randrange(mask+1)}
                        state={'spec':spec,'impl':ins}
                        self.assertTrue(related(doc,state,mask))
                        self.assertTrue(related(doc,transition(doc,state,inp,mask),mask),(family,width))

    def test_mutants_have_reset_reachable_witnesses(self):
        for width in (4,24,32,64):
            mask=(1<<width)-1
            for doc in cases(width):
                if doc['name'] in FAMILIES: continue
                for route in (False,True):
                    inp={'i.'+k:False if sort=='bool' else 0 for k,sort in doc['inputs'].items()}
                    inp.update({'i.factor':1,'i.route':route})
                    state=reset(doc,inp,mask)
                    self.assertTrue(related(doc,state,mask))
                    self.assertFalse(related(doc,transition(doc,state,inp,mask),mask),doc['name'])

    def test_alpha_preserves_all_symbols_and_expressions(self):
        seed=18073;width=24;mask=(1<<width)-1;rng=random.Random(seed)
        key=lambda kind,name:kind+'_'+hashlib.sha256((str(seed)+'/'+name).encode()).hexdigest()[:12]
        for doc in cases(width):
            new=alpha_rename(doc,seed)
            self.assertEqual(new,alpha_rename(doc,seed))
            for _ in range(32):
                inp={'i.'+k:bool(rng.getrandbits(1)) if sort=='bool' else rng.randrange(mask+1)
                     for k,sort in doc['inputs'].items()}
                renamed={'i.'+key('input',k[2:]):v for k,v in inp.items()}
                state=reset(doc,inp,mask);newstate=reset(new,renamed,mask)
                rekey=lambda st:{side:{key('state',k):v for k,v in fields.items()} for side,fields in st.items()}
                self.assertEqual(rekey(state),newstate)
                self.assertEqual(related(doc,state,mask),related(new,newstate,mask))
                self.assertEqual(rekey(transition(doc,state,inp,mask)),transition(new,newstate,renamed,mask))

if __name__=='__main__':unittest.main()
