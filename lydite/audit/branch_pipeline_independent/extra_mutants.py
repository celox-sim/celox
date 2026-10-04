#!/usr/bin/env python3
"""Independent implementation-only nonwriter false-forwarding controls."""
import copy
import hashlib
import json
from simulate import ROOT, cases, run


def main():
    folder=ROOT/'results/branch_pipeline_independent/extra_mutants'
    folder.mkdir(parents=True,exist_ok=True)
    rows=[]
    for width in (4,8,16,32):
        good=json.loads((ROOT/f'examples/branch_pipeline_w{width}.json').read_text())
        for stage in ('x','w'):
            bad=copy.deepcopy(good)
            bad['name']=f'branch_pipeline_w{width}_audit_bad_{stage}_nonwriter_forward'
            bad['impl']['wires'][stage+'_match']=['and','s.'+stage+'_valid',['eq','w.'+stage+'_rd','w.d_rs']]
            source=folder/(bad['name']+'.json')
            source.write_text(json.dumps(bad,indent=2)+'\n')
            assert bad['spec']==good['spec'] and bad['binding']==good['binding']
            caught=None
            for case in cases(width):
                try:
                    run(bad,case,check_binding=False,check_micro=False)
                except AssertionError as error:
                    caught={'case':case.__dict__,'failure':str(error)}
                    break
            assert caught, ('extra_mutant_survived',width,stage)
            rows.append({'source':str(source.relative_to(ROOT)),'sha256':hashlib.sha256(source.read_bytes()).hexdigest(),'binding_checked':False,'microarchitecture_checked':False,'counterexample':caught})
    (ROOT/'results/branch_pipeline_independent/extra_mutant_simulation.json').write_text(json.dumps({'status':'pass','rows':rows},indent=2)+'\n')
    print(json.dumps({'status':'pass','mutants_caught':len(rows)}))

if __name__=='__main__':main()
