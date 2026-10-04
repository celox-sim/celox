"""Independent concrete interpreter. No Z3 import, no evaluation of code strings."""
from dataclasses import dataclass

@dataclass(frozen=True)
class BV:
 w:int
 v:int
 def __post_init__(self):object.__setattr__(self,'v',self.v % (1<<self.w))
 def signed(self):return self.v-(1<<self.w) if self.v>>(self.w-1) else self.v
@dataclass
class Mem:
 a:int
 w:int
 default:int
 cells:dict
 def read(self,a):return BV(self.w,self.cells.get(a.v,self.default))
 def write(self,a,v):return Mem(self.a,self.w,self.default,{**self.cells,a.v:v.v})
 def __eq__(self,other):
  if not isinstance(other,Mem) or (self.a,self.w)!=(other.a,other.w):return False
  keys=set(self.cells)|set(other.cells)
  if self.default!=other.default and len(keys)!=(1<<self.a):return False
  return all(self.cells.get(k,self.default)==other.cells.get(k,other.default) for k in keys)

def evaluate(e,env,wires=None,cache=None,active=None):
 wires={} if wires is None else wires;cache={} if cache is None else cache;active=set() if active is None else active
 if type(e) is bool:return e
 if isinstance(e,str):
  if e.startswith('w.'):
   n=e[2:]
   if n in cache:return cache[n]
   if n in active:raise ValueError('wire cycle')
   active.add(n);v=evaluate(wires[n],env,wires,cache,active);active.remove(n);cache[n]=v;return v
  return env[e]
 op,*args=e
 ev=lambda x:evaluate(x,env,wires,cache,active)
 if op=='bv':return BV(*args)
 if op=='const_mem':
  v=ev(args[1]);return Mem(args[0],v.w,v.v,{})
 if op=='ite':return ev(args[1] if ev(args[0]) else args[2])
 if op=='extract':
  hi,lo,x=args;x=ev(x);return BV(hi-lo+1,x.v>>lo)
 if op in ('zext','sext'):
  n,x=args;x=ev(x);return BV(x.w+n,x.v if op=='zext' else x.signed())
 a=[ev(x) for x in args]
 if op=='eq':return a[0]==a[1]
 if op=='ne':return a[0]!=a[1]
 if op=='not':return not a[0]
 if op=='and':return a[0] and a[1]
 if op=='or':return a[0] or a[1]
 if op=='xor':return a[0]!=a[1]
 if op=='implies':return not a[0] or a[1]
 if op=='read':return a[0].read(a[1])
 if op=='write':return a[0].write(a[1],a[2])
 if op=='concat':return BV(a[0].w+a[1].w,(a[0].v<<a[1].w)|a[1].v)
 if op=='bnot':return BV(a[0].w,~a[0].v)
 x,y=a
 if op in ('ult','ule','slt','sle'):
  u,v=(x.signed(),y.signed()) if op.startswith('s') else (x.v,y.v)
  return u<v if op.endswith('lt') else u<=v
 funcs={'add':lambda:x.v+y.v,'sub':lambda:x.v-y.v,'mul':lambda:x.v*y.v,'band':lambda:x.v&y.v,'bor':lambda:x.v|y.v,'bxor':lambda:x.v^y.v,'shl':lambda:x.v<<y.v if y.v<x.w else 0,'lshr':lambda:x.v>>y.v if y.v<x.w else 0}
 if op not in funcs:raise ValueError('unsupported '+op)
 return BV(x.w,funcs[op]())

def initial(m):return {k:evaluate(e,{}) for k,e in m['reset'].items()}
def evaluate_record(m,which,s,i):
 env={**{'s.'+k:v for k,v in s.items()},**{'i.'+k:v for k,v in i.items()}};cache={}
 return {k:evaluate(e,env,m.get('wires',{}),cache) for k,e in m[which].items()}
def step(m,s,i,reset='rst'):return initial(m) if i[reset] else evaluate_record(m,'next',s,i)
def outputs(m,s,i):return evaluate_record(m,'outputs',s,i)
def related(doc,s,t):return evaluate(doc['binding'],{**{'spec.'+k:v for k,v in s.items()},**{'impl.'+k:v for k,v in t.items()}})
def decode(x):
 if type(x) is bool:return x
 if isinstance(x,dict) and 'unsigned' in x:return BV(x['width'],x['unsigned'])
 if isinstance(x,dict) and x.get('kind')=='memory':return Mem(x['address_width'],x['word_width'],x['default'],{a['address']:a['value'] for a in x['writes']})
 raise ValueError('counterexample not independently replayable: '+str(x))
def record_decode(x):return {k:decode(v) for k,v in x.items()}
