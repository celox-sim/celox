"""Independent automatic/unsplit comparison, concrete state enumeration and SAT replay."""
from pathlib import Path
import argparse,json,os,shutil,subprocess,sys,time
HERE=Path(__file__).resolve().parent;ROOT=HERE.parents[1]
sys.path.insert(0,str(HERE.parent))
from interpreter import BV,evaluate,evaluate_record,outputs,related
from replay_program_counterexamples import context
NAMES=['countdown_good','disjunction_bound_bad','implication_bound_bad','inverted_bound_bad','wrapped_literal_bad',
       'multiplier_good','multiplier_wrong_capture','multiplier_missing_decrement','multiplier_hang_capture']

def replay(d,report,out,ob):
    ctx=context(out,ob);rec=lambda pre:{k[len(pre):]:v for k,v in ctx.items() if k.startswith(pre)}
    i=rec('i.');name=ob['name']
    if name.startswith('program_'):
        c=d['program_contract'];s=rec('s.');ns=evaluate_record(d['spec'],'next',s,i)
        assert ns==rec('next.')
        p={k:v for k,v in ctx.items() if k.startswith('p.')}
        env={**{'s.'+k:v for k,v in s.items()},**p};nenv={**{'s.'+k:v for k,v in ns.items()},**p}
        active=evaluate(c['invariant'],env) and not evaluate(c['terminal'],env)
        if name.startswith('program_invariant_preserved'):
            assert active and not evaluate(c['invariant'],nenv)
            selected=[x for x in report['partition_plan'].get('candidates',[]) if x['decision']=='selected']
            if selected:
                suffix='_'.join('auto_'+x['state']+'_'+(str(s[x['state']].v) if s[x['state']].v in x['values'] else 'other') for x in selected)
                assert name=='program_invariant_preserved_'+suffix
        elif name=='program_rank_decreases':assert active and evaluate(c['rank'],nenv).v>=evaluate(c['rank'],env).v
        else:raise AssertionError(name)
    else:
        s,t=rec('spec.'),rec('impl.');r=related(d,s,t)
        ns=evaluate_record(d['spec'],'reset',{},i) if i[d['reset_input']] else evaluate_record(d['spec'],'next',s,i) if ctx['commit'] else s
        nt=evaluate_record(d['impl'],'reset',{},i) if i[d['reset_input']] else evaluate_record(d['impl'],'next',t,i)
        assert ns==rec('spec_next.') and nt==rec('impl_next.')
        if name=='microstep_refinement':assert r and not related(d,ns,nt)
        elif name=='noncommit_rank_decreases':assert r and not i[d['reset_input']] and ctx['progress_enabled'] and not ctx['commit'] and ctx['rank_next'].v>=ctx['rank'].v
        else:raise AssertionError(name)
    return name

if __name__=='__main__':
    parser=argparse.ArgumentParser();parser.add_argument('--binary',type=Path,default=ROOT/'target/debug/hwverify-rs')
    parser.add_argument('--z3',default=os.environ.get('Z3_BIN') or shutil.which('z3'))
    parser.add_argument('--out',type=Path,default=HERE/'evidence');args=parser.parse_args()
    if not args.z3:parser.error('Provide --z3 or Z3_BIN')
    args.out.mkdir(parents=True,exist_ok=True);rows=[]
    for name in NAMES:
        original=json.loads((HERE/(name+'.json')).read_text())
        for mode in ['auto','none']:
            d=json.loads(json.dumps(original));d['program_contract']['partitioning']=mode
            case=args.out/(name+'_'+mode);case.mkdir(parents=True,exist_ok=True)
            input_path=case/'input.json';input_path.write_text(json.dumps(d,indent=2)+'\n')
            started=time.monotonic();p=subprocess.run([str(args.binary.resolve()),str(input_path),'--out',str(case),'--z3',args.z3],capture_output=True,text=True,timeout=90)
            elapsed=time.monotonic()-started;report=json.loads((case/'report.json').read_text())
            expected='program_and_refinement_verified' if name.endswith('_good') else 'counterexample'
            assert report['status']==expected,(name,mode,report['status'],p.stderr)
            witnesses=[replay(d,report,case,o) for o in report['obligations'] if o['status']=='counterexample']
            if expected=='counterexample':assert witnesses
            rows.append({'case':name,'mode':mode,'status':report['status'],'seconds':elapsed,'replayed':witnesses,'plan':report['partition_plan']})
            print(name,mode,report['status'],round(elapsed,3),len(witnesses),flush=True)
    # Independent exhaustive oracle over all 256 states for the small adverse specs.
    enumerated=[]
    for name in NAMES[:5]:
        d=json.loads((HERE/(name+'.json')).read_text());c=d['program_contract'];bad=[]
        for xv in range(16):
            for yv in range(16):
                s={'x':BV(4,xv),'y':BV(4,yv)};env={'s.'+k:v for k,v in s.items()}
                if evaluate(c['invariant'],env) and not evaluate(c['terminal'],env):
                    ns=evaluate_record(d['spec'],'next',s,{'reset':False,'initial':BV(4,0)})
                    if not evaluate(c['invariant'],{'s.'+k:v for k,v in ns.items()}):bad.append([xv,yv])
        assert bool(bad)==name.endswith('_bad');enumerated.append({'case':name,'states':256,'bad_states':bad})
    result={'status':'pass','runs':rows,'enumeration':enumerated,'total_replayed':sum(len(r['replayed']) for r in rows)}
    (HERE/'results.json').write_text(json.dumps(result,indent=2)+'\n')
    print('PASS',len(rows),'runs,',result['total_replayed'],'counterexamples replayed')
