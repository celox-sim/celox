'use strict';
// Tokenize real and synthetic .lyd sources with the same engine VS Code uses.
// package.json is the source of truth for which file ships as the grammar.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vscodeTextmate = require('vscode-textmate');
const oniguruma = require('vscode-oniguruma');

const extension = path.resolve(__dirname, '..');
const lydite = path.resolve(extension, '../..');
const manifest = require(path.join(extension, 'package.json'));
const contributions = (manifest.contributes.grammars || []).filter(grammar => grammar.language === 'lydite');
assert.equal(contributions.length, 1, 'the manifest must contribute exactly one lydite grammar');
assert.equal(contributions[0].scopeName, 'source.lydite');
assert(manifest.contributes.languages.some(language => language.id === 'lydite' && language.extensions.includes('.lyd')));
const grammarFile = path.resolve(extension, contributions[0].path);
assert(fs.existsSync(grammarFile), `missing grammar file ${grammarFile}`);
const rawGrammar = vscodeTextmate.parseRawGrammar(fs.readFileSync(grammarFile, 'utf8'), grammarFile);
assert.equal(rawGrammar.scopeName, 'source.lydite');
assert.deepEqual(rawGrammar.fileTypes, ['.lyd']);

// Every scope this grammar introduces is language-specific, and every include resolves.
const repository = new Set(Object.keys(rawGrammar.repository));
const scopes = [];
function visit(node) {
  if (Array.isArray(node)) return node.forEach(visit);
  if (!node || typeof node !== 'object') return;
  if (typeof node.include === 'string' && node.include.startsWith('#')) {
    assert(repository.has(node.include.slice(1)), `unresolved include ${node.include}`);
  }
  for (const [key, value] of Object.entries(node)) {
    if (key === 'name' && typeof value === 'string') scopes.push(value);
    else visit(value);
  }
}
visit(rawGrammar.patterns);
visit(rawGrammar.repository);
assert(scopes.length > 0);
for (const scope of scopes) assert(scope.endsWith('.lydite'), `scope ${scope} must stay language-specific`);

// The callable operations are declared once in the syntax crate; keep the two lists equal.
const syntaxSource = fs.readFileSync(path.join(lydite, '../crates/lydite-syntax/src/lib.rs'), 'utf8');
const parserList = /if !\[([\s\S]*?)\]\s*\.contains\(&op\)/.exec(syntaxSource);
assert(parserList, 'crates/lydite-syntax/src/lib.rs must keep the builtin operation list');
const parserBuiltins = new Set([...parserList[1].matchAll(/"([a-z_]+)"/g)].map(match => match[1]));
const grammarBuiltins = /([a-z_]+(?:\|[a-z_]+)+)/.exec(rawGrammar.repository['builtin-operations'].patterns[0].match);
assert(grammarBuiltins, 'the builtin operation rule must keep one alternation list');
assert.deepEqual([...grammarBuiltins[1].split('|')].sort(), [...parserBuiltins].sort(), 'grammar builtins must match the parser');

async function loadGrammar() {
  const wasm = fs.readFileSync(require.resolve('vscode-oniguruma/release/onig.wasm'));
  await oniguruma.loadWASM(wasm.buffer.slice(wasm.byteOffset, wasm.byteOffset + wasm.byteLength));
  const registry = new vscodeTextmate.Registry({
    onigLib: Promise.resolve({
      createOnigString: oniguruma.createOnigString,
      createOnigScanner: oniguruma.createOnigScanner
    })
  });
  return registry.addGrammar(rawGrammar);
}

function tokenize(grammar, source) {
  const lines = source.split('\n');
  const tokens = [];
  let ruleStack = vscodeTextmate.INITIAL;
  for (const line of lines) {
    const result = grammar.tokenizeLine(line, ruleStack);
    assert.equal(result.tokens[0].startIndex, 0, 'lines are tokenized from their first byte');
    ruleStack = result.ruleStack;
    tokens.push(result.tokens);
  }
  return {lines, tokens, ruleStack};
}

function columnOf(line, needle, occurrence) {
  const escape = value => value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const pattern = `${/^[A-Za-z0-9_]/.test(needle) ? '\\b' : ''}${escape(needle)}${/[A-Za-z0-9_]$/.test(needle) ? '\\b' : ''}`;
  const matches = [...line.matchAll(new RegExp(pattern, 'g'))];
  assert(matches.length >= occurrence, `no occurrence ${occurrence} of ${JSON.stringify(needle)} in ${JSON.stringify(line)}`);
  return matches[occurrence - 1].index;
}

