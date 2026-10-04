'use strict';
// Exercise extension wiring without pretending to test VS Code rendering.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const path = require('node:path');

async function run() {
  const commands = new Map();
  const calls = [];
  let instance;
  let disposed = false;
  let branch = '';
  const token = {isCancellationRequested: false};
  const options = {uri: 'file:///counter.hwv', program: 'counter_step', step: 'step'};
  const vscode = {
    workspace: {isTrusted: true, getConfiguration: () => ({get: k => ({server: '/repo/editor/server.py', worker: '/repo/target/release/hwverify-editor', python: 'python3'})[k]}), createFileSystemWatcher: () => ({dispose() {}})},
    commands: {registerCommand: (name, fn) => {assert(!commands.has(name)); commands.set(name, fn); return {dispose() {commands.delete(name);}};}},
    EventEmitter: class {
      constructor() {this.listeners = new Set(); this.event = listener => {this.listeners.add(listener); return {dispose: () => this.listeners.delete(listener)};};}
      fire(value) {for (const listener of this.listeners) listener(value);}
      dispose() {this.listeners.clear();}
    },
    ProgressLocation: {Notification: 15},
    window: {
      activeTextEditor: {document: {languageId: 'hwverify', uri: {toString: () => options.uri}}},
      createOutputChannel: () => ({clear() {}, appendLine: x => calls.push(['output', x]), show() {}, dispose() {}}),
      showErrorMessage: x => {throw new Error(x);},
      showQuickPick: async items => items[0],
      showInputBox: async () => branch,
      showOpenDialog: async () => [{toString: () => 'file:///base.hwv'}],
      withProgress: async (opts, callback) => {assert.equal(opts.cancellable, true); return callback({}, token);}
    }
  };
  class LanguageClient {
    constructor(id, name, server, client) {instance = this; this.options = client; assert.equal(server.command, 'python3'); assert.equal(server.args[2], '/repo/target/release/hwverify-editor');}
    async start() {
      // Model the actual languageclient automatic executeCommand registration.
      vscode.commands.registerCommand('hwverify.prove', (...args) => this.options.middleware.executeCommand('hwverify.prove', args, () => {throw new Error('proof bypassed progress');}));
    }
    onNotification(method, handler) {this.notifications ||= new Map(); this.notifications.set(method, handler);}
    async sendRequest(method, params, cancel) {
      calls.push([method, params, cancel, arguments.length]);
      if (method === 'textDocument/codeLens') return [{command: {title: 'Check prefix', arguments: [options]}}];
      return {diagnosticOnly: true};
    }
    async dispose() {disposed = true;}
  }
  const sandbox = {module: {exports: {}}, require: name => {
    if (name === 'vscode') return vscode;
    if (name === 'vscode-languageclient/node') return {LanguageClient, ErrorAction: {Shutdown: 2}, CloseAction: {DoNotRestart: 1}};
    throw new Error(name);
  }};
  vm.runInNewContext(fs.readFileSync(path.join(__dirname, 'extension.js'), 'utf8'), sandbox);
  const extension = sandbox.module.exports;
  const api = await extension.activate({subscriptions: []});
  assert.equal(typeof api.checkProof, 'function');
  let started;
  const subscription = api.onProofStarted(event => {started = event;});
  instance.notifications.get('hwverify/proofStarted')({phase: 'worker_started'});
  assert.equal(started.phase, 'worker_started');
  subscription.dispose();
  assert(instance.options.documentSelector.some(x => x.language === 'json'));
  assert.equal((await commands.get('hwverify.check')()).diagnosticOnly, true);
  let proof = calls.find(x => x[0] === 'workspace/executeCommand' && x[1].command === 'hwverify.prove');
  assert.equal(proof[2], token);
  assert.equal(proof[1].arguments[0].step, 'step');
  branch = '2';
  await commands.get('hwverify.prove')(options);
  proof = calls.filter(x => x[0] === 'workspace/executeCommand' && x[1].command === 'hwverify.prove').at(-1);
  assert.equal(proof[1].arguments[0].branch, 2);
  assert.equal(options.branch, undefined);
  branch = undefined;
  assert.equal((await commands.get('hwverify.prove')({...options, branch: 0})).diagnosticOnly, true);
  assert.equal((await api.checkProof(options, token)).diagnosticOnly, true);
  assert.equal(calls.filter(x => x[0] === 'workspace/executeCommand').at(-1)[2], token);
  await api.checkProof(options);
  assert.equal(calls.filter(x => x[0] === 'workspace/executeCommand').at(-1)[3], 2);
  assert.equal(instance.options.errorHandler.closed().action, 1);
  await commands.get('hwverify.associate')();
  assert(calls.some(x => x[1]?.arguments?.[0]?.baseUri === 'file:///base.hwv'));
  await commands.get('hwverify.clearBase')();
  assert(calls.some(x => x[1]?.arguments?.[0]?.baseUri === null));
  await extension.deactivate();
  assert(disposed);
  vscode.workspace.isTrusted = false;
  const before = commands.size;
  await extension.activate({subscriptions: []});
  assert.equal(commands.size, before);
  console.log('VS Code client command, cancellation-token, association and lifecycle wiring passed');
}
run().catch(error => {console.error(error); process.exitCode = 1;});
