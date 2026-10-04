"""Proof-backed SIR relation executor; no concrete simulator or expected oracle.

Every control-flow/address/read concretization is accepted only after SAT(R)
and UNSAT(R AND value != candidate), using lydite's own finite solver.
State keeps symbolic SSA references, never a model-selected internal state.
"""
import dataclasses, json, math, pathlib, subprocess, gzip
from wide_words import Encoder, join, balanced
ROOT=pathlib.Path(__file__).resolve().parent
class Unsupported(ValueError):pass

def conjunction(xs):
    if not xs:return True
    if len(xs)==1:return xs[0]
    mid=len(xs)//2
    return ['and',conjunction(xs[:mid]),conjunction(xs[mid:])]
def disjunction(xs):
    if not xs:return False
    if len(xs)==1:return xs[0]
    mid=len(xs)//2
    return ['or',disjunction(xs[:mid]),disjunction(xs[mid:])]
@dataclasses.dataclass(frozen=True)
class Word:
    w:int
    t:object
    signed:bool=False

def const(w,n):
    if not 1<=w<=4096:raise Unsupported(f'word width {w} outside current 1..4096 implementation')
    return Word(w,['bv',w,int(n)&((1<<w)-1)])
def resize(x,w,signed=False):
    if x.w==w:return Word(w,x.t,signed)
    const(w,0)
    return Word(w,['sext' if signed else 'zext',w-x.w,x.t] if w>x.w else ['extract',w-1,0,x.t],signed)
def truth(x):return ['ne',x.t,const(x.w,0).t]
def boolword(x):return Word(1,['ite',x,const(1,1).t,const(1,0).t])
def slice_(x,offset,w):
    if offset<0:raise Unsupported('negative storage offset')
    if offset>=x.w:return const(w,0)
    size=min(w,x.w-offset)
    return resize(Word(size,['extract',offset+size-1,offset,x.t]),w)
def address(x):return (x['instance_id'],x['var_id'],x.get('region',0))
def bytes_value(x):return int.from_bytes(bytes(x),'little')

@dataclasses.dataclass(frozen=True)
class Storage:
    w:int
    segments:tuple=()
    fill:int=0
    def read(self,offset,width):
        boundaries={offset,offset+width}
        for begin,size,_ in self.segments:
            if offset<begin<offset+width:boundaries.add(begin)
            if offset<begin+size<offset+width:boundaries.add(begin+size)
        if offset<self.w<offset+width:boundaries.add(self.w)
        if offset<0<offset+width:boundaries.add(0)
        boundaries=sorted(boundaries);parts=[]
        for lo,hi in zip(boundaries,boundaries[1:]):
            part=const(hi-lo,((1<<(hi-lo))-1) if self.fill and 0<=lo<self.w else 0)
            if 0<=lo<self.w:
                for begin,size,value in reversed(self.segments):
                    if begin<=lo<begin+size:
                        if isinstance(value,tuple):
                            source,start=value;part=source.read(start+lo-begin,hi-lo)
                        else:part=slice_(value,lo-begin,hi-lo)
                        break
            parts.append(part)
        def combine(items):
            if len(items)==1:return items[0]
            mid=len(items)//2;lo=combine(items[:mid]);hi=combine(items[mid:])
            return Word(lo.w+hi.w,['concat',hi.t,lo.t])
        return combine(parts)
    def write(self,offset,width,value):
        if offset<0:raise Unsupported('negative storage offset')
        width=min(width,self.w-offset)
        if width<=0:return self
        kept=tuple(x for x in self.segments if not(offset<=x[0] and x[0]+x[1]<=offset+width))
        return Storage(self.w,kept+((offset,width,value),),self.fill)

