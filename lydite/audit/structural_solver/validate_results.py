"""Validate every retained final run against the intended fixture outcome."""
from pathlib import Path
import collections,json
HERE=Path(__file__).resolve().parent
r=json.loads((HERE/'results.json').read_text());rows=r['runs'];assert len(rows)==92
success={'auto_array_sum','auto_array_sum_renamed','auto_array_sum_reordered','array_sum','countdown_good','multiplier_good','memory_increment_good'}
for row in rows:
 n=Path(row['case']).stem
 expected='program_and_refinement_verified'if n in success else 'unknown'if n=='auto_array_sum_unsplit'else 'inadequate_contract'if n=='auto_array_sum_false_pre'else 'invalid_or_tool_error'if n=='auto_array_sum_input_invariant'else 'counterexample'
 assert row['status']==expected,(n,row['mode'],row['status'],expected)
 assert not row.get('error')or expected=='invalid_or_tool_error'
for case in {x['case']for x in rows}:
 xs=[x for x in rows if x['case']==case];assert {x['mode']for x in xs}=={'v0.4','v0.4_cached_hash','v0.5','v0.5_no_closure'}
out={'status':'pass','runs':len(rows),'cases':len(rows)//4,'mode_status_counts':{m:dict(collections.Counter(x['status']for x in rows if x['mode']==m))for m in sorted({x['mode']for x in rows})},'custom_closures':sum(x['custom_closed']for x in rows if x['mode']=='v0.5')}
(HERE/'validation_results.json').write_text(json.dumps(out,indent=2)+'\n');print(json.dumps(out,indent=2))
