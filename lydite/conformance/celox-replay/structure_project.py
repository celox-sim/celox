#!/usr/bin/env python3
"""Check native no_comb_path contracts against a compiled source project."""
import argparse
from pathlib import Path
import sys
import json
import subprocess
import project
from structure import extract

CHECKER = project.ROOT / '../target/release/lydite-structure'

def check(document, design, compiled, out, source=None, uri="structure.lyd"):
    artifact = extract(design, compiled)
    project.write(out / 'structural-graph.json', artifact)
    request = {'version': 1, 'uri': uri, 'artifact': artifact, **({'source': source} if source is not None else {'document': document})}
    report = project.invoke([CHECKER], request)
    report['checker_sha256'] = project.sha(CHECKER.read_bytes())
    report['trusted_extractor_sha256'] = project.sha((Path(__file__).parent / 'structure.py').read_bytes())
    project.write(out / 'structural-result.json', report)
    return report

def main():
    p = argparse.ArgumentParser(description=__doc__); p.add_argument('manifest', type=Path); p.add_argument('--out', type=Path, required=True); a=p.parse_args()
    try:
        project.check_dependencies(); m=project.load_json(a.manifest)
        project.exact(m,['version','top','sources','specification'],'structural project')
        if type(m['version']) is not int or m['version'] != 1: raise ValueError('structural project version must be 1')
        project.identifier(m['top']); root=a.manifest.resolve().parent
        if not isinstance(m['sources'],list) or not m['sources'] or any(not isinstance(s,str) for s in m['sources']): raise ValueError('nonempty source filenames required')
        paths=[project.project_file(root,s) for s in m['sources']]
        if len(set(paths)) != len(paths): raise ValueError('duplicate source paths')
        spec=project.project_file(root,m['specification'])
        design={'top':m['top'],'four_state':False,'sources':[{'path':s,'text':path.read_text()} for s,path in zip(m['sources'],paths)]}
        a.out.mkdir(parents=True,exist_ok=False); project.write(a.out/'design.json',design)
        compiled=project.invoke([project.FRONTEND,a.out/'design.json']); project.write(a.out/'compiled.json',compiled)
        result=check(project.load_json(spec) if spec.suffix=='.json' else None,design,compiled,a.out,source=spec.read_text() if spec.suffix!='.json' else None, uri=str(spec))
        print(json.dumps(result)); return 0 if result['status']=='verified' else 1 if result['status']=='violated' else 3
    except (ValueError,KeyError,TypeError,OSError,RuntimeError,subprocess.SubprocessError) as error:
        print(json.dumps({'status':'invalid_or_tool_error','error':str(error)})); return 2

if __name__=='__main__':sys.exit(main())
