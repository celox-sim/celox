#!/usr/bin/env python3
"""Mutation tests change upstream Rust/Veryl source, never a stored trace oracle."""
import argparse, copy, difflib, hashlib, json, pathlib, shutil, tempfile
from extract import DEFAULT, ROOT, extract
from check_extracted import check
if not __debug__: raise RuntimeError('Source controls require assertions; run without -O')
CASE='operators::test_bitwise_operations'
FILE=pathlib.Path('src/cases/operators.rs')

def actions(row):return row['actions']
def bare(row):
    return [{k:v for k,v in a.items() if k!='assertion'} for a in actions(row)]
def main():
    parser=argparse.ArgumentParser();parser.add_argument('--out',type=pathlib.Path,default=ROOT/'source-controls')
    out=parser.parse_args().out;out.mkdir(exist_ok=False)
    original=(DEFAULT/FILE).read_text()
    original_hash=hashlib.sha256((DEFAULT/FILE).read_bytes()).hexdigest()
    baseline=next(r for r in json.loads((ROOT/'traces.json').read_text())['cases'] if r['case']==CASE)
    variants=[
      ('expected_constant','assert_eq!(sim.get(o_and), 0x00u8.into());','assert_eq!(sim.get(o_and), 0x01u8.into());'),
      ('stimulus_input','io.set(a, 0xA5u8);','io.set(a, 0xA4u8);'),
      ('hdl_expression','o_or  = a | b;','o_or  = a & b;'),
      ('invalid_hdl','o_or  = a | b;','THIS_IS_INTENTIONALLY_INVALID_VERYL;'),
    ]
    summary=[]
    with tempfile.TemporaryDirectory(prefix='veryl-source-controls-',dir=ROOT) as temporary:
      source=pathlib.Path(temporary)/'suite';shutil.copytree(DEFAULT,source)
      try:
        for name,before,after in variants:
            dest=out/name;dest.mkdir()
            # The first stimulus occurrence belongs to the selected case. No
            # expected output is added by this test: source code is recompiled.
            if before not in original:raise RuntimeError('upstream control target changed')
            mutated=original.replace(before,after,1);(source/FILE).write_text(mutated)
            (dest/'mutation.patch').write_text(''.join(difflib.unified_diff(original.splitlines(True),mutated.splitlines(True),fromfile=str(FILE),tofile=str(FILE))))
            row=extract(source,[CASE],dest/'traces.json')['cases'][0]
            if row['status']!='extracted':raise RuntimeError(row)
            old=bare(baseline);new=bare(row)
            result={'control':name,'case':CASE,'extraction_status':row['status'],'source_sha256_changed':row['source']['sha256']!=baseline['source']['sha256']}
            if name=='expected_constant':
                deltas=[(a,b) for a,b in zip(old,new) if a!=b]
                assert len(deltas)==1 and deltas[0][0]['action']=='read'
                assert baseline['design']==row['design']
                result['captured_expected_changed']=True
            elif name=='stimulus_input':
                deltas=[(a,b) for a,b in zip(old,new) if a!=b]
                assert len(deltas)==1 and deltas[0][0]['action']=='write'
                assert baseline['design']==row['design']
                result['captured_stimulus_changed']=True
            else:
                assert old==new and baseline['design']!=row['design']
                result['unchanged_independent_assertions']=True
            if name!='invalid_hdl':
                result['finite_check']=check(row,dest/'fv',controls=False)
                assert result['finite_check']['status']=='failed'
                assert result['finite_check']['normal']['cases'][0]['feasible'] is True
            else:
                result['claim']='Extraction succeeds even with invalid HDL: it neither parses nor simulates the circuit to obtain expectations'
            summary.append(result)
      finally:
        # Restore generated files, executable, full manifest, and baseline traces.
        extract(DEFAULT,output=ROOT/'traces.json')
    assert hashlib.sha256((DEFAULT/FILE).read_bytes()).hexdigest()==original_hash
    (out/'summary.json').write_text(json.dumps(summary,indent=2)+'\n')
    print(json.dumps(summary,indent=2))
if __name__=='__main__':main()
