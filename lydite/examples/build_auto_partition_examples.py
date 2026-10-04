"""0.4 comparisons: same contract, no manually chosen partition hints."""
import copy,json
from pathlib import Path
D=Path(__file__).parent
def save(name,d): (D/(name+'.json')).write_text(json.dumps(d,indent=2)+'\n')
d=json.loads((D/'array_sum.json').read_text());d['program_contract'].pop('split')
save('auto_array_sum',d)
x=copy.deepcopy(d);x['program_contract']['partitioning']='none';save('auto_array_sum_unsplit',x)
def transform(v,rename=False,reorder=False):
    names={'pc':'location','idx':'cursor','acc':'aggregate'}
    if isinstance(v,str) and rename:
        for n,new in names.items():
            if v in ('s.'+n,'spec.'+n,'impl.'+n): return v.rsplit('.',1)[0]+'.'+new
    if isinstance(v,list):
        a=[transform(t,rename,reorder) for t in v]
        if reorder and len(a)==3 and a[0] in ('and','or','eq','add','mul'): a[1],a[2]=a[2],a[1]
        return a
    if isinstance(v,dict): return {(names.get(k,k) if rename else k):transform(t,rename,reorder) for k,t in v.items()}
    return v
save('auto_array_sum_renamed',transform(d,True))
save('auto_array_sum_reordered',transform(d,reorder=True))
for n in ('bad_program','false_invariant','false_pre','bad_rank','input_invariant','wrong_indexed_load'):
    x=json.loads((D/('array_sum_'+n+'.json')).read_text());x['program_contract'].pop('split',None)
    save('auto_array_sum_'+n,x)
