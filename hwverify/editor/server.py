#!/usr/bin/env python3
"""hwverify stdio LSP. Diagnostics are snapshots, never reusable proof authority."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import sys
import tempfile
import threading
from urllib.parse import unquote, urlparse

TOKEN = re.compile(r'//[^\n]*|/\*.*?\*/|"(?:\\.|[^"\\])*"|[A-Za-z_][A-Za-z_0-9]*|[^\s]', re.S)
KEYWORDS = 'design input reset_input spec impl state reset next outputs binding commit can_step progress enabled rank proof forall target rhs lemma context guard claim depends use result let responses accept pending rank bound cover_depth assume true false bool bv mem'.split()


def offset(text, pos):
    # Use LF delimiters with CRLF handling, not Unicode paragraph separators.
    lines = text.split('\n')
    line = max(pos.get('line', 0), 0)
    if line >= len(lines):
        return len(text)
    start = sum(len(part) + 1 for part in lines[:line])
    units = max(pos.get('character', 0), 0)
    content = lines[line]
    if line < len(lines) - 1 and content.endswith('\r'):
        content = content[:-1]
    for ch in content:
        width = len(ch.encode('utf-16-le')) // 2
        if units < width:
            break
        units -= width
        start += 1
        if units == 0:
            break
    return start


def position(text, at):
    at = min(max(at, 0), len(text))
    start = text.rfind('\n', 0, at) + 1
    line = text.count('\n', 0, at)
    end = text.find('\n', start)
    if end >= 0 and end > start and text[end-1] == '\r':
        at = min(at, end-1)
    return {'line': line, 'character': len(text[start:at].encode('utf-16-le')) // 2}


def region(text, start=0, end=None):
    return {'start': position(text, start), 'end': position(text, min(len(text), start + 1) if end is None else end)}


def path_of(uri):
    parsed = urlparse(uri)
    if parsed.scheme != 'file' or parsed.netloc not in ('', 'localhost'):
        raise ValueError('associated models must use local file URIs')
    return Path(unquote(parsed.path))


def display_json(value, limit=6000):
    text = json.dumps(value, indent=2, ensure_ascii=False)
    return text if len(text) <= limit else text[:limit] + '\n… [display truncated]'


def file_hash(path):
    digest = hashlib.sha256()
    with open(path, 'rb') as source:
        for chunk in iter(lambda: source.read(1024*1024), b''):
            digest.update(chunk)
    return digest.hexdigest()


def copy_snapshot(source, destination, expected, cancelled):
    digest = hashlib.sha256()
    with open(source, 'rb') as incoming, open(destination, 'wb') as outgoing:
        for chunk in iter(lambda: incoming.read(1024*1024), b''):
            if cancelled.is_set():
                raise InterruptedError('Request cancelled while snapshotting')
            digest.update(chunk)
            outgoing.write(chunk)
    if digest.hexdigest() != expected:
        raise ValueError('Source or worker changed during snapshot; retry the request')


class Index:
    """Tolerant navigation only; Rust remains the authority for name/type checks."""
    def __init__(self, text):
        self.text = text
        self.tokens = []
        self.symbols = []
        self.scopes = {0: (None, 'root')}
        stack = [0]
        raw = [m for m in TOKEN.finditer(text) if not m[0].startswith(('//', '/*', '"'))]
        for i, m in enumerate(raw):
            word = m[0]
            if word == '}':
                if len(stack) > 1:
                    stack.pop()
            tok = {'word': word, 'start': m.start(), 'end': m.end(), 'scope': stack[-1]}
            self.tokens.append(tok)
            prev = raw[i-1][0] if i else ''
            if re.fullmatch(r'[A-Za-z_]\w*', word) and prev in ('input', 'state', 'forall', 'let', 'lemma', 'use', 'target', 'module'):
                tail = text[m.end():].split(';', 1)[0].split('\n', 1)[0][:120]
                self.symbols.append(dict(tok, kind=prev, detail=prev + ' ' + word + tail))
            if word == '{':
                label = prev
                if i > 1 and raw[i-2][0] in ('target', 'module', 'lemma'):
                    label = raw[i-2][0] + ':' + prev
                child = len(self.scopes)
                self.scopes[child] = (stack[-1], label)
                stack.append(child)

    def ancestors(self, scope):
        result = []
        while scope is not None:
            result.append(scope)
            scope = self.scopes[scope][0]
        return result

    def token(self, at):
        return next((t for t in self.tokens if t['start'] <= at < t['end']),
                    next((t for t in reversed(self.tokens) if t['end'] == at and re.fullmatch(r'[A-Za-z_]\w*', t['word'])), None))

    def target(self, at):
        tok = self.token(at) or next((t for t in reversed(self.tokens) if t['start'] <= at), None)
        if not tok:
            return None
        for scope in self.ancestors(tok['scope']):
            label = self.scopes[scope][1]
            if label.startswith('target:'):
                return label.split(':', 1)[1]
        if tok['word'] in [s['word'] for s in self.symbols if s['kind'] == 'target']:
            return tok['word']
        return None

    def resolve(self, at):
        tok = self.token(at)
        if not tok:
            return None
        chain = self.ancestors(tok['scope'])
        prefix = self.text[:tok['start']]
        qualifier = re.search(r'(\w+)\.$', prefix)
        candidates = [s for s in self.symbols if s['word'] == tok['word']]
        if qualifier:
            name = qualifier[1]
            if name == 's':
                name = next((self.scopes[s][1] for s in chain if self.scopes[s][1] in ('spec', 'impl')), '')
            if name in ('spec', 'impl'):
                candidates = [s for s in candidates if s['kind'] == 'state' and self.scopes[s['scope']][1] == name]
            elif name == 'i':
                candidates = [s for s in candidates if s['kind'] == 'input']
            elif tok['word'] in ('pre', 'claim'):
                candidates = [s for s in self.symbols if s['word'] == name and s['kind'] == 'lemma' and s['scope'] in chain]
            else:
                return None
        else:
            candidates = [s for s in candidates if s['scope'] in chain]
            if candidates:
                nearest = min(chain.index(s['scope']) for s in candidates)
                candidates = [s for s in candidates if chain.index(s['scope']) == nearest]
        return candidates[0] if len(candidates) == 1 else None

    def completions(self, at):
        tok = next((t for t in reversed(self.tokens) if t['start'] < at), None)
        chain = self.ancestors(tok['scope']) if tok else [0]
        qualifier = re.search(r'(\w+)\.\w*$', self.text[:at])
        result = []
        for s in self.symbols:
            if qualifier:
                name = qualifier[1]
                if name == 's':
                    name = next((self.scopes[c][1] for c in chain if self.scopes[c][1] in ('spec', 'impl')), '')
                include = (name in ('spec', 'impl') and s['kind'] == 'state' and self.scopes[s['scope']][1] == name) or (name == 'i' and s['kind'] == 'input')
            else:
                include = s['scope'] in chain
            if include:
                result.append({'label': s['word'], 'kind': 6, 'detail': s['detail']})
        if qualifier and any(s['word'] == qualifier[1] and s['kind'] == 'lemma' and s['scope'] in chain for s in self.symbols):
            result += [{'label': k, 'kind': 10, 'detail': 'declared candidate sequent; requires a fresh checked dependency'} for k in ('pre', 'claim')]
        if not qualifier:
            result += [{'label': k, 'kind': 14} for k in KEYWORDS + ['pre', 'goal', 'lhs', 'rhs', 'impl', 'spec', 'i']]
        return list({x['label']: x for x in result}.values())


class Server:
    def __init__(self, worker, output=None):
        self.worker = str(Path(worker).resolve())
        self.output = output or sys.stdout.buffer
        self.lock = threading.RLock()
        self.write_lock = threading.Lock()
        self.docs = {}
        self.jobs = {}
        self.slots = threading.BoundedSemaphore(2)
        self.serial = 0
        self.stopped = False

    def send(self, value):
        data = json.dumps(value, ensure_ascii=False).encode()
        with self.write_lock:
            self.output.write(f'Content-Length: {len(data)}\r\n\r\n'.encode() + data)
            self.output.flush()

    def reply(self, rid, result=None, error=None):
        value = {'jsonrpc': '2.0', 'id': rid}
        value['error' if error else 'result'] = error if error else result
        self.send(value)

    def publish(self, uri):
        doc = self.docs.get(uri)
        if doc and uri.endswith('.hwv'):
            self.send({'jsonrpc': '2.0', 'method': 'textDocument/publishDiagnostics', 'params': {
                'uri': uri, 'version': doc['version'], 'diagnostics': doc['diagnostics'] + doc['proof_diagnostics']}})

    def refresh(self):
        # Clients need not support refresh; status is also available through hover.
        self.send({'jsonrpc': '2.0', 'method': 'hwverify/statusChanged', 'params': {}})

    def snapshot(self, uri):
        doc = self.docs[uri]
        request = {'uri': uri, 'text': doc['text']}
        if doc.get('base'):
            base_uri = doc['base']
            if base_uri in self.docs:
                request['base'] = {'uri': base_uri, 'text': self.docs[base_uri]['text']}
            else:
                path = path_of(base_uri)
                request['base'] = {'uri': base_uri, 'path': str(path), 'sha256': file_hash(path)}
        # All proof metadata, terms, dependencies and context are in these bytes.
        # Hash the executable as well: replacing it invalidates cached diagnostics.
        digest = file_hash(self.worker)
        request['worker_sha256'] = digest
        identity = hashlib.sha256(json.dumps([doc['generation'], doc['version'], request, digest], sort_keys=True).encode()).hexdigest()
        return request, identity

    def current(self, job):
        if job['cancel'].is_set() or self.stopped or job['uri'] not in self.docs:
            return False
        try:
            return self.snapshot(job['uri'])[1] == job['identity']
        except (OSError, ValueError):
            return False

    def invalidate(self, changed):
        affected = [uri for uri, d in self.docs.items() if uri == changed or d.get('base') == changed]
        for uri in affected:
            doc = self.docs[uri]
            timer = doc.pop('analysis_timer', None)
            if timer:
                timer.cancel()
            self.serial += 1
            doc.update(generation=self.serial, proof=None, metadata=None, diagnostics=[], proof_diagnostics=[], identity=None)
            for job in list(self.jobs.values()):
                if job['uri'] == uri:
                    self.cancel(job)
            self.publish(uri)
        self.refresh()
        return affected

    @staticmethod
    def cancel(job):
        job['cancel'].set()
        Server.stop_process(job.get('process'))

    @staticmethod
    def stop_process(proc):
        if proc and proc.poll() is None:
            try:
                os.killpg(proc.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass

    def diagnostic(self, item, uri, text):
        start, end = 0, 1
        span = item.get('span')
        if span and item.get('uri', uri) == uri:
            raw = text.encode()
            start = len(raw[:span['start']].decode(errors='ignore'))
            end = len(raw[:span['end']].decode(errors='ignore'))
        else:
            source = item.get('source', '')
            match = re.search(re.escape(uri) + r':(\d+):(\d+)', source or item.get('message', ''))
            if match:
                lines = text.split('\n')
                line = int(match[1])-1
                start = sum(len(part) + 1 for part in lines[:line]) + int(match[2])-1
                end = start + 1
        message = item.get('message', 'analysis failed')
        source = item.get('source')
        if source and not source.startswith(uri + ':'):
            message += f' Declaration location: {source}.'
        if item.get('uri', uri) != uri:
            message = f"Associated model {item['uri']}: {message}"
        return {'range': region(text, start, end), 'severity': item.get('severity', 1), 'source': 'hwverify', 'code': item.get('code', 'analysis'), 'message': message}

    def proof_diagnostics(self, value, uri, text):
        diagnostics = []
        for report in value.get('proof', {}).get('reports', []):
            meta = report.get('lemma_candidates', {})
            for c in meta.get('candidates', []):
                state = c['validity']
                severity = 3 if state == 'established' else 2
                if state == 'established':
                    state = c.get('usefulness', 'established')
                    if any(u.get('candidate') == c['id'] and u.get('state') == 'use_context_does_not_establish_guard' for u in meta.get('uses', [])):
                        state = 'proved_but_inapplicable'
                    elif state == 'established_but_unused' and not meta.get('target_closed'):
                        state = 'proved_target_not_closed'
                    message = f"Lemma {c['id']}: proved; {state.replace('_', ' ')}."
                else:
                    message = f"Lemma {c['id']}: {state.replace('_', ' ')}. No proof handle available."
                diagnostics.append(self.diagnostic(dict(c, message=message, severity=severity, code=state), uri, text))
            for use in meta.get('uses', []):
                if use['state'] != 'applied':
                    diagnostics.append(self.diagnostic(dict(use, message=f"Use {use['id']}: {use['state'].replace('_', ' ')}; a proved lemma is usable only when its entire antecedent holds here.", severity=2, code=use['state']), uri, text))
        error = value.get('proof', {}).get('error')
        if error and not diagnostics:
            diagnostics.append(self.diagnostic({'message': error}, uri, text))
        return diagnostics

    def start(self, uri, rid=None, options=None, ready=False):
        if uri not in self.docs:
            raise ValueError('open the .hwv document first')
        if options is not None:
            if not isinstance(options.get('program'), str) or not options['program']:
                raise ValueError('program must be a target name')
            if options.get('step') is not None and not isinstance(options['step'], str):
                raise ValueError('step must be a declaration name')
            branch = options.get('branch')
            if branch is not None and (type(branch) is not int or not 0 <= branch < 2**64):
                raise ValueError('branch must be a nonnegative integer')
        doc = self.docs[uri]
        previous = doc.pop('analysis_timer', None)
        if previous:
            previous.cancel()
        if options is None and not ready:
            def analyze():
                with self.lock:
                    if self.stopped or self.docs.get(uri) is not doc or doc.get('analysis_timer') is not timer:
                        return
                    doc.pop('analysis_timer', None)
                    try:
                        self.start(uri, ready=True)
                    except (OSError, ValueError) as exc:
                        doc['diagnostics'] = [self.diagnostic({'message': str(exc)}, uri, doc['text'])]
                        self.publish(uri)
            timer = threading.Timer(0.15, analyze)
            timer.daemon = True
            doc['analysis_timer'] = timer
            timer.start()
            return
        request, identity = self.snapshot(uri)
        for old in list(self.jobs.values()):
            if old['uri'] == uri and (options is not None or not old['proof']):
                self.cancel(old)
        self.serial += 1
        key = self.serial
        job = {'uri': uri, 'identity': identity, 'cancel': threading.Event(), 'rid': rid, 'proof': options is not None}
        self.jobs[key] = job
        if options is not None:
            self.docs[uri].update(proof=None, proof_diagnostics=[])
            request.update(operation='prove', program=options.get('program'), step=options.get('step'), branch=options.get('branch'))
        else:
            request['operation'] = 'analyze'
        job['request_identity'] = hashlib.sha256(json.dumps([identity, request], sort_keys=True).encode()).hexdigest()
        self.publish(uri)
        thread = threading.Thread(target=self.run_job, args=(key, job, request), daemon=True)
        job['thread'] = thread
        thread.start()

    def run_job(self, key, job, request):
        acquired = False
        try:
            while not acquired:
                if job['cancel'].is_set():
                    raise InterruptedError()
                acquired = self.slots.acquire(timeout=0.1)
            with tempfile.TemporaryDirectory(prefix='hwverify-editor-') as out:
                request['out'] = out
                executable = Path(out) / 'worker'
                copy_snapshot(self.worker, executable, request['worker_sha256'], job['cancel'])
                executable.chmod(0o700)
                base = request.get('base')
                if base and 'path' in base:
                    destination = Path(out) / 'model.snapshot'
                    copy_snapshot(base['path'], destination, base['sha256'], job['cancel'])
                    request['base'] = {'uri': base['uri'], 'path': str(destination)}
                with self.lock:
                    if job['cancel'].is_set():
                        raise InterruptedError()
                    proc = subprocess.Popen([str(executable)], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True)
                    job['process'] = proc
                    if job['proof']:
                        # Lifecycle observation only: the real child exists and
                        # is awaiting input. This never establishes a verdict.
                        self.send({'jsonrpc': '2.0', 'method': 'hwverify/proofStarted', 'params': {'uri': job['uri'], 'requestId': job['rid'], 'requestIdentity': job['request_identity'], 'documentVersion': self.docs[job['uri']]['version'], 'program': request['program'], 'phase': 'worker_started'}})
                try:
                    stdout, stderr = proc.communicate(json.dumps(request).encode(), timeout=120 if job['proof'] else 20)
                except subprocess.TimeoutExpired:
                    self.stop_process(proc)
                    proc.communicate()
                    raise TimeoutError('worker time limit reached; no result retained')
                if proc.returncode:
                    raise ValueError('worker exited without a result: ' + stderr.decode(errors='replace')[-1000:])
                value = json.loads(stdout)
            with self.lock:
                if not self.current(job):
                    if job['rid'] is not None:
                        self.reply(job['rid'], error={'code': -32800 if job['cancel'].is_set() else -32801, 'message': 'Request cancelled or document/dependency changed; result discarded'})
                    return
                doc = self.docs[job['uri']]
                doc['identity'] = job['identity']
                doc['metadata'] = value.get('proof_programs')
                doc['binding'] = value.get('binding')
                doc['diagnostics'] = [self.diagnostic(x, job['uri'], doc['text']) for x in value.get('diagnostics', [])]
                if job['proof']:
                    doc['proof'] = value
                    doc['proof_request_identity'] = job['request_identity']
                    doc['proof_diagnostics'] = self.proof_diagnostics(value, job['uri'], doc['text'])
                self.publish(job['uri'])
                self.refresh()
                if job['rid'] is not None:
                    self.reply(job['rid'], dict(value, documentVersion=doc['version'], identity=job['identity'], requestIdentity=job['request_identity'], diagnosticOnly=True))
        except Exception as exc:
            with self.lock:
                if job['rid'] is not None:
                    self.reply(job['rid'], error={'code': -32800 if job['cancel'].is_set() else -32603, 'message': str(exc) or 'Request cancelled'})
                elif self.current(job):
                    self.docs[job['uri']]['diagnostics'] = [self.diagnostic({'message': str(exc)}, job['uri'], request['text'])]
                    self.publish(job['uri'])
        finally:
            if acquired:
                self.slots.release()
            with self.lock:
                self.jobs.pop(key, None)

    def checked_doc(self, uri):
        doc = self.docs[uri]
        if doc.get('identity'):
            try:
                changed = self.snapshot(uri)[1] != doc['identity']
            except (OSError, ValueError):
                changed = True
            if changed:
                self.invalidate(uri)
                self.start(uri)
        return doc

    def base_symbols(self, doc, at):
        if not doc.get('base'):
            return None, []
        match = re.search(r'(impl|spec|i)\.\w*$', doc['text'][:at])
        if not match:
            return None, []
        uri = doc['base']
        if uri.lower().endswith('.json'):
            return None, []
        text = self.docs[uri]['text'] if uri in self.docs else path_of(uri).read_text(encoding='utf-8')
        # JSON models still participate in validation/invalidation, but their
        # canonical key locations are not guessed by a source-language index.
        if text.lstrip().startswith('{'):
            return None, []
        index = Index(text)
        symbols = [s for s in index.symbols if (match[1] == 'i' and s['kind'] == 'input') or (s['kind'] == 'state' and index.scopes[s['scope']][1] == match[1])]
        return (uri, text), symbols

    def hover(self, uri, at):
        doc = self.checked_doc(uri)
        index = Index(doc['text'])
        symbol = index.resolve(at)
        if not symbol:
            return None
        text = symbol['detail']
        metadata = doc.get('metadata') or {}
        if symbol['kind'] in ('lemma', 'use', 'target'):
            text += '\n\nBinding before transition: ' + display_json(doc.get('binding'))
            text += '\nUniversal variables (not assumptions): ' + display_json(metadata.get('variables', {}))
            text += '\nModule definitions: ' + display_json(metadata.get('lets', []))
        program = index.target(symbol['start'])
        for p in (doc.get('metadata') or {}).get('programs', []):
            if p['id'] != program:
                continue
            prior = []
            for step in p['steps']:
                if step['id'] == symbol['word']:
                    text += '\n\nDefinitions before this declaration: ' + display_json([s for s in prior if s['op'] == 'let'])
                    text += '\nPreviously declared steps (not assumed proved): ' + ', '.join(s['id'] for s in prior if s['op'] != 'let')
                    text += '\n\nDeclaration (not assumed true):\n' + display_json(step)
                    text += '\n\npre = the selected target branch antecedent; goal/lhs/rhs are the exact current target. Dependencies are checked afresh in program order. Guards do not imply feasibility or reset reachability.'
                prior.append(step)
        has_status = False
        if symbol['kind'] == 'target' and doc.get('proof'):
            for report in doc['proof'].get('proof', {}).get('reports', []):
                meta = report.get('lemma_candidates', {})
                if meta.get('program') == program:
                    text += '\n\nTarget status: ' + ('proved for the selected branch.' if meta.get('target_closed') else 'not closed by this request; prefix results do not certify the target.')
                    has_status = True
        for diag in doc['proof_diagnostics']:
            if diag['range']['start'] == position(doc['text'], symbol['start']):
                text += '\n\n' + diag['message']
                has_status = True
        if not has_status:
            text += '\n\nProof status: not checked for this document version. Run an explicit proof command.'
        value = doc.get('proof') or {}
        proof = value.get('proof', {})
        for report in proof.get('reports', []):
            meta = report.get('lemma_candidates', {})
            if meta.get('program') != program or symbol['kind'] not in ('target', 'lemma', 'use'):
                continue
            rows = meta.get('candidates', []) if symbol['kind'] == 'lemma' else meta.get('uses', [])
            row = next((r for r in rows if r['id'] == symbol['word']), None)
            if symbol['kind'] != 'target' and row is None:
                continue  # This declaration was not executed, even in this target.
            witnesses = value.get('witnesses', {})
            if symbol['kind'] != 'target':
                # Live query positions, not labels, identify a candidate or use.
                query_index = row.get('query_index')
                children = report.get('children', [])
                name = children[query_index].get('name') if isinstance(query_index, int) and 0 <= query_index < len(children) else None
                witnesses = {name: witnesses[name]} if name in witnesses else {}
            origin = f"target {program}, branch {proof.get('branch')}"
            if report.get('editor_prefix_through'):
                origin += f", prefix through {report['editor_prefix_through']}"
            text += '\n\nProof request identity: ' + doc['proof_request_identity']
            text += '\nDocument version: ' + str(doc['version'])
            text += '\n\nChecked target query for ' + origin + ' (source aliases; display only): ' + proof.get('query', '')
            if proof.get('query_display_truncated'):
                text += ' [truncated]'
            detail = 'target request' if symbol['kind'] == 'target' else f"{symbol['kind']} {symbol['word']}"
            text += f'\n\nWitnesses for {detail} in {origin} (auxiliary failures need not be target bugs or reset reachable):\n' + display_json(witnesses)
            break
        return {'contents': {'kind': 'plaintext', 'value': text}, 'range': region(doc['text'], symbol['start'], symbol['end'])}

    def dispatch(self, message):
        rid = message.get('id')
        method = message.get('method')
        p = message.get('params')
        if p is None:
            p = {}
        with self.lock:
            try:
                if not isinstance(p, dict):
                    raise ValueError('LSP method parameters must be an object')
                if method == 'initialize':
                    self.reply(rid, {'capabilities': {'positionEncoding': 'utf-16', 'textDocumentSync': {'openClose': True, 'change': 2}, 'definitionProvider': True, 'completionProvider': {'triggerCharacters': ['.']}, 'hoverProvider': True, 'codeLensProvider': {'resolveProvider': False}, 'executeCommandProvider': {'commands': ['hwverify.prove', 'hwverify.setBase']}}, 'serverInfo': {'name': 'hwverify', 'version': '0.1.0'}})
                elif method == 'shutdown':
                    self.stopped = True
                    for job in self.jobs.values():
                        self.cancel(job)
                    self.reply(rid)
                elif method == 'exit':
                    return False
                elif method == '$/cancelRequest':
                    for job in self.jobs.values():
                        if job['rid'] == p['id']:
                            self.cancel(job)
                elif method == 'textDocument/didOpen':
                    d = p['textDocument']
                    uri = d['uri']
                    self.docs[uri] = {'text': d['text'], 'version': d['version']}
                    affected = self.invalidate(uri)
                    for u in affected:
                        if u.endswith('.hwv'):
                            self.start(u)
                elif method == 'textDocument/didChange':
                    d = p['textDocument']
                    doc = self.docs[d['uri']]
                    if d['version'] <= doc['version']:
                        return True
                    text = doc['text']
                    for change in p['contentChanges']:
                        r = change.get('range')
                        text = text[:offset(text, r['start'])] + change['text'] + text[offset(text, r['end']):] if r else change['text']
                    doc.update(text=text, version=d['version'])
                    for u in self.invalidate(d['uri']):
                        if u.endswith('.hwv'):
                            self.start(u)
                elif method == 'textDocument/didClose':
                    uri = p['textDocument']['uri']
                    affected = self.invalidate(uri)
                    self.docs.pop(uri, None)
                    self.send({'jsonrpc': '2.0', 'method': 'textDocument/publishDiagnostics', 'params': {'uri': uri, 'diagnostics': []}})
                    for u in affected:
                        if u != uri and u.endswith('.hwv'):
                            self.start(u)
                elif method == 'workspace/didChangeWatchedFiles':
                    for change in p['changes']:
                        if change['uri'] in self.docs:
                            continue  # open buffer wins over disk
                        for u in self.invalidate(change['uri']):
                            if u.endswith('.hwv'):
                                self.start(u)
                elif method == 'workspace/executeCommand':
                    args = p.get('arguments') or []
                    options = args[0] if args else {}
                    uri = options['uri']
                    if p['command'] == 'hwverify.setBase':
                        base = options.get('baseUri')
                        if base:
                            path_of(base)  # reject unsupported URI schemes
                            if base == uri:
                                raise ValueError('a sidecar cannot be its own associated model')
                        self.docs[uri]['base'] = base
                        self.invalidate(uri)
                        self.start(uri)
                        self.reply(rid, {'baseUri': base})
                    elif p['command'] == 'hwverify.prove':
                        self.start(uri, rid, options)
                    else:
                        raise ValueError('unknown command')
                elif method in ('textDocument/definition', 'textDocument/completion', 'textDocument/hover', 'textDocument/codeLens'):
                    uri = p['textDocument']['uri']
                    doc = self.checked_doc(uri)
                    index = Index(doc['text'])
                    at = offset(doc['text'], p.get('position', {}))
                    if method.endswith('/definition'):
                        symbol = index.resolve(at)
                        location = {'uri': uri, 'range': region(doc['text'], symbol['start'], symbol['end'])} if symbol else None
                        token = index.token(at)
                        if not location and token:
                            base, symbols = self.base_symbols(doc, token['end'])
                            matches = [s for s in symbols if s['word'] == token['word']]
                            if base and len(matches) == 1:
                                location = {'uri': base[0], 'range': region(base[1], matches[0]['start'], matches[0]['end'])}
                        self.reply(rid, location)
                    elif method.endswith('/completion'):
                        _, symbols = self.base_symbols(doc, at)
                        items = index.completions(at) + [{'label': s['word'], 'kind': 6, 'detail': s['detail']} for s in symbols]
                        self.reply(rid, list({x['label']: x for x in items}.values()))
                    elif method.endswith('/hover'):
                        self.reply(rid, self.hover(uri, at))
                    else:
                        lenses = []
                        response = next((t for i, t in enumerate(index.tokens[:-1]) if t['word'] == 'responses' and index.tokens[i+1]['word'] == '{'), None)
                        if response:
                            lenses.append({'range': region(doc['text'], response['start'], response['end']), 'command': {'title': 'Check implementation responses and safety', 'command': 'hwverify.prove', 'arguments': [{'uri': uri, 'program': 'responses', 'branch': None}]}})
                        for symbol in index.symbols:
                            if symbol['kind'] not in ('target', 'lemma', 'use'):
                                continue
                            program = index.target(symbol['start'])
                            if not program:
                                continue
                            options = {'uri': uri, 'program': program}
                            if symbol['kind'] != 'target':
                                options['step'] = symbol['word']
                            lenses.append({'range': region(doc['text'], symbol['start'], symbol['end']), 'command': {'title': 'Check target' if symbol['kind'] == 'target' else 'Check prefix through ' + symbol['word'], 'command': 'hwverify.prove', 'arguments': [options]}})
                        self.reply(rid, lenses)
                elif rid is not None:
                    self.reply(rid, error={'code': -32601, 'message': 'Method not found'})
            except (KeyError, TypeError, ValueError, OSError) as exc:
                if rid is not None:
                    self.reply(rid, error={'code': -32602, 'message': str(exc)})
                else:
                    self.send({'jsonrpc': '2.0', 'method': 'window/logMessage', 'params': {'type': 1, 'message': str(exc)}})
        return True

    def close(self):
        with self.lock:
            self.stopped = True
            for doc in self.docs.values():
                timer = doc.pop('analysis_timer', None)
                if timer:
                    timer.cancel()
            jobs = list(self.jobs.values())
            for job in jobs:
                self.cancel(job)
        for job in jobs:
            job['thread'].join(timeout=5)


def read_message(stream):
    headers = {}
    while True:
        line = stream.readline()
        if not line:
            return None
        if line in (b'\r\n', b'\n'):
            break
        key, value = line.decode('ascii').split(':', 1)
        headers[key.lower()] = value.strip()
    length = int(headers['content-length'])
    if not 0 <= length <= 64*1024*1024:
        raise ValueError('LSP message size limit')
    raw = stream.read(length)
    if len(raw) != length:
        return None
    return json.loads(raw)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--worker', required=True)
    args = parser.parse_args()
    server = Server(args.worker)
    try:
        while (message := read_message(sys.stdin.buffer)) is not None:
            if not server.dispatch(message):
                break
    finally:
        server.close()


if __name__ == '__main__':
    main()
