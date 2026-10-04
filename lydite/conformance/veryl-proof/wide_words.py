"""Chunked Bool/BV formula encoding; no expected values or hardware execution.

All generated solver words are <=64 bits. Big words use little-endian32-bit
chunks; carry/product intermediates are reified as separate SSA equations.
"""
def balanced(op,xs,empty):
    if not xs:return empty
    if len(xs)==1:return xs[0]
    m=len(xs)//2;return [op,balanced(op,xs[:m],empty),balanced(op,xs[m:],empty)]

def lit(w,n):return ['bv',w,n&((1<<w)-1)]
def fit(t,old,new):
    if old==new:return t
    return ['zext',new-old,t] if new>old else ['extract',new-1,0,t]

def const_parts(width,value):return [(min(32,width-i),lit(min(32,width-i),value>>i)) for i in range(0,width,32)]
def join(parts):
    if not parts:raise ValueError('empty scalar join')
    w,t=parts[-1]
    for ow,ot in reversed(parts[:-1]):w,t=w+ow,['concat',t,ot]
    return w,t

def cut(parts,offset,width):
    """Slice arbitrary low-to-high segments, zero-filled beyond either end."""
    output=[];positions=[];start=0
    for w,t in parts:positions.append((start,start+w,t));start+=w
    for at in range(0,width,32):
        size=min(32,width-at);low=offset+at;high=low+size;pieces=[];cursor=low
        if cursor<0:
            n=min(high,0)-cursor
            if n>0:pieces.append((n,lit(n,0)));cursor+=n
        for begin,end,term in positions:
            l=max(cursor,begin);h=min(high,end)
            if h>l:
                bits=h-l;part=term if l==begin and h==end else ['extract',h-begin-1,l-begin,term]
                pieces.append((bits,part));cursor=h
            if cursor>=high:break
        if cursor<high:pieces.append((high-cursor,lit(high-cursor,0)))
        output.append(join(pieces))
    return output

class Encoder:
    def __init__(self,proof):self.proof=proof
    def width(self,t):
        if isinstance(t,str):return self.proof.decls[t]['bv']
        op=t[0]
        if op in ('bv','wide'):return t[1]
        if op=='extract':return t[1]-t[2]+1
        if op in ('zext','sext'):return t[1]+self.width(t[2])
        if op=='concat':return self.width(t[1])+self.width(t[2])
        if op=='ite':return self.width(t[2])
        return self.width(t[1])
    def scalar(self,w,t):return self.proof.scalar_bind(w,t)
    def bits(self,t):
        if isinstance(t,str):
            if t in self.proof.proven_vars:return const_parts(self.width(t),self.proof.proven_vars[t])
            return cut([(self.width(t),t)],0,self.width(t))
        op=t[0];w=self.width(t)
        if op=='bv':return const_parts(w,t[2])
        if op=='wide':return [part for _,term in t[2] for part in self.bits(term)]
        if op=='extract':return cut(self.bits(t[3]),t[2],w)
        if op in ('zext','sext'):
            x=self.bits(t[2]);old=self.width(t[2]);extra=w-old
            if op=='zext':padding=const_parts(extra,0)
            else:
                bit=cut(x,old-1,1)[0][1];sign=['eq',bit,lit(1,1)]
                padding=[(pw,['ite',sign,lit(pw,(1<<pw)-1),lit(pw,0)]) for pw,_ in const_parts(extra,0)]
            return cut(x+padding,0,w)
        if op=='concat':
            pending=[t];parts=[]
            while pending:
                item=pending.pop()
                if isinstance(item,list) and item[0]=='concat':pending.extend([item[1],item[2]])
                else:parts.extend(self.bits(item))
            return cut(parts,0,w)
        if op=='ite':
            condition=self.boolean(t[1]);a=self.bits(t[2]);b=self.bits(t[3]);return [(pw,['ite',condition,at,bt]) for (pw,at),(_,bt) in zip(a,b)]
        if op=='bnot':return [(pw,['bnot',pt]) for pw,pt in self.bits(t[1])]
        if op in ('band','bor','bxor'):
            a=self.bits(t[1]);b=self.bits(t[2]);return [(pw,[op,at,bt]) for (pw,at),(_,bt) in zip(a,b)]
        if op in ('add','sub'):
            return self.add(self.bits(t[1]),self.bits(t[2]),subtract=op=='sub')
        if op=='mul':
            if w<=64:return cut([(w,['mul',join(self.bits(t[1]))[1],join(self.bits(t[2]))[1]])],0,w)
            a=self.bits(t[1]);b=self.bits(t[2]);result=const_parts(w,0)
            for i,(aw,at) in enumerate(a):
                for j,(bw,bt) in enumerate(b):
                    if 32*(i+j)>=w:continue
                    product=self.scalar(64,['mul',fit(at,aw,64),fit(bt,bw,64)])
                    shifted=cut(const_parts(32*(i+j),0)+[(64,product)],0,w)
                    result=self.add(result,shifted)
            return result
        if op in ('shl','lshr'):
            if t[2][0]!='bv':raise ValueError('wide shifts require a proved unique literal count')
            shift=t[2][2]
            return cut(self.bits(t[1]),shift if op=='lshr' else -shift,w)
        raise ValueError('wide encoding operator '+str(op))
    def add(self,a,b,subtract=False):
        carry=lit(1,1 if subtract else 0);result=[]
        for (w,x),(_,y) in zip(a,b):
            if subtract:y=['bnot',y]
            total=self.scalar(w+1,['add',['add',fit(x,w,w+1),fit(y,w,w+1)],fit(carry,1,w+1)])
            result.append((w,['extract',w-1,0,total]));carry=['extract',w,w,total]
        return result
    def boolean(self,t):
        if isinstance(t,bool):return t
        op=t[0]
        if op in ('and','or','xor','not','implies'):
            return [op,*[self.boolean(x) for x in t[1:]]]
        if op in ('eq','ne','ult','ule','slt','sle'):
            a=self.bits(t[1]);b=self.bits(t[2])
            if sum(w for w,_ in a)!=sum(w for w,_ in b):raise ValueError('wide comparison width mismatch')
            differences=[['bxor',fit(x,pw,32),fit(y,pw,32)] for (pw,x),(_,y) in zip(a,b)]
            eq=['eq',balanced('bor',differences,lit(32,0)),lit(32,0)]
            if op=='eq':return eq
            if op=='ne':return ['not',eq]
            less=False
            for (_,x),(_,y) in zip(a,b):less=['or',['ult',x,y],['and',['eq',x,y],less]]
            if op.startswith('s') and a:
                w,x=a[-1];_,y=b[-1];sx=['extract',w-1,w-1,x];sy=['extract',w-1,w-1,y]
                less=['ite',['ne',sx,sy],['eq',sx,lit(1,1)],less]
            return ['or',less,eq] if op.endswith('le') else less
        if op=='ite':return ['ite',self.boolean(t[1]),self.boolean(t[2]),self.boolean(t[3])]
        raise ValueError('boolean encoding operator '+op)
