'use strict';
const assert = require('node:assert/strict');

function hoverMarkdown(items) {
  return (items ?? []).flatMap(h => h.contents).map(c => typeof c === 'string' ? c : c.value).join('\n');
}

function assertExactHoverLines(items, expectedPlain, encodePlain, context) {
  const actual = hoverMarkdown(items).split('\n');
  for (const plain of expectedPlain) {
    const encoded = encodePlain(plain);
    assert(actual.includes(encoded), JSON.stringify({context, expectedPlain: plain, expectedMarkdown: encoded, actualHoverLines: actual}, null, 2));
  }
}

module.exports = {hoverMarkdown, assertExactHoverLines};
