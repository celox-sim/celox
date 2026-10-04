#!/usr/bin/env python3
"""Verify unchanged original assertions across default and finite backends."""
from pathlib import Path
import hashlib
import json
ROOT=Path(__file__).resolve().parents[2]
root=ROOT/'results/branch_pipeline'
rows=[]
for p in sorted((root/'finite').glob('*/*.smt2')):
    if 'kernel-residual' in p.name:
        continue
    q=root/'default'/p.parent.name/p.name
    # Post-query model requests differ by backend; compare every byte before
    # check-sat, including all declarations, definitions and assertions.
    a=p.read_text().split('(check-sat)')[0]
    b=q.read_text().split('(check-sat)')[0]
    rows.append({'model':p.parent.name,'query':p.stem,'original_assertions_byte_identical':a==b,'whole_script_byte_identical':p.read_bytes()==q.read_bytes(),'assertions_sha256':hashlib.sha256(a.encode()).hexdigest()})
assert len(rows)==256 and all(r['original_assertions_byte_identical'] for r in rows)
result={'status':'pass','queries':len(rows),'note':'Only trailing get-model/get-value requests may differ; all original declarations/definitions/assertions are byte-identical.','rows':rows}
(ROOT/'results/branch_pipeline_independent/backend_formula_identity.json').write_text(json.dumps(result,indent=2)+'\n')
print(json.dumps({'status':'pass','queries':len(rows)}))
