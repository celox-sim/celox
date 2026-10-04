'use strict';
const assert = require('node:assert/strict');
const {assertExactHoverLines} = require('./hover-contract');
// Literal fixtures for the pinned VS Code plaintext-to-Markdown representation.
// Actual Extension Host tests obtain the encoding from vscode.MarkdownString.
const status = 'Lemma step: proved; target closed.';
const identity = 'Proof request identity: aabb';
const version = 'Document version: 3';
const encoded = new Map([
  [status, 'Lemma&nbsp;step:&nbsp;proved;&nbsp;target&nbsp;closed.'],
  [identity, 'Proof&nbsp;request&nbsp;identity:&nbsp;aabb'],
  [version, 'Document&nbsp;version:&nbsp;3']
]);
const encode = value => encoded.get(value);
const hover = value => [{contents: [{value}]}];
const correct = [...encoded.values()].join('\n\n');
assertExactHoverLines(hover(correct), [status, identity, version], encode, 'correct');
for (const wrong of [
  correct.replace(encoded.get(status), 'Previously&nbsp;declared&nbsp;steps&nbsp;(not&nbsp;assumed&nbsp;proved)'),
  correct.replace('aabb', 'ccdd'),
  correct.replace('version:&nbsp;3', 'version:&nbsp;2')
]) {
  assert.throws(() => assertExactHoverLines(hover(wrong), [status, identity, version], encode, 'regression'), error => {
    assert(error.message.includes('expectedMarkdown'));
    assert(error.message.includes('actualHoverLines'));
    return true;
  });
}
console.log('Exact hover contract rejects static proved text, wrong identity and stale version');
