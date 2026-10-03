'use strict';
const assert = require('node:assert/strict');
const path = require('node:path');
const vscode = require('vscode');

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
function hoverText(items) {
  return items.flatMap(h => h.contents).map(c => typeof c === 'string' ? c : c.value).join('\n');
}

exports.run = async function run() {
  const uri = vscode.Uri.file(path.join(process.env.HWVERIFY_EXTENSION_TEST_WORKSPACE, 'counter.hwv'));
  const doc = await vscode.workspace.openTextDocument(uri);
  await vscode.window.showTextDocument(doc);
  const extension = vscode.extensions.getExtension('hwverify-local.hwverify');
  assert(extension, 'development extension must be installed');
  const api = await extension.activate();
  assert(extension.isActive);
  assert.equal(doc.languageId, 'hwverify');
  assert.equal(typeof api.checkProof, 'function');
  const source = doc.getText();
  const diagnostics = () => vscode.languages.getDiagnostics(uri);
  const options = {uri: uri.toString(), program: 'counter_step', branch: 0};
  const proofAt = () => doc.positionAt(doc.getText().indexOf('lemma step') + 'lemma '.length);
  try {
    // Real VS Code providers, actual languageclient, Python LSP and Rust worker.
    const defs = await until('definition provider', async () => {
      const result = await vscode.commands.executeCommand('vscode.executeDefinitionProvider', uri, doc.positionAt(source.indexOf('step(context')));
      return result?.length ? result : null;
    });
    assert.equal(defs[0].uri.toString(), uri.toString());
    assert.equal(defs[0].range.start.line, proofAt().line);
    const completions = await vscode.commands.executeCommand('vscode.executeCompletionItemProvider', uri, doc.positionAt(source.indexOf('spec.x') + 5));
    assert(completions.items.some(c => c.label === 'x'));
    const lenses = await vscode.commands.executeCommand('vscode.executeCodeLensProvider', uri);
    assert(lenses.some(l => l.command?.command === 'hwverify.prove'));

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
    const result = await vscode.commands.executeCommand('hwverify.prove', options);
    assert(result?.diagnosticOnly);
    assert(result.proof.reports[0].lemma_candidates.target_closed);
    assert.equal(result.documentVersion, doc.version);
    await until('proof diagnostic reaches VS Code', () => diagnostics().some(d => d.message.includes('proved')));
    const hovers = await vscode.commands.executeCommand('vscode.executeHoverProvider', uri, proofAt());
    assert(hoverText(hovers).includes('proved'));

    await replace(doc, source + '\n// invalidate checked snapshot\n');
    await until('proof status cleared on unsaved edit', () => !diagnostics().some(d => d.message.includes('proved')));
    await until('hover does not reuse a stale proof', async () => hoverText(await vscode.commands.executeCommand('vscode.executeHoverProvider', uri, proofAt())).includes('not checked'));

    const hard = source.replace('    forall word: bv<8>;', '    mode shared_query;\n    forall a: bv<64>; forall b: bv<64>; forall c: bv<64>;').replace("claim impl.x' == spec.x';", 'claim a * (b + c) == a * b + a * c;');
    await replace(doc, hard);
    // Proves synchronization with the current unsaved buffer before testing
    // cancellation; this is a real bounded solver Unknown, not a fake worker.
    const unknown = await api.checkProof(options);
    assert.equal(unknown.proof.reports[0].lemma_candidates.candidates[0].validity, 'unknown_budget');
    const cancellation = new vscode.CancellationTokenSource();
    const cancelled = api.checkProof(options, cancellation.token).then(() => {throw new Error('cancelled proof unexpectedly returned a verdict');}, error => error);
    await new Promise(resolve => setTimeout(resolve, 50));
    cancellation.cancel();
    const cancelledError = await cancelled;
    assert([-32800, -32801].includes(cancelledError.code), String(cancelledError));
    cancellation.dispose();

    const stale = api.checkProof(options).then(() => {throw new Error('edited proof unexpectedly returned a verdict');}, error => error);
    await new Promise(resolve => setTimeout(resolve, 50));
    await replace(doc, source + '\n// edit during real proof request\n');
    const staleError = await stale;
    assert([-32800, -32801].includes(staleError.code), String(staleError));
    await until('no stale proof diagnostics', () => !diagnostics().some(d => d.message.includes('proved')));
    const fresh = await api.checkProof(options);
    assert(fresh.proof.reports[0].lemma_candidates.target_closed);
    assert.notEqual(fresh.identity, result.identity);
    assert.equal(fresh.documentVersion, doc.version);
    const twoTargets = source.replace("claim impl.x' == spec.x';", "claim impl.x' == spec.x' + 1u8;").replace('    }\n  }\n}\n', '    }\n    target other { rhs 0u8; lemma untouched { context true; guard pre; claim true; } use other_done: untouched(context: pre); result other_done; }\n  }\n}\n');
    await replace(doc, twoTargets);
    const failed = await api.checkProof(options);
    assert(Object.keys(failed.witnesses).length > 0);
    const untouchedAt = doc.positionAt(doc.getText().indexOf('lemma untouched') + 'lemma '.length);
    const unrelated = hoverText(await vscode.commands.executeCommand('vscode.executeHoverProvider', uri, untouchedAt));
    assert(unrelated.includes('not checked'));
    assert(!unrelated.includes('Checked target query'));
    assert(!unrelated.includes('Witnesses for'));
    console.log('PASS: real VS Code Extension Host activation, diagnostics, providers, checked command, Unknown, cancellation and stale-result invalidation');
  } finally {
    await vscode.commands.executeCommand('workbench.action.revertAndCloseActiveEditor');
  }
};
