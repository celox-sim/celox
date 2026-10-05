'use strict';
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const {runTests} = require('@vscode/test-electron');

function dumpFailureLogs(profile) {
  let remaining = 1024 * 1024;
  function visit(directory) {
    if (!fs.existsSync(directory) || remaining <= 0) return;
    for (const entry of fs.readdirSync(directory, {withFileTypes: true})) {
      const file = path.join(directory, entry.name);
      if (entry.isDirectory()) visit(file);
      else if (entry.isFile() && /\.(log|txt)$/.test(entry.name) && remaining > 0) {
        const size = fs.statSync(file).size;
        const count = Math.min(size, remaining, 128 * 1024);
        const buffer = Buffer.alloc(count);
        const fd = fs.openSync(file, 'r');
        try {fs.readSync(fd, buffer, 0, count, size-count);} finally {fs.closeSync(fd);}
        console.error(`\n[lydite E2E log: ${path.relative(profile, file)}, last ${count}/${size} bytes]\n${buffer.toString()}`);
        remaining -= count;
      }
    }
  }
  visit(path.join(profile, 'logs'));
}

async function main() {
  const root = path.resolve(__dirname, '../../..');
  const extension = path.resolve(__dirname, '..');
  const temp = fs.mkdtempSync(path.join(os.tmpdir(), 'lydite-extension-host-'));
  const workspace = path.join(temp, 'workspace');
  const profile = path.join(temp, 'profile');
  fs.mkdirSync(workspace);
  fs.mkdirSync(path.join(profile, 'User'), {recursive: true});
  fs.copyFileSync(path.join(root, 'audit/lemma_candidates/counter.lyd'), path.join(workspace, 'counter.lyd'));
  fs.copyFileSync(path.join(root, 'examples/scoped_response.lyd'), path.join(workspace, 'response.lyd'));
  const worker = path.join(root, '../target/release/lydite-editor');
  fs.accessSync(worker, fs.constants.X_OK);
  fs.writeFileSync(path.join(profile, 'User/settings.json'), JSON.stringify({
    'lydite.python': process.env.LYDITE_TEST_PYTHON || 'python3',
    'lydite.server': path.join(root, 'editor/server.py'),
    'lydite.worker': worker,
    'telemetry.telemetryLevel': 'off',
    'update.mode': 'none',
    'extensions.autoUpdate': false
  }));
  try {
    await runTests({
      version: '1.100.3',
      extensionDevelopmentPath: extension,
      extensionTestsPath: path.join(__dirname, 'suite.js'),
      extensionTestsEnv: {LYDITE_EXTENSION_TEST_WORKSPACE: workspace},
      launchArgs: [workspace, '--user-data-dir', profile, '--extensions-dir', path.join(temp, 'extensions'), '--disable-extensions', '--disable-workspace-trust', '--skip-welcome', '--skip-release-notes', '--disable-gpu']
    });
  } catch (error) {
    // Includes Extension Host logs and languageclient output (server stderr).
    try {dumpFailureLogs(profile);}
    catch (logError) {console.error('[lydite log capture failed]', logError);}
    throw error;
  } finally {
    fs.rmSync(temp, {recursive: true, force: true});
  }
}
main().catch(error => {console.error(error); process.exitCode = 1;});
