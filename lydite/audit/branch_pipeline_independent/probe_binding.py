#!/usr/bin/env python3
"""Construct broad relation members independently of implementation expressions.

This supplements proof review: all validity patterns and opcode classes, arbitrary
architectural registers/ROM/data, invalid payloads, and nonwriter W-result bits.
It is finite evidence, not exhaustive relation-adequacy proof.
"""
from collections import Counter
import hashlib
import json
from pathlib import Path
import random
from simulate import ROOT, BV, decode, encode, isa, evaluate_record, related


def references(x):
    if isinstance(x,str):
        yield x
    elif isinstance(x,list):
        for y in x[1:]:
            yield from references(y)


def construct(doc, rng, pattern, opcode):
    width = doc['impl']['state']['r0']['bv']
    pc = rng.randrange(4)
    regs = [rng.getrandbits(width), rng.getrandbits(width)]
    rom = [encode(width, opcode if opcode is not None else rng.randrange(8), rng.randrange(2), rng.randrange(2), rng.getrandbits(width)) for _ in range(4)]
    data = [rng.getrandbits(width) for _ in range(4)]
    t = {k:rng.choice((False,True)) if kind == 'bool' else BV(kind['bv'],rng.getrandbits(kind['bv'])) for k,kind in doc['impl']['state'].items()}
    t.update(pc=BV(2,pc),r0=BV(width,regs[0]),r1=BV(width,regs[1]),**{f'rom{i}':BV(width+5,v) for i,v in enumerate(rom)},**{f'data{i}':BV(width,v) for i,v in enumerate(data)})
    s = {k:t[k] for k in doc['spec']['state']}
    age = pc
    pending = regs[:]
    for n,stage in enumerate(('w','x','d')):
        t[stage+'_valid'] = bool(pattern & (1<<n))
        if not t[stage+'_valid']:
            continue
        ir = rom[age]
        t[stage+'_pc'],t[stage+'_ir'] = BV(2,age),BV(width+5,ir)
        op,rd,rs,_ = decode(ir,width)
        if stage == 'w':
            age,pending = isa(ir,age,pending,data,width)
            t['w_next_pc'] = BV(2,age)
            if op < 4:
                t['w_result'] = BV(width,pending[rd])
        else:
            if stage == 'x' and op in (1,2,4):
                t['x_operand'] = BV(width,pending[rs])
            age = (age+1)&3
    t['fetch_pc'] = BV(2,age)
    return s,t,rom,data


def main():
    rows = []
    for width in (4,8,16,32):
        file = ROOT/f'examples/branch_pipeline_w{width}.json'
        doc = json.loads(file.read_text())
        assert all(r.startswith(('spec.','impl.')) for r in references(doc['binding'])), 'binding_contains_nonstate_reference'
        rng = random.Random(0x55EED+width)
        cov = Counter()
        for pattern in range(8):
            for trial in range(64):
                opcode = trial%8 if trial < 32 else None
                s,t,rom,data = construct(doc,rng,pattern,opcode)
                assert related(doc,s,t), ('independent_relation_member_rejected',width,pattern,trial)
                cov['members'] += 1
                cov['validity_'+str(pattern)] += 1
                for st in ('w','x','d'):
                    cov['arbitrary_invalid_'+st] += int(not t[st+'_valid'])
                cov['arbitrary_nonwriter_result'] += int(t['w_valid'] and decode(t['w_ir'].v,width)[0]>=4)
                for reset in (False,True):
                    for stall in (False,True):
                        inp = {'rst':reset,'stall':stall,**{f'rom{i}':BV(width+5,rng.getrandbits(width+5)) for i in range(4)},**{f'data{i}':BV(width,rng.getrandbits(width)) for i in range(4)}}
                        tn = evaluate_record(doc['impl'],'reset' if reset else 'next',t,inp)
                        pc,regs = t['pc'].v,[t['r0'].v,t['r1'].v]
                        if reset:
                            pc,regs = 0,[0,0]
                            sn = evaluate_record(doc['spec'],'reset',s,inp)
                        else:
                            if t['w_valid'] and not stall:
                                pc,regs = isa(rom[pc],pc,regs,data,width)
                            sn = dict(s)
                            sn.update(pc=BV(2,pc),r0=BV(width,regs[0]),r1=BV(width,regs[1]))
                        assert tn['pc'].v==pc and [tn['r0'].v,tn['r1'].v]==regs, ('relation_transition_ISA',width,pattern,trial,reset,stall)
                        assert related(doc,sn,tn), ('relation_not_preserved',width,pattern,trial,reset,stall)
                        if stall and not reset:
                            assert tn==t, ('relation_hold',width,pattern,trial)
                        cov['transitions'] += 1
                # Adequacy controls: the relation must reject a wrong architectural
                # equality and, when applicable, wrong resolution/write/operand.
                bad = dict(t);bad['pc']=BV(2,t['pc'].v+1)
                assert not related(doc,s,bad), ('binding_ignores_pc',width)
                cov['negative_controls'] += 1
                if t['w_valid']:
                    bad=dict(t);bad['w_next_pc']=BV(2,t['w_next_pc'].v+1)
                    assert not related(doc,s,bad), ('binding_ignores_resolved_pc',width)
                    cov['negative_controls'] += 1
                    if decode(t['w_ir'].v,width)[0]<4:
                        bad=dict(t);bad['w_result']=BV(width,t['w_result'].v+1)
                        assert not related(doc,s,bad), ('binding_ignores_write',width)
                        cov['negative_controls'] += 1
                if t['x_valid'] and decode(t['x_ir'].v,width)[0] in (1,2,4):
                    bad=dict(t);bad['x_operand']=BV(width,t['x_operand'].v+1)
                    assert not related(doc,s,bad), ('binding_ignores_operand',width)
                    cov['negative_controls'] += 1
        rows.append({'width':width,'source_sha256':hashlib.sha256(file.read_bytes()).hexdigest(),'state_only_binding':True,'coverage':dict(cov)})
    result={'status':'pass','rows':rows}
    (ROOT/'results/branch_pipeline_independent/binding_probes.json').write_text(json.dumps(result,indent=2)+'\n')
    print(json.dumps(result))

if __name__=='__main__':main()
