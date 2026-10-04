"""Fresh finite latency proofs: import, exact partitioning, replay gates and sealing.

No function accepts saved reports as proof authority. Variant source identities
are pinned; each run imports the source and invokes the checker anew. Historical
reports remain historical evidence, never handles for another source or run.
"""
import argparse
import copy
import hashlib
import json
import os
from pathlib import Path
import re
from dataclasses import asdict
from audit.veryl_scaling import cpu_memory_contract as c
from audit.veryl_scaling import rv32i_spec as s

ROOT=Path(__file__).resolve().parents[2]
BV32={'bv':32}
INPUTS={'rst':'bool','stall':'bool','imem_response':BV32,'imem_fault':'bool',
        'dmem_response':BV32,'dmem_fault':'bool'}
PORTS={**{k:BV32 for k in ('imem_address','dmem_address','write_address','write_data','trap_pc','trap_value')},
       **{k:'bool' for k in ('imem_valid','dmem_valid','dmem_write','write_enable','commit','retire','trap_valid')},
       'write_mask':{'bv':4},'trap_cause':{'bv':4}}

def sha(path):return hashlib.sha256(Path(path).read_bytes()).hexdigest()
def digest(value):return hashlib.sha256(json.dumps(value,sort_keys=True,separators=(',',':')).encode()).hexdigest()

def validate_lemma(report):
    """Strict existing checker outcomes; Unknown and unvalidated SAT fail closed."""
    binding=report.get('implementation_binding',{});queries=binding.get('obligations',[])
    expected={'binding_reset_nonempty':'sat','binding_reset_establishes_product':'unsat','binding_product_preservation':'unsat'}
    if (report.get('status')!='binding_verified_no_examples' or binding.get('status')!='verified'
        or len(queries)!=len(expected) or {q.get('name') for q in queries}!=set(expected)):
        raise ValueError('incomplete or unsuccessful latency proof')
    for query in queries:
        wanted=expected[query['name']]
        if (query.get('status')!='passed' or query.get('solver_result')!=wanted
            or query.get('logical_expectation')!=wanted
            or query.get('backend') not in (('finite_bv',) if wanted=='sat' else ('finite_bv','structural_kernel'))
            or query.get('z3_seconds',0)!=0):
            raise ValueError('invalid latency verdict/backend')
        if wanted=='sat' and query.get('finite',{}).get('original_formula_validated') is not True:
            raise ValueError('unvalidated nonemptiness witness')

def validate_mutation(report):
    binding=report.get('implementation_binding',{});queries=binding.get('obligations',[])
    expected={'binding_reset_nonempty':('sat','passed','sat'),
              'binding_reset_establishes_product':('unsat','passed','unsat'),
              'binding_product_preservation':('sat','counterexample','unsat')}
    if (report.get('status')!='implementation_binding_failed' or binding.get('status')!='failed'
        or len(queries)!=len(expected) or {q.get('name') for q in queries}!=set(expected)):
        raise ValueError('mutation did not produce the expected failed binding')
    for query in queries:
        result,status,expectation=expected[query['name']]
        if (query.get('solver_result')!=result or query.get('status')!=status
            or query.get('logical_expectation')!=expectation or query.get('z3_seconds',0)!=0
            or query.get('backend') not in (('finite_bv',) if result=='sat' else ('finite_bv','structural_kernel'))):
            raise ValueError('invalid mutation counterexample verdict/backend')
        if result=='sat' and query.get('finite',{}).get('original_formula_validated') is not True:
            raise ValueError('mutation lacks original-formula validated finite SAT')

def source_path(variant):return ROOT/'conformance/veryl-symbolic'/variant.filename

