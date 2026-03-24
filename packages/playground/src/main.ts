import * as monaco from "monaco-editor";

// ── Examples ────────────────────────────────────────────

const EXAMPLES: Record<string, { veryl: string; testbench: string }> = {
  adder: {
    veryl: `module Adder (
    clk: input clock,
    rst: input reset,
    a:   input logic<16>,
    b:   input logic<16>,
    sum: output logic<17>,
) {
    always_comb {
        sum = a + b;
    }
}`,
    testbench: `sim.dut.a = 100n;
sim.dut.b = 200n;
sim.tick();
log("a=100, b=200 => sum=" + sim.dut.sum);

sim.dut.a = 0xFFFFn;
sim.dut.b = 1n;
sim.tick();
log("a=65535, b=1 => sum=" + sim.dut.sum);
`,
  },
  counter: {
    veryl: `module Counter (
    clk: input clock,
    rst: input reset,
    en:  input logic,
    count: output logic<8>,
) {
    var count_r: logic<8>;
    always_ff (clk, rst) {
        if_reset {
            count_r = 0;
        } else if en {
            count_r = count_r + 1;
        }
    }
    always_comb {
        count = count_r;
    }
}`,
    testbench: `// Reset (active-low)
sim.dut.rst = 0n;
sim.tick();
log("After reset: count=" + sim.dut.count);

// Release reset, enable counting
sim.dut.rst = 1n;
sim.dut.en = 1n;

for (let i = 0; i < 5; i++) {
    sim.tick();
    log(\`Cycle \${i + 1}: count=\${sim.dut.count}\`);
}
`,
  },
};

// ── Veryl language definition ───────────────────────────

