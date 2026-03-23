// Celox Playground - main entry point
//
// Loads the celox-wasm module, compiles Veryl source, instantiates the
// generated WASM simulation modules, and runs the user's testbench.

import initCelox, { SimHandle } from "./pkg/celox_wasm.js";

// ── Example sources ─────────────────────────────────────────────────

const EXAMPLES = {
    adder: {
        veryl: `module Adder (
    a: input logic<8>,
    b: input logic<8>,
    sum: output logic<9>,
) {
    assign sum = a + b;
}`,
        testbench: `// Set inputs and evaluate combinational logic
sim.set("a", 10);
sim.set("b", 20);
sim.evalComb();
log("a=10, b=20 => sum=" + sim.get("sum"));

sim.set("a", 255);
sim.set("b", 1);
sim.evalComb();
log("a=255, b=1 => sum=" + sim.get("sum"));
`,
    },
    counter: {
        veryl: `module Counter (
    clk: input '_ clock,
    rst: input '_ reset,
    count: output logic<8>,
) {
    var r_count: logic<8>;
    assign count = r_count;

    always_ff (clk, rst) {
        if_reset {
            r_count = 0;
        } else {
            r_count = r_count + 1;
        }
    }
}`,
        testbench: `// Reset the counter (active-low: rst=0 asserts reset)
sim.set("rst", 0);
sim.set("clk", 0);
sim.evalComb();
sim.set("clk", 1);
sim.evalComb();
sim.tickEvent("clk");
log("After reset: count=" + sim.get("count"));

// Release reset (active-low: rst=1 de-asserts)
sim.set("rst", 1);
sim.evalComb();

// Clock 5 times
for (let i = 0; i < 5; i++) {
    sim.set("clk", 0);
    sim.evalComb();
    sim.set("clk", 1);
    sim.evalComb();
    sim.tickEvent("clk");
    log("Cycle " + (i + 1) + ": count=" + sim.get("count"));
}
`,
    },
};

// ── Monaco Editor setup ─────────────────────────────────────────────

let verylEditor, tbEditor;

