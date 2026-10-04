#!/usr/bin/env python3
"""Actual D/X/W CPU ROM/data capacity sweep; two GPRs and stages stay fixed."""
import argparse,copy,importlib.util,json,pathlib,re,sys
ROOT=pathlib.Path(__file__).resolve().parents[2]
sys.path.insert(0,str(ROOT/'examples'))
from build_branch_pipeline import build
spec=importlib.util.spec_from_file_location('pipeline_runner',ROOT/'conformance/veryl-symbolic/run.py')
runner=importlib.util.module_from_spec(spec);spec.loader.exec_module(runner)

def model(n,axis='both'):
    rn=n if axis in ('both','rom') else 4;dn=n if axis in ('both','data') else 4
    a=(rn-1).bit_length();d=(dn-1).bit_length();doc=build(32)
    def rewrite(x):
        if isinstance(x,list):
            # Recognize the old four-word reference mux; rebuild its independent
            # architectural lookup over the new address space, not from RTL.
            if len(x)==4 and x[0]=='ite' and isinstance(x[2],str) and re.fullmatch(r'(s\.|impl\.)(rom|data)0',x[2]) and isinstance(x[1],list) and x[1][0]=='eq' and x[1][2]==['bv',2,0]:
                prefix=x[2][:-1];is_data='data' in prefix;count=dn if is_data else rn;bits=d if is_data else a
                address=['extract',d-1,0,rewrite(x[1][1][3])] if is_data else rewrite(x[1][1])
                value=prefix+str(count-1)
                for i in reversed(range(count-1)):value=['ite',['eq',address,['bv',bits,i]],prefix+str(i),value]
                return value
            if len(x)==3 and x[0]=='bv' and x[1]==2:return ['bv',a,x[2]]
            if len(x)==4 and x[:3]==['extract',1,0]:return ['extract',a-1,0,rewrite(x[3])]
            return [rewrite(v) for v in x]
        if isinstance(x,dict):
            if x=={'bv':2}:return {'bv':a}
            return {k:rewrite(v) for k,v in x.items()}
        return x
    doc=rewrite(doc)
    for kind,w,count in [('rom',37,rn),('data',32,dn)]:
        for i in range(4,count):
            k=f'{kind}{i}';doc['inputs'][k]={'bv':w}
            for machine in ['spec','impl']:
                doc[machine]['state'][k]={'bv':w};doc[machine]['reset'][k]='i.'+k;doc[machine]['next'][k]='s.'+k
            doc['binding']=['and',doc['binding'],['eq','spec.'+k,'impl.'+k]]
    # Conjunction associativity only: avoid JSON nesting becoming the limiting
    # factor for larger capacity fixtures. The leaf predicates are unchanged.
    if max(rn,dn)>=64:
        leaves=[]
        def flatten(value):
            if isinstance(value,list) and len(value)==3 and value[0]=='and':
                flatten(value[1]);flatten(value[2])
            else:leaves.append(value)
        def balanced(items):
            if len(items)==1:return items[0]
            middle=len(items)//2
            return ['and',balanced(items[:middle]),balanced(items[middle:])]
        flatten(doc['binding']);doc['binding']=balanced(leaves)
    return doc