function innermostScope(tokenized, lineText, needle, occurrence) {
  const lineIndex = tokenized.lines.indexOf(lineText);
  assert.notEqual(lineIndex, -1, `missing fixture line ${JSON.stringify(lineText)}`);
  assert.equal(lineIndex, tokenized.lines.lastIndexOf(lineText), `duplicate fixture line ${JSON.stringify(lineText)}`);
  const column = columnOf(lineText, needle, occurrence);
  const token = tokenized.tokens[lineIndex].find(candidate => candidate.startIndex <= column && column < candidate.endIndex);
  assert.equal(lineText.slice(token.startIndex, token.endIndex), needle, `${needle} must be one token`);
  return token.scopes[token.scopes.length - 1];
}

const samples = {};
samples.design = `design "Counter" {
  input rst: bool;
  reset_input rst;
  spec {
    state x: bv<8>;
    state mem: mem<2, 8>;
    reset { x = 0u8; mem = i.mem; }
    next { x = if i.stall { s.x } else { s.x + 1u8 }; }
    wires { y = read(s.mem, s.address); }
    outputs { can_step = x != 255u8; }
  }
  binding impl.x == spec.x;
  commit commit;
  can_step can_step;
  progress { enabled true; rank 0u2; }
  /* comments hold checked claims */
  proof {
    forall word: bv<8>;
    mode  shared_query;
    target counter_step {
      rhs spec.x';
      lemma step {
        context true;
        guard pre;
        claim impl.x' == spec.x';
      }
      use done: step(context: pre);
      result done;
    }
  }
}`;
samples.specification = `specification "Accepted requests complete within two nonreset steps"
spec Counter(output count: bv<4>) {
  state value: bv<4>;
  init value == 0u4;
  invariant count == value;
  expectation permitted {
    any {
      all { expect amount <= 2u4; }
    }
  }
  operation advance {
    expect permitted;
    expect count' == value';
  }
  example small {
    quantifiers {}
    expect positive;
    initial { count = 0u4; }
    trace { advance { inputs { amount = 2u4; } observe { count = 2u4; } } }
  }
  contract {
    parameters { data: mem<8, 8>; }
    parameter extra: bool;
    pre true;
    invariant s.value == value;
    split { pc = range(s.value, 0, 2); }
  }
}
composition Completed(output count: bv<4>) {
  use counter: Counter(flag: bool);
}
implementation {
  composition Completed;
  reset_input rst;
  inputs { rst: bool; }
  state q: bool;
  operations { advance = w.complete; }
  binding {
    bind counter { value = s.count; }
    output count = s.count;
  }
  responses {
    request_done {
      operation advance;
      accept w.accept;
      pending s.busy;
      bound 2;
      cover_depth 2;
      assume !i.stall;
    }
  }
}`;

samples.expectations = `specification "Named expectations with next-state aliases"
component Counter {
  state value: bv<4>;
  init value == 0u4;
  invariant o.count == s.value;
  steps { add = n.value == s.value + i.amount; }
  structure { no_comb_path isolated { from i.amount; to o.value; } }
  state pre: bool;
  invariant s.pre == false;
  operation advance { expect n.value == s.value + 1u4; }
  example small {
    expect positive;
    initial { value = 0u4; }
    trace { advance { inputs { amount = 2u4; } observe { no.count = 2u4; } } }
  }
  example quantified {
    exists amount: bv<4>;
    execution not_exists;
    initial { value = 0u4; }
    trace { add(flag: bool) => true; }
  }
}`;

// An unfinished type must not leak, and contextual words stay variables when
// they are used as names or as bare conditions.
samples.recovery = `design "Recovery" {
  input half: bv<8
  input whole: bool;
  input forall: bool;
  input bool: bool;
  inputs { state: bool; }
  state design: bool;
  binding true
    && impl.x == spec.x;
  reset {
    design = false;
  }
  next { x = if forall { i.forall } else { s.x }; }
}`;

// A header being typed keeps its string scope without leaking to the next line.
samples.header = `design "Unfinished
input rst: bool;`;

// An unbraced document may keep its first entry on the header line.
samples.inline = `specification "Inline" component Counter { init true; invariant true; steps { tick = true; } }`;