def state_types(text):
    """Restricted current fixture syntax; reject incomplete reset/state coverage."""
    if text.count('always_ff (clk) {')!=1:raise ValueError('unexpected sequential block layout')
    ff=text.split('always_ff (clk) {',1)[1]
    if '} else if' not in ff:raise ValueError('missing reset branch')
    reset=ff.split('} else if',1)[0]
    pattern=r'\b(\w+)\s*=(?!=)'
    names=set(re.findall(pattern,reset))
    if not names or names!=set(re.findall(pattern,ff)):
        raise ValueError('a sequentially assigned field lacks reset')
    types={name:({'bv':int(width)} if width else 'bool') for name,width in
           re.findall(r'\b(\w+)\s*:\s*(?:output\s+)?bit(?:<(\d+)>)?',text)}
    if names-set(types):raise ValueError('unknown sequential field widths')
    return {k:types[k] for k in sorted(names)}

def compile_machine(out,frontend,lifter,variant):
    out=Path(out);out.mkdir(parents=True,exist_ok=False)
    source=source_path(variant);text=source.read_text()
    if hashlib.sha256(text.encode()).hexdigest()!=variant.source_sha256:
        raise ValueError('variant source differs from pinned manifest')
    (out/'source.veryl').write_text(text)
    state=state_types(text)
    c.RUNNER.write_json(out/'design.json',{'top':variant.top,'four_state':False,'sources':[{'path':'source.veryl','text':text}]})
    compiled,_=c.execute([frontend,out/'design.json'],out/'compiled.json')
    if compiled.get('allowed_diagnostics')!=[]:raise ValueError('unexpected frontend diagnostics')
    cfg={'event':'clk','inputs':{'rst_n':{'name':'rst','type':'bool','expr':['not','i.rst']},
         **{k:{'type':v} for k,v in INPUTS.items() if k!='rst'}},'state':{k:{'type':v} for k,v in state.items()}}
    raw={'state':state}
    for mode,resetting in [('normal',False),('reset',True)]:
        cfg['overrides']={'rst':resetting}
        cfg['outputs']={} if resetting else {**{k:{'signal':k,'type':v} for k,v in PORTS.items()},
            **{k:{'signal':signal,'type':'bool'} for k,signal in [('h','hazard'),('r','redirect'),('f','fault')]}}
        c.RUNNER.write_json(out/(mode+'-bindings.json'),cfg)
        lifted,_=c.execute([lifter,out/'compiled.json',out/(mode+'-bindings.json')]+(['--inline'] if resetting else []),out/(mode+'-lift.json'))
        if resetting:
            if any(c.RUNNER.references(v,'s.') or c.RUNNER.references(v,'w.') for v in lifted['next'].values()):
                raise ValueError('nonconstant reset')
            raw['reset']=lifted['next']
        else:
            raw.update(next=lifted['next'],wires=lifted['wires'],outputs={k:lifted['outputs'][k] for k in PORTS})
            roots={k:lifted['outputs'][k] for k in ('h','r','f')}
    c.RUNNER.write_json(out/'machine.json',raw);c.RUNNER.write_json(out/'roots.json',roots)
    return raw,roots

def builders(variant):
    from audit.veryl_scaling import rv32i_latency_models as models
    if variant.family=='baseline':
        return models.BASELINE_CONTROL,models.baseline_source_document,models.baseline_control_document,19,4,3
    if variant.family=='fourstage':
        return models.FOURSTAGE_CONTROL,models.fourstage_source_document,models.fourstage_control_document,24,6,4
    raise ValueError('unknown model family')

def check_reset(raw,variant):
    control=builders(variant)[0]
    if any(raw['state'].get(k)!='bool' or raw['reset'].get(k) is not False for k in control):
        raise ValueError('source control reset differs from abstraction')
    for selector,width,value in variant.nonzero_resets:
        if raw['state'].get(selector)!={'bv':width} or raw['reset'].get(selector)!=s.b(width,value):
            raise ValueError('variant nonzero reset differs from manifest')

def conjuncts(expr):
    if isinstance(expr,list) and expr[0]=='and':
        for child in expr[1:]:yield from conjuncts(child)
    else:yield expr

