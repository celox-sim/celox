"""v0.4/rewrite-kernel/closure-disabled comparison with all raw evidence retained."""
from pathlib import Path
import argparse,collections,hashlib,json,os,re,subprocess,time
ROOT=Path(__file__).resolve().parents[2]; HERE=Path(__file__).resolve().parent

def parse(text):
 tokens=iter(re.findall(r'\(|\)|[^\s()]+', re.sub(r';[^\n]*','',text)))
 def expr(tok):
  if tok!='(':return tok
  out=[]
  for x in tokens:
   if x==')':return out
   out.append(expr(x))
  raise ValueError('unterminated')
 return [expr(x) for x in tokens]

def dump(x):return '('+' '.join(map(dump,x))+')' if isinstance(x,list) else x

def classify_closure(z3,directory,ob):
 # Original query shape comes from program::obligations, verified here.
 script=(directory/ob['evidence']).read_text();ast=parse(script)
 defs={x[1]:x[4] for x in ast if isinstance(x,list) and x and x[0]=='define-fun'}
 query=next(x[1] for x in ast if isinstance(x,list) and x and x[0]=='assert')
 root=defs[query];assert root[0]=='and',root
 active,rest=root[1:];rest=defs[rest]
 antecedent=['and',active,rest[1]] if rest[0]=='and' else active
 assert rest[0] in ('and','not'),rest
 base=re.sub(r'\(assert [^\n]+\)',f'(assert {dump(antecedent)})',script)
 path=directory/(ob['name']+'.antecedent.smt2');path.write_text(base)
 p=subprocess.run([z3,'-in','-smt2'],input=base,text=True,capture_output=True,timeout=30)
 (directory/(ob['name']+'.antecedent.out')).write_text(p.stdout+p.stderr)
 result=p.stdout.strip();assert result in ('sat','unsat','unknown'),result
 return {'sat':'nonvacuous_preservation','unsat':'contradictory_antecedent','unknown':'antecedent_unknown'}[result]

if __name__=='__main__':
 p=argparse.ArgumentParser();p.add_argument('--baseline',required=True,type=Path);p.add_argument('--baseline-cached',type=Path);p.add_argument('--current',type=Path,default=ROOT/'../target/release/lydite');p.add_argument('--z3',required=True);p.add_argument('--out',type=Path,default=ROOT/'results/structural_0_5');args=p.parse_args()
 examples=['auto_array_sum','auto_array_sum_renamed','auto_array_sum_reordered','auto_array_sum_unsplit','auto_array_sum_bad_program','auto_array_sum_false_invariant','auto_array_sum_false_pre','auto_array_sum_bad_rank','auto_array_sum_input_invariant','auto_array_sum_wrong_indexed_load','array_sum']
 cases=[ROOT/'examples'/f'{n}.json' for n in examples]
 cases += [ROOT/'audit/partition_independent'/f'{n}.json' for n in ['countdown_good','disjunction_bound_bad','implication_bound_bad','inverted_bound_bad','wrapped_literal_bad','multiplier_good','multiplier_wrong_capture','multiplier_missing_decrement','multiplier_hang_capture']]
 cases += sorted(HERE.glob('memory_increment_*.json'))
 result={'method':'One sequential run each; same release profile, same Z3; all wall times include scoring, kernel, SMT emission and I/O. Kernel-off disables closure, retains structural trials. No timeout/UNKNOWN omitted.','binaries':{label:{'path':str(b.resolve()),'sha256':hashlib.sha256(b.read_bytes()).hexdigest()} for label,b in [('v0.4',args.baseline),('v0.5',args.current)]+([('v0.4_cached_hash',args.baseline_cached)]if args.baseline_cached else [])},'runs':[]}
 args.out.mkdir(parents=True,exist_ok=True)
 for case in cases:
  for mode in ['v0.4']+(['v0.4_cached_hash']if args.baseline_cached else [])+['v0.5','v0.5_no_closure']:
   directory=args.out/mode/case.stem;directory.mkdir(parents=True,exist_ok=True)
   (directory/'input.json').write_text(case.read_text())
   env=dict(os.environ);env.pop('LYDITE_KERNEL',None)
   if mode.endswith('no_closure'):env['LYDITE_KERNEL']='off'
   binary=args.baseline if mode=='v0.4' else args.baseline_cached if mode=='v0.4_cached_hash' else args.current
   started=time.monotonic()
   try:
    proc=subprocess.run([str(binary.resolve()),str(case),'--out',str(directory),'--z3',args.z3],capture_output=True,text=True,env=env,timeout=120)
    wall=time.monotonic()-started
    (directory/'run.stdout').write_text(proc.stdout);(directory/'run.stderr').write_text(proc.stderr)
    report=json.loads((directory/'report.json').read_text());obs=report.get('obligations',[])
    row={'case':str(case.relative_to(ROOT)),'mode':mode,'exit_code':proc.returncode,'status':report['status'],'wall_seconds':wall,'queries':len(obs),'custom_closed':sum(o.get('backend')=='structural_kernel' for o in obs),'z3_queries':sum(o.get('backend','z3')=='z3' and o.get('solver_result')!='not_run' for o in obs),'scoring_seconds':(report.get('partition_plan') or {}).get('scoring_seconds',0),'kernel_seconds':sum(o.get('kernel',{}).get('seconds',0) for o in obs),'z3_seconds':sum(o.get('z3_seconds',o.get('seconds',0)) for o in obs if o.get('backend','z3')=='z3'),'failures':[{'name':o['name'],'status':o['status']} for o in obs if o['status']!='passed'],'plan':report.get('partition_plan'),'error':report.get('error')}
    if mode=='v0.5':
     categories=collections.Counter();classified=[]
     for ob in obs:
      if ob.get('backend')!='structural_kernel':continue
      kind=classify_closure(args.z3,directory,ob) if ob['name'].startswith('program_invariant_preserved') else 'other_obligation'
      categories[kind]+=1;classified.append({'name':ob['name'],'category':kind})
     row['closure_categories']=dict(categories);row['custom_obligations']=classified
   except subprocess.TimeoutExpired as e:
    row={'case':str(case.relative_to(ROOT)),'mode':mode,'status':'external_watchdog_timeout','wall_seconds':time.monotonic()-started}
   result['runs'].append(row);(HERE/'results.json').write_text(json.dumps(result,indent=2)+'\n')
   print(case.stem,mode,row['status'],round(row['wall_seconds'],3),row.get('custom_closed',0),row.get('closure_categories',''),flush=True)