samples.review = `specification "Review" {
  component Calls {
    input use: bool;
    input pre: bool;
    input goal: bool;
    input lhs: bool;
    input rhs: bool;
    init pre && goal && lhs && rhs;
    invariant i.use == use;
    use child: Child(flag: use);
    example calls {
      trace {
        component() => true;
        state(flag: true) => true;
        use(flag: true) => true;
        add(amount: if flag { add(bv, read(m, index)) } else { bv(4, 0) }) => true;
        add(amount: (read(m, index))) => true;
        add(amount:
          add(read(m, index), 1u4)
        ) => true;
        component(
          flag: true
        ) => true;
      }
    }
    invariant read
      (s.mem, s.address) == word;
    invariant custom
      (s.value) == word;
    outputs { read = s.value; }
  }
}`;

// Nested expression braces must not end the surrounding proof context.
samples.proofReview = `lemmas "Proof review" {
  target check {
    rhs spec.x';
    lemma step {
      context true;
      guard pre && goal && lhs == rhs;
      claim if pre { lhs } else { rhs };
    }
    use done: step(context: pre);
    result done;
  }
}`;

samples.proofExit = `design "Proof exit" {
  proof { target check { rhs spec.x'; lemma step { context true; guard pre; claim goal; } } }
  spec { state pre: bool; state goal: bool; init pre == goal; }
}`;

