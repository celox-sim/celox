import pathlib,json,subprocess,math
import argparse
parser = argparse.ArgumentParser()
parser.add_argument('--exporter', type=pathlib.Path, required=True)
parser.add_argument('--out', type=pathlib.Path, required=True)
args = parser.parse_args()
binary = args.exporter.resolve()
out = args.out.resolve()
out.mkdir(parents=True, exist_ok=False)
def top_signal_ids(data):
    if 'signals' in data:
        return {s['path'][0]: s['address']['var_id'] for s in data['signals']
                if not s['instances'] and len(s['path']) == 1}
    mapping = {a['var_id']: b['var_id'] for a, b in data['lookup_state']}
    return {row['variable']['path'][0]: mapping[row['variable']['id']]
            for row in data['lookup_variables']
            if row['module'] == 0 and len(row['variable']['path']) == 1}

shapes=[([],[]),([8],[]),([3,5],[]),([10,20],[]),([2,3,7],[]),([8],[4]),([3,5],[4]),([2,7],[3,5])]
rows=[]
for idx,(packed,array) in enumerate(shapes):
 typ='logic'+('<'+','.join(map(str,packed))+'>' if packed else '')+('['+','.join(map(str,array))+']' if array else '')
 for mode in ['signal']+(['type'] if not array else []):
  arg='d' if mode=='signal' else typ
  code=f'module Top (clk: input clock, d: input {typ}, qs: output logic<32>, qb: output logic<32>) {{ always_ff(clk) {{ qs = $size({arg}); qb = $bits({arg}); }} }}'
  name=f'{idx}-{mode}';p=out/(name+'.json');p.write_text(json.dumps({'top':'Top','four_state':False,'sources':[{'path':'dimension.veryl','text':code}]}))
  run=subprocess.run([str(binary),str(p)],capture_output=True,text=True);(out/(name+'.stderr')).write_text(run.stderr);assert run.returncode==0,(name,run.stderr)
  data=json.loads(run.stdout);(out/(name+'.sir.json')).write_text(run.stdout)
  names={value:name for name,value in top_signal_ids(data).items()};actual={}
  for u in data['sir']['eval_apply_ffs'][0]['units']:
   assert len(u['blocks'])==1
   vals={}
   for inst in next(iter(u['blocks'].values()))['instructions']:
    [(op,a)]=inst.items()
    if op=='Imm':reg,bits=a;assert not any(bits['mask']);vals[reg]=int.from_bytes(bytes(bits['payload']),'little')
    elif op=='Store':actual[names[a[0]['var_id']]]=vals[a[3]]
    elif op=='Commit':pass
    else:raise AssertionError((name,inst))
  expected={'qs':(array or packed or [1])[0],'qb':math.prod(array+packed)}
  assert actual==expected,(name,actual,expected);rows.append({'name':name,'type':typ,'actual':actual,'expected':expected})
(out/'summary.json').write_text(json.dumps({'status':'passed','cases':rows},indent=2)+'\n');print('passed',len(rows))