def partition_source(source_doc,expected_count):
    """Exact conjunction decomposition; no assumptions or handles are inserted."""
    docs={};coverage=[];source_terms=list(conjuncts(source_doc['specs']['Contract']['operations']['tick']))
    if len(source_terms)!=expected_count:raise ValueError('source equation count changed')
    split_lhs=set()
    for j,term in enumerate(source_terms):
        split=isinstance(term,list) and len(term)==3 and term[0]=='eq' and term[1] in ('n.d_pc','n.d_ir')
        if split:
            if source_doc['specs']['Contract']['state'][term[1][2:]]!={'bv':32} or term[1] in split_lhs:
                raise ValueError('duplicate or non-BV32 split equation')
            split_lhs.add(term[1])
        terms=[s.eq(s.ex(bit,bit,term[1]),s.ex(bit,bit,term[2])) for bit in range(32)] if split else [term]
        for bit,part in enumerate(terms):
            doc=copy.deepcopy(source_doc);doc['specs']['Contract']['operations']['tick']=part
            name=f'source-{j}-{bit}';docs[name]=doc
            coverage.append({'name':name,'conjunct':j,'bit':bit if split else None,'operation_sha256':digest(part)})
    if split_lhs!={'n.d_pc','n.d_ir'}:raise ValueError('missing payload split equations')
    expected_parts=expected_count+62
    if len(coverage)!=expected_parts or len(set(x['name'] for x in coverage))!=expected_parts:
        raise ValueError('incomplete source conjunction coverage')
    return docs,coverage

def dependencies(variant,frontend,lifter,checker):
    from audit.veryl_scaling import rv32i_latency_models,rv32i_latency_variants,rv32i_latency_witnesses
    return [frontend,lifter,checker,Path(__file__),Path(rv32i_latency_models.__file__),
            Path(rv32i_latency_variants.__file__),Path(rv32i_latency_witnesses.__file__),
            Path(__file__).with_name(variant.wrapper+'.py'),Path(c.__file__),Path(s.__file__),
            Path(c.registers.__file__),Path(c.RUNNER.__file__),ROOT/'examples/build_pipeline.py',
            ROOT/'audit/interpreter.py',source_path(variant)]

