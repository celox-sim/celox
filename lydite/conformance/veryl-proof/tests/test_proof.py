import pathlib,tempfile,unittest
from proof_backend import Proof,Word,const,Unsupported
class ProofTests(unittest.TestCase):
 def setUp(self):
  self.temp=tempfile.TemporaryDirectory();self.p=Proof(pathlib.Path(self.temp.name)/'proof')
 def tearDown(self):self.p.close();self.temp.cleanup()
 def test_free_value_is_not_arbitrarily_selected(self):
  self.p.decls['free']={'bv':2}
  with self.assertRaises(Unsupported):self.p.unique(Word(2,'free'))
 def test_unique_output_does_not_select_nonunique_internal_state(self):
  self.p.decls['free']={'bv':2};x=self.p.bind(Word(2,['band','free',const(2,0).t]))
  self.assertEqual(self.p.unique(x),0);self.p.compact()
  self.assertNotIn('free',self.p.proven_vars)
  with self.assertRaises(Unsupported):self.p.unique(Word(2,'free'))
 def test_relational_domain_is_not_dropped_by_compaction(self):
  self.p.decls['free']={'bv':2};self.p.constraints.append(['ne','free',const(2,0).t]);x=self.p.bind(const(2,0))
  self.p.compact();self.assertEqual(self.p.epoch,0);self.assertEqual(len(self.p.constraints),2)
  self.assertEqual(self.p.query(['eq','free',const(2,0).t])['solver_result'],'unsat')
 def test_infeasible_prefix_never_rebased(self):
  self.p.bind(const(2,1));self.p.constraints.append(False)
  self.p.compact();self.assertEqual(self.p.epoch,0)
  with self.assertRaises(RuntimeError):self.p.feasible()
 def test_joint_unique_ssa_rebase_preserves_saved_expression(self):
  x=self.p.bind(const(8,251));saved=Word(8,['add',x.t,const(8,7).t]);self.p.compact()
  self.assertEqual(self.p.epoch,1);self.assertEqual(self.p.unique(saved),2)
 def test_wide_saved_reference_survives_compaction(self):
  x=self.p.bind(const(128,(1<<100)+19));self.p.compact()
  y=self.p.bind(x);self.assertEqual(self.p.unique(y),(1<<100)+19)
 def test_compaction_and_full_prefix_agree(self):
  x=self.p.bind(const(8,0))
  for i in range(20):x=self.p.bind(Word(8,['add',x.t,const(8,i).t]))
  before=self.p.unique(x);self.p.compact();after=self.p.unique(x);self.assertEqual(before,after)
if __name__=='__main__':unittest.main()