class Proof:
    def __init__(self,out):
        self.out=pathlib.Path(out);self.out.mkdir(parents=True,exist_ok=False)
        self.p=subprocess.Popen([str(ROOT/'target/release/veryl-proof-finite-service')],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
        self.decls={};self.constraints=[];self.queries=[];self.unique_cache={};self.encoder=Encoder(self);self.proven_vars={};self.active_vars=[];self.epoch=0;self.compact_threshold=128;self.definitions={};self.negative_control=None
    def close(self):
        self.p.stdin.close();code=self.p.wait(timeout=10);self.p.stdout.close();self.p.stderr.close()
        if code:raise RuntimeError(f'finite service exited with status {code}')
        with gzip.open(self.out/'proof-audit.json.gz','wt') as file:json.dump(self.queries,file,separators=(',',':'))
        with gzip.open(self.out/'relation.json.gz','wt') as file:json.dump({'epoch':self.epoch,'declarations':self.decls,'constraints':self.constraints,'proved_prefix_values':self.proven_vars,'negative_control':self.negative_control},file,separators=(',',':'))
    def scalar_bind(self,width,term):
        name=f'v{len(self.decls)}';self.decls[name]={'bv':width}
        self.constraints.append(['eq',name,term]);self.active_vars.append(name);self.definitions[name]=term;return name
    def bind(self,value):
        parts=self.encoder.bits(value.t)
        if value.w<=64:
            return Word(value.w,self.scalar_bind(value.w,join(parts)[1]),value.signed)
        return Word(value.w,['wide',value.w,[[w,self.scalar_bind(w,t)] for w,t in parts]],value.signed)
    def used_declarations(self,value):
        used=set();pending=[value]
        while pending:
            item=pending.pop()
            if isinstance(item,str) and item in self.decls:used.add(item)
            elif isinstance(item,list):pending.extend(item)
            elif isinstance(item,dict):pending.extend(item.values())
        return {name:self.decls[name] for name in sorted(used)}
    def query(self,extra=True,context=None,hint='sat'):
        encoded_extra=self.encoder.boolean(extra) if extra is not True else True
        formula=conjunction(self.constraints+([encoded_extra] if encoded_extra is not True else []))
        request={'declarations':self.used_declarations([formula,context or {}]),'formula':formula, 'context':context or {},'hint':hint}
        self.p.stdin.write(json.dumps(request,separators=(',',':'))+'\n');self.p.stdin.flush()
        line=self.p.stdout.readline()
        if not line:raise RuntimeError('finite service terminated: '+self.p.stderr.read())
        result=json.loads(line)
        if 'error'in result:raise RuntimeError(result['error'])
        self.queries.append({'epoch':self.epoch,'constraint_count':len(self.constraints),'query':len(self.queries),'encoded_extra':encoded_extra,'context':context or {},**result})
        return result
    def feasible(self):
        result=self.query()
        if result['solver_result']!='sat' or not result['original_formula_validated']:
            raise RuntimeError('relation not proven feasible: '+str(result))
    def unique(self,word,purpose='read'):
        parts=self.encoder.bits(word.t)
        result=self.query(context={str(i):t for i,(_,t) in enumerate(parts)})
        if result['solver_result']!='sat' or not result['original_formula_validated']:
            raise RuntimeError(f'{purpose}: relation not feasible: '+str(result))
        value=0;offset=0
        for i,(width,_) in enumerate(parts):
            value|=result['context_values'][str(i)]['value']<<offset;offset+=width
        uniqueness=self.query(['ne',word.t,const(word.w,value).t],hint='unsat')
        if uniqueness['solver_result']!='unsat':raise Unsupported(f'{purpose}: output not proven unique: '+str(uniqueness))
        self.queries[-1]['purpose']=purpose
        if purpose.startswith('sample') and self.negative_control is None:
            negative=self.query(['ne',word.t,const(word.w,value^1).t],hint='sat')
            if negative['solver_result']!='sat' or not negative['original_formula_validated']:
                raise RuntimeError('poisoned observation did not produce a validated counterexample')
            self.negative_control={'status':'passed','query':len(self.queries)-1,'purpose':purpose}
        if len(self.constraints)>=self.compact_threshold:self.compact()
        return value
    def compact(self):
        # Never substitute a mere SAT assignment. Prove JOINT uniqueness of all
        # live SSA definitions first; nonunique internal state remains symbolic.
        expected=[['eq',name,self.definitions[name]] for name in self.active_vars]
        if self.constraints!=expected:
            # Retain any extra relational assumptions/guards, including domains
            # of nonunique state; this optimization only eliminates pure SSA.
            self.compact_threshold=max(self.compact_threshold*2,len(self.constraints)*2);return
        context={name:name for name in self.active_vars}
        sat=self.query(context=context)
        if sat['solver_result']!='sat' or not sat['original_formula_validated']:
            raise RuntimeError('SSA compaction relation is not feasible')
        values={name:entry['value'] for name,entry in sat['context_values'].items()}
        differences=[]
        for name,value in values.items():
            item=resize(Word(self.decls[name]['bv'],name),64).t
            differences.append(['bxor',item,const(64,value).t])
        changed=['ne',balanced('bor',differences,const(64,0).t),const(64,0).t]
        proof=self.query(changed,hint='unsat')
        if proof['solver_result']!='unsat':
            self.compact_threshold=max(self.compact_threshold*2,len(self.constraints)*2);return
        certificate={'epoch':self.epoch,'declarations':self.used_declarations(self.constraints),'constraints':self.constraints,
            'proved_unique_values':values,'feasibility_query':len(self.queries)-2,'joint_uniqueness_query':len(self.queries)-1}
        with gzip.open(self.out/f'epoch-{self.epoch}.json.gz','wt') as file:json.dump(certificate,file,separators=(',',':'))
        self.proven_vars.update(values);self.constraints=[];self.active_vars=[];self.epoch+=1;self.compact_threshold=128
    def check(self,predicate):
        self.feasible()
        result=self.query(['not',predicate],hint='unsat')
        if result['solver_result']!='unsat':raise AssertionError('original assertion violated or UNKNOWN: '+str(result))

def normalize_compiled(compiled):
    if 'signals' in compiled:return compiled
    if 'lookup_variables' not in compiled or 'lookup_state' not in compiled:raise Unsupported('missing typed signal lookup')
    # Compatibility with the independent patch-audit exporter. Its unqualified
    # variable listing is sufficient only for an explicitly single-module graph.
    if any(x['module']!=0 for x in compiled['lookup_variables']) or any(a['instance_id']!=0 for a,b in compiled['lookup_state']):
        raise Unsupported('legacy lookup projection cannot represent hierarchy')
    mapping={a['var_id']:b for a,b in compiled['lookup_state']}
    compiled=dict(compiled);compiled['signals']=[]
    for item in compiled['lookup_variables']:
        v=item['variable']
        if v['id'] in mapping:compiled['signals'].append({'instances':[],'path':v['path'],'kind':v['var_kind'],'signed':v['signed'],'metadata':v['metadata'],'address':mapping[v['id']]})
    return compiled

class Backend:
    def __init__(self,compiled,four_state,out):
        if four_state:raise Unsupported('four-state encoding not implemented in this scalar vertical slice')
        compiled=normalize_compiled(compiled)
        self.code=compiled;self.state={};self.meta={};self.sparse={};self.dirty=True;self.operations=0;self.event_log=[];self.div_cache={};self.div_events=[]
        self.signals={}
        for signal in compiled['signals']:
            key=(tuple(tuple(x) for x in signal['instances']),'.'.join(signal['path']))
            self.signals[key]=None if key in self.signals else signal
        for obj in compiled['design']['state_objects']:
            key=address(obj['address']);metadata=obj['metadata'];width=metadata['width']*math.prod(metadata['array_dims'])
            self.meta[key]=metadata;self.state[key]=Storage(width)
        for initial in compiled['design']['initial_state']:
            key=address(initial['address']);data=initial['data']
            if 'Writes' not in data:raise Unsupported('unimplemented initial-state form')
            for run in data['Writes']:
                self.state[key]=self.state[key].write(run['bit_offset'],run['bit_width'],const(run['bit_width'],bytes_value(run['value_bytes'])))
        self.proof=Proof(out)
        self.events={address(x['event']):x['units'] for x in compiled['sir']['eval_apply_ffs']}
        self.aliases={address(a):address(b) for a,b in compiled['design']['event_aliases']}
    def close(self):
        self.proof.close();(self.proof.out/'nonfatal-events.json').write_text(json.dumps(self.event_log,indent=2)+'\n')
        (self.proof.out/'division-reuse.json').write_text(json.dumps(self.div_events,indent=2)+'\n')
    def signal(self,name,instances=()):
        key=(tuple((x['name'],x.get('index') or 0) if isinstance(x,dict) else tuple(x) for x in instances),name)
        if key not in self.signals or self.signals[key] is None:raise Unsupported('unresolved/ambiguous API signal path '+str(key))
        return self.signals[key]
    def write(self,name,payload,mask=0,instances=()):
        if int(mask):raise Unsupported('nonzero input mask in two-state vertical slice')
        signal=self.signal(name,instances);key=address(signal['address']);old=self.state[key]
        self.state[key]=old.write(0,old.w,self.proof.bind(const(old.w,payload)));self.dirty=True
    def read(self,name,instances=()):
        if self.dirty:self.eval_comb()
        key=address(self.signal(name,instances)['address'])
        return self.proof.unique(self.state[key].read(0,self.state[key].w),'sample '+name),0
    def eval_comb(self):
        for unit in self.code['sir']['eval_comb']:self.execute(unit)
        self.dirty=False;self.operations+=1
    def tick(self,name):
        if self.dirty:self.eval_comb()
        key=address(self.signal(name)['address']);key=self.aliases.get(key,key)
        if key not in self.events:raise Unsupported('event has no eval/apply SIR '+name)
        for unit in self.events[key]:self.execute(unit)
        self.eval_comb()
    def load(self,key,offset,width):
        if key[2]==2:raise Unsupported('sparse next-state region cannot be loaded')
        if key not in self.state:
            base=key[:2]+(0,);self.state[key]=Storage(self.state[base].w)
        return self.state[key].read(offset,width)
    def store(self,key,offset,width,value):
        base=key[:2]+(0,)
        if key not in self.state:self.state[key]=Storage(self.state[base].w)
        self.state[key]=self.state[key].write(offset,width,value)
    def offset(self,offset,regs):
        if 'Static'in offset:return offset['Static']
        if 'PackedElements'in offset:return offset['PackedElements']['bit_offset']
        if 'Dynamic'in offset:return self.proof.unique(regs[offset['Dynamic']],'dynamic bit offset')
        if 'Element'in offset:
            x=offset['Element'];n=self.proof.unique(regs[x['index']],'array index')*x['element_width']+x['bit_offset']
            if x['dynamic_bit_offset'] is not None:n+=self.proof.unique(regs[x['dynamic_bit_offset']],'dynamic element offset')
            return n
        raise Unsupported('unrecognized storage offset')
    def unary(self,op,x,w):
        if op in ('Ident','ToTwoState'):return resize(x,w,x.signed if op=='Ident' else False)
        if op=='BitNot':return Word(w,['bnot',resize(x,w,x.signed).t])
        if op=='Minus':return Word(w,['sub',const(w,0).t,resize(x,w,True).t])
        if op=='LogicNot':return resize(boolword(['not',truth(x)]),w)
        if op in ('And','Or'):
            p=['eq',x.t,const(x.w,(1<<x.w)-1).t] if op=='And' else truth(x)
            return resize(boolword(p),w)
        if op=='Xor':
            p=balanced('xor',[truth(slice_(x,bit,1)) for bit in range(x.w)],False)
            return resize(boolword(p),w)
        if op in ('PopCount','CountLeadingZeros','CountTrailingZeros'):
            result=const(w,0 if op=='PopCount' else x.w)
            for bit in range(x.w):
                test=truth(slice_(x,bit,1))
                if op=='PopCount':result=Word(w,['add',result.t,resize(boolword(test),w).t])
                else:
                    k=x.w-1-bit
                    index=x.w-1-bit if op=='CountTrailingZeros' else bit
                    result=Word(w,['ite',truth(slice_(x,index,1)),const(w,k).t,result.t])
                result=self.proof.bind(result)
            return result
        raise Unsupported('unary '+op)
    def division(self,a,b,w,signed=False,remainder=False):
        query_start=len(self.proof.queries)
        a=resize(a,w);b=resize(b,w)
        divisor=self.proof.unique(b,'division divisor')
        if divisor==0:return const(w,0)
        numerator=self.proof.unique(a,'division numerator')
        key=(w,signed,numerator,divisor)
        if key in self.div_cache:
            q,r,source_event=self.div_cache[key]
            self.div_events.append({'key':key,'reused_from':source_event,'queries':[query_start,len(self.proof.queries)],'quotient':q,'remainder':r})
            return const(w,r if remainder else q)
        # Congruence-based reuse only after both operands have been proved
        # unique. First result is still obtained from the encoded divider.
        a=const(w,numerator);b=const(w,divisor)
        sign_a=truth(slice_(a,w-1,1));sign_b=truth(slice_(b,w-1,1))
        if signed:
            a_abs=self.proof.bind(Word(w,['ite',sign_a,['sub',const(w,0).t,a.t],a.t]))
            b_abs=self.proof.bind(Word(w,['ite',sign_b,['sub',const(w,0).t,b.t],b.t]))
        else:a_abs,b_abs=a,b
        rem=const(w+1,0);quotient=const(w,0)
        for bit in reversed(range(w)):
            moved=self.proof.bind(Word(w+1,['bor',['shl',rem.t,const(w+1,1).t],resize(slice_(a_abs,bit,1),w+1).t]))
            enough=['ule',resize(b_abs,w+1).t,moved.t]
            rem=self.proof.bind(Word(w+1,['ite',enough,['sub',moved.t,resize(b_abs,w+1).t],moved.t]))
            quotient=self.proof.bind(Word(w,['bor',['shl',quotient.t,const(w,1).t],resize(boolword(enough),w).t]))
            if len(self.proof.constraints)>=self.proof.compact_threshold:self.proof.compact()
        quotient_result=quotient;remainder_result=resize(rem,w)
        if signed:
            quotient_result=Word(w,['ite',['xor',sign_a,sign_b],['sub',const(w,0).t,quotient.t],quotient.t])
            remainder_result=Word(w,['ite',sign_a,['sub',const(w,0).t,remainder_result.t],remainder_result.t])
        q=self.proof.unique(quotient_result,'encoded quotient result')
        r=self.proof.unique(remainder_result,'encoded remainder result')
        self.div_cache[key]=(q,r,len(self.div_events))
        self.div_events.append({'key':key,'encoded':True,'queries':[query_start,len(self.proof.queries)],'quotient':q,'remainder':r})
        return const(w,r if remainder else q)
    def binary(self,op,a,b,w):
        if op in ('DivU','DivS','RemU','RemS'):return self.division(a,b,w,op.endswith('S'),op.startswith('Rem'))
        if op in ('Shl','Shr','Sar'):
            # The count is data, so uniquely determine it before using a host index.
            count=self.proof.unique(b,'shift count');count=min(count,max(a.w,w))
            if op=='Shl':return Word(w,['shl',resize(a,w,a.signed).t,const(w,min(count,w)).t])
            x=resize(a,max(a.w,w),op=='Sar');amount=const(x.w,min(count,x.w)).t
            term=['lshr',x.t,amount]
            if op=='Sar':term=['ite',truth(slice_(a,a.w-1,1)),['bnot',['lshr',['bnot',x.t],amount]],term]
            return resize(Word(x.w,term),w)
        if op in ('Eq','Ne','EqCase','NeCase','EqWildcard','NeWildcard','LtU','LeU','GtU','GeU','LtS','LeS','GtS','GeS'):
            width=max(a.w,b.w);sg=op.endswith('S');x=resize(a,width,sg).t;y=resize(b,width,sg).t
            if op.startswith(('Eq','Ne')):p=['ne' if op.startswith('Ne') else 'eq',x,y]
            else:
                operator=('s' if sg else 'u')+('le' if op.startswith(('Le','Ge')) else 'lt')
                p=[operator,y,x] if op.startswith(('Gt','Ge')) else [operator,x,y]
            return resize(boolword(p),w)
        if op in ('LogicAnd','LogicOr'):return resize(boolword(['and' if op=='LogicAnd' else 'or',truth(a),truth(b)]),w)
        names={'Add':'add','Sub':'sub','Mul':'mul','And':'band','Or':'bor','Xor':'bxor'}
        if op not in names:raise Unsupported('binary '+op)
        return Word(w,[names[op],resize(a,w,a.signed).t,resize(b,w).t])
    def runtime_event(self,operation,args,regs,masks=None):
        if operation=='CombCaptureEnableIfChanged':
            # Only nonfatal side-channel delivery flags; no circuit state.
            sites=args['sites']
        else:sites=[args['site_id']]
        metadata=self.code.get('runtime_event_sites',[])
        for site in sites:
            if site>=len(metadata):raise Unsupported('missing runtime-event metadata')
            kind=metadata[site]['kind']
            if kind not in ('Display','Write','AssertContinue'):
                raise Unsupported('fatal runtime-event activation requires explicit observer semantics')
        if args.get('fatal_error_code') is not None:raise Unsupported('fatal comb capture outside current event model')
        values=[]
        for arg in args.get('args',[]):
            p=self.proof.unique(regs[arg],'diagnostic argument')
            m=self.proof.unique(masks[arg],'diagnostic mask') if masks is not None else 0
            values.append([p,m])
        self.event_log.append({'operation':operation,'sites':sites,'values':values,
                              'claim':'nonfatal diagnostic arguments checked; console delivery is outside Backend observation contract'})
    def execute(self,unit):
        regs={};types={int(k):next(iter(t.items())) for k,t in unit['register_map'].items()}
        def width(r):return types[r][1]['width']
        def setreg(r,x):
            x=resize(x,width(r));x=Word(x.w,x.t,types[r][0]=='Bit' and types[r][1].get('signed',False));regs[r]=self.proof.bind(x)
        block=unit['entry_block_id'];steps=0
        while True:
            steps+=1
            if steps>10000:raise Unsupported('SIR control-flow step budget exceeded')
            body=unit['blocks'][str(block)]
            for instruction in body['instructions']:
                op,args=next(iter(instruction.items()))
                if op=='Imm':r,v=args;setreg(r,const(width(r),bytes_value(v['payload'])))
                elif op=='Unary':r,operation,x=args;setreg(r,self.unary(operation,regs[x],width(r)))
                elif op=='Binary':r,a,operation,b=args;setreg(r,self.binary(operation,regs[a],regs[b],width(r)))
                elif op=='Load':r,loc,off,w=args;setreg(r,self.load(address(loc),self.offset(off,regs),w))
                elif op=='Store':loc,off,w,r,triggers,sites=args;self.store(address(loc),self.offset(off,regs),w,regs[r])
                elif op=='Commit':
                    src,dst,off,w,triggers=args;src,dst=address(src),address(dst);offset=self.offset(off,regs)
                    if src not in self.state:self.state[src]=Storage(self.state[src[:2]+(0,)].w)
                    if dst not in self.state:self.state[dst]=Storage(self.state[dst[:2]+(0,)].w)
                    source=self.state[src]
                    if src[2]==2:
                        for begin,size,value in source.segments:self.state[dst]=self.state[dst].write(begin,size,value)
                        self.state[src]=Storage(source.w)
                    elif offset==0 and w==source.w and w==self.state[dst].w:
                        self.state[dst]=source
                    elif w<=4096:self.state[dst]=self.state[dst].write(offset,w,self.proof.bind(source.read(offset,w)))
                    else:self.state[dst]=self.state[dst].write(offset,w,(source,offset))
                elif op=='Concat':
                    r,parts=args
                    if not parts:raise Unsupported('empty concatenation')
                    x=regs[parts[0]]
                    for part in parts[1:]:x=Word(x.w+regs[part].w,['concat',x.t,regs[part].t])
                    setreg(r,x)
                elif op=='Slice':r,x,off,w=args;setreg(r,slice_(regs[x],off,w))
                elif op=='Mux':r,c,a,b=args;setreg(r,Word(width(r),['ite',truth(regs[c]),resize(regs[a],width(r)).t,resize(regs[b],width(r)).t]))
                elif op in ('RuntimeEvent','CombCaptureEvent','CombCaptureEnableIfChanged'):self.runtime_event(op,args,regs)
                else:raise Unsupported('SIR instruction '+op)
            term=body['terminator']
            if term=='Return':return
            kind,data=next(iter(term.items()))
            if kind=='Error':raise RuntimeError('SIR runtime error '+str(data))
            if kind=='Jump':target,arguments=data
            elif kind=='Branch':
                cond=self.proof.unique(regs[data['cond']],'CFG branch')
                target,arguments=data['true_block' if cond else 'false_block']
            elif kind=='Switch':
                value=self.proof.unique(regs[data['selector']],'CFG switch');target=data['default'];arguments=[]
                for case in data['cases']:
                    if bytes_value(case['value'])==value:target=case['target'];break
            else:raise Unsupported('terminator '+kind)
            values=[regs[x] for x in arguments]
            params=unit['blocks'][str(target)]['params']
            if len(values)!=len(params):raise RuntimeError('block argument arity')
            for param,value in zip(params,values):setreg(param,value)
            block=target
