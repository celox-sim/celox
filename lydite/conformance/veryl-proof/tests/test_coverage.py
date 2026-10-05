import copy,gzip,json,pathlib,tempfile,unittest
from coverage import GateError,collect,compare,indexed
BLOCKED=['blocked::one','blocked::two']
EXCEPTIONS={name:{'kind':'veryl_language_restriction','reason':'restricted','panic':'compile error'} for name in BLOCKED}
class CoverageTests(unittest.TestCase):
 def test_duplicate_catalog_and_golden_drift(self):
  with self.assertRaises(GateError):indexed([{'case':'a'},{'case':'a'}],'test')
  with self.assertRaises(GateError):compare({'reads':1},{'reads':2})
 def test_whole_catalog_fail_closed_controls(self):
  with tempfile.TemporaryDirectory() as temp:
   raw=pathlib.Path(temp);listed=[];rows=[]
   names=sorted(BLOCKED)+[f'case{i}' for i in range(663)]
   final={'query':2,'solver_result':'sat','original_formula_validated':True,'kind':'bounded bit-blast/CDCL diagnostics; not an independently checkable proof certificate','encoded_extra':True,'context':{}}
   query={'query':0,'solver_result':'sat','original_formula_validated':True,'kind':'bounded bit-blast/CDCL diagnostics; not an independently checkable proof certificate','encoded_extra':['ne','a',1]}
   for i,name in enumerate(names):
    blocked=name in BLOCKED;rejected=2<=i<10;smoke=10<=i<13;reads=0 if blocked or rejected or smoke else 1
    expectation='CompilationError' if rejected else 'Simulation'
    listed.append({'case':name,'expectation':expectation,'category':'Test','source':{'file':'src/cases/test.vtest','line':i},'script':name})
    rows.append({'case':name,'status':'failed' if blocked else 'passed','expectation':expectation,'errors':[],'panic':'compile error' if blocked else None,'designs':1})
    d=raw/name.replace('::','__')/'design-1';d.mkdir(parents=True)
    diag={'stage':'analyzer','diagnostics':[{'code':'InvalidForRange','kind':'NegativeBound'}],'detail':'diagnostic'} if blocked or rejected else None
    record={'status':'failed' if blocked or rejected else 'passed','compilation_rejected':blocked or rejected,'error':'diagnostic' if blocked or rejected else None,'reads':reads,'operations':0,'commands':2,'design_sha256':'source','protocol_sha256':'flow','diagnostic':diag,'negative_control':{'status':'passed','query':0} if reads else None}
    (d/'backend-result.json').write_text(json.dumps(record));(d/'design.json').write_text(json.dumps({'four_state':False}))
    if not (blocked or rejected):
     (d/'proof').mkdir();qs=[query,dict(query,query=1,solver_result='unsat',purpose='sample x'),final] if reads else [dict(final,query=0)]
     with gzip.open(d/'proof/proof-audit.json.gz','wt') as f:json.dump(qs,f)
   summary=raw/'summary.json';summary.write_text(json.dumps(rows));baseline,_=collect(raw,listed,EXCEPTIONS,1)
   for label,exceptions in [('unknown_case',dict(EXCEPTIONS,missing={'kind':'veryl_language_restriction','reason':'r','panic':'p'})),('no_reason',{**EXCEPTIONS,BLOCKED[0]:dict(EXCEPTIONS[BLOCKED[0]],reason='')}),('bad_kind',{**EXCEPTIONS,BLOCKED[0]:dict(EXCEPTIONS[BLOCKED[0]],kind='flaky')}),('changed_failure',{**EXCEPTIONS,BLOCKED[0]:dict(EXCEPTIONS[BLOCKED[0]],panic='other error')})]:
    with self.subTest(label=label),self.assertRaises(GateError):collect(raw,listed,exceptions,1)
   with self.assertRaises(GateError):collect(raw,listed,EXCEPTIONS,0)
   for label,change in [('missing',lambda rs:rs.pop()),('duplicate',lambda rs:rs.append(rs[-1])),('newfailure',lambda rs:rs[-1].update(status='failed')),('blocked_success',lambda rs:rs[0].update(status='passed')),('sticky_transport',lambda rs:rs[-1].update(errors=['broken pipe']))]:
    mutated=copy.deepcopy(rows);change(mutated);summary.write_text(json.dumps(mutated))
    with self.subTest(label=label),self.assertRaises(GateError):collect(raw,listed,EXCEPTIONS,1)
   summary.write_text(json.dumps(rows));d=raw/names[-1].replace('::','__')/'design-1';p=d/'proof/proof-audit.json.gz'
   for label,change in [('UNKNOWN',lambda q:q.update(solver_result='unknown')),('fallback',lambda q:q.update(kind='z3')),('unvalidated_model',lambda q:q.update(original_formula_validated=False))]:
    bad=copy.deepcopy(query);change(bad)
    with gzip.open(p,'wt') as f:json.dump([bad],f)
    with self.subTest(label=label),self.assertRaises(GateError):collect(raw,listed,EXCEPTIONS,1)
   with gzip.open(p,'wt') as f:json.dump([query,dict(query,query=1,solver_result='unsat',purpose='sample x'),final],f)
   block=raw/names[0].replace('::','__')/'design-1/backend-result.json';record=json.loads(block.read_text());record['diagnostic']['diagnostics'][0]['kind']='Other';block.write_text(json.dumps(record))
   actual,_=collect(raw,listed,EXCEPTIONS,1)
   with self.assertRaises(GateError):compare(actual,baseline)
   record['diagnostic']['diagnostics'][0]['kind']='NegativeBound';record['design_sha256']='legal-source-mutation';block.write_text(json.dumps(record))
   actual,_=collect(raw,listed,EXCEPTIONS,1)
   with self.assertRaises(GateError):compare(actual,baseline)
if __name__=='__main__':unittest.main()
