"""Summarize fresh execution evidence; saved diagnostics never authorize a proof."""
import argparse
import json
from pathlib import Path
from audit.equality_sharing.ci import digest, read_json, require
from audit.word_frontier.ci import validate_results


def load_runs(root):
    top=read_json(root/'summary.json')
    runs=[read_json(Path(p)) for p in top['runs']]
    validate_results(runs)
    return runs


def detail(row):
    directory=Path(row['input_path']).parent
    report=read_json(directory/'output/report.json')
    primitives={};plans=[];attempts=[]
    def visit(q,route):
        if q.get('automatic_plan') is not None:plans.append({'name':q['name'],**q['automatic_plan']})
        if q.get('backend') in ('finite_bv','structural_kernel'):
            primitives.setdefault(q['evidence'],{'name':q['name'],'evidence':str(directory/'output'/q['evidence']),
                'backend':q['backend'],'status':q['status'],'solver_result':q['solver_result'],
                'finite_work':q.get('finite',{}).get('work',0),'reason':q.get('finite',{}).get('reason'),
                'original_formula_validated':q.get('finite',{}).get('original_formula_validated')})
        for key in ('original_attempt','original_recheck'):
            if q.get(key) is not None:
                item=q[key];attempts.append({'kind':key,'name':item['name'],'status':item['status'],
                    'evidence':item.get('evidence'),'finite_work':item.get('finite',{}).get('work')})
                visit(item,route+'/'+key)
        for key in ('children','conjunctive_children'):
            for child in q.get(key,[]):visit(child,route+'/'+key)
    for q in report['obligations']:visit(q,q['name'])
    work=sum(p['finite_work'] for p in primitives.values())
    require(work==row['accounting']['finite_work_all_originals_and_auxiliaries'],'work mismatch')
    micro=next(q for q in report['obligations'] if q['name']=='microstep_refinement')
    return {'status':row['status'],'accounting':row['accounting'],'automatic_plans':plans,
            'original_attempts_and_rechecks':attempts,'all_primitive_attempts':list(primitives.values()),
            'direct_original_sat':micro['backend']=='finite_bv' and micro['solver_result']=='sat'
                 and micro.get('finite',{}).get('original_formula_validated') is True,
            'report_path':str(directory/'output/report.json'),'report_sha256':digest((directory/'output/report.json').read_bytes())}


def totals(rows):
    return {'executed_cases':len(rows),
            'primitive_queries':sum(r['accounting']['primitive_queries'] for r in rows),
            'finite_work_all_originals_and_auxiliaries':sum(r['accounting']['finite_work_all_originals_and_auxiliaries'] for r in rows),
            'bundle_work_including_validation_and_derived_steps':sum(r['accounting']['bundle_work_including_validation_and_derived_steps'] for r in rows),
            'bundle_work_overlaps_primitive_work':True,
            'recorded_unknown_work_cap_plus_one_attempts':sum(len(r['accounting']['unknown_work_cap_plus_one_attempts']) for r in rows)}


def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--baseline',type=Path,required=True);p.add_argument('--candidate',type=Path,required=True)
    p.add_argument('--phase2-gate',type=Path,required=True);p.add_argument('--prior-run',type=Path,action='append',default=[])
    p.add_argument('--out',type=Path,required=True);a=p.parse_args()
    bs,cs=load_runs(a.baseline),load_runs(a.candidate)
    pairs=[]
    for b,c in zip(bs,cs):
        for br,cr in zip(b['rows'],c['rows']):
            require(br['name']==cr['name'] and br['input_sha256']==cr['input_sha256'],'paired inputs differ')
            pairs.append({'name':br['name'],'width':b['width'],'alpha_seed':b['alpha_seed'],'input_sha256':br['input_sha256'],
                          'classification':'original_formula_sat_mutant' if br['role']=='negative' else 'no_regression_control_not_coverage_gain',
                          'baseline':detail(br),'candidate':detail(cr)})
    gate=read_json(a.phase2_gate/'summary.json')
    require(gate['status']=='passed_with_diagnostic_unknowns','phase2 gate failed or changed limitation results')
    prior=[]
    for path in a.prior_run:
        s=read_json(path/'summary.json');prior.append({'directory':str(path),'summary_sha256':digest((path/'summary.json').read_bytes()),
            'checker_sha256':s['checker_sha256'],'totals':totals(s['rows']),
            'rows':[{'name':r['name'],'input_sha256':r['input_sha256'],'status':r.get('status'),'errors':r['errors'],'accounting':r['accounting']} for r in s['rows']]})
    allrows=[r for s in bs+cs for r in s['rows']]+gate['rows']
    for path in a.prior_run:allrows+=read_json(path/'summary.json')['rows']
    result={'claim':'No independent generic coverage gain found within two bank/mux families. All are no-regression and scheduling controls.',
            'baseline_checker_sha256':bs[0]['checker_sha256'],'candidate_checker_sha256':cs[0]['checker_sha256'],
            'paired_input_hashes_equal':True,'positive_controls':6,'mutants_per_binary':12,'independent_coverage_gains':0,
            'pairs':pairs,'canonical_baseline_totals':totals([r for s in bs for r in s['rows']]),
            'canonical_candidate_totals':totals([r for s in cs for r in s['rows']]),
            'phase2_gate':{'directory':str(a.phase2_gate),'summary_sha256':digest((a.phase2_gate/'summary.json').read_bytes()),
                'status':gate['status'],'verified_covered_positives':gate['verified_covered_positives'],
                'validated_original_counterexamples':gate['validated_original_counterexamples'],
                'diagnostic_unknowns':gate['diagnostic_unknowns'],'totals':totals(gate['rows'])},
            'prior_attempt_ledger':prior,'all_experiment_totals':totals(allrows),
            'scope_note':'No RV source was copied. Two families only; widths and alpha renaming are controls, not new logical families.',
            'authority_note':'Every listed verdict originates in a fresh frozen-checker execution; saved reports are audit diagnostics, never proof authority. All finite defaults are unchanged and executable external solver tripwires were unused.'}
    a.out.write_text(json.dumps(result,indent=2)+'\n')
    print(json.dumps({k:v for k,v in result.items() if k not in ('pairs','prior_attempt_ledger')},indent=2))

if __name__=='__main__':main()
