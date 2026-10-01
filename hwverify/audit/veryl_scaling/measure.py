#!/usr/bin/env python3
"""Isolated symbolic RTL scaling contracts, not whole-CPU scaling."""
import argparse, hashlib, json, math, os, pathlib, subprocess, sys, time
ROOT=pathlib.Path(__file__).resolve().parents[2]
W=32
B=lambda w,n:['bv',w,n]
EQ=lambda a,b:['eq',a,b]
ITE=lambda g,a,b:['ite',g,a,b]

def conj(xs):
    if not xs:return True
    if len(xs)==1:return xs[0]
    middle=len(xs)//2
    return ['and',conj(xs[:middle]),conj(xs[middle:])]

def read_mux(n,bits,idx):
    result=B(W,0)
    for i in range(n):result=ITE(EQ(idx,B(bits,i)),f's.m{i}',result)
    return result

def memory(n,ports,bad,readonly=False):
    bits=max(1,(n-1).bit_length())
    inputs={'rst':'bool','we':'bool','wa':{'bv':bits},'wd':{'bv':W},**{f'ra{r}':{'bv':bits} for r in range(ports)}}
    state={**{f'm{i}':{'bv':W} for i in range(n)},**{f'q{r}':{'bv':W} for r in range(ports)}}
    ports_src=', '.join(f'ra{r}: input bit<{bits}>, q{r}: output bit<{W}>' for r in range(ports))
    source=f'''module Top (clk: input clock, rst: input bit, we: input bit, wa: input bit<{bits}>, wd: input bit<{W}>, {ports_src}, mem: output bit<{W}>[{n}]) {{
    always_ff (clk) {{
        if rst {{
            {''.join(f'mem[{i}] = 0; ' for i in range(n))}
            {''.join(f'q{r} = 0; ' for r in range(ports))}
        }} else {{
            if we {{ mem[wa] = wd; }}
            {''.join(f'q{r} = mem[ra{r}'+(f" + {bits}'d1" if bad and r==0 else '')+']; ' for r in range(ports))}
        }}
    }}
}}
'''
    nxt={f'm{i}':ITE(['and','i.we',EQ('i.wa',B(bits,i))],'i.wd',f's.m{i}') for i in range(n)}
    nxt.update({f'q{r}':read_mux(n,bits,f'i.ra{r}') for r in range(ports)})
    if readonly:
        source=source.replace('if we { mem[wa] = wd; }','')
        for i in range(n):nxt[f'm{i}']=f's.m{i}'
    bindings={**{f'm{i}':{'signal':'mem','element':i,'type':{'bv':W}} for i in range(n)},**{f'q{r}':{'type':{'bv':W}} for r in range(ports)}}
    return source,inputs,state,nxt,bindings

def pipeline(n,bad):
    inputs={'rst':'bool','en':'bool','data':{'bv':W}}
    state={f'q{i}':{'bv':W} for i in range(n)}
    source=f'''module Top (clk: input clock, rst: input bit, en: input bit, data: input bit<{W}>, {', '.join(f'q{i}: output bit<{W}>' for i in range(n))}) {{
    always_ff (clk) {{
        if rst {{ {''.join(f'q{i} = 0; ' for i in range(n))} }}
        else if en {{
            q0 = data;
            {''.join(f'q{i} = '+('data' if bad and i==n-1 else f'q{i-1}')+'; ' for i in range(1,n))}
        }}
    }}
}}
'''
    nxt={f'q{i}':ITE('i.en','i.data' if i==0 else f's.q{i-1}',f's.q{i}') for i in range(n)}
    return source,inputs,state,nxt,{k:{'type':v} for k,v in state.items()}

def execute(args,path,timeout=120):
    t=time.monotonic()
    try:r=subprocess.run(list(map(str,args)),capture_output=True,text=True,timeout=timeout,env={**os.environ,'HWVERIFY_SOLVER':'finite'})
    except subprocess.TimeoutExpired:
        return None,{'status':'process_timeout','seconds':time.monotonic()-t}
    path.write_text(r.stdout);path.with_suffix(path.suffix+'.stderr').write_text(r.stderr)
    if r.returncode not in (0,1,2,3):return None,{'status':'error','exit_code':r.returncode,'stderr':r.stderr[:1000],'seconds':time.monotonic()-t}
    try:value=json.loads(r.stdout)
    except json.JSONDecodeError:return None,{'status':'invalid_output','exit_code':r.returncode,'stderr':r.stderr[:1000],'seconds':time.monotonic()-t}
    return value,{'status':'completed','exit_code':r.returncode,'seconds':time.monotonic()-t}

