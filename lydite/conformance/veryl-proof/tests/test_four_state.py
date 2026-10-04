import pathlib,tempfile,unittest
from proof_backend import Proof,const,Word
from four_state import FourStateBackend
STATES=[(0,0),(1,0),(1,1),(0,1)]
class FourStateTests(unittest.TestCase):
 def setUp(self):
  self.temp=tempfile.TemporaryDirectory();self.backend=FourStateBackend.__new__(FourStateBackend);self.backend.proof=Proof(pathlib.Path(self.temp.name)/'proof')
 def tearDown(self):self.backend.proof.close();self.temp.cleanup()
 def value(self,pair):return tuple(self.backend.proof.unique(x) for x in pair)
 def pair(self,x):return const(1,x[0]),const(1,x[1])
 def test_bitwise_truth_tables(self):
  for a in STATES:
   for b in STATES:
    ap,am=self.pair(a);bp,bm=self.pair(b)
    for op in ['And','Or','Xor']:
     if op=='And':expected=(0,0) if a==(0,0) or b==(0,0) else (1,1) if a[1] or b[1] else (1,0)
     elif op=='Or':expected=(1,0) if a==(1,0) or b==(1,0) else (1,1) if a[1] or b[1] else (0,0)
     else:expected=(1,1) if a[1] or b[1] else (a[0]^b[0],0)
     self.assertEqual(self.value(self.backend.binary4(op,ap,am,bp,bm,1)),expected)
 def test_case_and_logical_equality(self):
  for a in STATES:
   for b in STATES:
    ap,am=self.pair(a);bp,bm=self.pair(b)
    for op in ['Eq','Ne','EqCase','NeCase','EqWildcard','NeWildcard']:
     unequal=op.startswith('Ne')
     if op.endswith('Case'):expected=(int((a==b)^unequal),0)
     elif op.endswith('Wildcard') and b[1]:expected=(int(not unequal),0)
     elif a[1] or b[1]:expected=(1,1)
     else:expected=(int((a==b)^unequal),0)
     self.assertEqual(self.value(self.backend.binary4(op,ap,am,bp,bm,1)),expected)
 def test_mux_preserves_matching_z_and_merges_differences(self):
  for c in STATES:
   for a in STATES:
    for b in STATES:
     expected=a if c==(1,0) else b if c==(0,0) else a if a==b else (1,1)
     self.assertEqual(self.value(self.backend.mux4(*self.pair(c),*self.pair(a),*self.pair(b),1)),expected)
 def test_count_operators_exhaustive_small_widths(self):
  for w in range(1,6):
   for n in range(1<<w):
    for op,expected in [('PopCount',n.bit_count()),('CountLeadingZeros',w-n.bit_length()),('CountTrailingZeros',w if n==0 else (n&-n).bit_length()-1)]:
     self.assertEqual(self.backend.proof.unique(self.backend.unary(op,const(w,n),w)),expected)
 def test_unknown_arithmetic_is_all_x(self):
  for op in ['Add','Sub','Mul']:
   self.assertEqual(self.value(self.backend.binary4(op,const(8,0),const(8,1),const(8,19),const(8,0),8)),(255,255))
if __name__=='__main__':unittest.main()
