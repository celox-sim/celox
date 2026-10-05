'use strict';
const assert = require('node:assert/strict');
const path = require('node:path');
const vscode = require('vscode');
const {hoverMarkdown, assertExactHoverLines} = require('./hover-contract');
// LSP plaintext is escaped by languageclient via MarkdownString.appendText.
const encodePlainHover = text => new vscode.MarkdownString().appendText(text).value;

async function until(description, check, timeout = 15000) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    const result = await check();
    if (result) return result;
    await new Promise(resolve => setTimeout(resolve, 40));
  }
  throw new Error(`Timed out: ${description}`);
}
async function replace(document, text) {
  const edit = new vscode.WorkspaceEdit();
  edit.replace(document.uri, new vscode.Range(document.positionAt(0), document.positionAt(document.getText().length)), text);
  assert(await vscode.workspace.applyEdit(edit));
  assert(document.isDirty, 'must exercise an unsaved buffer');
}


function onNextProofStarted(api, uri, version, interrupt) {
  let subscription;
  let timer;
  const promise = new Promise((resolve, reject) => {
    timer = setTimeout(() => {subscription.dispose(); reject(new Error('No real proof-worker start notification'));}, 15000);
    subscription = api.onProofStarted(event => {
      if (event.uri !== uri.toString() || event.documentVersion !== version) return;
      clearTimeout(timer);
      subscription.dispose();
      try {
        assert.equal(event.phase, 'worker_started');
        assert.equal(event.program, 'counter_step');
        assert.match(event.requestIdentity, /^[0-9a-f]{64}$/);
        // Interrupt directly in the actual client notification callback, with
        // no elapsed-time assumption about server or solver startup.
        Promise.resolve(interrupt()).then(() => resolve(event), reject);
      } catch (error) {reject(error);}
    });
  });
  return promise;
}