def one(axis,n,bad,out,mutant_hint="query"):
    out.mkdir(parents=True,exist_ok=False)
    source,inputs,state,nxt,bindings=(pipeline(n,bad) if axis=='pipeline' else memory(n,2 if axis=='registerfile' else 1,bad,axis=='readonly'))
    (out/'source.veryl').write_text(source)
    write=lambda name,obj:(out/name).write_text(json.dumps(obj,indent=2)+'\n')
    write('design.json',{'top':'Top','four_state':False,'sources':[{'path':'source.veryl','text':source}]})
    result={'axis':axis,'size':n,'width':W,'mutant':bad,'search_hint':mutant_hint if bad else 'query','state_bits':W*len(state),'scope':'isolated_component_contract_not_CPU','phases':{}}
    compiled,phase=execute([ROOT/'conformance/veryl-proof/target/debug/veryl-proof-frontend',out/'design.json'],out/'compiled.json')
    result['phases']['frontend']=phase
    if compiled is None:result['status']='frontend_failed';write('summary.json',result);return result
    cfg={'event':'clk','inputs':{k:{'type':v} for k,v in inputs.items()},'state':bindings,'outputs':{}}
    impl={'state':state,'outputs':{'commit':True}}
    for mode,rst in [('normal',False),('reset',True)]:
        cfg['overrides']={'rst':rst};write(mode+'-bindings.json',cfg)
        lifted,phase=execute([ROOT/'target/release/hwverify-sir-lift',out/'compiled.json',out/(mode+'-bindings.json')]+(['--inline'] if rst else []),out/(mode+'-lift.json'))
        result['phases'][mode+'_lift']=phase
        if lifted is None:result['status']='lift_failed';write('summary.json',result);return result
        if rst:
            if 's.' in json.dumps(lifted['next']):raise RuntimeError('reset prestate leaked')
            impl['reset']=lifted['next']
        else:
            impl['next']=lifted['next'];impl['wires']=lifted['wires']
            result['lift_statistics']=lifted['statistics'];result['wire_count']=len(lifted['wires'])
            result['lift_bytes']=(out/(mode+'-lift.json')).stat().st_size
    # DAG wires avoid imposing a JSON nesting limit on long reference muxes.
    # The lowered expressions are unchanged; names never enter the RTL source.
    spec_wires={}
    def share(value):
        if not isinstance(value,list) or value[0]=='bv':return value
        expr=[value[0],*[share(x) for x in value[1:]]]
        name=f'ref{len(spec_wires):06d}';spec_wires[name]=expr;return 'w.'+name
    shared_next={k:share(v) for k,v in nxt.items()}
    spec={'state':state,'reset':{k:B(W,0) for k in state},'wires':spec_wires,'next':shared_next,'outputs':{'can_step':True}}
    doc={'version':2,'name':f'{axis}_{n}'+('_bad' if bad else ''),'inputs':inputs,'reset_input':'rst','spec':spec,'impl':impl,'binding':conj([EQ('spec.'+k,'impl.'+k) for k in state]),'commit':'commit','can_step':'can_step','progress':{'enabled':True,'rank':B(1,0)}}
    write('refinement.json',doc)
    report,phase=execute([ROOT/'target/release/hwverify-rs',out/'refinement.json','--out',out/'proof']+(['--finite-search-hint',mutant_hint] if bad else []),out/'report.json')
    result['phases']['check']=phase
    if report is None or 'obligations' not in report:
        result['status']='checker_failed';result['checker_error']=report;write('summary.json',result);return result
    if report['engine_summary']['z3_queries']:raise RuntimeError('external solver used')
    result['status']=report['status'];result['engine_summary']=report['engine_summary']
    q=next(x for x in report['obligations'] if x['name']=='microstep_refinement')
    result['microstep']={k:q.get(k) for k in ['status','solver_result','seconds','emission_seconds','finite_seconds','kernel']}
    result['microstep']['finite']=q.get('finite')
    result['correct_outcome']=(report['status']=='stuttering_refinement_verified' if not bad else q.get('solver_result')=='sat' and q.get('finite',{}).get('original_formula_validated') is True)
    # UNKNOWN or a timeout is a measured limit, never a successful proof/control.
    write('summary.json',result)
    return result

def main():
    p=argparse.ArgumentParser();p.add_argument('--out',type=pathlib.Path,required=True);p.add_argument('--axes',nargs='+',choices=['memory','registerfile','pipeline','readonly'],default=['memory','registerfile','pipeline']);p.add_argument('--require-success',action='store_true');p.add_argument('--mutant-hint',choices=['query','sat'],default='query');p.add_argument('--only-mutants',action='store_true');p.add_argument('--sizes',nargs='+',type=int,default=[2,4,8,16,32,64]);a=p.parse_args();a.out=a.out.resolve();a.out.mkdir(parents=True,exist_ok=False)
    all_results=[]
    for axis in a.axes:
        for n in a.sizes:
            if n<2 or n&(n-1):raise ValueError('power-of-two size >=2 required')
            for bad in ([True] if a.only_mutants else [False,True]):
                r=one(axis,n,bad,a.out/(f'{axis}_{n}'+('_bad' if bad else '')),a.mutant_hint)
                all_results.append(r);(a.out/'summary.json').write_text(json.dumps(all_results,indent=2)+'\n');print(json.dumps({k:r.get(k) for k in ['axis','size','mutant','status','correct_outcome','wire_count','lift_bytes','phases']}),flush=True)
            if any(not r.get('correct_outcome') for r in all_results[-(1 if a.only_mutants else 2):]):break
    if a.require_success and any(not r.get('correct_outcome') for r in all_results):
        raise SystemExit('scaling regression failed: UNKNOWN/error is not success')
if __name__=='__main__':main()
