"""Exercise bare-name printer/parser against the existing independent finite oracle."""
import importlib.util,json,subprocess,sys
from pathlib import Path
root=Path(__file__).resolve().parents[2]
edited=Path(sys.argv[1]);binary=Path(sys.argv[2]);z3=sys.argv[3]
def module(name,path):
 s=importlib.util.spec_from_file_location(name,path);m=importlib.util.module_from_spec(s);s.loader.exec_module(m);return m
oracle=module('oracle',root/'audit/prime_0_9_independent/quantifier_oracle.py')
printer=module('printer',edited/'scripts/json_to_lyd.py')
doc,expected=oracle.fixture();out=root/'results/bare_quantifiers_oracle';out.mkdir(parents=True,exist_ok=True)
# Use the public printer CLI rather than duplicating its invocation contract.
source=out/'oracle.json';source.write_text(json.dumps(doc,indent=2)+'\n')
lyd=out/'oracle.lyd'
run=subprocess.run([sys.executable,str(edited/'scripts/json_to_lyd.py'),str(source),str(lyd)],capture_output=True,text=True)
assert run.returncode==0,run.stderr
text=lyd.read_text();assert 'q.' not in text
run=subprocess.run([str(binary),str(lyd),'--out',str(out/'run'),'--z3',z3],capture_output=True,text=True,timeout=300)
(out/'run.stdout').write_text(run.stdout);(out/'run.stderr').write_text(run.stderr)
report=json.loads((out/'run/report.json').read_text());actual={(e['target'],e['example']):e for e in report['examples']}
assert set(actual)==set(expected),(len(actual),len(expected))
for key,want in expected.items():
 got=actual[key]
 assert got['valid']==want['valid'],(key,'valid',got)
 assert got['feasibility']['holds']==want['feasible'],(key,'feasibility',got)
 if 'nonvacuous' in want:assert got['nonvacuous']['holds']==want['nonvacuous'],(key,'nonvacuous',got)
result={'status':'pass','case_count':len(expected),'generated_source_has_q_prefix':False,'valid':sum(x['valid'] for x in expected.values()),'invalid':sum(not x['valid'] for x in expected.values())}
(root/'audit/bare_quantifiers_independent/oracle_results.json').write_text(json.dumps(result,indent=2)+'\n');print(result)
