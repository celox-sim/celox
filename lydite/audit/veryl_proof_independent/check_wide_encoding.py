import random,json,hashlib
from pathlib import Path
import argparse, sys
parser = argparse.ArgumentParser()
parser.add_argument('--module-dir', type=Path, required=True)
parser.add_argument('--out', type=Path, required=True)
args = parser.parse_args()
module_dir = args.module_dir.resolve()
args.out.mkdir(parents=True, exist_ok=False)
sys.path.insert(0, str(module_dir))
from wide_words import Encoder
class P:
 def __init__(self):self.decls={};self.values={};self.proven_vars={};self.enc=Encoder(self)
 def scalar_bind(self,w,t):
  assert 1<=w<=64,w
  v,tw=self.eval(t);assert tw==w,(tw,w)
  n='x'+str(len(self.values));self.decls[n]={'bv':w};self.values[n]=(v,w)
  if len(self.values)%2==0:self.proven_vars[n]=v
  return n
 def eval(self,t):
  if isinstance(t,bool):return t,0
  if isinstance(t,str):return self.values[t]
  op=t[0]
  if op=='bv':return t[2]&((1<<t[1])-1),t[1]
  if op=='extract':
   v,w=self.eval(t[3]);assert 0<=t[2]<=t[1]<w
   n=t[1]-t[2]+1;return(v>>t[2])&((1<<n)-1),n
  if op in ('zext','sext'):
   v,w=self.eval(t[2]);n=w+t[1]
   if op=='sext' and v>>(w-1):v|=((1<<t[1])-1)<<w
   return v,n
  if op=='ite':return self.eval(t[2] if self.eval(t[1])[0] else t[3])
  if op=='not':return not self.eval(t[1])[0],0
  a,aw=self.eval(t[1])
  if op=='bnot':return (~a)&((1<<aw)-1),aw
  b,bw=self.eval(t[2])
  if op=='concat':return (a<<bw)|b,aw+bw
  if op in ('and','or','xor','implies'):
   assert aw==bw==0
   return {'and':a and b,'or':a or b,'xor':bool(a)^bool(b),'implies':not a or b}[op],0
  assert aw==bw,(op,aw,bw)
  if op in ('eq','ne','ult','ule','slt','sle'):
   if op.startswith('s'):a=a-(1<<aw) if a>>(aw-1) else a;b=b-(1<<bw) if b>>(bw-1) else b
   return {'eq':a==b,'ne':a!=b,'ult':a<b,'ule':a<=b,'slt':a<b,'sle':a<=b}[op],0
  value={'add':lambda:a+b,'sub':lambda:a-b,'mul':lambda:a*b,'band':lambda:a&b,'bor':lambda:a|b,'bxor':lambda:a^b}[op]()
  return value&((1<<aw)-1),aw
 def input(self,w,n):
  return ['wide',w,[[min(32,w-i),self.scalar_bind(min(32,w-i),['bv',min(32,w-i),n>>i])] for i in range(0,w,32)]]
 def encoded(self,t):
  v=0;shift=0
  for w,term in self.enc.bits(t):
   x,xw=self.eval(term);assert xw==w and w<=64;v|=x<<shift;shift+=w
  return v
rng=random.Random(81732);count=0
widths=[1,2,7,31,32,33,63,64,65,95,96,127,128,129,255,256,1024,2048]
for w in widths:
 mask=(1<<w)-1
 for rep in range(20):
  a,b=(rng.getrandbits(w),rng.getrandbits(w)) if rep>=5 else [(0,0),(mask,1),(1,mask),(mask,mask),(1<<(w-1),1)][rep]
  for op,fn in [('add',lambda:a+b),('sub',lambda:a-b),('mul',lambda:a*b),('band',lambda:a&b),('bor',lambda:a|b),('bxor',lambda:a^b)]:
   if op=='mul' and w>256 and rep>=5:continue
   p=P();actual=p.encoded([op,p.input(w,a),p.input(w,b)]);expected=fn()&mask;assert actual==expected,(w,op,a,b,actual,expected);count+=1
  for op in ['eq','ne','ult','ule','slt','sle']:
   p=P();actual=p.eval(p.enc.boolean([op,p.input(w,a),p.input(w,b)]))[0];sa=a-(1<<w) if a>>(w-1) else a;sb=b-(1<<w) if b>>(w-1) else b
   expected={'eq':a==b,'ne':a!=b,'ult':a<b,'ule':a<=b,'slt':sa<sb,'sle':sa<=sb}[op];assert actual==expected,(w,op,a,b);count+=1
  for shift in [0,1,31,32,33,w-1,w,w+1,2*w]:
   for op in ['shl','lshr']:
    p=P();actual=p.encoded([op,p.input(w,a),['bv',max(w,16),shift]]);expected=((a<<shift) if op=='shl' else a>>shift)&mask;assert actual==expected,(w,op,a,shift,actual,expected);count+=1
  for extra in [1,31,32,33,65]:
   for op in ['zext','sext']:
    p=P();actual=p.encoded([op,extra,p.input(w,a)]);signed=a-(1<<w) if a>>(w-1) else a;expected=(signed if op=='sext' else a)&((1<<(w+extra))-1);assert actual==expected,(w,op,extra);count+=1
for left in widths:
 for right in widths:
  p=P();a=rng.getrandbits(left);b=rng.getrandbits(right)
  joined=['concat',p.input(left,a),p.input(right,b)];expected=(a<<right)|b
  assert p.encoded(joined)==expected;count+=1
  for _ in range(3):
   lo=rng.randrange(left+right);hi=rng.randrange(lo,left+right)
   assert p.encoded(['extract',hi,lo,joined])==(expected>>lo)&((1<<(hi-lo+1))-1);count+=1
for w in widths:
 for choice in [False,True]:
  p=P();a=rng.getrandbits(w);b=rng.getrandbits(w)
  assert p.encoded(['ite',choice,p.input(w,a),p.input(w,b)])==(a if choice else b);count+=1
  assert p.encoded(['bnot',p.input(w,a)])==((~a)&((1<<w)-1));count+=1
summary={'status':'passed','checks':count,'widths':widths,'seed':81732,'encoder_sha256':hashlib.sha256((module_dir/'wide_words.py').read_bytes()).hexdigest(),'scope':'independent concrete evaluation of generated chunk formulas, not solver verification'}
(args.out/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(summary)