exports.run = async function run() {
  let stage;
  const enter = name => {stage = name; console.log(`[lydite E2E] ${name}`);};
  enter('open document and activate extension');
  const uri = vscode.Uri.file(path.join(process.env.LYDITE_EXTENSION_TEST_WORKSPACE, 'counter.lyd'));
  const doc = await vscode.workspace.openTextDocument(uri);
  await vscode.window.showTextDocument(doc);
  const extension = vscode.extensions.getExtension('lydite-local.lydite');
  assert(extension, 'development extension must be installed');
  const api = await extension.activate();
  assert(extension.isActive);
  assert.equal(doc.languageId, 'lydite');
  assert.equal(typeof api.checkProof, 'function');
  const source = doc.getText();
  const diagnostics = () => vscode.languages.getDiagnostics(uri);
  const options = {uri: uri.toString(), program: 'counter_step', branch: 0};
  const proofAt = () => doc.positionAt(doc.getText().indexOf('lemma step') + 'lemma '.length);
  let failure;
  try {
    enter('definition, completion and CodeLens');
    // Real VS Code providers, actual languageclient, Python LSP and Rust worker.
    const defs = await until('definition provider', async () => {
      enter('checked proof command and exact hover identity');
    const result = await vscode.commands.executeCommand('vscode.executeDefinitionProvider', uri, doc.positionAt(source.indexOf('step(context')));
      return result?.length ? result : null;
    });
    assert.equal(defs[0].uri.toString(), uri.toString());
    assert.equal(defs[0].range.start.line, proofAt().line);
    const completions = await vscode.commands.executeCommand('vscode.executeCompletionItemProvider', uri, doc.positionAt(source.indexOf('spec.x') + 5));
    assert(completions.items.some(c => c.label === 'x'));
    const lenses = await vscode.commands.executeCommand('vscode.executeCodeLensProvider', uri);
    assert(lenses.some(l => l.command?.command === 'lydite.prove'));

    enter('Unicode/CRLF unsaved edits and diagnostics');
    const unicode = source.replace('Counter with native lemma proposals', 'A\u2028B\u2029C\u0085😀').replace('binding impl.x', 'binding /* 😀\u2028 */ impl.x');
    await replace(doc, unicode);
    assert(await vscode.window.activeTextEditor.edit(edit => edit.setEndOfLine(vscode.EndOfLine.CRLF)));
    const nameAt = doc.getText().indexOf('impl.x ==') + 'impl.'.length;
    const incremental = new vscode.WorkspaceEdit();
    incremental.replace(uri, new vscode.Range(doc.positionAt(nameAt), doc.positionAt(nameAt + 1)), 'missing');
    assert(await vscode.workspace.applyEdit(incremental));
    const invalid = await until('name diagnostic on unsaved edit', () => diagnostics().find(d => d.message.includes('missing')));
    assert.equal(invalid.severity, vscode.DiagnosticSeverity.Error);
    assert.equal(invalid.range.start.character, doc.positionAt(doc.getText().indexOf('impl.missing')).character);
    await replace(doc, source + '\n');
    await until('diagnostics clear after repair', () => diagnostics().length === 0);

    // Invoke the same registered command as CodeLens; explicit branch avoids a
    // modal input dialog, while still using the real progress/result handler.
    enter('checked proof command and exact hover identity');
    const result = await vscode.commands.executeCommand('lydite.prove', options);
    assert(result?.diagnosticOnly);
    assert(result.proof.reports[0].lemma_candidates.target_closed);
    assert.equal(result.documentVersion, doc.version);
    const expectedStatus = 'Lemma step: proved; target closed.';
    await until('exact proof diagnostic reaches VS Code', () => diagnostics().some(d => d.message === expectedStatus && d.code === 'target_closed'));
    const hovers = await vscode.commands.executeCommand('vscode.executeHoverProvider', uri, proofAt());
    assertExactHoverLines(hovers, [
      expectedStatus,
      `Proof request identity: ${result.requestIdentity}`,
      `Document version: ${result.documentVersion}`
    ], encodePlainHover, {uri: uri.toString(), position: proofAt(), documentVersion: doc.version, requestIdentity: result.requestIdentity});

    enter('invalidate checked hover after edit');
    await replace(doc, source + '\n// invalidate checked snapshot\n');
    await until('proof status cleared on unsaved edit', () => !diagnostics().some(d => d.message.includes('proved')));
    await until('hover does not reuse a stale proof', async () => hoverMarkdown(await vscode.commands.executeCommand('vscode.executeHoverProvider', uri, proofAt())).split('\n').includes(encodePlainHover('Proof status: not checked for this document version. Run an explicit proof command.')));

    enter('uncancelled extension API request and bounded Unknown');
    const hard = source.replace('    forall word: bv<8>;', '    mode shared_query;\n    forall a: bv<64>; forall b: bv<64>; forall c: bv<64>;').replace("claim impl.x' == spec.x';", 'claim a * (b + c) == a * b + a * c;');
    await replace(doc, hard);
    // Proves synchronization with the current unsaved buffer before testing
    // cancellation; this is a real bounded solver Unknown, not a fake worker.
    const unknown = await api.checkProof(options);
    assert.equal(unknown.proof.reports[0].lemma_candidates.candidates[0].validity, 'unknown_budget');
    enter('cancel after real worker startup');
    const cancellation = new vscode.CancellationTokenSource();
    const cancellationStarted = onNextProofStarted(api, uri, doc.version, () => cancellation.cancel());
    const cancelled = api.checkProof(options, cancellation.token).then(() => {throw new Error('cancelled proof unexpectedly returned a verdict');}, error => error);
    await cancellationStarted;
    const cancelledError = await cancelled;
    assert([-32800, -32801].includes(cancelledError.code), String(cancelledError));
    cancellation.dispose();

    enter('edit after real worker startup');
    const staleStarted = onNextProofStarted(api, uri, doc.version, () => replace(doc, source + '\n// edit during real proof request\n'));
    const stale = api.checkProof(options).then(() => {throw new Error('edited proof unexpectedly returned a verdict');}, error => error);
    await staleStarted;
    const staleError = await stale;
    assert([-32800, -32801].includes(staleError.code), String(staleError));
    await until('no stale proof diagnostics', () => !diagnostics().some(d => d.message.includes('proved')));
    enter('fresh proof after interruption');
    const fresh = await api.checkProof(options);
    assert(fresh.proof.reports[0].lemma_candidates.target_closed);
    assert.notEqual(fresh.identity, result.identity);
    assert.equal(fresh.documentVersion, doc.version);
    enter('cross-target witness attribution');
    const twoTargets = source.replace("claim impl.x' == spec.x';", "claim impl.x' == spec.x' + 1u8;").replace('    }\n  }\n}\n', '    }\n    target other { rhs 0u8; lemma untouched { context true; guard pre; claim true; } use other_done: untouched(context: pre); result other_done; }\n  }\n}\n');
    await replace(doc, twoTargets);
    const failed = await api.checkProof(options);
    assert(Object.keys(failed.witnesses).length > 0);
    const untouchedAt = doc.positionAt(doc.getText().indexOf('lemma untouched') + 'lemma '.length);
    const unrelatedHovers = await vscode.commands.executeCommand('vscode.executeHoverProvider', uri, untouchedAt);
    assertExactHoverLines(unrelatedHovers, ['Proof status: not checked for this document version. Run an explicit proof command.'], encodePlainHover, {target: 'other', declaration: 'untouched'});
    const unrelated = hoverMarkdown(unrelatedHovers);
    assert(!unrelated.includes(encodePlainHover('Checked target query')), JSON.stringify({unexpectedQuery: unrelated}));
    assert(!unrelated.includes(encodePlainHover('Witnesses for')), JSON.stringify({unexpectedWitness: unrelated}));
    enter('native scoped response contract and infeasible environment');
    const responseUri = vscode.Uri.file(path.join(process.env.LYDITE_EXTENSION_TEST_WORKSPACE, 'response.lyd'));
    const responseDoc = await vscode.workspace.openTextDocument(responseUri);
    await vscode.window.showTextDocument(responseDoc);
    const responseOptions = {uri: responseUri.toString(), program: 'responses', branch: null};
    const responseLenses = await until('response CodeLens', async () => {
      const items = await vscode.commands.executeCommand('vscode.executeCodeLensProvider', responseUri);
      return items?.some(item => item.command?.arguments?.[0]?.program === 'responses') ? items : null;
    });
    assert(responseLenses.length > 0);
    const responseResult = await vscode.commands.executeCommand('lydite.prove', responseOptions);
    assert.equal(responseResult.verification.implementation_binding.responses[0].status, 'verified');
    assert.equal(responseResult.verification.implementation_binding.responses[0].adequacy.reset_acceptance_cover.status, 'reached');
    assert.equal(responseResult.verification.implementation_binding.responses[0].adequacy.reset_acceptance_cover.witness.original_transitions_validated, true);
    await until('reset-acceptance cover reaches Problems', () => vscode.languages.getDiagnostics(responseUri).some(d => d.code === 'response_reset_acceptance_cover' && d.severity === vscode.DiagnosticSeverity.Information));
    await until('acceptance adequacy warning reaches Problems', () => vscode.languages.getDiagnostics(responseUri).some(d => d.code === 'response_external_service_not_specified' && d.severity === vscode.DiagnosticSeverity.Warning));
    const responseSource = responseDoc.getText();
    await replace(responseDoc, responseSource.replace('count = if w.complete { s.count + 1u4 } else { s.count };', 'count = 3u4;'));
    const failedSafety = await api.checkProof(responseOptions);
    assert.equal(failedSafety.verification.implementation_binding.responses[0].status, 'not_established_due_to_binding_failure');
    await until('failed safety prerequisite reaches Problems', () => vscode.languages.getDiagnostics(responseUri).some(d => d.code === 'implementation_binding_not_verified' && d.severity === vscode.DiagnosticSeverity.Error));
    await replace(responseDoc, responseSource.replace('assume !i.stall;', 'assume false;'));
    const impossible = await api.checkProof(responseOptions);
    assert.equal(impossible.verification.implementation_binding.status, 'failed');
    assert(impossible.diagnostics.some(d => d.message.includes('environment_nonempty: failed_nonvacuity') && d.span.line > 30));
    console.log('PASS: real VS Code Extension Host activation, diagnostics, providers, checked command, Unknown, cancellation and stale-result invalidation');
  } catch (error) {
    failure = error;
    console.error(`[lydite E2E failed at ${stage}]`, error.stack || error);
    console.error('[lydite diagnostics]', JSON.stringify(diagnostics()));
    throw error;
  } finally {
    try {await vscode.commands.executeCommand('workbench.action.revertAndCloseActiveEditor');}
    catch (error) {if (!failure) throw error; console.error('[lydite cleanup error]', error);}
  }
};