async function setupEditors() {
    await window.__monacoReady;
    const monaco = window.monaco;

    // Register Veryl language
    monaco.languages.register({ id: "veryl" });

    monaco.languages.setMonarchTokensProvider("veryl", {
        keywords: [
            "module", "interface", "package", "function", "import", "export",
            "input", "output", "inout", "ref", "modport",
            "logic", "bit", "clock", "reset", "reset_async_low", "reset_async_high",
            "var", "let", "const", "param", "localparam", "type",
            "assign", "always_ff", "always_comb", "initial", "final",
            "if", "else", "if_reset", "for", "in", "case", "switch", "default",
            "return", "break",
            "pub", "proto", "embed", "include", "alias", "bind",
            "inst", "enum", "struct", "union", "unsafe",
            "step", "posedge", "negedge",
            "as", "repeat", "inside",
        ],
        typeKeywords: [
            "u8", "u16", "u32", "u64", "i8", "i16", "i32", "i64", "bool",
        ],
        operators: [
            "=", "==", "!=", "<", ">", "<=", ">=",
            "+", "-", "*", "/", "%",
            "&", "|", "^", "~", "<<", ">>", ">>>",
            "&&", "||", "!",
            "+=", "-=", "*=", "/=",
            "->", "=>",
        ],
        symbols: /[=><!~?:&|+\-*/^%]+/,
        tokenizer: {
            root: [
                [/[a-zA-Z_]\w*/, {
                    cases: {
                        "@keywords": "keyword",
                        "@typeKeywords": "type",
                        "@default": "identifier",
                    },
                }],
                [/'[a-zA-Z_]\w*/, "annotation"],  // clock domain annotations like '_ or 'a
                [/[{}()\[\]]/, "@brackets"],
                [/@symbols/, {
                    cases: {
                        "@operators": "operator",
                        "@default": "",
                    },
                }],
                [/\d[\d_]*/, "number"],
                [/\d+'\s*[bodh][\da-fA-F_]+/, "number.hex"],
                [/"([^"\\]|\\.)*$/, "string.invalid"],
                [/"/, { token: "string.quote", bracket: "@open", next: "@string" }],
                [/\/\/.*$/, "comment"],
                [/\/\*/, "comment", "@comment"],
            ],
            string: [
                [/[^\\"]+/, "string"],
                [/"/, { token: "string.quote", bracket: "@close", next: "@pop" }],
            ],
            comment: [
                [/[^/*]+/, "comment"],
                [/\*\//, "comment", "@pop"],
                [/[/*]/, "comment"],
            ],
        },
    });

    // Veryl completions
    monaco.languages.registerCompletionItemProvider("veryl", {
        provideCompletionItems(model, position) {
            const word = model.getWordUntilPosition(position);
            const range = {
                startLineNumber: position.lineNumber,
                endLineNumber: position.lineNumber,
                startColumn: word.startColumn,
                endColumn: word.endColumn,
            };

            const snippets = [
                { label: "module", insertText: "module ${1:Name} (\n    ${2}\n) {\n    ${0}\n}", detail: "Module declaration" },
                { label: "always_ff", insertText: "always_ff (${1:clk}, ${2:rst}) {\n    if_reset {\n        ${3}\n    } else {\n        ${0}\n    }\n}", detail: "Sequential always block" },
                { label: "always_comb", insertText: "always_comb {\n    ${0}\n}", detail: "Combinational always block" },
                { label: "assign", insertText: "assign ${1:out} = ${0};", detail: "Continuous assignment" },
                { label: "if_reset", insertText: "if_reset {\n    ${1}\n} else {\n    ${0}\n}", detail: "Reset branch" },
                { label: "input logic", insertText: "${1:name}: input logic<${2:8}>", detail: "Input port" },
                { label: "output logic", insertText: "${1:name}: output logic<${2:8}>", detail: "Output port" },
                { label: "var", insertText: "var ${1:name}: logic<${2:8}>;", detail: "Variable declaration" },
                { label: "for", insertText: "for ${1:i} in 0..${2:N} {\n    ${0}\n}", detail: "For loop" },
                { label: "case", insertText: "case ${1:expr} {\n    ${2:val}: {\n        ${0}\n    }\n    default: {\n    }\n}", detail: "Case statement" },
                { label: "inst", insertText: "inst ${1:name}: ${2:Module};", detail: "Instance" },
                { label: "clock", insertText: "${1:clk}: input '${2:_} clock", detail: "Clock port" },
                { label: "reset", insertText: "${1:rst}: input '${2:_} reset", detail: "Reset port" },
            ];

            const keywords = [
                "module", "interface", "package", "function", "import",
                "input", "output", "inout", "logic", "bit", "clock", "reset",
                "var", "let", "const", "param", "assign",
                "always_ff", "always_comb", "if", "else", "if_reset",
                "for", "in", "case", "default", "return",
                "inst", "enum", "struct", "pub",
            ];

            return {
                suggestions: [
                    ...snippets.map(s => ({
                        label: s.label,
                        kind: monaco.languages.CompletionItemKind.Snippet,
                        insertText: s.insertText,
                        insertTextRules: monaco.languages.CompletionItemInsertTextRule.InsertAsSnippet,
                        detail: s.detail,
                        range,
                    })),
                    ...keywords.map(kw => ({
                        label: kw,
                        kind: monaco.languages.CompletionItemKind.Keyword,
                        insertText: kw,
                        range,
                    })),
                ],
            };
        },
    });

    // Define dark theme
    monaco.editor.defineTheme("celox-dark", {
        base: "vs-dark",
        inherit: true,
        rules: [
            { token: "keyword", foreground: "c678dd" },
            { token: "type", foreground: "e5c07b" },
            { token: "identifier", foreground: "abb2bf" },
            { token: "number", foreground: "d19a66" },
            { token: "number.hex", foreground: "d19a66" },
            { token: "string", foreground: "98c379" },
            { token: "comment", foreground: "5c6370", fontStyle: "italic" },
            { token: "operator", foreground: "56b6c2" },
            { token: "annotation", foreground: "61afef" },
        ],
        colors: {
            "editor.background": "#0d1117",
            "editor.foreground": "#c9d1d9",
            "editorLineNumber.foreground": "#484f58",
            "editorCursor.foreground": "#58a6ff",
            "editor.selectionBackground": "#264f78",
        },
    });

    const commonOpts = {
        theme: "celox-dark",
        fontSize: 13,
        fontFamily: "'Fira Code', 'Cascadia Code', monospace",
        fontLigatures: true,
        minimap: { enabled: false },
        scrollBeyondLastLine: false,
        automaticLayout: true,
        tabSize: 4,
        renderLineHighlight: "line",
        padding: { top: 8 },
    };

    verylEditor = monaco.editor.create(
        document.getElementById("veryl-editor-container"),
        { ...commonOpts, language: "veryl", value: "" },
    );

    tbEditor = monaco.editor.create(
        document.getElementById("tb-editor-container"),
        { ...commonOpts, language: "javascript", value: "" },
    );

    // Add testbench-specific completions for the sim API
    monaco.languages.registerCompletionItemProvider("javascript", {
        provideCompletionItems(model, position) {
            const word = model.getWordUntilPosition(position);
            const range = {
                startLineNumber: position.lineNumber,
                endLineNumber: position.lineNumber,
                startColumn: word.startColumn,
                endColumn: word.endColumn,
            };
            const textBefore = model.getValueInRange({
                startLineNumber: position.lineNumber,
                startColumn: 1,
                endLineNumber: position.lineNumber,
                endColumn: position.column,
            });
            if (!textBefore.match(/sim\.\s*$/)) return { suggestions: [] };

            return {
                suggestions: [
                    { label: "set", insertText: 'set("${1:signal}", ${2:value})', insertTextRules: 4, detail: "Set signal value", kind: 1, range },
                    { label: "get", insertText: 'get("${1:signal}")', insertTextRules: 4, detail: "Get signal value", kind: 1, range },
                    { label: "evalComb", insertText: "evalComb()", detail: "Evaluate combinational logic", kind: 1, range },
                    { label: "tickEvent", insertText: 'tickEvent("${1:clk}")', insertTextRules: 4, detail: "Trigger clock event", kind: 1, range },
                ],
            };
        },
        triggerCharacters: ["."],
    });
}

// ── Console output helpers ──────────────────────────────────────────

const consoleEl = document.getElementById("console");

function appendConsole(text, cls = "") {
    const span = document.createElement("span");
    if (cls) span.className = cls;
    span.textContent = text + "\n";
    consoleEl.appendChild(span);
    consoleEl.scrollTop = consoleEl.scrollHeight;
}

function clearConsole() {
    consoleEl.innerHTML = "";
}

// ── Simulation wrapper ──────────────────────────────────────────────

class WasmSimulation {
    constructor(memory, layout, combInstance, eventInstances) {
        this._memory = memory;
        this._layout = layout;
        this._combRun = combInstance.exports.run;
        this._events = eventInstances;
        this._view = new DataView(memory.buffer);
    }

    set(name, value) {
        const sig = this._layout[name];
        if (!sig) throw new Error(`Signal '${name}' not found in layout`);
        const { offset, byteSize } = sig;
        if (byteSize <= 4) {
            for (let i = 0; i < byteSize; i++) {
                this._view.setUint8(offset + i, (value >> (i * 8)) & 0xff);
            }
        } else {
            let v = BigInt(value);
            for (let i = 0; i < byteSize; i++) {
                this._view.setUint8(offset + i, Number(v & 0xffn));
                v >>= 8n;
            }
        }
    }

    get(name) {
        const sig = this._layout[name];
        if (!sig) throw new Error(`Signal '${name}' not found in layout`);
        const { offset, width, byteSize } = sig;
        let value = 0;
        for (let i = Math.min(byteSize, 4) - 1; i >= 0; i--) {
            value = (value << 8) | this._view.getUint8(offset + i);
        }
        if (width < 32) {
            value &= (1 << width) - 1;
        }
        return value >>> 0;
    }

    evalComb() {
        const rc = this._combRun();
        if (rc !== 0n && rc !== 0) {
            throw new Error(`eval_comb returned error code ${rc}`);
        }
    }

    tickEvent(name) {
        const inst = this._events[name];
        if (!inst) throw new Error(`Event '${name}' not found`);
        const rc = inst.exports.run();
        if (rc !== 0n && rc !== 0) {
            throw new Error(`event '${name}' returned error code ${rc}`);
        }
        this.evalComb();
    }
}

// ── Main initialization ─────────────────────────────────────────────

const statusEl = document.getElementById("status");
const runBtn = document.getElementById("run-btn");
const examplesSelect = document.getElementById("examples");

async function main() {
    try {
        await Promise.all([initCelox(), setupEditors()]);
        statusEl.textContent = "Ready";
        runBtn.disabled = false;
    } catch (e) {
        statusEl.textContent = "Failed to load";
        appendConsole("Failed to initialize: " + e.message, "log-error");
        return;
    }

    loadExample("adder");

    examplesSelect.addEventListener("change", () => {
        if (examplesSelect.value) loadExample(examplesSelect.value);
    });

    runBtn.addEventListener("click", runSimulation);

    // Ctrl+Enter to run
    document.addEventListener("keydown", (e) => {
        if ((e.ctrlKey || e.metaKey) && e.key === "Enter" && !runBtn.disabled) {
            runSimulation();
        }
    });
}

function loadExample(name) {
    const ex = EXAMPLES[name];
    if (ex) {
        verylEditor.setValue(ex.veryl);
        tbEditor.setValue(ex.testbench);
    }
}

async function runSimulation() {
    clearConsole();
    runBtn.disabled = true;
    statusEl.textContent = "Compiling...";

    try {
        const source = verylEditor.getValue();
        const tb = tbEditor.getValue();

        appendConsole("[compile] Compiling Veryl source...", "log-info");
        const t0 = performance.now();
        const topName = source.match(/module\s+(\w+)/)?.[1] || "Top";
        const handle = new SimHandle(source, topName);
        const t1 = performance.now();
        appendConsole(`[compile] Done in ${(t1 - t0).toFixed(1)}ms`, "log-success");

        const layout = JSON.parse(handle.layoutJson());
        const events = JSON.parse(handle.eventsJson());
        const totalSize = handle.totalSize();
        appendConsole(`[info] Memory: ${totalSize}B, Signals: ${Object.keys(layout).length}, Events: ${Object.keys(events).length}`, "log-info");

        const pages = Math.max(1, Math.ceil(totalSize / 65536));
        const memory = new WebAssembly.Memory({ initial: pages });

        const combBytes = handle.combWasmBytes();
        const combModule = await WebAssembly.compile(combBytes);
        const combInstance = await WebAssembly.instantiate(combModule, { env: { memory } });

        const eventInstances = {};
        for (const eventName of Object.keys(events)) {
            try {
                const eventBytes = handle.eventWasmBytes(eventName);
                const eventModule = await WebAssembly.compile(eventBytes);
                eventInstances[eventName] = await WebAssembly.instantiate(eventModule, { env: { memory } });
            } catch (e) {
                appendConsole(`[warn] Event '${eventName}': ${e.message}`, "log-warn");
            }
        }

        const sim = new WasmSimulation(memory, layout, combInstance, eventInstances);

        statusEl.textContent = "Running...";
        appendConsole("[run] Executing testbench...", "log-info");

        const log = (msg) => appendConsole(String(msg));
        const fn = new Function("sim", "log", tb);
        fn(sim, log);

        appendConsole("[run] Testbench complete.", "log-success");
        statusEl.textContent = "Done";
    } catch (e) {
        appendConsole("[error] " + e.message, "log-error");
        statusEl.textContent = "Error";
    } finally {
        runBtn.disabled = false;
    }
}

main();
