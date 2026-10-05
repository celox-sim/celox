"""Four-state payload/mask SIR equations; X=(1,1), Z=(0,1).

This encodes operators into the same finite Bool/BV equations as the two-state
route. It does not obtain expected values from a circuit or simulator.
"""
from proof_backend import *

def band(a,b):return Word(a.w,['band',a.t,b.t])
def bor(a,b):return Word(a.w,['bor',a.t,b.t])
def bxor(a,b):return Word(a.w,['bxor',a.t,b.t])
def inv(a):return Word(a.w,['bnot',a.t])
def mux(c,a,b):return Word(a.w,['ite',c,a.t,b.t])
def one(a,m):return truth(band(a,inv(m)))
def zero(a,m):return truth(band(inv(a),inv(m)))
def unknown(m):return truth(m)
def boollogic(known_true,known_false):
    mask=boolword(['not',['or',known_true,known_false]])
    return bor(boolword(known_true),mask),mask

class FourStateBackend(Backend):
    def __init__(self,compiled,four_state,out):
        super().__init__(compiled,False,out)
        self.masks={};self.four_state=True
        for key,metadata in self.meta.items():
            width=self.state[key].w;initial=int(metadata['is_4state'])
            self.state[key]=Storage(width,fill=initial);self.masks[key]=Storage(width,fill=initial)
        for initial in self.code['design']['initial_state']:
            key=address(initial['address'])
            for run in initial['data']['Writes']:
                self.state[key]=self.state[key].write(run['bit_offset'],run['bit_width'],const(run['bit_width'],bytes_value(run['value_bytes'])))
                mask=bytes_value(run['mask_bytes']) if self.meta[key]['is_4state'] else 0
                self.masks[key]=self.masks[key].write(run['bit_offset'],run['bit_width'],const(run['bit_width'],mask))
    def write(self,name,payload,mask=0,instances=()):
        signal=self.signal(name,instances);key=address(signal['address']);w=self.state[key].w
        self.state[key]=self.state[key].write(0,w,self.proof.bind(const(w,payload)))
        self.masks[key]=self.masks[key].write(0,w,self.proof.bind(const(w,mask if self.meta[key]['is_4state'] else 0)))
        self.dirty=True
    def read(self,name,instances=()):
        if self.dirty:self.eval_comb()
        key=address(self.signal(name,instances)['address']);w=self.state[key].w
        return self.proof.unique(self.state[key].read(0,w),'sample payload '+name),self.proof.unique(self.masks[key].read(0,w),'sample mask '+name)
    def load_mask(self,key,offset,w):
        if key not in self.masks:self.masks[key]=Storage(self.state[key[:2]+(0,)].w)
        return self.masks[key].read(offset,w)
    def store_mask(self,key,offset,w,value):
        root=key[:2]+(0,)
        if key not in self.masks:self.masks[key]=Storage(self.state[root].w)
        if not self.meta[root]['is_4state']:value=const(value.w,0)
        self.masks[key]=self.masks[key].write(offset,w,value)
    def poison(self,value,w,condition):
        ones=const(w,(1<<w)-1);mask=mux(condition,ones,const(w,0))
        return mux(condition,ones,resize(value,w)),mask
    def unary4(self,op,a,m,w):
        if op=='Ident':return resize(a,w,a.signed and w<=64),resize(m,w,a.signed and w<=64)
        if op=='ToTwoState':return resize(band(a,inv(m)),w),const(w,0)
        if op=='BitNot':
            mask=resize(m,w,a.signed and w<=64);return bor(inv(resize(a,w,a.signed and w<=64)),mask),mask
        if op=='LogicNot':return boollogic(['and',['not',one(a,m)],['not',unknown(m)]],one(a,m))
        if op=='Or':return boollogic(one(a,m),['and',['not',one(a,m)],['not',unknown(m)]])
        if op=='And':return boollogic(['and',['not',zero(a,m)],['not',unknown(m)]],zero(a,m))
        return self.poison(self.unary(op,a,w),w,unknown(m))
    def binary4(self,op,a,am,b,bm,w):
        any_unknown=['or',unknown(am),unknown(bm)]
        if op in ('And','Or','Xor'):
            ap=resize(a,w,a.signed and w<=64);ma=resize(am,w,a.signed and w<=64);bp=resize(b,w);mb=resize(bm,w)
            if op=='Xor':mask=bor(ma,mb);return bor(bxor(ap,bp),mask),mask
            a1=band(ap,inv(ma));b1=band(bp,inv(mb));a0=band(inv(ap),inv(ma));b0=band(inv(bp),inv(mb))
            if op=='And':ones=band(a1,b1);zeros=bor(a0,b0)
            else:ones=bor(a1,b1);zeros=band(a0,b0)
            mask=inv(bor(ones,zeros));return bor(ones,mask),mask
        if op in ('Eq','Ne','EqWildcard','NeWildcard'):
            size=max(a.w,b.w);a,am,b,bm=[resize(x,size) for x in (a,am,b,bm)]
            mismatch=truth(band(bxor(a,b),inv(bor(am,bm))))
            cared_unknown=truth(band(am,inv(bm))) if op.endswith('Wildcard') else ['or',unknown(am),unknown(bm)]
            mask=boolword(['and',['not',mismatch],cared_unknown])
            payload=boolword(mismatch if op.startswith('Ne') else ['not',mismatch])
            return resize(bor(payload,mask),w),resize(mask,w)
        if op in ('EqCase','NeCase'):
            equal=['and',['eq',a.t,b.t],['eq',am.t,bm.t]]
            return resize(boolword(equal if op=='EqCase' else ['not',equal]),w),const(w,0)
        if op in ('LogicAnd','LogicOr'):
            at,bt=one(a,am),one(b,bm)
            af=['not',truth(bor(a,am))];bf=['not',truth(bor(b,bm))]
            if op=='LogicAnd':return boollogic(['and',at,bt],['or',af,bf])
            return boollogic(['or',at,bt],['and',af,bf])
        if op in ('Shl','Shr','Sar'):
            p=self.binary(op,a,b,w);m=self.binary(op,Word(am.w,am.t,a.signed),b,w)
            ones=const(w,(1<<w)-1);return mux(unknown(bm),ones,p),mux(unknown(bm),ones,m)
        condition=any_unknown
        if op in ('DivU','DivS','RemU','RemS'):condition=['or',condition,['eq',b.t,const(b.w,0).t]]
        # Prove the guard before partial evaluation; never choose a convenient
        # unknown/defined branch from one SAT model. This also avoids expanding
        # a large division/multiply whose language-defined result is already X.
        bad=self.proof.unique(boolword(condition),'four-state arithmetic guard')
        if bad:return const(w,(1<<w)-1),const(w,(1<<w)-1)
        return self.binary(op,a,b,w),const(w,0)
    def mux4(self,c,cm,a,am,b,bm,w):
        a,am,b,bm=[resize(x,w) for x in (a,am,b,bm)]
        differs=bor(bxor(a,b),bxor(am,bm));merge_p=bor(a,differs);merge_m=bor(am,differs)
        return mux(one(c,cm),a,mux(unknown(cm),merge_p,b)),mux(one(c,cm),am,mux(unknown(cm),merge_m,bm))
    def offset4(self,off,regs,masks):
        ids=[]
        if 'Dynamic' in off:ids=[off['Dynamic']]
        elif 'Element' in off:
            value=off['Element'];ids=[value['index']]+([value['dynamic_bit_offset']] if value['dynamic_bit_offset'] is not None else [])
        for identifier in ids:
            if self.proof.unique(masks[identifier],'array-address mask')!=0:
                raise Unsupported('unknown array-address mask requires explicit address semantics')
        return self.offset(off,regs)
    def execute(self,unit):
        regs={};masks={};types={int(k):next(iter(t.items())) for k,t in unit['register_map'].items()}
        def width(r):return types[r][1]['width']
        def setreg(r,p,m):
            p=resize(p,width(r));p=Word(p.w,p.t,types[r][0]=='Bit' and types[r][1].get('signed',False))
            regs[r]=self.proof.bind(p);masks[r]=self.proof.bind(resize(m,width(r)))
        def concat(parts):
            if not parts:raise Unsupported('empty concat')
            if len(parts)==1:return parts[0]
            mid=len(parts)//2;hi=concat(parts[:mid]);lo=concat(parts[mid:]);return Word(hi.w+lo.w,['concat',hi.t,lo.t])
        block=unit['entry_block_id'];steps=0
        while True:
            steps+=1
            if steps>10000:raise Unsupported('SIR control-flow step budget exceeded')
            body=unit['blocks'][str(block)]
            for instruction in body['instructions']:
                op,args=next(iter(instruction.items()))
                if op=='Imm':r,v=args;setreg(r,const(width(r),bytes_value(v['payload'])),const(width(r),bytes_value(v['mask'])))
                elif op=='Unary':r,kind,a=args;setreg(r,*self.unary4(kind,regs[a],masks[a],width(r)))
                elif op=='Binary':r,a,kind,b=args;setreg(r,*self.binary4(kind,regs[a],masks[a],regs[b],masks[b],width(r)))
                elif op=='Load':
                    r,loc,off,w=args;key=address(loc);offset=self.offset4(off,regs,masks)
                    setreg(r,self.load(key,offset,w),self.load_mask(key,offset,w))
                elif op=='Store':
                    loc,off,w,r,triggers,sites=args;key=address(loc);offset=self.offset4(off,regs,masks)
                    self.store(key,offset,w,regs[r]);self.store_mask(key,offset,w,masks[r])
                elif op=='Commit':
                    src,dst,off,w,triggers=args;src,dst=address(src),address(dst);offset=self.offset4(off,regs,masks)
                    for plane in (self.state,self.masks):
                        if src not in plane:plane[src]=Storage(self.state[src[:2]+(0,)].w)
                        if dst not in plane:plane[dst]=Storage(self.state[dst[:2]+(0,)].w)
                        source=plane[src]
                        if src[2]==2:
                            for begin,size,value in source.segments:plane[dst]=plane[dst].write(begin,size,value)
                            plane[src]=Storage(source.w)
                        elif offset==0 and w==source.w and w==plane[dst].w:plane[dst]=source
                        elif w<=4096:plane[dst]=plane[dst].write(offset,w,self.proof.bind(source.read(offset,w)))
                        else:plane[dst]=plane[dst].write(offset,w,(source,offset))
                elif op=='Concat':r,parts=args;setreg(r,concat([regs[x] for x in parts]),concat([masks[x] for x in parts]))
                elif op=='Slice':r,a,offset,w=args;setreg(r,slice_(regs[a],offset,w),slice_(masks[a],offset,w))
                elif op=='Mux':r,c,a,b=args;setreg(r,*self.mux4(regs[c],masks[c],regs[a],masks[a],regs[b],masks[b],width(r)))
                elif op in ('RuntimeEvent','CombCaptureEvent','CombCaptureEnableIfChanged'):self.runtime_event(op,args,regs,masks)
                else:raise Unsupported('SIR instruction '+op)
            term=body['terminator']
            if term=='Return':return
            kind,data=next(iter(term.items()))
            if kind=='Error':raise RuntimeError('SIR runtime error '+str(data))
            if kind=='Jump':target,arguments=data
            elif kind=='Branch':
                value=self.proof.unique(regs[data['cond']],'SIR procedural branch payload')
                target,arguments=data['true_block' if value else 'false_block']
            elif kind=='Switch':
                value=self.proof.unique(regs[data['selector']],'SIR switch');target=data['default'];arguments=[]
                for case in data['cases']:
                    if bytes_value(case['value'])==value:target=case['target'];break
            else:raise Unsupported('terminator '+kind)
            values=[(regs[x],masks[x]) for x in arguments];params=unit['blocks'][str(target)]['params']
            if len(values)!=len(params):raise RuntimeError('block argument arity')
            for r,(value,mask) in zip(params,values):setreg(r,value,mask)
            block=target
