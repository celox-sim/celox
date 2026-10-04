"""Fail-closed regression checks for the consolidated latency harness."""
import copy
import unittest
from audit.veryl_scaling import rv32i_latency_common as l
from audit.veryl_scaling.rv32i_latency_variants import VARIANTS

class SharedTests(unittest.TestCase):
    def doc(self):
        terms=[l.s.eq('n.d_pc','i.pc'),l.s.eq('n.d_ir','i.ir')]+[True]*17
        return {'specs':{'Contract':{'state':{'d_pc':{'bv':32},'d_ir':{'bv':32}},'operations':{'tick':l.c.conj(terms)}}}}
    def test_exact_partition_coverage(self):
        docs,cover=l.partition_source(self.doc(),19)
        self.assertEqual(len(docs),81)
        for j in (0,1):self.assertEqual([x['bit'] for x in cover if x['conjunct']==j],list(range(32)))
        self.assertEqual(len([x for x in cover if x['bit'] is None]),17)
    def test_partition_reordering(self):
        d=self.doc();terms=list(l.conjuncts(d['specs']['Contract']['operations']['tick']))
        terms=terms[2:]+terms[:2];d['specs']['Contract']['operations']['tick']=l.c.conj(terms)
        _,cover=l.partition_source(d,19)
        self.assertEqual({x['conjunct'] for x in cover if x['bit'] is not None},{17,18})
    def test_bad_width_rejected(self):
        d=self.doc();d['specs']['Contract']['state']['d_ir']={'bv':16}
        with self.assertRaises(ValueError):l.partition_source(d,19)
    def test_duplicate_or_missing_split_rejected(self):
        d=self.doc();terms=list(l.conjuncts(d['specs']['Contract']['operations']['tick']));terms[1]=terms[0]
        d['specs']['Contract']['operations']['tick']=l.c.conj(terms)
        with self.assertRaises(ValueError):l.partition_source(d,19)
    def test_nonzero_reset_projection(self):
        v=VARIANTS['onehot'];raw={'state':{k:'bool' for k in l.builders(v)[0]},'reset':{k:False for k in l.builders(v)[0]}}
        for k,_,_ in v.nonzero_resets:raw['state'][k]={'bv':32};raw['reset'][k]=l.s.b(32,1)
        l.check_reset(raw,v)
        raw['reset']['d_sel1']=l.s.b(32,0)
        with self.assertRaises(ValueError):l.check_reset(raw,v)
    def test_missing_reset_field_rejected(self):
        source='var a: bit; var b: bit; always_ff (clk) { if !rst_n { a=0; } else if !stall { a=1; b=0; } }'
        with self.assertRaises(ValueError):l.state_types(source)
    def test_comparisons_not_parsed_as_assignments(self):
        source='var a: bit; always_ff (clk) { if !rst_n { a=0; } else if !stall { if other == 3 { a=1; } } }'
        self.assertEqual(l.state_types(source),{'a':'bool'})
    def test_unknown_and_forged_reports_rejected(self):
        for report in ({},{'status':'unknown'},{'status':'binding_verified_no_examples','implementation_binding':{'status':'verified','obligations':[]}}):
            with self.assertRaises(ValueError):l.validate_lemma(report)
    def test_unreplayed_mutation_rejected(self):
        q={'name':'binding_product_preservation','solver_result':'sat','backend':'finite_bv','finite':{'original_formula_validated':False}}
        with self.assertRaises(ValueError):l.validate_mutation({'implementation_binding':{'obligations':[q]}})
    def test_mutation_requires_counterexample_status(self):
        records=[]
        for name,result,status,expect in [('binding_reset_nonempty','sat','passed','sat'),
          ('binding_reset_establishes_product','unsat','passed','unsat'),
          ('binding_product_preservation','sat','counterexample','unsat')]:
            records.append({'name':name,'solver_result':result,'status':status,'logical_expectation':expect,
                'backend':'finite_bv','finite':{'original_formula_validated':True}})
        report={'status':'implementation_binding_failed','implementation_binding':{'status':'failed','obligations':records}}
        l.validate_mutation(report)
        for key,value in [('status','passed'),('logical_expectation','sat')]:
            altered=copy.deepcopy(report);altered['implementation_binding']['obligations'][-1][key]=value
            with self.assertRaises(ValueError):l.validate_mutation(altered)
        report['status']='unknown'
        with self.assertRaises(ValueError):l.validate_mutation(report)
    def test_selector_projection_is_independent(self):
        from audit.veryl_scaling.rv32i_latency_models import selector_alignment_document
        raw={'state':{'d_ir':{'bv':32},'d_sel1':{'bv':32},'halted':'bool'},
             'reset':{'d_ir':l.s.b(32,0),'d_sel1':l.s.b(32,1),'halted':False},
             'next':{'d_ir':['ite','s.halted','s.d_ir','i.imem_response'],'d_sel1':'s.d_sel1','halted':'s.halted'},'wires':{}}
        doc=selector_alignment_document(raw,'d_sel1')
        self.assertEqual(set(doc['implementation']['state']),{'d_ir','d_sel1'})
        self.assertIn('pre_halted',doc['implementation']['inputs'])
        self.assertEqual(doc['implementation']['reset']['d_sel1'],l.s.b(32,1))
        self.assertIs(doc['specs']['Contract']['operations']['tick'],True)
        self.assertNotIn('tag',str(doc));self.assertNotIn('age',str(doc['specs']['Contract']['invariant']))
    def test_variant_source_identities_are_distinct(self):
        self.assertEqual(len({v.source_sha256 for v in VARIANTS.values()}),5)
        self.assertEqual(VARIANTS['onehot'].nonzero_resets,(('d_sel1',32,1),('d_sel2',32,1)))
    def test_model_families_remain_distinct(self):
        self.assertEqual(l.builders(VARIANTS['baseline'])[4:],(4,3))
        self.assertEqual(l.builders(VARIANTS['onehot'])[4:],(6,4))
        self.assertNotEqual(l.digest(l.builders(VARIANTS['baseline'])[2]()),l.digest(l.builders(VARIANTS['onehot'])[2]()))

if __name__=='__main__':unittest.main()
