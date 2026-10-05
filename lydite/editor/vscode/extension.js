'use strict';
const vscode = require('vscode');
const { LanguageClient, ErrorAction, CloseAction } = require('vscode-languageclient/node');
let client;

async function activate(context) {
  if (!vscode.workspace.isTrusted) return;
  const config = vscode.workspace.getConfiguration('lydite');
  const server = config.get('server');
  const worker = config.get('worker');
  if (!server || !worker) {
    vscode.window.showErrorMessage('Set lydite.server and lydite.worker to absolute paths, then reload the window. See docs/usage.md.');
    return;
  }
  const output = vscode.window.createOutputChannel('lydite proof results');
  const watcher = vscode.workspace.createFileSystemWatcher('**/*.{lyd,json}');
  context.subscriptions.push(output, watcher);
  // Track unsaved JSON models too; the server publishes diagnostics only for .lyd.
  let prove;
  client = new LanguageClient('lydite', 'lydite', {
    command: config.get('python'), args: [server, '--worker', worker]
  }, {documentSelector: [{scheme: 'file', language: 'lydite'}, {scheme: 'file', language: 'json'}],
      synchronize: {fileEvents: watcher},
      errorHandler: {error: () => ({action: ErrorAction.Shutdown}), closed: () => ({action: CloseAction.DoNotRestart})},
      middleware: {executeCommand: (command, args, next) => command === 'lydite.prove' ? prove(args[0]) : next(command, args)}});
  await client.start();
  client.onNotification('lydite/statusChanged', () => {});
  const proofStarted = new vscode.EventEmitter();
  context.subscriptions.push(proofStarted);
  client.onNotification('lydite/proofStarted', event => proofStarted.fire(event));
  const checkProof = (options, token) => {
    const params = {command: 'lydite.prove', arguments: [options]};
    // The string-method overload treats an explicit undefined token as a
    // second positional parameter, producing [params, null] on the wire.
    return token === undefined ? client.sendRequest('workspace/executeCommand', params) : client.sendRequest('workspace/executeCommand', params, token);
  };
  // Own the command so CodeLens and the palette both get cancellable progress.
  prove = async (options) => {
    if (!options) {
      const editor = vscode.window.activeTextEditor;
      if (!editor || editor.document.languageId !== 'lydite') return;
      const lenses = await client.sendRequest('textDocument/codeLens', {textDocument: {uri: editor.document.uri.toString()}});
      const choices = (lenses || []).map(l => ({label: l.command.title, description: l.command.arguments[0].program, options: l.command.arguments[0]}));
      const selected = await vscode.window.showQuickPick(choices, {placeHolder: 'Choose an explicit proof target or program prefix'});
      if (!selected) return;
      options = selected.options;
    }
    // Optional branch selection is explicit; never silently prove a different branch.
    options = {...options};
    if (!Object.prototype.hasOwnProperty.call(options, 'branch')) {
      const branch = await vscode.window.showInputBox({prompt: 'Branch index (zero-based); leave empty for a uniquely matching target', validateInput: x => x === '' || /^\d+$/.test(x) ? undefined : 'Enter a nonnegative integer'});
      if (branch === undefined) return;
      if (branch !== '') options.branch = Number(branch);
    }
    return vscode.window.withProgress({location: vscode.ProgressLocation.Notification, title: 'lydite: checking fresh proof', cancellable: true}, async (_, token) => {
      try {
        const result = await checkProof(options, token);
        output.clear();
        output.appendLine('Snapshot diagnostics only; no cached result is proof authority.');
        output.appendLine('Counterexamples may concern an auxiliary lemma or guard, not the target or reset reachability.');
        output.appendLine(JSON.stringify({documentVersion: result.documentVersion, identity: result.identity, requestIdentity: result.requestIdentity, diagnostics: result.diagnostics, verification: result.verification, scope: result.proof?.scope, branch: result.proof?.branch, matchingBranches: result.proof?.matching_branches, error: result.proof?.error, query: result.proof?.query, queryDisplayTruncated: result.proof?.query_display_truncated, targets: result.proof?.reports.map(r => r.lemma_candidates), witnesses: result.witnesses}, null, 2));
        output.show(true);
        return result;
      } catch (error) {
        if (!token.isCancellationRequested) vscode.window.showErrorMessage(`lydite: ${error.message}`);
      }
    });
  };
  context.subscriptions.push(vscode.commands.registerCommand('lydite.check', () => prove()));
  const associate = async (clear) => {
    const editor = vscode.window.activeTextEditor;
    if (!editor || editor.document.languageId !== 'lydite') return;
    let baseUri = null;
    if (!clear) {
      const picked = await vscode.window.showOpenDialog({canSelectMany: false, filters: {'lydite design': ['lyd', 'json']}, openLabel: 'Associate model'});
      if (!picked) return;
      baseUri = picked[0].toString();
    }
    await client.sendRequest('workspace/executeCommand', {command: 'lydite.setBase', arguments: [{uri: editor.document.uri.toString(), baseUri}]});
  };
  context.subscriptions.push(vscode.commands.registerCommand('lydite.associate', () => associate(false)));
  context.subscriptions.push(vscode.commands.registerCommand('lydite.clearBase', () => associate(true)));
  // Diagnostic-only extension API for clients that supply their own cancellation UI.
  return {checkProof, onProofStarted: proofStarted.event};
}
async function deactivate() { if (client) await client.dispose(); }
module.exports = {activate, deactivate};