monaco.languages.register({ id: "veryl" });
monaco.languages.setMonarchTokensProvider("veryl", {
  keywords: [
    "module", "interface", "package", "function", "import", "export",
    "input", "output", "inout", "ref", "modport",
    "logic", "bit", "clock", "reset",
    "var", "let", "const", "param", "localparam", "type",
    "assign", "always_ff", "always_comb", "initial", "final",
    "if", "else", "if_reset", "for", "in", "case", "switch", "default",
    "return", "break", "pub", "proto", "embed", "include", "alias", "bind",
    "inst", "enum", "struct", "union", "unsafe", "step", "posedge", "negedge",
    "as", "repeat", "inside",
  ],
  typeKeywords: ["u8", "u16", "u32", "u64", "i8", "i16", "i32", "i64", "bool"],
  operators: ["=", "==", "!=", "<", ">", "<=", ">=", "+", "-", "*", "/", "%", "&", "|", "^", "~", "<<", ">>", ">>>", "&&", "||", "!"],
  symbols: /[=><!~?:&|+\-*/^%]+/,
  tokenizer: {
    root: [
      [/[a-zA-Z_]\w*/, { cases: { "@keywords": "keyword", "@typeKeywords": "type", "@default": "identifier" } }],
      [/'[a-zA-Z_]\w*/, "annotation"],
      [/[{}()\[\]]/, "@brackets"],
      [/@symbols/, { cases: { "@operators": "operator", "@default": "" } }],
      [/\d[\d_]*/, "number"],
      [/"([^"\\]|\\.)*$/, "string.invalid"],
      [/"/, { token: "string.quote", bracket: "@open", next: "@string" }],
      [/\/\/.*$/, "comment"],
      [/\/\*/, "comment", "@comment"],
    ],
    string: [[/[^\\"]+/, "string"], [/"/, { token: "string.quote", bracket: "@close", next: "@pop" }]],
    comment: [[/[^/*]+/, "comment"], [/\*\//, "comment", "@pop"], [/[/*]/, "comment"]],
  },
});

monaco.languages.registerCompletionItemProvider("veryl", {
  provideCompletionItems(model, position) {
    const word = model.getWordUntilPosition(position);
    const range = { startLineNumber: position.lineNumber, endLineNumber: position.lineNumber, startColumn: word.startColumn, endColumn: word.endColumn };
    const snippets = [
      { label: "module", insertText: "module ${1:Name} (\n    ${2}\n) {\n    ${0}\n}", detail: "Module declaration" },
      { label: "always_ff", insertText: "always_ff (${1:clk}, ${2:rst}) {\n    if_reset {\n        ${3}\n    } else {\n        ${0}\n    }\n}", detail: "Sequential block" },
      { label: "always_comb", insertText: "always_comb {\n    ${0}\n}", detail: "Combinational block" },
      { label: "assign", insertText: "assign ${1:out} = ${0};", detail: "Continuous assignment" },
      { label: "if_reset", insertText: "if_reset {\n    ${1}\n} else {\n    ${0}\n}", detail: "Reset branch" },
    ];
    return {
      suggestions: [
        ...snippets.map(s => ({ label: s.label, kind: monaco.languages.CompletionItemKind.Snippet, insertText: s.insertText, insertTextRules: monaco.languages.CompletionItemInsertTextRule.InsertAsSnippet, detail: s.detail, range })),
      ],
    };
  },
});

monaco.editor.defineTheme("celox-dark", {
  base: "vs-dark", inherit: true,
  rules: [
    { token: "keyword", foreground: "c678dd" },
    { token: "type", foreground: "e5c07b" },
    { token: "number", foreground: "d19a66" },
    { token: "string", foreground: "98c379" },
    { token: "comment", foreground: "5c6370", fontStyle: "italic" },
    { token: "operator", foreground: "56b6c2" },
    { token: "annotation", foreground: "61afef" },
  ],
  colors: { "editor.background": "#0d1117", "editor.foreground": "#c9d1d9" },
});

// ── UI ──────────────────────────────────────────────────

document.getElementById("app")!.innerHTML = `
<style>
  * { box-sizing: border-box; margin: 0; padding: 0; }
  body { font-family: system-ui, sans-serif; background: #1a1a2e; color: #e0e0e0; overflow: hidden; }
  header { background: #16213e; padding: 8px 20px; display: flex; align-items: center; gap: 12px; border-bottom: 1px solid #0f3460; height: 42px; }
  header h1 { font-size: 1.1rem; color: #e94560; }
  select, button { padding: 4px 10px; border-radius: 4px; border: 1px solid #0f3460; background: #1a1a2e; color: #e0e0e0; font-size: 0.85rem; cursor: pointer; }
  button { background: #e94560; color: #fff; border-color: #e94560; font-weight: 600; }
  button:hover { background: #c73e54; }
  button:disabled { opacity: 0.5; cursor: not-allowed; }
  .status { margin-left: auto; font-size: 0.8rem; color: #888; }
  .panels { display: grid; grid-template-columns: 1fr 1fr; grid-template-rows: 1fr 180px; height: calc(100vh - 42px); }
  .panel { display: flex; flex-direction: column; border: 1px solid #0f3460; overflow: hidden; }
  .panel-hdr { background: #16213e; padding: 3px 10px; font-size: 0.7rem; font-weight: 600; color: #666; text-transform: uppercase; letter-spacing: 0.05em; flex-shrink: 0; }
  .editor-container { flex: 1; }
  #console { flex: 1; background: #0d1117; color: #c9d1d9; padding: 8px 10px; font-family: 'Fira Code', monospace; font-size: 0.8rem; line-height: 1.4; overflow-y: auto; white-space: pre-wrap; }
  .log-info { color: #58a6ff; } .log-error { color: #f85149; } .log-success { color: #3fb950; }
</style>
<header>
  <h1>Celox Playground</h1>
  <select id="examples"><option value="">-- Example --</option><option value="adder">Adder</option><option value="counter">Counter</option></select>
  <button id="run" disabled>Run</button>
  <span class="status" id="status">Loading WASM…</span>
</header>
<div class="panels">
  <div class="panel"><div class="panel-hdr">Veryl Source</div><div class="editor-container" id="veryl-editor"></div></div>
  <div class="panel"><div class="panel-hdr">Testbench (TypeScript)</div><div class="editor-container" id="tb-editor"></div></div>
  <div class="panel" style="grid-column:1/-1"><div class="panel-hdr">Console</div><div id="console"></div></div>
</div>`;

const consoleEl = document.getElementById("console")!;
const runBtn = document.getElementById("run") as HTMLButtonElement;
const statusEl = document.getElementById("status")!;
const examplesEl = document.getElementById("examples") as HTMLSelectElement;

function appendConsole(msg: string, cls = "") {
  const span = document.createElement("span");
  if (cls) span.className = cls;
  span.textContent = msg + "\n";
  consoleEl.appendChild(span);
  consoleEl.scrollTop = consoleEl.scrollHeight;
}
function clearConsole() { consoleEl.innerHTML = ""; }

// ── Monaco editors ──────────────────────────────────────

const editorOpts: monaco.editor.IStandaloneEditorConstructionOptions = {
  theme: "celox-dark", fontSize: 13, fontFamily: "'Fira Code', monospace",
  minimap: { enabled: false }, scrollBeyondLastLine: false, automaticLayout: true,
  tabSize: 4, padding: { top: 8 },
};

const verylEditor = monaco.editor.create(document.getElementById("veryl-editor")!, {
  ...editorOpts, language: "veryl", value: "",
});

// Configure TS compiler options for the testbench editor
monaco.languages.typescript.typescriptDefaults.setCompilerOptions({
  target: monaco.languages.typescript.ScriptTarget.ESNext,
  moduleResolution: monaco.languages.typescript.ModuleResolutionKind.NodeJs,
  strict: false,
  noEmit: true,
});

const tbModel = monaco.editor.createModel("", "typescript", monaco.Uri.parse("file:///testbench.ts"));
const tbEditor = monaco.editor.create(document.getElementById("tb-editor")!, {
  ...editorOpts, model: tbModel,
});

// ── DUT type injection ──────────────────────────────────

let currentExtraLib: monaco.IDisposable | null = null;

function updateDutTypes(ports: Record<string, { direction: string; width: number }>) {
  if (currentExtraLib) currentExtraLib.dispose();

  const portEntries = Object.entries(ports)
    .filter(([_, p]) => p.direction === "input" || p.direction === "output")
    .map(([name, _]) => `    ${name}: bigint;`)
    .join("\n");

  const dts = `
declare const sim: {
  readonly dut: {
${portEntries}
  };
  tick(): void;
  evalComb(): void;
  dispose(): void;
};
declare function log(msg: any): void;
`;
  currentExtraLib = monaco.languages.typescript.typescriptDefaults.addExtraLib(dts, "file:///celox-sim.d.ts");
}

// Extract port names from Veryl source with regex (works without WASM)
function extractPortsFromSource(source: string): Record<string, { direction: string; width: number }> {
  const ports: Record<string, { direction: string; width: number }> = {};
  // Match: name: input/output logic<N> or clock/reset
  const portRe = /(\w+)\s*:\s*(input|output|inout)\s+(?:'[_a-zA-Z]*\s+)?(logic|bit|clock|reset)(?:<(\d+)>)?/g;
  let m;
  while ((m = portRe.exec(source)) !== null) {
    ports[m[1]] = { direction: m[2], width: m[4] ? parseInt(m[4]) : 1 };
  }
  return ports;
}

// Parse error messages from genTsFromSource/NativeSimulatorHandle into Monaco markers
function parseVerylErrors(errorMsg: string): monaco.editor.IMarkerData[] {
  const markers: monaco.editor.IMarkerData[] = [];
  // Match patterns like: ╭─[main.veryl:1:35]  or  ╭─[test.veryl:3:10]
  const locRe = /╭─\[(?:[\w./]+):(\d+):(\d+)\]/g;
  // Match error/warning descriptions: × "..." or ⚠ "..."
  const msgRe = /[×✕⚠]\s+(.+)/g;

  const messages: string[] = [];
  let m;
  while ((m = msgRe.exec(errorMsg)) !== null) messages.push(m[1].trim());

  // Get source lines for token length detection
  const sourceLines = verylEditor.getValue().split("\n");

  let i = 0;
  while ((m = locRe.exec(errorMsg)) !== null) {
    const line = parseInt(m[1]);
    const col = parseInt(m[2]);

    // Try to determine token length from source
    let endCol = col + 1;
    const srcLine = sourceLines[line - 1];
    if (srcLine) {
      const rest = srcLine.substring(col - 1);
      const tok = rest.match(/^\w+/);
      if (tok) {
        endCol = col + tok[0].length;
      }
    }

    markers.push({
      severity: monaco.MarkerSeverity.Error,
      message: messages[i] || "Veryl error",
      startLineNumber: line,
      startColumn: col,
      endLineNumber: line,
      endColumn: endCol,
    });
    i++;
  }

  // Fallback: if no location found but there's an error message
  if (markers.length === 0 && errorMsg.trim()) {
    // Try to extract a simpler message
    const simple = errorMsg.match(/(?:Parse error|Unexpected token):\s*(.+?)(?:\n|$)/);
    if (simple) {
      markers.push({
        severity: monaco.MarkerSeverity.Error,
        message: simple[1],
        startLineNumber: 1,
        startColumn: 1,
        endLineNumber: 1,
        endColumn: 1,
      });
    }
    // Don't add generic "Veryl error" — if we can't parse it, skip it
  }

  return markers;
}

// Update types + diagnostics when Veryl source changes
let updateTimer: ReturnType<typeof setTimeout>;
function onVerylChange() {
  clearTimeout(updateTimer);
  updateTimer = setTimeout(() => {
    const source = verylEditor.getValue();
    const model = verylEditor.getModel();

    // Try WASM-based analysis (accurate types + diagnostics)
    if (celox) {
      try {
        const result = JSON.parse(celox.genTsFromSource([{ content: source, path: "main.veryl" }]));
        if (result.modules?.[0]?.ports) {
          updateDutTypes(result.modules[0].ports);
        }
        // Show structured diagnostics (if any) as Monaco markers
        if (model) {
          const diags: monaco.editor.IMarkerData[] = (result.diagnostics || []).map((d: any) => ({
            severity: d.severity === "error" ? monaco.MarkerSeverity.Error : monaco.MarkerSeverity.Warning,
            message: d.message + (d.help ? `\n${d.help}` : "") + (d.url ? `\n${d.url}` : ""),
            startLineNumber: d.line,
            startColumn: d.column,
            endLineNumber: d.endLine ?? d.line,
            endColumn: d.endColumn ?? d.column + 1,
          }));
          // Fallback to string-based parsing for warnings field
          const legacyWarnings: monaco.editor.IMarkerData[] = (result.warnings || [])
            .flatMap((w: string) => parseVerylErrors(w))
            .map((m: monaco.editor.IMarkerData) => ({ ...m, severity: monaco.MarkerSeverity.Warning }));
          monaco.editor.setModelMarkers(model, "veryl", [...diags, ...legacyWarnings]);
        }
        return;
      } catch (e: any) {
        // Analysis failed — show diagnostics
        if (model) {
          const markers = parseVerylErrors(e.message || String(e));
          monaco.editor.setModelMarkers(model, "veryl", markers);
        }
        // Still try regex fallback for types
      }
    }

    // Fallback: regex-based port extraction (no WASM needed)
    const ports = extractPortsFromSource(source);
    if (Object.keys(ports).length > 0) {
      updateDutTypes(ports);
    }
  }, 500);
}
verylEditor.onDidChangeModelContent(onVerylChange);

// ── WASM loading ────────────────────────────────────────

let celox: any;

async function init() {
  try {
    celox = await import("./celox-wasm-loader.js");
    statusEl.textContent = "Ready";
    runBtn.disabled = false;
    onVerylChange(); // Initial type generation
  } catch (e: any) {
    statusEl.textContent = "Failed";
    appendConsole("[error] Failed to load WASM: " + e.message, "log-error");
  }
}

// ── Run ─────────────────────────────────────────────────

async function run() {
  clearConsole();
  runBtn.disabled = true;
  statusEl.textContent = "Compiling…";

  try {
    const verylSource = verylEditor.getValue();
    const topMatch = verylSource.match(/module\s+(\w+)/);
    if (!topMatch) throw new Error("No module found in Veryl source");
    const topName = topMatch[1];

    const t0 = performance.now();
    const handle = new celox.NativeSimulatorHandle(
      [{ content: verylSource, path: "main.veryl" }], topName
    );
    const layout = JSON.parse(handle.layoutJson);
    const events = JSON.parse(handle.eventsJson);
    const totalSize = handle.totalSize;
    const t1 = performance.now();
    appendConsole(`[compile] ${(t1 - t0).toFixed(0)}ms — ${Object.keys(layout).length} signals, ${Object.keys(events).length} events`, "log-success");

    // Instantiate WASM simulation modules
    const pages = Math.max(1, Math.ceil(totalSize / 65536));
    const memory = new WebAssembly.Memory({ initial: pages });
    const combInst = await WebAssembly.instantiate(new Uint8Array(handle.combWasmBytes()), { env: { memory } });
    const eventInsts: Record<string, WebAssembly.Instance> = {};
    for (const name of Object.keys(events)) {
      try {
        const { instance } = await WebAssembly.instantiate(new Uint8Array(handle.eventWasmBytes(name)), { env: { memory } });
        eventInsts[name] = instance;
      } catch {}
    }

    const view = new DataView(memory.buffer);
    const dut = new Proxy({} as Record<string, bigint>, {
      get(_, prop: string) {
        const sig = layout[prop];
        if (!sig) return undefined;
        let v = 0n;
        for (let i = sig.byte_size - 1; i >= 0; i--) v = (v << 8n) | BigInt(view.getUint8(sig.offset + i));
        if (sig.width < 64) v &= (1n << BigInt(sig.width)) - 1n;
        return v;
      },
      set(_, prop: string, value: bigint) {
        const sig = layout[prop];
        if (!sig) throw new Error(`Signal '${prop}' not found`);
        let v = BigInt(value);
        for (let i = 0; i < sig.byte_size; i++) { view.setUint8(sig.offset + i, Number(v & 0xFFn)); v >>= 8n; }
        return true;
      },
    });

    const sim = {
      dut,
      evalComb() { (combInst.instance.exports.run as Function)(); },
      tick(eventName?: string) {
        if (eventName) { const inst = eventInsts[eventName]; if (inst) (inst.exports.run as Function)(); }
        else { for (const inst of Object.values(eventInsts)) (inst.exports.run as Function)(); }
        (combInst.instance.exports.run as Function)();
      },
      dispose() {},
    };

    // Transpile TS → JS via Monaco's TS worker
    statusEl.textContent = "Running…";
    const tsSource = tbEditor.getValue();
    const tsWorker = await monaco.languages.typescript.getTypeScriptWorker();
    const model = tbEditor.getModel()!;
    const client = await tsWorker(model.uri);
    const output = await client.getEmitOutput(model.uri.toString());
    const jsCode = output.outputFiles[0]?.text ?? tsSource;

    appendConsole("[run] Executing…", "log-info");
    const log = (msg: any) => appendConsole(String(msg));
    new Function("sim", "log", jsCode)(sim, log);
    appendConsole("[run] Done.", "log-success");
    statusEl.textContent = "Done";
  } catch (e: any) {
    appendConsole("[error] " + e.message, "log-error");
    statusEl.textContent = "Error";
  } finally {
    runBtn.disabled = false;
  }
}

// ── Events ──────────────────────────────────────────────

function loadExample(name: string) {
  const ex = EXAMPLES[name];
  if (ex) { verylEditor.setValue(ex.veryl); tbEditor.setValue(ex.testbench); }
}

examplesEl.addEventListener("change", () => { if (examplesEl.value) loadExample(examplesEl.value); });
runBtn.addEventListener("click", run);
document.addEventListener("keydown", (e) => { if ((e.ctrlKey || e.metaKey) && e.key === "Enter" && !runBtn.disabled) run(); });

loadExample("adder");
onVerylChange(); // Inject types immediately from regex (no WASM needed)
init();