def run(out,variant,checker=None):
    out=Path(out);out.mkdir(parents=True,exist_ok=False)
    frontend=ROOT/'conformance/veryl-proof/target/debug/veryl-proof-frontend'
    lifter=ROOT/'../target/release/lydite-sir-lift';checker=Path(checker or ROOT/'../target/release/lydite')
    if any(k.startswith('LYDITE_') and k!='LYDITE_SOLVER' for k in os.environ):
        raise ValueError('nondefault LYDITE environment is not permitted')
    fingerprints={str(x.resolve()):sha(x) for x in dependencies(variant,frontend,lifter,checker)}
    snapshots=out/'source-snapshots';snapshots.mkdir()
    for path,fingerprint in fingerprints.items():
        if path.endswith('.py'):
            data=Path(path).read_bytes()
            if hashlib.sha256(data).hexdigest()!=fingerprint:raise ValueError('snapshot race')
            (snapshots/(fingerprint+'-'+Path(path).name)).write_bytes(data)
    raw,roots=compile_machine(out/'import',frontend,lifter,variant);check_reset(raw,variant)
    _,source_document,control_document,count,bound,nominal=builders(variant)
    source_doc=source_document(raw,roots)
    c.RUNNER.write_json(out/'source-simulation-conjunction.json',source_doc)
    docs,coverage=partition_source(source_doc,count);docs['token-bound']=control_document()
    result={'variant':asdict(variant),'source_conjunction_sha256':digest(source_doc),'coverage':coverage,
            'source_sha256':sha(out/'import/source.veryl'),'machine_sha256':digest(raw),'files':fingerprints,'proofs':{}}
    for name,doc in docs.items():
        c.RUNNER.write_json(out/(name+'.json'),doc)
        report,seconds=c.execute([checker,out/(name+'.json'),'--out',out/(name+'-proof')],out/(name+'-report.json'),(0,1,3))
        validate_lemma(report)
        result['proofs'][name]={'seconds':seconds,'report_sha256':sha(out/(name+'-report.json')),
            'document_sha256':sha(out/(name+'.json')),
            'finite_work':sum(q.get('finite',{}).get('work',0) for q in report['implementation_binding']['obligations'])}
    result['auxiliary_proofs']={}
    if variant.nonzero_resets:
        from audit.veryl_scaling.rv32i_latency_models import selector_alignment_document
        for selector,_,_ in variant.nonzero_resets:
            name='alignment-'+selector
            doc=selector_alignment_document(raw,selector)
            c.RUNNER.write_json(out/(name+'.json'),doc)
            report,seconds=c.execute([checker,out/(name+'.json'),'--out',out/(name+'-proof')],out/(name+'-report.json'),(0,1,3))
            validate_lemma(report)
            result['auxiliary_proofs'][name]={'seconds':seconds,'report_sha256':sha(out/(name+'-report.json')),
                'document_sha256':sha(out/(name+'.json')),
                'finite_work':sum(q.get('finite',{}).get('work',0) for q in report['implementation_binding']['obligations'])}
            mutant=copy.deepcopy(raw);mutant['next'][selector]='s.'+selector
            name='stale-'+selector
            c.RUNNER.write_json(out/(name+'.json'),selector_alignment_document(mutant,selector))
            report,_=c.execute([checker,out/(name+'.json'),'--out',out/(name+'-proof')],out/(name+'-report.json'),(0,1,3))
            validate_mutation(report)
            result['auxiliary_proofs'][name]={'rejected':True,'report_sha256':sha(out/(name+'-report.json')),
                'document_sha256':sha(out/(name+'.json'))}
    for name in ('stuck','early-retire','missed-squash'):
        c.RUNNER.write_json(out/(name+'.json'),control_document(name))
        report,_=c.execute([checker,out/(name+'.json'),'--out',out/(name+'-proof')],out/(name+'-report.json'),(0,1,3))
        validate_mutation(report)
        result['proofs'][name]={'rejected':True,'report_sha256':sha(out/(name+'-report.json')),
                              'document_sha256':sha(out/(name+'.json'))}
    from audit.veryl_scaling.rv32i_latency_witnesses import actual_witnesses
    witnesses=actual_witnesses(raw,roots,variant)
    c.RUNNER.write_json(out/'witnesses.json',witnesses);result['witnesses_sha256']=sha(out/'witnesses.json')
    result['artifacts']={str(x.relative_to(out)):sha(x) for x in out.rglob('*') if x.is_file()}
    result['aggregate_positive_seconds']=sum(x.get('seconds',0) for x in result['proofs'].values())
    result['aggregate_positive_finite_work']=sum(x.get('finite_work',0) for x in result['proofs'].values())
    result['aggregate_auxiliary_seconds']=sum(x.get('seconds',0) for x in result['auxiliary_proofs'].values())
    result['aggregate_auxiliary_finite_work']=sum(x.get('finite_work',0) for x in result['auxiliary_proofs'].values())
    if any(sha(path)!=value for path,value in fingerprints.items()):raise ValueError('source/tool changed during proof')
    result.update(status='verified',bound_subsequent_enabled_edges=bound,nominal_subsequent_enabled_edges=nominal)
    c.RUNNER.write_json(out/'certificate.json',result)
    return result

def main(variant):
    ap=argparse.ArgumentParser();ap.add_argument('--out',type=Path,required=True);ap.add_argument('--checker',type=Path)
    args=ap.parse_args();print(json.dumps(run(args.out,variant,args.checker),indent=2))
