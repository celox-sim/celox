#!/usr/bin/env python3
"""JSONL bridge for the unmodified Rust test suite's proof-backed Backend."""
import json,pathlib,subprocess,sys,os,hashlib
from proof_backend import Backend,ROOT
from four_state import FourStateBackend
out=pathlib.Path(sys.argv[1]);out.mkdir(parents=True,exist_ok=False)
backend=None;sticky=None;reads=0;commands=[];compiled_rejection=False;diagnostic=None;design_hash=None

def send(value):print(json.dumps(value),flush=True)
for line in sys.stdin:
    request=json.loads(line);command=request['command']
    if command=='compile':
        design=request['design'];identity={'top':design['top'],'four_state':design['four_state'],'sources':[{'sha256':hashlib.sha256(x['text'].encode()).hexdigest()} for x in design['sources']]}
        design_hash=hashlib.sha256(json.dumps(identity,sort_keys=True,separators=(',',':')).encode()).hexdigest()
        commands.append({'command':'compile','design_sha256':design_hash})
    else:commands.append(request)
    if command=='close':
        try:
            if backend and not sticky:backend.proof.feasible()
        except Exception as error:sticky=f'{type(error).__name__}: {error}'
        if backend:backend.close()
        result={'status':'failed' if sticky else 'passed','error':sticky,'reads':reads,'commands':len(commands),'design_sha256':design_hash,'protocol_sha256':hashlib.sha256(json.dumps(commands,sort_keys=True,separators=(',',':')).encode()).hexdigest(),'compilation_rejected':compiled_rejection,'diagnostic':diagnostic,'negative_control':backend.proof.negative_control if backend else None,'operations':backend.operations if backend else 0}
        (out/'backend-result.json').write_text(json.dumps(result,indent=2)+'\n');send(result);break
    if sticky:send({'error':sticky});continue
    try:
        if command=='compile':
            (out/'design.json').write_text(json.dumps(request['design']))
            process=subprocess.run([os.environ.get('SIR_EXPORTER_BIN',str(ROOT/'../../../target/debug/lydite-celox-export')),str(out/'design.json')],capture_output=True,text=True,timeout=45)
            (out/'frontend.stderr').write_text(process.stderr)
            if process.returncode:
                message=process.stderr.strip()
                if message.startswith('analyzer-diagnostics:'):
                    diagnostic=json.loads(message.split(':',1)[1])
                    rejection=diagnostic.get('stage')=='analyzer' and bool(diagnostic.get('diagnostics'))
                else:
                    rejection=message.startswith(('parse:','frontend diagnostics:')) or message.startswith('lower: IllegalContext')
                    diagnostic={'stage':message.split(':',1)[0],'detail':message}
                compiled_rejection=rejection
                (out/'frontend-diagnostic.json').write_text(json.dumps(diagnostic,indent=2)+'\n')
                sticky=message or 'compiler failed without diagnostics'
                send({'error':sticky,'compilation_rejected':rejection});continue
            compiled=json.loads(process.stdout);(out/'scheduled-sir.json').write_text(process.stdout)
            backend=(FourStateBackend if request['design']['four_state'] else Backend)(compiled,request['design']['four_state'],out/'proof')
            send({'status':'ready'})
        elif command=='write':
            path=request['path'];backend.write(path['name'],int(request['payload']),int(request['mask']),path['instances']);send({'status':'ok'})
        elif command=='read':
            path=request['path'];payload,mask=backend.read(path['name'],path['instances']);reads+=1
            send({'payload':str(payload),'mask':str(mask)})
        elif command=='eval_comb':backend.eval_comb();send({'status':'ok'})
        elif command=='tick':backend.tick(request['event']);send({'status':'ok'})
        else:raise ValueError('unknown backend command '+command)
    except Exception as error:
        sticky=f'{type(error).__name__}: {error}';send({'error':sticky})