// [sample, line, token, occurrence, expected innermost scope]
const expectations = [
  ['review', '    input use: bool;', 'use', 1, 'variable.other.lydite'],
  ['review', '    invariant i.use == use;', 'use', 1, 'variable.other.lydite'],
  ['review', '    invariant i.use == use;', 'use', 2, 'variable.other.lydite'],
  ['review', '    use child: Child(flag: use);', 'use', 1, 'keyword.control.lydite'],
  ['review', '    use child: Child(flag: use);', 'use', 2, 'variable.other.lydite'],
  ['review', '        component() => true;', 'component', 1, 'entity.name.function.lydite'],
  ['review', '        state(flag: true) => true;', 'state', 1, 'entity.name.function.lydite'],
  ['review', '        use(flag: true) => true;', 'use', 1, 'entity.name.function.lydite'],
  ['review', '        add(amount: if flag { add(bv, read(m, index)) } else { bv(4, 0) }) => true;', 'add', 1, 'entity.name.function.lydite'],
  ['review', '        add(amount: if flag { add(bv, read(m, index)) } else { bv(4, 0) }) => true;', 'add', 2, 'support.function.builtin.lydite'],
  ['review', '        add(amount: if flag { add(bv, read(m, index)) } else { bv(4, 0) }) => true;', 'read', 1, 'support.function.builtin.lydite'],
  ['review', '        add(amount: (read(m, index))) => true;', 'add', 1, 'entity.name.function.lydite'],
  ['review', '        add(amount:', 'add', 1, 'entity.name.function.lydite'],
  ['review', '          add(read(m, index), 1u4)', 'add', 1, 'support.function.builtin.lydite'],
  ['review', '        component(', 'component', 1, 'entity.name.function.lydite'],
  // A name with its opening parenthesis on a later line falls back locally.
  ['review', '    invariant read', 'read', 1, 'variable.other.lydite'],
  ['review', '    invariant custom', 'custom', 1, 'variable.other.lydite'],
  ['review', '      (s.mem, s.address) == word;', 's', 1, 'variable.language.namespace.lydite'],
  ['review', '    outputs { read = s.value; }', 'read', 1, 'variable.other.lydite'],
  ...['pre', 'goal', 'lhs', 'rhs'].flatMap(word => [
    ['review', `    input ${word}: bool;`, word, 1, 'variable.other.lydite'],
    ['review', '    init pre && goal && lhs && rhs;', word, 1, 'variable.other.lydite'],
    ['proofReview', '      guard pre && goal && lhs == rhs;', word, 1, 'variable.language.lydite']
  ]),
  ...['pre', 'lhs', 'rhs'].map(word => ['proofReview', '      claim if pre { lhs } else { rhs };', word, 1, 'variable.language.lydite']),
  ['proofReview', '    use done: step(context: pre);', 'pre', 1, 'variable.language.lydite'],
  ['proofExit', "  proof { target check { rhs spec.x'; lemma step { context true; guard pre; claim goal; } } }", 'pre', 1, 'variable.language.lydite'],
  ['proofExit', '  spec { state pre: bool; state goal: bool; init pre == goal; }', 'pre', 2, 'variable.other.lydite'],
  ['proofExit', '  spec { state pre: bool; state goal: bool; init pre == goal; }', 'goal', 2, 'variable.other.lydite'],
  ['design', 'design "Counter" {', 'design', 1, 'keyword.other.document.lydite'],
  ['design', 'design "Counter" {', 'Counter', 1, 'string.quoted.double.lydite'],
  ['design', 'design "Counter" {', '{', 1, 'punctuation.section.block.begin.lydite'],
  ['design', '  input rst: bool;', 'input', 1, 'storage.modifier.lydite'],
  ['design', '  input rst: bool;', 'bool', 1, 'storage.type.lydite'],
  ['design', '  reset_input rst;', 'reset_input', 1, 'keyword.other.lydite'],
  ['design', '  spec {', 'spec', 1, 'keyword.other.lydite'],
  ['design', '    state x: bv<8>;', 'state', 1, 'storage.modifier.lydite'],
  ['design', '    state x: bv<8>;', 'bv', 1, 'storage.type.lydite'],
  ['design', '    state x: bv<8>;', '8', 1, 'constant.numeric.integer.lydite'],
  ['design', '    state x: bv<8>;', '<', 1, 'punctuation.definition.typeparameters.begin.lydite'],
  ['design', '    state mem: mem<2, 8>;', 'mem', 2, 'storage.type.lydite'],
  ['design', '    reset { x = 0u8; mem = i.mem; }', '0u8', 1, 'constant.numeric.bv.lydite'],
  ['design', '    reset { x = 0u8; mem = i.mem; }', 'i', 1, 'variable.language.namespace.lydite'],
  ['design', '    next { x = if i.stall { s.x } else { s.x + 1u8 }; }', 'if', 1, 'keyword.control.lydite'],
  ['design', '    next { x = if i.stall { s.x } else { s.x + 1u8 }; }', 'else', 1, 'keyword.control.lydite'],
  ['design', '    next { x = if i.stall { s.x } else { s.x + 1u8 }; }', 's', 1, 'variable.language.namespace.lydite'],
  // A dotted field stays a variable even when a block follows it.
  ['design', '    next { x = if i.stall { s.x } else { s.x + 1u8 }; }', 'stall', 1, 'variable.other.lydite'],
  ['design', '    wires { y = read(s.mem, s.address); }', 'read', 1, 'support.function.builtin.lydite'],
  ['design', '    outputs { can_step = x != 255u8; }', 'can_step', 1, 'variable.other.lydite'],
  ['design', '    outputs { can_step = x != 255u8; }', '!=', 1, 'keyword.operator.lydite'],
  ['design', '  binding impl.x == spec.x;', 'binding', 1, 'keyword.other.lydite'],
  ['design', '  binding impl.x == spec.x;', 'impl', 1, 'variable.language.namespace.lydite'],
  ['design', '  commit commit;', 'commit', 1, 'keyword.other.lydite'],
  ['design', '  commit commit;', 'commit', 2, 'variable.other.lydite'],
  ['design', '  can_step can_step;', 'can_step', 1, 'keyword.other.lydite'],
  ['design', '  can_step can_step;', 'can_step', 2, 'variable.other.lydite'],
  ['design', '  progress { enabled true; rank 0u2; }', 'rank', 1, 'keyword.other.lydite'],
  ['design', '  progress { enabled true; rank 0u2; }', 'true', 1, 'constant.language.lydite'],
  ['design', '  /* comments hold checked claims */', '/*', 1, 'punctuation.definition.comment.begin.lydite'],
  ['design', '  /* comments hold checked claims */', ' comments hold checked claims ', 1, 'comment.block.lydite'],
  ['design', '  proof {', 'proof', 1, 'keyword.other.lydite'],
  ['design', '    forall word: bv<8>;', 'forall', 1, 'keyword.other.lydite'],
  ['design', '    mode  shared_query;', 'mode', 1, 'keyword.other.lydite'],
  ['design', '    mode  shared_query;', 'shared_query', 1, 'constant.language.lydite'],
  ['design', '    target counter_step {', 'counter_step', 1, 'entity.name.section.lydite'],
  ['design', "      rhs spec.x';", 'rhs', 1, 'keyword.other.lydite'],
  ['design', "      rhs spec.x';", "'", 1, 'punctuation.definition.prime.lydite'],
  ['design', '        guard pre;', 'pre', 1, 'variable.language.lydite'],
  ['design', "        claim impl.x' == spec.x';", 'claim', 1, 'keyword.other.lydite'],
  ['design', "        claim impl.x' == spec.x';", 'spec', 1, 'variable.language.namespace.lydite'],
  ['design', '      use done: step(context: pre);', 'use', 1, 'keyword.control.lydite'],
  ['design', '      use done: step(context: pre);', 'step', 1, 'entity.name.function.lydite'],
  // A lemma use connection named `context` is not the lemma property.
  ['design', '      use done: step(context: pre);', 'context', 1, 'variable.other.lydite'],
  ['design', '      use done: step(context: pre);', 'pre', 1, 'variable.language.lydite'],
  ['design', '      result done;', 'result', 1, 'keyword.other.lydite'],
  ['specification', 'specification "Accepted requests complete within two nonreset steps"', 'specification', 1, 'keyword.other.document.lydite'],
  ['specification', 'spec Counter(output count: bv<4>) {', 'Counter', 1, 'entity.name.function.lydite'],
  // Ports carry a direction even though they are not entry-start declarations.
  ['specification', 'spec Counter(output count: bv<4>) {', 'output', 1, 'storage.modifier.lydite'],
  ['specification', '  state value: bv<4>;', 'state', 1, 'storage.modifier.lydite'],
  ['specification', '  init value == 0u4;', 'init', 1, 'keyword.other.lydite'],
  ['specification', '  invariant count == value;', 'invariant', 1, 'keyword.other.lydite'],
  ['specification', '  expectation permitted {', 'expectation', 1, 'keyword.other.lydite'],
  ['specification', '  expectation permitted {', 'permitted', 1, 'entity.name.section.lydite'],
  ['specification', '    any {', 'any', 1, 'keyword.other.lydite'],
  ['specification', '      all { expect amount <= 2u4; }', 'all', 1, 'keyword.other.lydite'],
  ['specification', '      all { expect amount <= 2u4; }', 'expect', 1, 'keyword.other.lydite'],
  ['specification', '      all { expect amount <= 2u4; }', '2u4', 1, 'constant.numeric.bv.lydite'],
  ['specification', '  operation advance {', 'operation', 1, 'keyword.other.lydite'],
  ['specification', '    expect positive;', 'positive', 1, 'constant.language.lydite'],
  ['specification', '    initial { count = 0u4; }', 'initial', 1, 'keyword.other.lydite'],
  ['specification', '    trace { advance { inputs { amount = 2u4; } observe { count = 2u4; } } }', 'trace', 1, 'keyword.other.lydite'],
  ['specification', '    trace { advance { inputs { amount = 2u4; } observe { count = 2u4; } } }', 'inputs', 1, 'keyword.other.lydite'],
  ['specification', '    trace { advance { inputs { amount = 2u4; } observe { count = 2u4; } } }', 'observe', 1, 'keyword.other.lydite'],
  ['specification', '    parameters { data: mem<8, 8>; }', 'parameters', 1, 'keyword.other.lydite'],
  ['specification', '    parameter extra: bool;', 'parameter', 1, 'storage.modifier.lydite'],
  ['specification', '    quantifiers {}', 'quantifiers', 1, 'keyword.other.lydite'],
  ['specification', '    pre true;', 'pre', 1, 'keyword.other.lydite'],
  ['specification', '    split { pc = range(s.value, 0, 2); }', 'split', 1, 'keyword.other.lydite'],
  ['specification', '    split { pc = range(s.value, 0, 2); }', 'range', 1, 'support.function.builtin.lydite'],
  ['specification', '  use counter: Counter(flag: bool);', 'use', 1, 'keyword.control.lydite'],
  ['specification', '  composition Completed;', 'composition', 1, 'keyword.other.lydite'],
  ['specification', '  reset_input rst;', 'reset_input', 1, 'keyword.other.lydite'],
  ['specification', '    bind counter { value = s.count; }', 'bind', 1, 'keyword.other.lydite'],
  ['specification', '    output count = s.count;', 'output', 1, 'storage.modifier.lydite'],
  ['specification', '    request_done {', 'request_done', 1, 'entity.name.section.lydite'],
  ['specification', '      accept w.accept;', 'accept', 1, 'keyword.other.lydite'],
  ['specification', '      pending s.busy;', 'pending', 1, 'keyword.other.lydite'],
  ['specification', '      bound 2;', 'bound', 1, 'keyword.other.lydite'],
  ['specification', '      cover_depth 2;', 'cover_depth', 1, 'keyword.other.lydite'],
  ['specification', '      assume !i.stall;', 'assume', 1, 'keyword.other.lydite'],
  // Every dotted reference names its scope, so the qualifier is structural.
  ['expectations', 'component Counter {', 'component', 1, 'keyword.other.lydite'],
  ['expectations', '  invariant o.count == s.value;', 'o', 1, 'variable.language.namespace.lydite'],
  ['expectations', '  invariant o.count == s.value;', 'count', 1, 'variable.other.lydite'],
  ['expectations', '  invariant o.count == s.value;', 's', 1, 'variable.language.namespace.lydite'],
  ['expectations', '  steps { add = n.value == s.value + i.amount; }', 'steps', 1, 'keyword.other.lydite'],
  ['expectations', '  steps { add = n.value == s.value + i.amount; }', 'n', 1, 'variable.language.namespace.lydite'],
  ['expectations', '  steps { add = n.value == s.value + i.amount; }', 'i', 1, 'variable.language.namespace.lydite'],
  ['expectations', '  structure { no_comb_path isolated { from i.amount; to o.value; } }', 'no_comb_path', 1, 'keyword.other.lydite'],
  ['expectations', '  structure { no_comb_path isolated { from i.amount; to o.value; } }', 'isolated', 1, 'entity.name.section.lydite'],
  ['expectations', '  operation advance { expect n.value == s.value + 1u4; }', 'expect', 1, 'keyword.other.lydite'],
  ['expectations', '    trace { advance { inputs { amount = 2u4; } observe { no.count = 2u4; } } }', 'observe', 1, 'keyword.other.lydite'],
  ['expectations', '    trace { advance { inputs { amount = 2u4; } observe { no.count = 2u4; } } }', 'no', 1, 'variable.language.namespace.lydite'],
  ['expectations', '    exists amount: bv<4>;', 'exists', 1, 'keyword.other.lydite'],
  ['expectations', '    execution not_exists;', 'execution', 1, 'keyword.other.lydite'],
  ['expectations', '    execution not_exists;', 'not_exists', 1, 'constant.language.lydite'],
  // A user operation sharing a built-in name is an invocation, not a built-in.
  ['expectations', '    trace { add(flag: bool) => true; }', 'add', 1, 'entity.name.function.lydite'],
  ['expectations', '    trace { add(flag: bool) => true; }', '=>', 1, 'keyword.operator.lydite'],
  // Connection and invocation values are identifiers, not type positions.
  ['expectations', '    trace { add(flag: bool) => true; }', 'bool', 1, 'variable.other.lydite'],
  ['specification', '  use counter: Counter(flag: bool);', 'bool', 1, 'variable.other.lydite'],
  // A field named after a proof frame variable stays an ordinary variable.
  ['expectations', '  state pre: bool;', 'pre', 1, 'variable.other.lydite'],
  ['expectations', '  state pre: bool;', 'bool', 1, 'storage.type.lydite'],
  ['expectations', '  invariant s.pre == false;', 'pre', 1, 'variable.other.lydite'],
  // A line-start dotted reference keeps its namespace qualifier.
  ['recovery', '    && impl.x == spec.x;', 'impl', 1, 'variable.language.namespace.lydite'],
  ['recovery', '    && impl.x == spec.x;', 'spec', 1, 'variable.language.namespace.lydite'],
  // An unbraced document may start its entries on the header line.
  ['inline', 'specification "Inline" component Counter { init true; invariant true; steps { tick = true; } }', 'specification', 1, 'keyword.other.document.lydite'],
  ['inline', 'specification "Inline" component Counter { init true; invariant true; steps { tick = true; } }', 'component', 1, 'keyword.other.lydite'],
  ['inline', 'specification "Inline" component Counter { init true; invariant true; steps { tick = true; } }', 'Counter', 1, 'entity.name.section.lydite'],
  // An unfinished type stays local, so later lines keep their scopes.
  ['recovery', '  input half: bv<8', 'bv', 1, 'variable.other.lydite'],
  ['recovery', '  input half: bv<8', '8', 1, 'constant.numeric.integer.lydite'],
  ['recovery', '  input whole: bool;', 'input', 1, 'storage.modifier.lydite'],
  ['recovery', '  input whole: bool;', 'bool', 1, 'storage.type.lydite'],
  // Contextual words are variables when used as names, conditions or fields.
  ['recovery', '  input forall: bool;', 'forall', 1, 'variable.other.lydite'],
  ['recovery', '  input bool: bool;', 'bool', 1, 'variable.other.lydite'],
  ['recovery', '  input bool: bool;', 'bool', 2, 'storage.type.lydite'],
  ['recovery', '  inputs { state: bool; }', 'inputs', 1, 'keyword.other.lydite'],
  ['recovery', '  inputs { state: bool; }', 'state', 1, 'variable.other.lydite'],
  // A document-kind word used as a name is not a document kind.
  ['recovery', '    design = false;', 'design', 1, 'variable.other.lydite'],
  // A header being typed keeps its string scope and stops at the line end.
  ['header', 'design "Unfinished', '"', 1, 'punctuation.definition.string.begin.lydite'],
  ['header', 'design "Unfinished', 'Unfinished', 1, 'string.quoted.double.lydite'],
  ['header', 'input rst: bool;', 'input', 1, 'storage.modifier.lydite'],
  ['header', 'input rst: bool;', 'bool', 1, 'storage.type.lydite'],
  ['recovery', '  next { x = if forall { i.forall } else { s.x }; }', 'forall', 1, 'variable.other.lydite'],
  ['recovery', '  next { x = if forall { i.forall } else { s.x }; }', 'forall', 2, 'variable.other.lydite'],
  ['recovery', '  next { x = if forall { i.forall } else { s.x }; }', 'i', 1, 'variable.language.namespace.lydite'],
  ['recovery', '  next { x = if forall { i.forall } else { s.x }; }', 'else', 1, 'keyword.control.lydite']
];

