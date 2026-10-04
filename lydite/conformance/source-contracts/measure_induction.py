#!/usr/bin/env python3
"""Measure fresh induction proofs from existing source preparation evidence."""
import argparse
import hashlib
import json
from pathlib import Path
import statistics
import sys
import time

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT))
from protocols.axi4lite_project import replay


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--evidence', type=Path, required=True, help='an induct output directory containing model.json and induction-candidates.json')
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--runs', type=int, choices=range(1,11), default=3)
    args = parser.parse_args(); args.out.mkdir(parents=True,exist_ok=False)
    document=replay.load_json(args.evidence/'model.json'); candidates=replay.load_json(args.evidence/'induction-candidates.json')
    result={'engine_sha256':hashlib.sha256(replay.CORE.read_bytes()).hexdigest(),
            'document_sha256':replay.sha(replay.canonical(document)), 'candidates_sha256':replay.sha(replay.canonical(candidates)), 'runs':{}}
    for label, proposed in [('baseline',[]),('strengthened',candidates)]:
        runs=[]
        for index in range(args.runs):
            output=args.out/f'{label}-{index}';start=time.monotonic()
            report=replay.core(document,'induct',candidates=proposed,out=str(output))
            elapsed=time.monotonic()-start;replay.write(args.out/f'{label}-{index}.json',report)
            costs=[q.get('cost',{}) for q in report['proofs']]
            runs.append({'status':report['status'],'seconds':elapsed,'roots':len(report['proofs']),
                         'clauses':sum(c.get('allocated_clauses',0) for c in costs),
                         'work':sum(c.get('work_including_validation_and_derived_steps',0) for c in costs)})
        result['runs'][label]={'median_seconds':statistics.median(r['seconds'] for r in runs),'samples':runs}
    replay.write(args.out/'measurements.json',result);print(json.dumps(result))


if __name__=='__main__':main()
