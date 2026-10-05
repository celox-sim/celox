import json,pathlib,subprocess
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

cases=[]
for formal in [2,4,8]:
 for a in range(4):
  for b in range(4):cases.append((f'add_f{formal}_{a}_{b}',formal,f"2'b{a:02b} + 2'b{b:02b}",(a+b)&((1<<formal)-1)))
cases.extend([('cast_boundary',4,"(2'b11 + 2'b01) as 2",0),('concat_boundary',4,"{2'b11 + 2'b01}",0),('concat_parts',4,"{2'b11, 2'b01}",13),('multiply_context',4,"2'b11 * 2'b11",9),('shift_context',4,"2'b01 << 2'b10",4),('comparison_boundary',4,"(2'b11 + 2'b01) == 2'b00",1),('minus_context',4,"-2'b11",13)])
results=[]
for name,width,expr,expected in cases:
 source=f'module Top (q: output logic<8>) {{ function f(x: input logic<{width}>) -> logic<8> {{ return x; }} assign q = f({expr}); }}'
 design={'top':'Top','four_state':False,'sources':[{'path':'matrix.veryl','text':source}]};file=out/(name+'.json');file.write_text(json.dumps(design))
 p=subprocess.run([str(binary),str(file)],text=True,capture_output=True);(out/(name+'.stderr')).write_text(p.stderr)
 assert p.returncode==0,(name,p.stderr)
 data=json.loads(p.stdout);(out/(name+'.sir.json')).write_text(p.stdout)
 # These are constant-folding regression tests: insist q receives a literal.
 q=top_signal_ids(data)['q']
 values={};actual=None
 for unit in data['sir']['eval_comb']:
  assert len(unit['blocks'])==1
  for i in next(iter(unit['blocks'].values()))['instructions']:
   [(op,args)]=i.items()
   if op=='Imm':
    reg,bits=args;assert not any(bits['mask']);values[reg]=int.from_bytes(bytes(bits['payload']),'little')
   elif op=='Store':
    addr,offset,bits,reg,events,extra=args;assert offset=={'Static':0}
    if addr['var_id']==q:actual=values[reg]&((1<<bits)-1)
   else:raise AssertionError((name,'not constant folded',i))
 result={'case':name,'actual':actual,'expected':expected,'passed':actual==expected};results.append(result)
(out/'summary.json').write_text(json.dumps(results,indent=2)+'\n');print('passed',sum(x['passed'] for x in results),'of',len(results));print([x for x in results if not x['passed']]);assert all(x['passed'] for x in results)
