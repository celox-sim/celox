"""Lossless migration of existing proof-program prove/apply data; never proves anything."""
import argparse
import copy
import json
from pathlib import Path


def handle_references(value):
    if isinstance(value, str) and value.startswith('handle.'):
        return {value.split('.')[1]}
    if isinstance(value, list):
        return set().union(*(handle_references(x) for x in value))
    return set()


def migrate(metadata):
    result = copy.deepcopy(metadata)
    for program in result['programs']:
        steps = program['steps']
        migrated = []
        i = 0
        while i < len(steps):
            step = steps[i]
            # Collapse only the exact checked guard-discharge/apply idiom.
            if step['op'] == 'prove' and i+1 < len(steps):
                use = steps[i+1]
                referenced_later = json.dumps(steps[i+2:])
                if (use['op'] == 'apply' and use['premise'] == step['id']
                        and step['post'] == 'handle.'+use['lemma']+'.pre'
                        and ('"'+step['id']+'"') not in referenced_later
                        and ('handle.'+step['id']+'.') not in referenced_later
                        and program['result'] != step['id']):
                    migrated.append({'op':'use_candidate', 'id':use['id'],
                                     'candidate':use['lemma'], 'context':step['pre']})
                    i += 2
                    continue
            if step['op'] == 'prove':
                # Factoring true does not alter the exact sequent term.
                guarded = step['id'] == 'delivery' or step['id'].startswith('alu_')
                migrated.append({'op':'candidate', 'id':step['id'], 'frame':'current_query',
                                 'context':True if guarded else step['pre'],
                                 'guard':step['pre'] if guarded else True,
                                 'claim':step['post'],
                                 'depends_on':sorted(handle_references(step['pre']) | handle_references(step['post'])),
                                 'source':f"proof_programs/{program['id']}/steps/{i}"})
            else:
                migrated.append(step)
            i += 1
        program['steps'] = migrated
    return result


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('input', type=Path); p.add_argument('output', type=Path)
    args = p.parse_args()
    doc = json.loads(args.input.read_text())
    original = copy.deepcopy(doc)
    doc['proof_programs'] = migrate(doc['proof_programs'])
    assert {k:v for k,v in doc.items() if k!='proof_programs'} == {k:v for k,v in original.items() if k!='proof_programs'}
    with args.output.open('x') as f:
        f.write(json.dumps(doc, indent=2)+'\n')

if __name__ == '__main__':
    main()
