'use strict';
// Exercise the actual pinned JSON-RPC serializer against the actual stdio server.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const {spawn} = require('node:child_process');
const rpc = require('vscode-jsonrpc/node');

async function run() {
  const root = path.resolve(__dirname, '../../..');
  const child = spawn(process.env.HWVERIFY_TEST_PYTHON || 'python3', [path.join(root, 'editor/server.py'), '--worker', path.join(root, 'target/release/hwverify-editor')]);
  let stderr = '';
  child.stderr.on('data', bytes => stderr += bytes);
  const exited = new Promise(resolve => child.once('exit', resolve));
  const connection = rpc.createMessageConnection(new rpc.StreamMessageReader(child.stdout), new rpc.StreamMessageWriter(child.stdin));
  connection.onClose(() => connection.dispose());
  connection.listen();
  const timeout = setTimeout(() => {console.error('RPC integration timed out'); child.kill(); process.exitCode = 1;}, 30000);
  const uri = 'file:///counter.hwv';
  const source = fs.readFileSync(path.join(root, 'audit/lemma_candidates/counter.hwv'), 'utf8');
  const params = {command: 'hwverify.prove', arguments: [{uri, program: 'counter_step'}]};
  try {
    await connection.sendRequest('initialize', {});
    await connection.sendNotification('textDocument/didOpen', {textDocument: {uri, version: 1, languageId: 'hwverify', text: source}});
    // This exact string-overload mistake caused the hosted server crash:
    // JSON-RPC sends positional params [params, null] for an undefined token.
    await assert.rejects(connection.sendRequest('workspace/executeCommand', params, undefined), error => error.code === -32602);
    const result = await connection.sendRequest('workspace/executeCommand', params);
    assert(result.proof.reports[0].lemma_candidates.target_closed);
    const hard = source.replace('    forall word: bv<8>;', '    mode shared_query;\n    forall a: bv<64>; forall b: bv<64>; forall c: bv<64>;').replace("claim impl.x' == spec.x';", 'claim a * (b + c) == a * b + a * c;');
    await connection.sendNotification('textDocument/didChange', {textDocument: {uri, version: 2}, contentChanges: [{text: hard}]});
    const cancellation = new rpc.CancellationTokenSource();
    let started = false;
    connection.onNotification('hwverify/proofStarted', event => {
      if (event.documentVersion === 2) {started = true; cancellation.cancel();}
    });
    await assert.rejects(connection.sendRequest('workspace/executeCommand', params, cancellation.token), error => error.code === -32800);
    assert(started);
    cancellation.dispose();
    await connection.sendNotification('textDocument/didChange', {textDocument: {uri, version: 3}, contentChanges: [{text: source}]});
    const fresh = await connection.sendRequest('workspace/executeCommand', params);
    assert(fresh.proof.reports[0].lemma_candidates.target_closed);
    assert.equal(fresh.documentVersion, 3);
    await connection.sendRequest('shutdown', {});
    await connection.sendNotification('exit');
    await exited;
    assert.equal(child.exitCode, 0);
    assert.equal(stderr, '');
    console.log('Pinned JSON-RPC client: malformed params rejected, no-token proof succeeds, cancellation and subsequent proof preserve connection');
  } finally {
    clearTimeout(timeout);
    connection.dispose();
    if (child.exitCode === null) child.kill();
    await exited;
    if (stderr) console.error('[RPC server stderr]\n' + stderr);
  }
}
run().catch(error => {console.error(error); process.exitCode = 1;});
