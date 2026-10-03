"""Protocol regressions, including a real checked .hwv proof (no editor required)."""
import io
import json
import os
from pathlib import Path
import queue
import subprocess
import sys
import tempfile
import threading
import time
import unittest

from editor.server import Index, offset, position, read_message

ROOT = Path(__file__).resolve().parents[1]
WORKER = Path(os.environ.get('HWVERIFY_EDITOR_BIN', ROOT / 'target/release/hwverify-editor'))
SOURCE = (ROOT / 'audit/lemma_candidates/counter.hwv').read_text()
URI = (ROOT / 'audit/lemma_candidates/counter.hwv').as_uri()


class Client:
    def __init__(self, worker=WORKER):
        self.proc = subprocess.Popen([sys.executable, str(ROOT / 'editor/server.py'), '--worker', str(worker)], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.messages = queue.Queue()
        self.pending = []
        self.seq = 0
        def read():
            while (m := read_message(self.proc.stdout)) is not None:
                self.messages.put(m)
        self.thread = threading.Thread(target=read, daemon=True)
        self.thread.start()
        self.request('initialize', {})
        self.notify('initialized', {})

    def send(self, value):
        raw = json.dumps(value).encode()
        self.proc.stdin.write(f'Content-Length: {len(raw)}\r\n\r\n'.encode() + raw)
        self.proc.stdin.flush()

    def notify(self, method, params):
        self.send(dict(jsonrpc='2.0', method=method, params=params))

    def begin(self, method, params):
        self.seq += 1
        self.send(dict(jsonrpc='2.0', id=self.seq, method=method, params=params))
        return self.seq

    def wait(self, predicate, timeout=30):
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            for i, m in enumerate(self.pending):
                if predicate(m):
                    return self.pending.pop(i)
            try:
                m = self.messages.get(timeout=end-time.monotonic())
            except queue.Empty:
                break
            if predicate(m):
                return m
            self.pending.append(m)
        raise AssertionError('timed out waiting for message; pending=' + repr(self.pending)[-4000:])

    def request(self, method, params):
        rid = self.begin(method, params)
        msg = self.wait(lambda m: m.get('id') == rid)
        if 'error' in msg:
            raise AssertionError(msg['error'])
        return msg['result']

    def open(self, text=SOURCE, uri=URI):
        self.notify('textDocument/didOpen', {'textDocument': {'uri': uri, 'version': 1, 'languageId': 'hwverify' if uri.endswith('.hwv') else 'json', 'text': text}})

    def edit(self, text, version=2, uri=URI):
        self.notify('textDocument/didChange', {'textDocument': {'uri': uri, 'version': version}, 'contentChanges': [{'text': text}]})

    def prove(self, **extra):
        return self.request('workspace/executeCommand', {'command': 'hwverify.prove', 'arguments': [dict(uri=URI, program='counter_step', **extra)]})

    def close(self):
        if self.proc.poll() is None:
            self.request('shutdown', {})
            self.notify('exit', {})
            self.proc.stdin.close()
            self.proc.wait(timeout=10)
        self.thread.join(timeout=2)
        err = self.proc.stderr.read().decode()
        self.proc.stdout.close()
        self.proc.stderr.close()
        if err:
            raise AssertionError(err)


class Positions(unittest.TestCase):
    def test_utf16(self):
        text = '😀a\n日本😀x'
        for n in range(len(text)+1):
            self.assertEqual(offset(text, position(text, n)), n)
        self.assertEqual(offset(text, {'line': 0, 'character': 1}), 0)
        self.assertEqual(offset(text, {'line': 1, 'character': 0}), 3)

    def test_unicode_separators_and_crlf_positions(self):
        from editor.server import Server
        text = 'design "A\u2028B\u2029C\x85😀" {\r\n  binding impl.x == spec.x;\r\n}'
        at = text.index('impl.x')
        self.assertEqual(position(text, at), {'line': 1, 'character': 10})
        self.assertEqual(offset(text, {'line': 1, 'character': 10}), at)
        self.assertEqual(offset(text, {'line': 0, 'character': 1000}), text.index('\r'))
        self.assertEqual(position(text, text.index('\r')), position(text, text.index('\n')))
        diagnostic = Server(WORKER, io.BytesIO()).diagnostic({'source': URI+':2:11', 'message': 'source fallback'}, URI, text)
        self.assertEqual(diagnostic['range']['start'], {'line': 1, 'character': 10})

    def test_scoped_navigation_and_incomplete(self):
        text = SOURCE + '\n// 😀 incomplete\n'
        index = Index(text)
        for expr, kind in [('impl.x ==', 'impl'), ('spec.x;', 'spec')]:
            symbol = index.resolve(text.index(expr) + len(kind)+1)
            self.assertEqual(index.scopes[symbol['scope']][1], kind)
        self.assertEqual(index.resolve(text.index('step(context')), next(s for s in index.symbols if s['kind'] == 'lemma'))
        incomplete = Index('design "a" { impl { state foo: bv<8>; next { foo = s.')
        self.assertIn('foo', [s['label'] for s in incomplete.completions(len(incomplete.text))])
        self.assertIsNone(Index('let x = 0; let x = 1; x').resolve(22))


class Protocol(unittest.TestCase):
    def setUp(self):
        self.client = Client()
        self.addCleanup(self.client.close)

    def test_incremental_edit_unicode_separators_astral_and_crlf(self):
        c = self.client
        for index, newline in enumerate(('\n', '\r\n')):
            with self.subTest(newline=repr(newline)):
                source = SOURCE.replace('Counter with native lemma proposals', 'A\u2028B\u2029C\x85😀').replace('binding impl.x', 'binding /* 😀\u2028\u2029\x85 */ impl.x').replace('\n', newline)
                version = 1 + index*3
                if index == 0:
                    c.open(source)
                else:
                    c.edit(source, version)
                at = source.index('impl.x ==') + len('impl.')
                c.notify('textDocument/didChange', {'textDocument': {'uri': URI, 'version': version+1}, 'contentChanges': [{'range': {'start': position(source, at), 'end': position(source, at+1)}, 'text': 'missing'}]})
                bad = source[:at] + 'missing' + source[at+1:]
                result = c.prove()
                self.assertIn('missing', result['diagnostics'][0]['message'])
                self.assertEqual(result['diagnostics'][0]['span']['start'], len(bad[:at-len('impl.')].encode()))
                c.notify('textDocument/didChange', {'textDocument': {'uri': URI, 'version': version+2}, 'contentChanges': [{'range': {'start': position(bad, at), 'end': position(bad, at+7)}, 'text': 'x'}]})
                repaired = c.prove()
                self.assertTrue(repaired['proof']['reports'][0]['lemma_candidates']['target_closed'])
                self.assertEqual(repaired['documentVersion'], version+2)

    def test_edit_diagnostics_without_a_proof_request(self):
        c = self.client
        source = SOURCE.replace('binding impl.x == spec.x;', 'binding /* 😀 */ impl.missing == spec.x;')
        c.open(source)
        message = c.wait(lambda m: m.get('method') == 'textDocument/publishDiagnostics' and m['params'].get('version') == 1 and m['params']['diagnostics'])
        diagnostic = message['params']['diagnostics'][0]
        self.assertIn('missing', diagnostic['message'])
        self.assertEqual(diagnostic['range']['start'], position(source, source.index('impl.missing')))
        c.edit(SOURCE.replace('s.x + 1u8', 's.x + 1u7'))
        message = c.wait(lambda m: m.get('method') == 'textDocument/publishDiagnostics' and m['params'].get('version') == 2 and m['params']['diagnostics'])
        self.assertEqual(message['params']['diagnostics'][0]['severity'], 1)
        self.assertEqual(message['params']['diagnostics'][0]['code'], 'analysis')

    def test_real_source_navigation_proof_and_unsaved_utf16(self):
        c = self.client
        source = SOURCE.replace('design "Counter', 'design "😀 Counter')
        c.open(source)
        at = source.index('step(context')
        definition = c.request('textDocument/definition', {'textDocument': {'uri': URI}, 'position': position(source, at)})
        self.assertEqual(definition['range']['start'], position(source, source.index('lemma step {') + len('lemma ')))
        lenses = c.request('textDocument/codeLens', {'textDocument': {'uri': URI}})
        self.assertEqual(len(lenses), 3)
        result = c.prove()
        self.assertTrue(result['diagnosticOnly'])
        self.assertTrue(result['proof']['reports'][0]['coverage']['fresh_handles_only'])
        self.assertTrue(result['proof']['reports'][0]['lemma_candidates']['target_closed'])
        self.assertFalse(result['proof']['saved_reports_are_authority'])
        hover = c.request('textDocument/hover', {'textDocument': {'uri': URI}, 'position': position(source, source.index('lemma step {') + len('lemma '))})
        self.assertIn('Lemma step: proved; target closed.', hover['contents']['value'].splitlines())
        self.assertIn('Proof request identity: ' + result['requestIdentity'], hover['contents']['value'].splitlines())
        self.assertIn('Document version: 1', hover['contents']['value'].splitlines())
        # Edit in UTF-16 coordinates on the line with an astral character.
        at = source.index('😀')
        c.notify('textDocument/didChange', {'textDocument': {'uri': URI, 'version': 2}, 'contentChanges': [{'range': {'start': position(source, at), 'end': position(source, at+1)}, 'text': '中'}]})
        hover = c.request('textDocument/hover', {'textDocument': {'uri': URI}, 'position': position(source.replace('😀', '中'), source.replace('😀', '中').index('lemma step {') + len('lemma '))})
        self.assertIn('not checked', hover['contents']['value'])
        changed = c.prove(step='step')
        self.assertNotEqual(changed['identity'], result['identity'])
        candidate = changed['proof']['reports'][0]['lemma_candidates']['candidates'][0]
        self.assertEqual(candidate['validity'], 'established')
        self.assertEqual(candidate['usefulness'], 'not_evaluated_in_prefix')
        self.assertFalse(changed['proof']['reports'][0]['lemma_candidates']['target_closed'])

    def test_false_claim_guard_unused_and_width_error(self):
        c = self.client
        c.open(SOURCE.replace("claim impl.x' == spec.x';", "claim impl.x' == spec.x' + 1u8;"))
        result = c.prove()
        candidate = result['proof']['reports'][0]['lemma_candidates']['candidates'][0]
        self.assertEqual(candidate['validity'], 'lemma_counterexample')
        self.assertTrue(result['witnesses'])
        self.assertTrue(any('impl.x' in w['context'] for w in result['witnesses'].values()))
        c.edit(SOURCE.replace('guard pre;', 'guard false;'))
        result = c.prove()
        meta = result['proof']['reports'][0]['lemma_candidates']
        self.assertEqual(meta['candidates'][0]['validity'], 'established')
        self.assertEqual(meta['uses'][0]['state'], 'use_context_does_not_establish_guard')
        from editor.server import Server
        diagnostics = Server(WORKER, io.BytesIO()).proof_diagnostics(result, URI, SOURCE)
        self.assertEqual(diagnostics[0]['code'], 'proved_but_inapplicable')
        unused = SOURCE.replace('      result done;', "      lemma spare { context true; guard pre; claim impl.x' == spec.x'; }\n      result done;")
        c.edit(unused, 3)
        result = c.prove()
        candidates = result['proof']['reports'][0]['lemma_candidates']['candidates']
        self.assertEqual(candidates[1]['usefulness'], 'established_but_unused')
        c.edit(SOURCE.replace('s.x + 1u8', 's.x + 1u7'), 4)
        result = c.prove()
        self.assertTrue(result['diagnostics'])
        self.assertNotIn('proof', result)
        c.edit('design "unfinished" { spec { state', 5)
        result = c.prove()
        self.assertTrue(result['diagnostics'])
        c.edit(SOURCE.replace("claim impl.x'", 'claim missing'), 6)
        self.assertTrue(c.prove()['diagnostics'])

    def test_hover_proof_details_belong_to_target_and_executed_step(self):
        c = self.client
        source = SOURCE.replace("claim impl.x' == spec.x';", "claim impl.x' == spec.x' + 1u8;")
        source = source.replace('      lemma step {', '      lemma seed { context true; guard pre; claim true; }\n      lemma step {')
        source = source.replace('      result done;', '      lemma later { context true; guard pre; claim true; }\n      result done;')
        source = source.replace('    }\n  }\n}\n', "    }\n    target other { rhs 0u8; lemma untouched { context true; guard pre; claim true; } use other_done: untouched(context: pre); result other_done; }\n  }\n}\n")
        c.open(source)
        result = c.prove()
        self.assertTrue(result['witnesses'])
        def hover(name):
            at = source.index('lemma ' + name) + len('lemma ')
            return c.request('textDocument/hover', {'textDocument': {'uri': URI}, 'position': position(source, at)})['contents']['value']
        failed = hover('step')
        self.assertIn('target counter_step, branch 0', failed)
        for witness in result['witnesses']:
            # Only the failed candidate's query is attributed to that lemma;
            # the original target replay may also be present in the full result.
            self.assertNotIn(witness, hover('seed'))
        self.assertIn('editor_query_0001', failed)
        for unexecuted in ('later', 'untouched'):
            text = hover(unexecuted)
            self.assertIn('not checked', text)
            self.assertNotIn('Checked target query', text)
            self.assertNotIn('Witnesses for', text)

    def test_real_worker_start_signal_and_interruptions(self):
        c = self.client
        c.open()
        params = {'command': 'hwverify.prove', 'arguments': [{'uri': URI, 'program': 'counter_step'}]}
        def started(rid):
            event = c.wait(lambda m: m.get('method') == 'hwverify/proofStarted' and m['params']['requestId'] == rid)['params']
            self.assertEqual(event['uri'], URI)
            self.assertEqual(event['program'], 'counter_step')
            self.assertEqual(event['phase'], 'worker_started')
            return event
        rid = c.begin('workspace/executeCommand', params)
        event = started(rid)
        result = c.wait(lambda m: m.get('id') == rid)['result']
        self.assertEqual(event['requestIdentity'], result['requestIdentity'])
        self.assertEqual(event['documentVersion'], result['documentVersion'])
        hard = SOURCE.replace('    forall word: bv<8>;', '    mode shared_query;\n    forall a: bv<64>; forall b: bv<64>; forall c: bv<64>;').replace("claim impl.x' == spec.x';", 'claim a * (b + c) == a * b + a * c;')
        c.edit(hard, 2)
        rid = c.begin('workspace/executeCommand', params)
        self.assertEqual(started(rid)['documentVersion'], 2)
        c.notify('$/cancelRequest', {'id': rid})
        self.assertEqual(c.wait(lambda m: m.get('id') == rid)['error']['code'], -32800)
        rid = c.begin('workspace/executeCommand', params)
        started(rid)
        c.edit(SOURCE + '\n', 3)
        self.assertEqual(c.wait(lambda m: m.get('id') == rid)['error']['code'], -32800)

    def test_unknown_cycle_and_ambiguous_branch(self):
        c = self.client
        hard = SOURCE.replace('    forall word: bv<8>;', '    mode shared_query;\n    forall a: bv<64>; forall b: bv<64>; forall c: bv<64>;')
        hard = hard.replace("claim impl.x' == spec.x';", 'claim a * (b + c) == a * b + a * c;')
        c.open(hard)
        result = c.prove()
        meta = result['proof']['reports'][0]['lemma_candidates']
        self.assertEqual(meta['candidates'][0]['validity'], 'unknown_budget')
        self.assertFalse(meta['target_closed'])
        self.assertEqual(meta['uses'], [])
        c.edit(SOURCE.replace('        context true;', '        depends step;\n        context true;'))
        self.assertTrue(c.prove()['diagnostics'])
        c.edit(SOURCE.replace('binding impl.x == spec.x;', 'binding (impl.x == spec.x) && (impl.x + 1u8 == spec.x);').replace("rhs spec.x';", 'rhs 0u8;'), 3)
        ambiguous = c.prove()
        self.assertIn('branches', ambiguous['diagnostics'][0]['message'])
        chosen = c.prove(branch=0)
        self.assertEqual(chosen['proof']['matching_branches'], 2)
        self.assertEqual(chosen['proof']['branch'], 0)
        for invalid in (-1, '0', True):
            rid = c.begin('workspace/executeCommand', {'command': 'hwverify.prove', 'arguments': [{'uri': URI, 'program': 'counter_step', 'branch': invalid}]})
            self.assertEqual(c.wait(lambda m: m.get('id') == rid)['error']['code'], -32602)

    def test_base_unsaved_dependency_and_disk_invalidation(self):
        c = self.client
        with tempfile.TemporaryDirectory() as temp:
            base = Path(temp) / 'base.hwv'
            base_source = SOURCE.split('  proof {')[0] + '}\n'
            base.write_text(base_source)
            sidecar = SOURCE[SOURCE.index('  proof {'):].replace('  proof {', 'lemmas "counter" {', 1).rsplit('}', 1)[0]
            c.open(sidecar)
            c.request('workspace/executeCommand', {'command': 'hwverify.setBase', 'arguments': [{'uri': URI, 'baseUri': base.as_uri()}]})
            first = c.prove()
            self.assertTrue(first['proof']['reports'][0]['lemma_candidates']['target_closed'])
            definition = c.request('textDocument/definition', {'textDocument': {'uri': URI}, 'position': position(sidecar, sidecar.index('spec.x')+5)})
            self.assertEqual(definition['uri'], base.as_uri())
            completion = c.request('textDocument/completion', {'textDocument': {'uri': URI}, 'position': position(sidecar, sidecar.index('spec.x')+5)})
            self.assertIn('x', [x['label'] for x in completion])
            c.open(base_source.replace('s.x + 1u8', 's.x + 2u8'), base.as_uri())
            second = c.prove()
            self.assertNotEqual(first['identity'], second['identity'])
            c.edit(base_source.replace('s.x + 1u8', 's.x + 1u7'), uri=base.as_uri())
            self.assertTrue(c.prove()['diagnostics'])
            c.notify('textDocument/didClose', {'textDocument': {'uri': base.as_uri()}})
            self.assertTrue(c.prove()['proof']['reports'][0]['lemma_candidates']['target_closed'])
            base.write_text(base_source.replace('s.x + 1u8', 's.x + 1u7'))
            # Even without a watcher event, a read of cached status checks disk bytes.
            hover = c.request('textDocument/hover', {'textDocument': {'uri': URI}, 'position': position(sidecar, sidecar.index('lemma step {') + len('lemma '))})
            self.assertIn('not checked', hover['contents']['value'])
            self.assertTrue(c.prove()['diagnostics'])


class Lifecycle(unittest.TestCase):
    def test_cancel_stale_worker_identity_and_shutdown(self):
        with tempfile.TemporaryDirectory() as temp:
            worker = Path(temp) / 'worker'
            worker.write_text('#!/usr/bin/env python3\nimport json,sys,time,subprocess\nr=json.load(sys.stdin)\nif r.get("step")=="slow":\n subprocess.Popen([sys.executable,"-c","import time; time.sleep(30)"])\n time.sleep(30)\nif r["operation"]=="prove": time.sleep(0.6)\nprint(json.dumps({"diagnostics":[],"proof_programs":None,"proof":{"reports":[]}}))\n')
            worker.chmod(0o755)
            c = Client(worker)
            try:
                c.open()
                params = {'command': 'hwverify.prove', 'arguments': [{'uri': URI, 'program': 'counter_step'}]}
                slow = {'command': 'hwverify.prove', 'arguments': [{'uri': URI, 'program': 'counter_step', 'step': 'slow'}]}
                rid = c.begin('workspace/executeCommand', slow)
                time.sleep(0.15)
                c.notify('$/cancelRequest', {'id': rid})
                self.assertEqual(c.wait(lambda m: m.get('id') == rid)['error']['code'], -32800)
                rid = c.begin('workspace/executeCommand', params)
                c.edit(SOURCE + '\n')
                self.assertEqual(c.wait(lambda m: m.get('id') == rid)['error']['code'], -32800)
                rid = c.begin('workspace/executeCommand', params)
                time.sleep(0.15)
                replacement = worker.with_suffix('.new')
                replacement.write_text(worker.read_text() + '# replaced binary\n')
                replacement.chmod(0o755)
                replacement.replace(worker)
                self.assertEqual(c.wait(lambda m: m.get('id') == rid)['error']['code'], -32801)
                base = Path(temp) / 'base.json'
                base.write_text('{}')
                c.request('workspace/executeCommand', {'command': 'hwverify.setBase', 'arguments': [{'uri': URI, 'baseUri': base.as_uri()}]})
                rid = c.begin('workspace/executeCommand', params)
                time.sleep(0.15)
                base.write_text('{\"changed\":true}')
                self.assertEqual(c.wait(lambda m: m.get('id') == rid)['error']['code'], -32801)
                c.begin('workspace/executeCommand', slow)
                time.sleep(0.15)
            finally:
                c.close()
            self.assertEqual(c.proc.returncode, 0)

    def test_unknown_is_not_proved(self):
        from editor.server import Server
        server = Server(WORKER, io.BytesIO())
        value = {'proof': {'reports': [{'lemma_candidates': {'candidates': [{'id': 'x', 'validity': 'unknown_budget', 'source': URI+':1:1'}]}}]}}
        diagnostics = server.proof_diagnostics(value, URI, SOURCE)
        self.assertEqual(diagnostics[0]['code'], 'unknown_budget')
        self.assertIn('No proof handle', diagnostics[0]['message'])


if __name__ == '__main__':
    unittest.main()
