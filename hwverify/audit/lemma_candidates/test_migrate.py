import unittest
from audit.lemma_candidates.migrate import migrate

class Migration(unittest.TestCase):
    def metadata(self, steps, result='done'):
        return dict(version=1, mode='independent_lemmas', variables={}, lets=[],
                    programs=[dict(id='example', match_rhs=['bv',8,0], steps=steps, result=result)])

    def test_exact_guard_discharge_pair_and_idempotence(self):
        old=self.metadata([
            dict(op='prove',id='delivery',pre='let.pre',post='let.claim'),
            dict(op='prove',id='premise',pre='$pre',post='handle.delivery.pre'),
            dict(op='apply',id='done',lemma='delivery',premise='premise')])
        new=migrate(old)
        self.assertEqual(old['programs'][0]['steps'][0]['op'],'prove')
        c,use=new['programs'][0]['steps']
        self.assertEqual((c['context'],c['guard'],c['claim']),(True,'let.pre','let.claim'))
        self.assertEqual(use,dict(op='use_candidate',id='done',candidate='delivery',context='$pre'))
        self.assertEqual(migrate(new),new)

    def test_reused_guard_handle_is_not_dropped(self):
        old=self.metadata([dict(op='prove',id='g',pre='$pre',post='handle.lemma.pre'),
            dict(op='apply',id='used',lemma='lemma',premise='g'),
            dict(op='apply',id='done',lemma='lemma',premise='g')])
        new=migrate(old)['programs'][0]['steps']
        self.assertEqual(new[0]['op'],'candidate')
        self.assertEqual(new[0]['depends_on'],['lemma'])
        self.assertEqual(len(new),3)

    def test_arbitrary_premise_is_not_reinterpreted_as_a_guard(self):
        old=self.metadata([dict(op='prove',id='p',pre='$pre',post=True),
            dict(op='apply',id='done',lemma='lemma',premise='p')])
        self.assertEqual(migrate(old)['programs'][0]['steps'][1]['op'],'apply')

    def test_plan_getters_remain_exact_fresh_proof_expressions(self):
        old=self.metadata([dict(op='prove',id='done',pre='plan.r.pre',post='plan.r.post')])
        new=migrate(old)['programs'][0]['steps'][0]
        self.assertEqual((new['context'],new['claim'],new['depends_on']),('plan.r.pre','plan.r.post',[]))

class Acceptance(unittest.TestCase):
    def test_missing_unproved_or_guard_failed_diagnostics_reject(self):
        import copy
        from audit.lemma_candidates.validate import validate
        metadata={'programs':[{'id':'p','steps':[{'op':'candidate','id':'c'},{'op':'use_candidate','id':'u'}]}]}
        d={'program':'p','target_closed':True,'saved_reports_are_authority':False,'error':None,
           'candidates':[{'id':'c','state':'applied','validity':'established','claim_true':True}],
           'uses':[{'id':'u','state':'applied'}]}
        report={'obligations':[{'children':[{'lemma_candidates':d}]}]}
        self.assertEqual(validate(report,metadata),dict(programs=1,checked_candidates=1,checked_guard_uses=1))
        for kind in ('missing','unproved','guard','target','authority'):
            bad=copy.deepcopy(report);row=bad['obligations'][0]['children'][0]['lemma_candidates']
            if kind=='missing':row['candidates']=[]
            elif kind=='unproved':row['candidates'][0]['claim_true']=None
            elif kind=='guard':row['uses'][0]['state']='unknown_budget'
            elif kind=='target':row['target_closed']=False
            else:row['saved_reports_are_authority']=True
            with self.assertRaises(ValueError):validate(bad,metadata)

    def test_native_rv_gate_rejects_missing_source_coverage(self):
        import copy
        from audit.lemma_candidates.validate import validate_native_rv_sources
        children=[]
        for program, ids in [('m_address',['ir']),('x_operand1',['delivery','equality'])]:
            children.append({'lemma_candidates':{'program':program,'candidates':[
                {'id':name,'source':'rv.hwv:7:11','state':'applied','usefulness':'target_closed'} for name in ids]}})
        report={'obligations':[{'children':children}]}
        validate_native_rv_sources(report,'rv.hwv')
        unused=copy.deepcopy(report)
        unused['obligations'][0]['children'][0]['lemma_candidates']['candidates'][0]['usefulness']='established_but_unused'
        with self.assertRaises(ValueError):validate_native_rv_sources(unused,'rv.hwv')
        for source in (None,'JSON source','rv.hwv:0:1','other.hwv:7:11'):
            bad=copy.deepcopy(report)
            bad['obligations'][0]['children'][0]['lemma_candidates']['candidates'][0]['source']=source
            with self.assertRaises(ValueError):validate_native_rv_sources(bad,'rv.hwv')

if __name__=='__main__':unittest.main()
