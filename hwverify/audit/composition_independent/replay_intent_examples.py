"""Replay concrete witnesses from the three documented good/bad-intent examples."""
from pathlib import Path
import argparse,json,subprocess
from replay_traces import replay
ROOT=Path(__file__).resolve().parents[2]
p=argparse.ArgumentParser();p.add_argument('--binary',required=True,type=Path);a=p.parse_args();base=ROOT/'results/composition_0_7_intent_replay';base.mkdir(exist_ok=True);rows=[]
for name in ['budgeted_counter','contradictory_composition','weakened_budget']:
 out=base/name;out.mkdir(exist_ok=True);source=out/'canonical.json'
 subprocess.run([str(a.binary.resolve()),str(ROOT/'examples'/f'{name}.hwv'),'--emit-json',str(source),'--out',str(out/'validation')],capture_output=True,text=True,check=True)
 d=json.loads(source.read_text());evidence=ROOT/'results/compositional-specifications'/f'final-{name}';r=json.loads((evidence/'report.json').read_text());checked=replay(d,r,evidence)
 rows.append({'case':name,'status':r['status'],'replayed':checked,'failed_examples':[{'target':e['target'],'name':e['example'],'admitted':e['admitted'],'expect':e['expect']} for e in r['examples'] if e['status']=='failed']})
(ROOT/'audit/composition_independent/intent_replay_results.json').write_text(json.dumps({'status':'pass','cases':rows,'count':sum(len(r['replayed']) for r in rows)},indent=2)+'\n');print('PASS',sum(len(r['replayed']) for r in rows),'intent-example witnesses')