def source(n,axis='both'):
    rn=n if axis in ('both','rom') else 4;dn=n if axis in ('both','data') else 4
    a=(rn-1).bit_length();d=(dn-1).bit_length();s=(ROOT/'conformance/veryl-symbolic/pipeline.veryl.in').read_text()
    # Port widths, branch/load addresses and increments track capacity. ISA
    # opcode/register/immediate fields retain their independent32-bit format.
    s=s.replace('bit<2>','bit<'+str(a)+'>').replace("2'd",f"{a}'d").replace('[1:0]',f'[{a-1}:0]')
    s=s.replace(f'case x_ir[{a-1}:0]',f'case x_ir[{d-1}:0]')
    extra=[]
    for kind,w,count in [('rom','@IW@',rn),('data','@W@',dn)]:
        for i in range(4,count):extra.extend([f'seed_{kind}{i}: input bit<{w}>,',f'{kind}{i}: output bit<{w}>,'])
    s=s.replace('    pc: output', '    '+' '.join(extra)+'\n    pc: output')
    s=s.replace("            pc = '0;",'            '+''.join(f'{kind}{i} = seed_{kind}{i}; ' for kind,count in [('rom',rn),('data',dn)] for i in range(4,count))+"\n            pc = '0;")
    for value,prefix,count,bits in [('result','data',dn,d),('fetched','rom',rn,a)]:
        old=f"{a}'d0: {value} = {prefix}0;"
        start=s.index(old);end=s.index(f'default: {value} = {prefix}3;',start)+len(f'default: {value} = {prefix}3;')
        s=s[:start]+'\n                '.join([f"{bits}'d{i}: {value} = {prefix}{i};" for i in range(count-1)]+[f'default: {value} = {prefix}{count-1};'])+s[end:]
    return s

def main():
    p=argparse.ArgumentParser();p.add_argument('--out',type=pathlib.Path,required=True);p.add_argument('--axis',choices=['both','rom','data'],default='both');p.add_argument('--require-success',action='store_true');p.add_argument('--faults',nargs='*',choices=list(runner.FAULTS),default=['no_flush']);p.add_argument('--sizes',type=int,nargs='+',default=[4,8,16]);args=p.parse_args();args.out=args.out.resolve();args.out.mkdir(parents=True,exist_ok=False);results=[]
    for n in args.sizes:
        if n<4 or n&(n-1):raise ValueError('power of two >=4 required')
        setup=args.out/f'setup_{n}';setup.mkdir();(setup/'pipeline.veryl.in').write_text(source(n,args.axis));runner.__file__=str(setup/'runner.py');doc=model(n,args.axis);runner.build=lambda width:copy.deepcopy(doc)
        runner.FAULTS['wrong_target']=(f'if taken {{ fetch_pc = x_ir[{(n if args.axis in ("both","rom") else 4).bit_length()-2}:0]; }}',f'if taken {{ fetch_pc = x_ir[{(n if args.axis in ("both","rom") else 4).bit_length()-2}:0] + {(n if args.axis in ("both","rom") else 4).bit_length()-1}\'d1; }}')
        for fault in [None,*args.faults]:
            out=args.out/(f'cpu_{n}'+('_bad_'+fault if fault else ''))
            accepted=False
            try:
                r=runner.run_case(32,fault,out,ROOT/'conformance/veryl-proof/target/debug/veryl-proof-frontend',ROOT/'../target/release/lydite-sir-lift',ROOT/'../target/release/lydite')
                accepted=r['status']==('stuttering_refinement_verified' if fault is None else 'reset_rejected' if fault=='missing_reset' else 'counterexample')
            except RuntimeError as e:
                report=json.load(open(out/'report.json')) if (out/'report.json').exists() else {}
                r={'status':report.get('status','error'),'error':str(e)}
            report=json.load(open(out/'report.json')) if (out/'report.json').exists() else {}
            q=next((o for o in report.get('obligations',[]) if o['name']=='microstep_refinement'),{})
            r.update(correct_outcome=accepted,capacity=n,axis=args.axis,scope='actual_branch_CPU_two_GPR_fixed_DXW',mutant=bool(fault),microstep=q)
            results.append(r);(args.out/'summary.json').write_text(json.dumps(results,indent=2)+'\n');print(json.dumps({k:r.get(k) for k in ['capacity','mutant','status','seconds','error']}),flush=True)
        if any(not r['correct_outcome'] for r in results[-(len(args.faults)+1):]):break
    if args.require_success and any(not r['correct_outcome'] for r in results):raise SystemExit('CPU capacity regression failed')
if __name__=='__main__':main()
