"""Fail-closed tests for the selected source's physical control invariant."""
import copy
import unittest
from audit import interpreter as i
from audit.veryl_scaling import rv32i_control_invariants as q


class ControlInvariantTests(unittest.TestCase):
    def env(self, **kwargs):
        values = dict(d_valid=False,x_valid=False,m_valid=False,w_valid=False,
                      fetch_pc=i.BV(32,0), d_pc=i.BV(32,0))
        values.update(kwargs)
        return {'s.'+k:v for k,v in values.items()}

    def test_vacuous_empty_reset(self):
        self.assertIs(i.evaluate(q.invariant(), self.env()), True)

    def test_any_later_valid_requires_d(self):
        for key in ('x_valid','m_valid','w_valid'):
            self.assertIs(i.evaluate(q.invariant(),self.env(**{key:True})),False)

    def test_wraparound_is_modular(self):
        self.assertIs(i.evaluate(q.invariant(),self.env(d_valid=True,
            d_pc=i.BV(32,0xfffffffc), fetch_pc=i.BV(32,0))),True)

    def test_stale_fetch_is_rejected(self):
        self.assertIs(i.evaluate(q.invariant(),self.env(d_valid=True)),False)

    def test_no_inputs_or_architectural_state_in_predicate(self):
        self.assertFalse(q.c.RUNNER.references(q.invariant(),'i.'))
        text=str(q.invariant())
        for forbidden in ('s.pc','s.r0','s.x_ir','s.w_next_pc','s.halted'):
            self.assertNotIn(forbidden,text)

    def test_unproved_full_adjacency_fails_closed(self):
        with self.assertRaises(ValueError):q.invariant(adjacency=True)

    def test_exact_source_transition_projection(self):
        keys=('d_valid','x_valid','m_valid','w_valid','fetch_pc','d_pc','x_pc','m_pc','w_pc')
        state={k:('bool' if k.endswith('valid') else {'bv':32}) for k in keys}
        state['halted']='bool'
        raw={'state':state,'reset':{k:q.c.zero(v) for k,v in state.items()},
             'next':{k:['ite','s.halted','s.'+k,'s.'+k] for k in state},
             'wires':{'control':['and','s.halted','s.d_valid']}}
        before=copy.deepcopy(raw)
        doc=q.document(raw)
        self.assertEqual(raw,before)
        impl=doc['implementation']
        self.assertEqual(set(impl['state']),set(keys))
        self.assertEqual(impl['inputs']['pre_halted'],'bool')
        self.assertEqual(impl['next']['d_valid'],['ite','i.pre_halted','s.d_valid','s.d_valid'])
        self.assertEqual(impl['wires']['control'],['and','i.pre_halted','s.d_valid'])
        self.assertEqual(impl['reset'],{k:raw['reset'][k] for k in keys})
        self.assertIs(doc['specs']['Contract']['operations']['tick'],True)
        self.assertNotIn('assumptions',str(doc))

    def test_selected_source_is_pinned(self):
        self.assertEqual(q.p.sha(q.p.source_path(q.VARIANT)),q.VARIANT.source_sha256)

    def test_unknown_and_unvalidated_sat_rejected(self):
        for report in ({},{'status':'unknown'}):
            with self.assertRaises(ValueError):q.p.validate_lemma(report)
            with self.assertRaises(ValueError):q.p.validate_mutation(report)


if __name__ == '__main__':unittest.main()