function findFiles(directory) {
  return fs.readdirSync(directory, {withFileTypes: true}).flatMap(entry => {
    if (entry.isDirectory()) return entry.name === 'node_modules' || entry.name.startsWith('.') ? [] : findFiles(path.join(directory, entry.name));
    return entry.name.endsWith('.lyd') ? [path.join(directory, entry.name)] : [];
  });
}

async function run() {
  const grammar = await loadGrammar();
  const tokenized = new Map(Object.entries(samples).map(([name, source]) => [name, tokenize(grammar, source)]));
  for (const [name, line, needle, occurrence, scope] of expectations) {
    assert.equal(innermostScope(tokenized.get(name), line, needle, occurrence), scope, `${JSON.stringify(needle)} on ${JSON.stringify(line)}`);
  }
  // Committed sources must not leave an unterminated comment, string or type open,
  // and every word they contain must match some rule.
  const rootDepth = grammar.tokenizeLine('', vscodeTextmate.INITIAL).ruleStack.depth;
  const corpus = findFiles(lydite);
  assert(corpus.length > 20, `expected the committed .lyd corpus, found ${corpus.length} files`);
  for (const file of corpus) {
    const result = tokenize(grammar, fs.readFileSync(file, 'utf8'));
    assert.equal(result.ruleStack.depth, rootDepth, `${path.relative(lydite, file)} leaves an open tokenization state`);
    for (const [lineIndex, line] of result.lines.entries()) {
      for (const token of result.tokens[lineIndex]) {
        const text = line.slice(token.startIndex, token.endIndex);
        const scope = token.scopes[token.scopes.length - 1];
        assert(scope !== 'source.lydite' || !/[A-Za-z_]/.test(text), `${path.relative(lydite, file)}:${lineIndex + 1} leaves ${JSON.stringify(text)} unscoped`);
        // A keyword never qualifies a dotted reference, so no keyword scope may
        // sit directly before a dot.
        assert(!/^(?:keyword|storage|constant)\./.test(scope) || line[token.endIndex] !== '.', `${path.relative(lydite, file)}:${lineIndex + 1} colors ${JSON.stringify(text)} as ${scope} before a dot`);
      }
    }
  }
  console.log(`lydite grammar scopes ${expectations.length} declarations and covers ${corpus.length} sources without leaving a word unscoped`);
}
run().catch(error => {console.error(error); process.exitCode = 1;});
