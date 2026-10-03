'use strict';
const vscode = require('vscode');
const { LanguageClient } = require('vscode-languageclient/node');
let client;

async function activate(context) {
  if (!vscode.workspace.isTrusted) return;
  const config = vscode.workspace.getConfiguration('hwverify');
  const server = config.get('server');
  const worker = config.get('worker');
  if (!server || !worker) {
    vscode.window.showErrorMessage('Set hwverify.server and hwverify.worker to absolute paths, then reload the window. See docs/usage.md.');
    return;
  }
  const output = vscode.window.createOutputChannel('hwverify proof results');
  const watcher = vscode.workspace.createFileSystemWatcher('**/*.{hwv,json}');
  context.subscriptions.push(output, watcher);
  // Track unsaved JSON models too; the server publishes diagnostics only for .hwv.
  let prove;
  client = new LanguageClient('hwverify', 'hwverify', {
    command: config.get('python'), args: [server, '--worker', worker]
  }, {documentSelector: [{scheme: 'file', language: 'hwverify'}, {scheme: 'file', language: 'json'}],
      synchronize: {fileEvents: watcher}, middleware: {executeCommand: (command, args, next) => command === 'hwverify.prove' ? prove(args[0]) : next(command, args)}});
  await client.start();
  client.onNotification('hwverify/statusChanged', () => {});
  // Own the command so CodeLens and the palette both get cancellable progress.
  prove = async (options) => {
    if (!options) {
      const editor = vscode.window.activeTextEditor;
      if (!editor || editor.document.languageId !== 'hwverify') return;
      const lenses = await client.sendRequest('textDocument/codeLens', {textDocument: {uri: editor.document.uri.toString()}});
      const choices = (lenses || []).map(l => ({label: l.command.title, description: l.command.arguments[0].program, options: l.command.arguments[0]}));
      const selected = await vscode.window.showQuickPick(choices, {placeHolder: 'Choose an explicit proof target or program prefix'});
      if (!selected) return;
      options = selected.options;
    }
    // Optional branch selection is explicit; never silently prove a different branch.
    const branch = await vscode.window.showInputBox({prompt: 'Branch index (zero-based); leave empty for a uniquely matching target', validateInput: x => x === '' || /^\d+$/.test(x) ? undefined : 'Enter a nonnegative integer'});
    if (branch === undefined) return;
    options = {...options};
    if (branch !== '') options.branch = Number(branch);
    await vscode.window.withProgress({location: vscode.ProgressLocation.Notification, title: 'hwverify: checking fresh proof', cancellable: true}, async (_, token) => {
      try {
        const result = await client.sendRequest('workspace/executeCommand', {command: 'hwverify.prove', arguments: [options]}, token);
        output.clear();
        output.appendLine('Snapshot diagnostics only; no cached result is proof authority.');
        output.appendLine('Counterexamples may concern an auxiliary lemma or guard, not the target or reset reachability.');
        output.appendLine(JSON.stringify({documentVersion: result.documentVersion, identity: result.identity, requestIdentity: result.requestIdentity, diagnostics: result.diagnostics, scope: result.proof?.scope, branch: result.proof?.branch, matchingBranches: result.proof?.matching_branches, error: result.proof?.error, query: result.proof?.query, queryDisplayTruncated: result.proof?.query_display_truncated, targets: result.proof?.reports.map(r => r.lemma_candidates), witnesses: result.witnesses}, null, 2));
        output.show(true);
      } catch (error) {
        if (!token.isCancellationRequested) vscode.window.showErrorMessage(`hwverify: ${error.message}`);
      }
    });
  };
  context.subscriptions.push(vscode.commands.registerCommand('hwverify.check', () => prove()));
  const associate = async (clear) => {
    const editor = vscode.window.activeTextEditor;
    if (!editor || editor.document.languageId !== 'hwverify') return;
    let baseUri = null;
    if (!clear) {
      const picked = await vscode.window.showOpenDialog({canSelectMany: false, filters: {'hwverify design': ['hwv', 'json']}, openLabel: 'Associate model'});
      if (!picked) return;
      baseUri = picked[0].toString();
    }
    await client.sendRequest('workspace/executeCommand', {command: 'hwverify.setBase', arguments: [{uri: editor.document.uri.toString(), baseUri}]});
  };
  context.subscriptions.push(vscode.commands.registerCommand('hwverify.associate', () => associate(false)));
  context.subscriptions.push(vscode.commands.registerCommand('hwverify.clearBase', () => associate(true)));
}
async function deactivate() { if (client) await client.dispose(); }
module.exports = {activate, deactivate};
