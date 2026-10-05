"""Re-execute saved finite queries; UNSAT still trusts the custom solver.
This is replayable query evidence, not an independently checked proof certificate.
"""
import argparse,json,pathlib,subprocess,gzip
from proof_backend import ROOT,conjunction
parser=argparse.ArgumentParser();parser.add_argument('proof',type=pathlib.Path);args=parser.parse_args()
def read(path):
 if path.exists():return json.load(open(path))
 with gzip.open(str(path)+'.gz','rt') as file:return json.load(file)
queries=read(args.proof/'proof-audit.json')
epochs={int(p.name.split('-')[1].split('.')[0]):read(p if p.suffix!='.gz' else p.with_suffix('')) for p in args.proof.glob('epoch-*.json*')}
last=read(args.proof/'relation.json');epochs[last['epoch']]=last
solver=subprocess.Popen([str(ROOT/'../../../target/release/lydite-finite-service')],stdin=subprocess.PIPE,stdout=subprocess.PIPE,text=True)
try:
 for index,query in enumerate(queries):
  epoch=epochs[query['epoch']];constraints=epoch['constraints'][:query['constraint_count']]
  extra=query['encoded_extra'];formula=conjunction(constraints+([extra] if extra is not True else []))
  request={'declarations':epoch['declarations'],'formula':formula,'context':query['context'],'hint':query['search_hint']}
  solver.stdin.write(json.dumps(request)+'\n');solver.stdin.flush();actual=json.loads(solver.stdout.readline())
  if actual.get('solver_result')!=query['solver_result']:raise RuntimeError(f'query {index} verdict changed')
  if actual.get('solver_result')=='sat' and not actual.get('original_formula_validated'):raise RuntimeError('SAT witness not validated')
 print(json.dumps({'queries_replayed':len(queries),'epochs':len(epochs),'status':'passed'}))
finally:
 solver.stdin.close();solver.wait();solver.stdout.close()
