#!/usr/bin/env python3
"""Build and execute original Rust expectations under assertion capture."""
import argparse, collections, hashlib, json, os, pathlib, subprocess
from generate import DEFAULT, ROOT, generate
ORIGINAL_FIVE=[
 'operators::test_bitwise_operations',
 'context_width::test_runtime_variable_width3_subtraction',
 'operators::test_signed_arithmetic_shift_right',
 'flip_flop::test_ff_nonblocking',
 'flip_flop::test_ff_if_reset_basic',
]
FV_EXTRA=[
 'basic::test_simple_assignment',
 'operators::test_shift_logical_vs_arithmetic',
 'operators::test_subtraction_underflow',
 'basic::test_always_comb_read_before_write_uses_previous_value',
]
SHOWCASE=[
 'std_delay::test_delay_three',
 'packed_scatter_store::packed_scatter_last_lane_does_not_touch_adjacent_storage',
 'recovered_unrolled_fold::recovered_independent_reductions_share_input_correctly',
 'signed_divrem::signed_divrem_four_state_zero_divisor',
 'multi_clock::test_independent_clock_domains',
 'shift_bug_test::test_shift_in_if_reset',
]

def extract(source=DEFAULT, cases=(), output=ROOT/'traces.json'):
    manifest=generate(source,ROOT/'generated-suite')
    output.parent.mkdir(parents=True, exist_ok=True)
    build=subprocess.run(['cargo','build','--locked','--offline','--manifest-path',str(ROOT/'Cargo.toml'),'--target-dir',str(ROOT/'target'),'-p','extract-suite'],capture_output=True,text=True)
    (output.parent/'build.log').write_text(build.stdout+build.stderr)
    if build.returncode: raise RuntimeError(build.stderr)
    env=dict(os.environ,XDG_CACHE_HOME=str(ROOT/'cache'))
    run=subprocess.run([str(ROOT/'target/debug/extract-suite'),*cases],env=env,capture_output=True,text=True,timeout=120)
    if run.returncode: raise RuntimeError(run.stderr)
    data=json.loads(run.stdout)
    for row in data['cases']:
        row['source']=manifest['cases'][row['case']]
        for source in (row.get('design') or {}).get('sources',[]): source['sha256']=hashlib.sha256(source['text'].encode()).hexdigest()
        for a in row.get('actions',row.get('partial_actions',[])):
            if 'assertion' in a:
                for location in ('location','sample_location'):
                    loc=a['assertion'][location];rel=loc['file'].removeprefix('generated-suite/')
                    if rel not in manifest['files']: raise RuntimeError('assertion source not in manifest')
                    loc['file']=rel;loc['sha256']=manifest['files'][rel]
                    loc['url']=f'https://github.com/celox-sim/celox/blob/{manifest["upstream_commit"]}/crates/celox-test-suite-veryl/{rel}#L{loc["line"]}'
    status=collections.Counter(r['status'] for r in data['cases'])
    data['coverage']={'enumerated':len(data['cases']),'status':dict(status),'assertions_extracted':sum(r['assertion_count'] for r in data['cases'] if r['status']=='extracted'),
        'unsupported_reasons':dict(collections.Counter(reason['code'] for r in data['cases'] for reason in r['reasons']))}
    data['provenance']={'upstream_commit':manifest['upstream_commit'],'original_files':manifest['files'],'extractor':'Rust AST assertion instrumentation; original expected Rust calculations; no circuit evaluation'}
    output.write_text(json.dumps(data,indent=2)+'\n')
    return data

def main():
    p=argparse.ArgumentParser();p.add_argument('--source',type=pathlib.Path,default=DEFAULT);p.add_argument('--out',type=pathlib.Path,default=ROOT/'traces.json');p.add_argument('--case',action='append',default=[])
    a=p.parse_args();a.out.parent.mkdir(parents=True,exist_ok=True);data=extract(a.source,a.case,a.out)
    if not a.case:
        rows={r['case']:r for r in data['cases']}
        if len(rows)!=len(data['cases']) or len(rows)!=665: raise RuntimeError('pinned corpus cardinality changed')
        for name in ORIGINAL_FIVE+FV_EXTRA+SHOWCASE:
            if rows[name]['status']!='extracted': raise RuntimeError(f'showcase extraction failed: {name}')
        (ROOT/'selected-traces.json').write_text(json.dumps({'schema':data['schema'],'selected_cases':[rows[n] for n in ORIGINAL_FIVE+FV_EXTRA]},indent=2)+'\n')
        (ROOT/'showcase-traces.json').write_text(json.dumps({'schema':data['schema'],'selected_cases':[rows[n] for n in SHOWCASE]},indent=2)+'\n')
    print(json.dumps(data['coverage'],indent=2))
if __name__=='__main__':main()
