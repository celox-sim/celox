import pathlib,random,tempfile,unittest
from proof_backend import Proof,Storage,const
class StorageTests(unittest.TestCase):
 def setUp(self):
  self.tmp=tempfile.TemporaryDirectory();self.proof=Proof(pathlib.Path(self.tmp.name)/'proof')
 def tearDown(self):self.proof.close();self.tmp.cleanup()
 def test_random_partial_writes_and_snapshots(self):
  rng=random.Random(491)
  for width in [1,3,8,31,32,33,64,65,127,130]:
   for fill in [0,1]:
    store=Storage(width,fill=fill);expected=((1<<width)-1) if fill else 0
    snapshots=[]
    for _ in range(12):
     snapshots.append((store,expected));offset=rng.randrange(width+5);size=rng.randrange(1,36);source_width=rng.randrange(1,36);value=rng.getrandbits(source_width)
     store=store.write(offset,size,const(source_width,value))
     count=max(0,min(size,width-offset));mask=((1<<count)-1)<<offset
     expected=(expected&~mask)|((value<<offset)&mask)
     self.assertEqual(self.proof.unique(store.read(0,width)),expected)
    for old,bits in snapshots[::4]:self.assertEqual(self.proof.unique(old.read(0,width)),bits)
 def test_sparse_giant_array_does_not_require_dense_state(self):
  width=32*1048576;s=Storage(width);s=s.write(width-32,32,const(32,0xdeadbeef))
  self.assertEqual(self.proof.unique(s.read(width-32,32)),0xdeadbeef)
  self.assertEqual(self.proof.unique(s.read(width-64,32)),0)
  self.assertEqual(len(s.segments),1)
 def test_partial_snapshot_copy_survives_later_source_write(self):
  source=Storage(64).write(0,64,const(64,0x1122334455667788))
  dest=Storage(64).write(8,24,(source,16));source=source.write(0,64,const(64,0))
  self.assertEqual(self.proof.unique(dest.read(0,64)),0x44556600)
if __name__=='__main__':unittest.main()
