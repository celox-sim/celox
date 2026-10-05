import pathlib,json,subprocess
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

rows=[]
for signed in [False,True]:
 for width in [2,5,8,16]:
  for bits in range(32):
   value=bits-32 if signed and bits>=16 else bits;mask=(1<<width)-1
   raw=value&mask;a=raw-(1<<width) if signed and raw>>(width-1) else raw
   b=(2&mask);b=b-(1<<width) if signed and b>>(width-1) else b
   q=abs(a)//abs(b);q=-q if (a<0)!=(b<0) else q;rem=a-q*b
   expected={'qc':a&0xffffffff,'qd':q&0xffffffff,'qr':rem&0xffffffff,'ql':int(a<b)}
   code=f'module Top(qc: output logic<32>, qd: output logic<32>, qr: output logic<32>, ql: output logic) {{ const A: {"signed " if signed else ""}logic<5> = {value}; assign qc = A as {width}; assign qd = (A as {width}) / (2 as {width}); assign qr = (A as {width}) % (2 as {width}); assign ql = (A as {width}) <: (2 as {width}); }}'
   name=f'{signed}-{width}-{bits}';p=out/(name+'.json');p.write_text(json.dumps({'top':'Top','four_state':False,'sources':[{'path':'cast.veryl','text':code}]}));run=subprocess.run([str(binary),str(p)],capture_output=True,text=True);(out/(name+'.stderr')).write_text(run.stderr);assert run.returncode==0,(name,run.stderr)
   d=json.loads(run.stdout);(out/(name+'.sir.json')).write_text(run.stdout);names={value:name for name,value in top_signal_ids(d).items()};actual={}
   for unit in d['sir']['eval_comb']:
    assert len(unit['blocks'])==1;vals={}
    for i in next(iter(unit['blocks'].values()))['instructions']:
     [(op,args)]=i.items()
     if op=='Imm':reg,b=args;assert not any(b['mask']);vals[reg]=int.from_bytes(bytes(b['payload']),'little')
     elif op=='Store':actual[names[args[0]['var_id']]]=vals[args[3]]&((1<<args[2])-1)
     else:raise AssertionError((name,'not folded',i))
   rows.append({'name':name,'actual':actual,'expected':expected,'passed':actual==expected})
(out/'summary.json').write_text(json.dumps({'status':'passed' if all(x['passed'] for x in rows) else 'failed','cases':rows},indent=2)+'\n');print('passed',sum(x['passed'] for x in rows),'of',len(rows));print([x for x in rows if not x['passed']][:8]);assert all(x['passed'] for x in rows)
