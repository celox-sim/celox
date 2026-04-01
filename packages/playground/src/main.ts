import { chai, JestAsymmetricMatchers, JestChaiExpect } from "@vitest/expect";
import * as monaco from "monaco-editor";
import {
	WaveformViewer,
	generateVcdText,
	type VcdSignalInfo,
	type VcdSnapshot,
	type VcdTrace,
} from "./waveform-viewer.js";
import celoxDutDts from "../../celox/dist/dut.d.ts?raw";
import celoxIndexDts from "../../celox/dist/index.d.ts?raw";
import celoxNapiBridgeDts from "../../celox/dist/napi-bridge.d.ts?raw";
import celoxNapiHelpersDts from "../../celox/dist/napi-helpers.d.ts?raw";
import celoxSimulationDts from "../../celox/dist/simulation.d.ts?raw";
import celoxSimulatorDts from "../../celox/dist/simulator.d.ts?raw";
// Import real .d.ts files from @celox-sim/celox for Monaco type injection
import celoxTypesDts from "../../celox/dist/types.d.ts?raw";
import celoxWasmBridgeDts from "../../celox/dist/wasm-bridge.d.ts?raw";

// ── Examples ────────────────────────────────────────────

const DEFAULT_VERYL_TOML = `[project]
name    = "playground"
version = "0.1.0"

[build]
clock_type = "posedge"
reset_type = "async_low"
sources    = ["src"]
`;

const DEFAULT_CELOX_TOML = `[simulation]
max_steps = 100
`;

interface Example {
	veryl: string;
	testbench: string;
	verylToml?: string;
	celoxToml?: string;
}

const EXAMPLES: Record<string, Example> = {
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
		testbench: `import { describe, it, expect } from "vitest";
import { Simulator } from "@celox-sim/celox";
import { Adder } from "../src/Adder.veryl";

describe("Adder", () => {
    it("adds two small numbers", () => {
        const sim = Simulator.create(Adder);
        sim.dut.a = 100n;
        sim.dut.b = 200n;
        expect(sim.dut.sum).toBe(300n);
        sim.dispose();
    });

    it("handles overflow", () => {
        const sim = Simulator.create(Adder);
        sim.dut.a = 0xFFFFn;
        sim.dut.b = 1n;
        expect(sim.dut.sum).toBe(0x10000n);
        sim.dispose();
    });
});
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
		testbench: `import { describe, it, expect } from "vitest";
import { Simulator } from "@celox-sim/celox";
import { Counter } from "../src/Counter.veryl";

describe("Counter", () => {
    it("resets to zero", () => {
        const sim = Simulator.create(Counter);
        const rst = sim.event("rst");

        // Assert async reset (active-low)
        sim.dut.rst = 0n;
        sim.tick(rst);
        expect(sim.dut.count).toBe(0n);
        sim.dispose();
    });

    it("counts up when enabled", () => {
        const sim = Simulator.create(Counter);
        const rst = sim.event("rst");

        // Assert async reset
        sim.dut.rst = 0n;
        sim.tick(rst);

        // Release reset, enable counting
        sim.dut.rst = 1n;
        sim.dut.en = 1n;
        for (let i = 0; i < 5; i++) sim.tick();

        expect(sim.dut.count).toBe(5n);
        sim.dispose();
    });
});
`,
	},
	counter_sim: {
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
		testbench: `import { describe, it, expect } from "vitest";
import { Simulation } from "@celox-sim/celox";
import { Counter } from "../src/Counter.veryl";

describe("Counter (Simulation)", () => {
    it("resets and counts with time-based simulation", () => {
        const sim = Simulation.create(Counter);
        sim.addClock("clk", { period: 10 });

        // Assert and release async reset automatically
        sim.reset("rst");

        // Enable counting
        sim.dut.en = 1n;

        // Wait for 5 rising clock edges
        sim.waitForCycles("clk", 5);
        expect(sim.dut.count).toBe(5n);

        // Run for 30 more time units (3 more cycles)
        const t = sim.time();
        sim.runUntil(t + 30);
        expect(sim.dut.count).toBe(8n);

        sim.dispose();
    });

    it("stays at zero when disabled", () => {
        const sim = Simulation.create(Counter);
        sim.addClock("clk", { period: 10 });
        sim.reset("rst");

        sim.dut.en = 0n;
        sim.waitForCycles("clk", 5);
        expect(sim.dut.count).toBe(0n);

        sim.dispose();
    });
});
`,
	},
	counter_vcd: {
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
		testbench: `import { describe, it, expect } from "vitest";
import { Simulation } from "@celox-sim/celox";
import { Counter } from "../src/Counter.veryl";

describe("Counter (Waveform)", () => {
    it("records waveform while counting", () => {
        const sim = Simulation.create(Counter);
        sim.addClock("clk", { period: 10 });

        sim.dump(sim.time());
        sim.reset("rst");
        sim.dump(sim.time());

        // Enable counting
        sim.dut.en = 1n;
        sim.dump(sim.time());

        // Step through and dump at each event
        for (let i = 0; i < 100; i++) {
            const t = sim.step();
            if (t === null) break;
            sim.dump(t);
        }

        expect(sim.dut.count).toBeGreaterThan(0n);
        sim.dispose();
    });
});
`,
	},
};

// ── Language definitions ─────────────────────────────────

// TOML
monaco.languages.register({ id: "toml" });
monaco.languages.setMonarchTokensProvider("toml", {
	tokenizer: {
		root: [
			[/\[[^\]]*\]/, "keyword"],
			[/#.*$/, "comment"],
			[/[a-zA-Z_][\w-]*(?=\s*=)/, "variable"],
			[/"[^"]*"/, "string"],
			[/'[^']*'/, "string"],
			[/\b(?:true|false)\b/, "keyword"],
			[/\d[\d._]*/, "number"],
			[/=/, "operator"],
		],
	},
});

// Veryl
monaco.languages.register({ id: "veryl" });
monaco.languages.setMonarchTokensProvider("veryl", {
	keywords: [
		"module",
		"interface",
		"package",
		"function",
		"import",
		"export",
		"input",
		"output",
		"inout",
		"ref",
		"modport",
		"logic",
		"bit",
		"clock",
		"reset",
		"var",
		"let",
		"const",
		"param",
		"localparam",
		"type",
		"assign",
		"always_ff",
		"always_comb",
		"initial",
		"final",
		"if",
		"else",
		"if_reset",
		"for",
		"in",
		"case",
		"switch",
		"default",
		"return",
		"break",
		"pub",
		"proto",
		"embed",
		"include",
		"alias",
		"bind",
		"inst",
		"enum",
		"struct",
		"union",
		"unsafe",
		"step",
		"posedge",
		"negedge",
		"as",
		"repeat",
		"inside",
	],
	typeKeywords: ["u8", "u16", "u32", "u64", "i8", "i16", "i32", "i64", "bool"],
	operators: [
		"=",
		"==",
		"!=",
		"<",
		">",
		"<=",
		">=",
		"+",
		"-",
		"*",
		"/",
		"%",
		"&",
		"|",
		"^",
		"~",
		"<<",
		">>",
		">>>",
		"&&",
		"||",
		"!",
	],
	symbols: /[=><!~?:&|+\-*/^%]+/,
	tokenizer: {
		root: [
			[
				/[a-zA-Z_]\w*/,
				{
					cases: {
						"@keywords": "keyword",
						"@typeKeywords": "type",
						"@default": "identifier",
					},
				},
			],
			[/'[a-zA-Z_]\w*/, "annotation"],
			[/[{}()[\]]/, "@brackets"],
			[/@symbols/, { cases: { "@operators": "operator", "@default": "" } }],
			[/\d[\d_]*/, "number"],
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
			{
				label: "module",
				insertText: "module ${1:Name} (\n    ${2}\n) {\n    ${0}\n}",
				detail: "Module declaration",
			},
			{
				label: "always_ff",
				insertText:
					"always_ff (${1:clk}, ${2:rst}) {\n    if_reset {\n        ${3}\n    } else {\n        ${0}\n    }\n}",
				detail: "Sequential block",
			},
			{
				label: "always_comb",
				insertText: "always_comb {\n    ${0}\n}",
				detail: "Combinational block",
			},
			{
				label: "assign",
				insertText: "assign ${1:out} = ${0};",
				detail: "Continuous assignment",
			},
			{
				label: "if_reset",
				insertText: "if_reset {\n    ${1}\n} else {\n    ${0}\n}",
				detail: "Reset branch",
			},
		];
		return {
			suggestions: [
				...snippets.map((s) => ({
					label: s.label,
					kind: monaco.languages.CompletionItemKind.Snippet,
					insertText: s.insertText,
					insertTextRules:
						monaco.languages.CompletionItemInsertTextRule.InsertAsSnippet,
					detail: s.detail,
					range,
				})),
			],
		};
	},
});

monaco.editor.defineTheme("celox-dark", {
	base: "vs-dark",
	inherit: true,
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
  .panels { display: grid; grid-template-columns: 1fr 1fr; height: calc(100vh - 42px); }
  .panel { display: flex; flex-direction: column; border: 1px solid #0f3460; overflow: hidden; }
  .tab-bar { display: flex; background: #16213e; border-bottom: 1px solid #0f3460; flex-shrink: 0; overflow-x: auto; }
  .tab-bar::-webkit-scrollbar { height: 0; }
  .tab { padding: 5px 14px; font-size: 0.75rem; font-family: system-ui, sans-serif; color: #666; cursor: pointer; border-right: 1px solid #0f3460; white-space: nowrap; display: flex; align-items: center; gap: 6px; user-select: none; }
  .tab:hover { background: #1a1a2e; color: #999; }
  .tab.active { background: #0d1117; color: #c9d1d9; }
  .tab .folder { color: #555; }
  .tab .tab-close { display: inline-flex; align-items: center; justify-content: center; width: 14px; height: 14px; border-radius: 3px; font-size: 0.65rem; color: #555; cursor: pointer; margin-left: 4px; line-height: 1; }
  .tab .tab-close:hover { background: #e9456040; color: #f85149; }
  .tab-add { padding: 5px 10px; font-size: 0.85rem; color: #555; cursor: pointer; border-right: none; display: flex; align-items: center; user-select: none; flex-shrink: 0; }
  .tab-add:hover { color: #c9d1d9; background: #1a1a2e; }
  .tab-rename { background: #0d1117; color: #c9d1d9; border: 1px solid #e94560; outline: none; font-size: 0.75rem; font-family: system-ui, sans-serif; padding: 0 4px; width: 160px; }
  .editor-container { flex: 1; }
  .panel-hdr { background: #16213e; padding: 3px 10px; font-size: 0.7rem; font-weight: 600; color: #666; text-transform: uppercase; letter-spacing: 0.05em; flex-shrink: 0; }
  #console { flex: 1; background: #0d1117; color: #c9d1d9; padding: 8px 10px; font-family: 'Fira Code', monospace; font-size: 0.8rem; line-height: 1.4; overflow-y: auto; white-space: pre-wrap; }
  .log-info { color: #58a6ff; } .log-error { color: #f85149; } .log-success { color: #3fb950; }
  .right-tabs { display: flex; background: #16213e; border-bottom: 1px solid #0f3460; flex-shrink: 0; }
  .right-tab { padding: 4px 14px; font-size: 0.7rem; font-weight: 600; color: #666; text-transform: uppercase; letter-spacing: 0.05em; cursor: pointer; user-select: none; border-right: 1px solid #0f3460; }
  .right-tab:hover { color: #999; }
  .right-tab.active { background: #0d1117; color: #c9d1d9; }
  .right-content { display: none; flex: 1; overflow: hidden; }
  .right-content.active { display: flex; flex-direction: column; }
  #waveform-toolbar { display: flex; gap: 4px; padding: 3px 8px; background: #16213e; border-bottom: 1px solid #0f3460; flex-shrink: 0; }
  #waveform-toolbar button { font-size: 0.7rem; padding: 2px 8px; background: #1a1a2e; color: #c9d1d9; border-color: #0f3460; font-weight: 400; }
  #waveform-toolbar button:hover { background: #0f3460; }
  #waveform-container { flex: 1; overflow: hidden; }
  .wv-badge { display: none; font-size: 0.55rem; background: #3fb950; color: #0d1117; border-radius: 6px; padding: 0 4px; margin-left: 4px; font-weight: 700; }
  .wv-badge.visible { display: inline; }
</style>
<header>
  <h1>Celox Playground</h1>
  <select id="examples"><option value="">-- Example --</option><option value="adder">Adder</option><option value="counter">Counter (Simulator)</option><option value="counter_sim">Counter (Simulation)</option><option value="counter_vcd">Counter (Waveform)</option></select>
  <select id="run-target"></select>
  <input id="run-args" type="text" placeholder="vitest args (e.g. --grep &quot;add&quot;)" style="width: 180px; font-size: 0.8rem;" />
  <button id="run" disabled>Run</button>
  <span class="status" id="status">Loading WASM…</span>
</header>
<div class="panels">
  <div class="panel">
    <div class="tab-bar" id="tab-bar"></div>
    <div class="editor-container" id="editor"></div>
  </div>
  <div class="panel">
    <div class="right-tabs">
      <div class="right-tab active" data-tab="console">Console</div>
      <div class="right-tab" data-tab="waveform">Waveform<span class="wv-badge" id="wv-badge"></span></div>
    </div>
    <div id="console-panel" class="right-content active">
      <div id="console"></div>
    </div>
    <div id="waveform-panel" class="right-content">
      <div id="waveform-toolbar">
        <button id="wv-zoom-in">Zoom +</button>
        <button id="wv-zoom-out">Zoom -</button>
        <button id="wv-fit">Fit</button>
        <button id="wv-download">Download VCD</button>
      </div>
      <div id="waveform-container"></div>
    </div>
  </div>
</div>`;

const consoleEl = document.getElementById("console")!;
const runBtn = document.getElementById("run") as HTMLButtonElement;
const statusEl = document.getElementById("status")!;
const examplesEl = document.getElementById("examples") as HTMLSelectElement;
const runTargetEl = document.getElementById("run-target") as HTMLSelectElement;
const runArgsEl = document.getElementById("run-args") as HTMLInputElement;
const tabBarEl = document.getElementById("tab-bar")!;
const consolePanelEl = document.getElementById("console-panel")!;
const waveformPanelEl = document.getElementById("waveform-panel")!;
const wvBadgeEl = document.getElementById("wv-badge")!;

function appendConsole(msg: string, cls = "") {
	const span = document.createElement("span");
	if (cls) span.className = cls;
	span.textContent = `${msg}\n`;
	consoleEl.appendChild(span);
	consoleEl.scrollTop = consoleEl.scrollHeight;
}
function clearConsole() {
	consoleEl.innerHTML = "";
}

// ── Right-panel tab switching ──────────────────────────────

const rightTabs = document.querySelectorAll<HTMLElement>(".right-tab");
function switchRightTab(tabName: string) {
	for (const t of rightTabs) {
		t.classList.toggle("active", t.dataset.tab === tabName);
	}
	consolePanelEl.classList.toggle("active", tabName === "console");
	waveformPanelEl.classList.toggle("active", tabName === "waveform");
	if (tabName === "waveform") waveformViewer.render();
}
for (const t of rightTabs) {
	t.addEventListener("click", () => switchRightTab(t.dataset.tab!));
}

// ── Waveform viewer ────────────────────────────────────────

const waveformViewer = new WaveformViewer(
	document.getElementById("waveform-container")!,
);
let currentVcdTrace: VcdTrace | null = null;

// VCD recording state — reset at the start of each run()
let vcdSignals: VcdSignalInfo[] = [];
let vcdSnapshots: VcdSnapshot[] = [];

document.getElementById("wv-zoom-in")!.addEventListener("click", () => waveformViewer.zoomIn());
document.getElementById("wv-zoom-out")!.addEventListener("click", () => waveformViewer.zoomOut());
document.getElementById("wv-fit")!.addEventListener("click", () => waveformViewer.fit());
document.getElementById("wv-download")!.addEventListener("click", () => {
	if (!currentVcdTrace) return;
	const text = generateVcdText(currentVcdTrace);
	const blob = new Blob([text], { type: "text/plain" });
	const url = URL.createObjectURL(blob);
	const a = document.createElement("a");
	a.href = url;
	a.download = "dump.vcd";
	a.click();
	URL.revokeObjectURL(url);
});

// ── Monaco editor (single instance, model switching) ────

const editorOpts: monaco.editor.IStandaloneEditorConstructionOptions = {
	theme: "celox-dark",
	fontSize: 13,
	fontFamily: "'Fira Code', monospace",
	minimap: { enabled: false },
	scrollBeyondLastLine: false,
	automaticLayout: true,
	tabSize: 4,
	padding: { top: 8 },
};

// Configure TS compiler options for testbench files
monaco.languages.typescript.typescriptDefaults.setCompilerOptions({
	target: monaco.languages.typescript.ScriptTarget.ESNext,
	moduleResolution: monaco.languages.typescript.ModuleResolutionKind.NodeJs,
	allowArbitraryExtensions: true,
	strict: false,
	noEmit: true,
});
monaco.languages.typescript.typescriptDefaults.setDiagnosticsOptions({
	diagnosticsOptions: {
		noSemanticValidation: false,
		noSyntaxValidation: false,
	},
});

const editor = monaco.editor.create(document.getElementById("editor")!, {
	...editorOpts,
	language: "veryl",
	value: "",
});

// ── File / Tab management ───────────────────────────────

interface PlaygroundFile {
	path: string;
	model: monaco.editor.ITextModel;
	viewState: monaco.editor.ICodeEditorViewState | null;
}

const files = new Map<string, PlaygroundFile>();
let activeFilePath: string | null = null;

function langForPath(path: string): string {
	if (path.endsWith(".veryl")) return "veryl";
	if (path.endsWith(".ts") || path.endsWith(".tsx")) return "typescript";
	if (path.endsWith(".js")) return "javascript";
	if (path.endsWith(".toml")) return "toml";
	return "plaintext";
}

function createFile(path: string, content: string): PlaygroundFile {
	const existing = files.get(path);
	if (existing) {
		existing.model.setValue(content);
		return existing;
	}
	const lang = langForPath(path);
	const uri = monaco.Uri.parse(`file:///${path}`);
	const model = monaco.editor.createModel(content, lang, uri);
	const file: PlaygroundFile = { path, model, viewState: null };
	files.set(path, file);

	// Listen for changes on .veryl files
	if (lang === "veryl") {
		model.onDidChangeContent(() => onVerylChange());
	}

	return file;
}

function removeAllFiles() {
	for (const f of files.values()) f.model.dispose();
	files.clear();
	activeFilePath = null;
}

function removeFile(path: string) {
	const file = files.get(path);
	if (!file) return;
	file.model.dispose();
	files.delete(path);

	// If the closed tab was active, switch to an adjacent one
	if (activeFilePath === path) {
		const remaining = [...files.keys()];
		activeFilePath = remaining.length > 0 ? remaining[remaining.length - 1] : null;
		if (activeFilePath) {
			const next = files.get(activeFilePath)!;
			editor.setModel(next.model);
			if (next.viewState) editor.restoreViewState(next.viewState);
		} else {
			editor.setModel(null);
		}
	}
	renderTabs();
}

function activateFile(path: string) {
	const file = files.get(path);
	if (!file) return;

	// Save current view state
	if (activeFilePath) {
		const prev = files.get(activeFilePath);
		if (prev) prev.viewState = editor.saveViewState();
	}

	activeFilePath = path;
	editor.setModel(file.model);
	if (file.viewState) editor.restoreViewState(file.viewState);
	editor.focus();
	renderTabs();
}

function renderTabs() {
	tabBarEl.innerHTML = "";
	for (const [path] of files) {
		const tab = document.createElement("div");
		tab.className = `tab${path === activeFilePath ? " active" : ""}`;

		// Show folder/file split
		const lastSlash = path.lastIndexOf("/");
		if (lastSlash >= 0) {
			const folderSpan = document.createElement("span");
			folderSpan.className = "folder";
			folderSpan.textContent = path.substring(0, lastSlash + 1);
			tab.appendChild(folderSpan);
			tab.appendChild(document.createTextNode(path.substring(lastSlash + 1)));
		} else {
			tab.textContent = path;
		}

		// Close button
		const closeBtn = document.createElement("span");
		closeBtn.className = "tab-close";
		closeBtn.textContent = "\u00d7";
		closeBtn.title = "Close";
		closeBtn.addEventListener("click", (e) => {
			e.stopPropagation();
			removeFile(path);
		});
		tab.appendChild(closeBtn);

		tab.addEventListener("click", () => activateFile(path));
		tab.addEventListener("dblclick", (e) => {
			e.stopPropagation();
			startRename(path, tab);
		});
		tabBarEl.appendChild(tab);
	}

	// "+" button for new file
	const addBtn = document.createElement("div");
	addBtn.className = "tab-add";
	addBtn.textContent = "+";
	addBtn.title = "New file";
	addBtn.addEventListener("click", promptNewFile);
	tabBarEl.appendChild(addBtn);

	updateRunTargets();
}

function isTestFile(path: string): boolean {
	return /\.test\.[tj]sx?$/.test(path);
}

function updateRunTargets() {
	const prev = runTargetEl.value;
	runTargetEl.innerHTML = "";

	// "All tests" runs every .test.ts file
	const allOpt = document.createElement("option");
	allOpt.value = "*";
	allOpt.textContent = "All tests";
	runTargetEl.appendChild(allOpt);

	for (const [path] of files) {
		if (!isTestFile(path)) continue;
		const opt = document.createElement("option");
		opt.value = path;
		opt.textContent = path;
		runTargetEl.appendChild(opt);
	}
	// Restore previous selection if still valid
	if (prev && [...runTargetEl.options].some((o) => o.value === prev)) {
		runTargetEl.value = prev;
	}
}

function startRename(oldPath: string, tabEl: HTMLElement) {
	const input = document.createElement("input");
	input.className = "tab-rename";
	input.value = oldPath;
	// Select just the filename part
	const lastSlash = oldPath.lastIndexOf("/");
	tabEl.innerHTML = "";
	tabEl.appendChild(input);
	input.focus();
	input.setSelectionRange(lastSlash + 1, oldPath.length);

	const commit = () => {
		const newPath = input.value.trim();
		if (newPath && newPath !== oldPath) {
			renameFile(oldPath, newPath);
		} else {
			renderTabs();
		}
	};
	input.addEventListener("blur", commit);
	input.addEventListener("keydown", (e) => {
		if (e.key === "Enter") {
			e.preventDefault();
			input.blur();
		}
		if (e.key === "Escape") {
			input.removeEventListener("blur", commit);
			renderTabs();
		}
	});
}

function renameFile(oldPath: string, newPath: string) {
	if (files.has(newPath)) {
		renderTabs();
		return;
	} // Don't overwrite existing
	const file = files.get(oldPath);
	if (!file) {
		renderTabs();
		return;
	}

	const content = file.model.getValue();
	const viewState = file.viewState;
	const wasActive = activeFilePath === oldPath;

	// Remove old
	file.model.dispose();
	files.delete(oldPath);

	// Create new
	const newFile = createFile(newPath, content);
	newFile.viewState = viewState;

	if (wasActive) {
		activeFilePath = newPath;
		editor.setModel(newFile.model);
		if (newFile.viewState) editor.restoreViewState(newFile.viewState);
	}
	renderTabs();
}

function promptNewFile() {
	// Insert a temporary input in the tab bar
	const input = document.createElement("input");
	input.className = "tab-rename";
	input.value = "src/";
	input.placeholder = "path/to/file.veryl";

	// Insert before the "+" button
	const addBtn = tabBarEl.querySelector(".tab-add")!;
	tabBarEl.insertBefore(input, addBtn);
	input.focus();
	input.setSelectionRange(input.value.length, input.value.length);

	const commit = () => {
		const path = input.value.trim();
		if (path && !files.has(path)) {
			createFile(path, "");
			activateFile(path);
		} else {
			renderTabs();
		}
	};
	input.addEventListener("blur", commit);
	input.addEventListener("keydown", (e) => {
		if (e.key === "Enter") {
			e.preventDefault();
			input.blur();
		}
		if (e.key === "Escape") {
			input.removeEventListener("blur", commit);
			renderTabs();
		}
	});
}

// ── vitest type declarations for Monaco ─────────────────

monaco.languages.typescript.typescriptDefaults.addExtraLib(
	`
declare module "vitest" {
  export function describe(name: string, fn: () => void): void;
  export namespace describe { export function skip(name: string, fn: () => void): void; }
  export function it(name: string, fn: () => void | Promise<void>): void;
  export namespace it { export function skip(name: string, fn: () => void | Promise<void>): void; }
  export const test: typeof it;
  export function beforeEach(fn: () => void | Promise<void>): void;
  export function afterEach(fn: () => void | Promise<void>): void;
  export function beforeAll(fn: () => void | Promise<void>): void;
  export function afterAll(fn: () => void | Promise<void>): void;
  interface Assertion<T = any> {
    toBe(expected: T): void;
    toEqual(expected: T): void;
    toStrictEqual(expected: T): void;
    toBeTruthy(): void;
    toBeFalsy(): void;
    toBeNull(): void;
    toBeUndefined(): void;
    toBeDefined(): void;
    toBeNaN(): void;
    toBeGreaterThan(expected: number | bigint): void;
    toBeGreaterThanOrEqual(expected: number | bigint): void;
    toBeLessThan(expected: number | bigint): void;
    toBeLessThanOrEqual(expected: number | bigint): void;
    toContain(expected: any): void;
    toHaveLength(expected: number): void;
    toMatch(expected: string | RegExp): void;
    toThrow(expected?: string | RegExp): void;
    toThrowError(expected?: string | RegExp): void;
    toHaveProperty(key: string, value?: any): void;
    not: Assertion<T>;
  }
  export function expect<T>(actual: T): Assertion<T>;
}
`,
	"file:///vitest.d.ts",
);

// ── @celox-sim/celox type registration (from real .d.ts) ──

// Rewrite relative imports in .d.ts to match Monaco's virtual FS paths
function fixDtsImports(content: string): string {
	return content.replace(
		/from\s+["']\.\/(\w+)\.js["']/g,
		'from "@celox-sim/celox/$1"',
	);
}

// Register all @celox-sim/celox .d.ts files so Monaco resolves imports
const celoxDtsFiles: Record<string, string> = {
	types: celoxTypesDts,
	simulator: celoxSimulatorDts,
	simulation: celoxSimulationDts,
	index: celoxIndexDts,
	dut: celoxDutDts,
	"napi-helpers": celoxNapiHelpersDts,
	"wasm-bridge": celoxWasmBridgeDts,
	"napi-bridge": celoxNapiBridgeDts,
};
for (const [name, content] of Object.entries(celoxDtsFiles)) {
	const fixed = fixDtsImports(content);
	// Register as both the sub-module path and the node_modules-style path
	monaco.languages.typescript.typescriptDefaults.addExtraLib(
		fixed,
		`file:///node_modules/@celox-sim/celox/dist/${name}.d.ts`,
	);
	// Also register as a bare module specifier for "from '@celox-sim/celox/xxx'" resolution
	monaco.languages.typescript.typescriptDefaults.addExtraLib(
		fixed,
		`file:///node_modules/@celox-sim/celox/${name}.d.ts`,
	);
}

// ── DUT type injection (.veryl module declarations) ─────

let currentExtraLib: monaco.IDisposable | null = null;

function getVerylSource(): string {
	for (const f of files.values()) {
		if (f.path.endsWith(".veryl")) return f.model.getValue();
	}
	return "";
}

function getVerylModel(): monaco.editor.ITextModel | null {
	for (const f of files.values()) {
		if (f.path.endsWith(".veryl")) return f.model;
	}
	return null;
}

function getTestModels(): { path: string; model: monaco.editor.ITextModel }[] {
	const target = runTargetEl.value;
	if (target === "*") {
		// All test files
		const result: { path: string; model: monaco.editor.ITextModel }[] = [];
		for (const [path, f] of files) {
			if (isTestFile(path)) result.push({ path, model: f.model });
		}
		return result;
	}
	const f = files.get(target);
	if (f) return [{ path: target, model: f.model }];
	return [];
}

function updateDutTypes(
	ports: Record<string, { direction: string; width: number }>,
) {
	if (currentExtraLib) currentExtraLib.dispose();

	// Extract module name from Veryl source
	const topMatch = getVerylSource().match(/module\s+(\w+)/);
	const moduleName = topMatch?.[1] || "Top";

	const portEntries = Object.entries(ports)
		.filter(([_, p]) => p.direction === "input" || p.direction === "output")
		.map(([name, _]) => `    ${name}: bigint;`)
		.join("\n");

	// Only generate .veryl module declarations — @celox-sim/celox types are
	// registered once at startup from the real .d.ts files.
	const dts = `
declare module "./${moduleName}.veryl" {
  import type { ModuleDefinition } from "@celox-sim/celox";
  export interface ${moduleName}Ports {
${portEntries}
  }
  export const ${moduleName}: ModuleDefinition<${moduleName}Ports>;
}

declare module "../src/${moduleName}.veryl" {
  import type { ModuleDefinition } from "@celox-sim/celox";
  export interface ${moduleName}Ports {
${portEntries}
  }
  export const ${moduleName}: ModuleDefinition<${moduleName}Ports>;
}
`;
	currentExtraLib = monaco.languages.typescript.typescriptDefaults.addExtraLib(
		dts,
		"file:///celox-sim.d.ts",
	);

	// Also register the .veryl module as a virtual file so TS can resolve the import
	const verylModuleDts = `
import type { ModuleDefinition } from "@celox-sim/celox";
export interface ${moduleName}Ports {
${portEntries}
}
export declare const ${moduleName}: ModuleDefinition<${moduleName}Ports>;
`;
	// Register under multiple paths to ensure resolution works
	// test/foo.test.ts imports "../src/Adder.veryl" → resolves to file:///src/Adder.veryl
	monaco.languages.typescript.typescriptDefaults.addExtraLib(
		verylModuleDts,
		`file:///src/${moduleName}.veryl`,
	);
	monaco.languages.typescript.typescriptDefaults.addExtraLib(
		verylModuleDts,
		`file:///src/${moduleName}.d.veryl.ts`,
	);
}

// Extract port names from Veryl source with regex (works without WASM)
function extractPortsFromSource(
	source: string,
): Record<string, { direction: string; width: number }> {
	const ports: Record<string, { direction: string; width: number }> = {};
	// Match: name: input/output logic<N> or clock/reset
	const portRe =
		/(\w+)\s*:\s*(input|output|inout)\s+(?:'[_a-zA-Z]*\s+)?(logic|bit|clock|reset)(?:<(\d+)>)?/g;
	let m;
	while ((m = portRe.exec(source)) !== null) {
		ports[m[1]] = { direction: m[2], width: m[4] ? parseInt(m[4], 10) : 1 };
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
	const sourceLines = getVerylSource().split("\n");

	let i = 0;
	while ((m = locRe.exec(errorMsg)) !== null) {
		const line = parseInt(m[1], 10);
		const col = parseInt(m[2], 10);

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
		const simple = errorMsg.match(
			/(?:Parse error|Unexpected token):\s*(.+?)(?:\n|$)/,
		);
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
		const source = getVerylSource();
		const model = getVerylModel();

		// Try WASM-based analysis (accurate types + diagnostics)
		if (celox) {
			try {
				const result = JSON.parse(
					celox.genTsFromSource([{ content: source, path: "main.veryl" }]),
				);
				if (result.modules?.[0]?.ports) {
					updateDutTypes(result.modules[0].ports);
				}
				// Show structured diagnostics (if any) as Monaco markers
				if (model) {
					const diags: monaco.editor.IMarkerData[] = (
						result.diagnostics || []
					).map((d: any) => ({
						severity:
							d.severity === "error"
								? monaco.MarkerSeverity.Error
								: monaco.MarkerSeverity.Warning,
						message:
							d.message +
							(d.help ? `\n${d.help}` : "") +
							(d.url ? `\n${d.url}` : ""),
						startLineNumber: d.line,
						startColumn: d.column,
						endLineNumber: d.endLine ?? d.line,
						endColumn: d.endColumn ?? d.column + 1,
					}));
					// Fallback to string-based parsing for warnings field
					const legacyWarnings: monaco.editor.IMarkerData[] = (
						result.warnings || []
					)
						.flatMap((w: string) => parseVerylErrors(w))
						.map((m: monaco.editor.IMarkerData) => ({
							...m,
							severity: monaco.MarkerSeverity.Warning,
						}));
					monaco.editor.setModelMarkers(model, "veryl", [
						...diags,
						...legacyWarnings,
					]);
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
		appendConsole(`[error] Failed to load WASM: ${e.message}`, "log-error");
	}
}

// ── vitest expect setup ─────────────────────────────────

chai.use(JestChaiExpect);
chai.use(JestAsymmetricMatchers);
const expect = chai.expect as any;

// ── Run ─────────────────────────────────────────────────

async function run() {
	clearConsole();
	runBtn.disabled = true;
	statusEl.textContent = "Compiling…";

	// Reset VCD recording state
	vcdSignals = [];
	vcdSnapshots = [];
	currentVcdTrace = null;
	wvBadgeEl.classList.remove("visible");
	wvBadgeEl.textContent = "";

	const targetPath = runTargetEl.value;
	if (!targetPath) {
		appendConsole("[error] No test file selected", "log-error");
		statusEl.textContent = "Error";
		runBtn.disabled = false;
		return;
	}

	try {
		const verylSource = getVerylSource();
		const topMatch = verylSource.match(/module\s+(\w+)/);
		if (!topMatch) throw new Error("No module found in Veryl source");
		const topName = topMatch[1];

		const t0 = performance.now();
		const genTsResult = (() => {
			try {
				return JSON.parse(
					celox.genTsFromSource([{ content: verylSource, path: "main.veryl" }]),
				);
			} catch {
				return null;
			}
		})();

		const handle = new celox.NativeSimulatorHandle(
			[{ content: verylSource, path: "main.veryl" }],
			topName,
		);
		const layout = JSON.parse(handle.layoutJson);
		const events = JSON.parse(handle.eventsJson);
		const totalSize = handle.totalSize;
		const t1 = performance.now();
		appendConsole(
			`[compile] ${(t1 - t0).toFixed(0)}ms — ${Object.keys(layout).length} signals, ${Object.keys(events).length} events`,
			"log-success",
		);

		// Build simulation factory (creates fresh instance per call)
		function createSim() {
			const pages = Math.max(1, Math.ceil(totalSize / 65536));
			const memory = new WebAssembly.Memory({ initial: pages });
			const combModule = new WebAssembly.Module(
				new Uint8Array(handle.combWasmBytes()),
			);
			const combInst = new WebAssembly.Instance(combModule, {
				env: { memory },
			});

			// Build event instances and id→name mapping (mirrors real Simulator)
			const eventNames = Object.keys(events);
			const eventInsts: Record<number, WebAssembly.Instance> = {};
			for (const name of eventNames) {
				const id: number = events[name];
				try {
					const mod = new WebAssembly.Module(
						new Uint8Array(handle.eventWasmBytes(name)),
					);
					eventInsts[id] = new WebAssembly.Instance(mod, { env: { memory } });
				} catch {}
			}
			const defaultEventId = eventNames.length > 0 ? events[eventNames[0]] : -1;

			const view = new DataView(memory.buffer);
			let dirty = false;

			const dut = new Proxy({} as Record<string, bigint>, {
				get(_, prop: string) {
					const sig = layout[prop];
					if (!sig) return undefined;
					// Lazy evalComb on output reads (matches real Simulator DUT behavior)
					if (dirty && sig.direction !== "input") {
						(combInst.exports.run as Function)();
						dirty = false;
					}
					let v = 0n;
					for (let i = sig.byte_size - 1; i >= 0; i--)
						v = (v << 8n) | BigInt(view.getUint8(sig.offset + i));
					if (sig.width < 64) v &= (1n << BigInt(sig.width)) - 1n;
					return v;
				},
				set(_, prop: string, value: bigint) {
					const sig = layout[prop];
					if (!sig) throw new Error(`Signal '${prop}' not found`);
					let v = BigInt(value);
					for (let i = 0; i < sig.byte_size; i++) {
						view.setUint8(sig.offset + i, Number(v & 0xffn));
						v >>= 8n;
					}
					dirty = true;
					return true;
				},
			});

			function evalComb() {
				(combInst.exports.run as Function)();
				dirty = false;
			}

			function tickOne(eventId: number) {
				if (dirty) evalComb();
				const inst = eventInsts[eventId];
				if (inst) (inst.exports.run as Function)();
				(combInst.exports.run as Function)();
				dirty = false;
			}

			return {
				dut,
				warnings: [] as readonly string[],
				evalComb,
				tick(
					eventOrCount?: { name: string; id: number } | number,
					count?: number,
				) {
					let eventId: number;
					let ticks: number;
					if (typeof eventOrCount === "object" && eventOrCount !== null) {
						eventId = eventOrCount.id;
						ticks = count ?? 1;
					} else if (typeof eventOrCount === "number") {
						eventId = defaultEventId;
						ticks = eventOrCount;
					} else {
						eventId = defaultEventId;
						ticks = 1;
					}
					for (let i = 0; i < ticks; i++) tickOne(eventId);
				},
				event(name: string): { name: string; id: number } {
					const id = events[name];
					if (id === undefined) {
						throw new Error(
							`Unknown event '${name}'. Available: ${eventNames.join(", ")}`,
						);
					}
					return { name, id };
				},
				fourState(portName: string): {
					__fourState: true;
					value: bigint;
					mask: bigint;
				} {
					const sig = layout[portName];
					if (!sig) {
						throw new Error(
							`Unknown port '${portName}'. Available: ${Object.keys(layout).join(", ")}`,
						);
					}
					let value = 0n;
					for (let i = sig.byte_size - 1; i >= 0; i--)
						value = (value << 8n) | BigInt(view.getUint8(sig.offset + i));
					if (sig.width < 64) value &= (1n << BigInt(sig.width)) - 1n;
					// 4-state mask lives immediately after value bytes
					let mask = 0n;
					if (sig.is_4state) {
						for (let i = sig.byte_size - 1; i >= 0; i--)
							mask =
								(mask << 8n) |
								BigInt(view.getUint8(sig.offset + sig.byte_size + i));
						if (sig.width < 64) mask &= (1n << BigInt(sig.width)) - 1n;
					}
					return { __fourState: true, value, mask };
				},
				dump(timestamp: number) {
					if (vcdSignals.length === 0) {
						for (const [name, sig] of Object.entries(layout)) {
							vcdSignals.push({ name, width: sig.width });
						}
					}
					if (dirty) { evalComb(); }
					const values: bigint[] = [];
					for (const sig of Object.values(layout)) {
						let v = 0n;
						for (let i = sig.byte_size - 1; i >= 0; i--)
							v = (v << 8n) | BigInt(view.getUint8(sig.offset + i));
						if (sig.width < 64) v &= (1n << BigInt(sig.width)) - 1n;
						values.push(v);
					}
					vcdSnapshots.push({ timestamp, values });
				},
				dispose() {},
			};
		}

		// Build module definitions from genTsFromSource result for .veryl imports
		const moduleNames = (genTsResult?.modules || [])
			.map((m: any) => m.moduleName)
			.filter(Boolean);

		const Simulator = {
			create(_module: any) {
				return createSim();
			},
		};

		// Time-based Simulation: event queue with signal toggling on top of WASM bridge
		// Mirrors the native Simulation's step() logic:
		//   1. Pop all events at current time
		//   2. Write signal values to memory
		//   3. Detect edges (posedge/negedge) and fire triggered FF handlers
		//   4. Evaluate combinational logic
		//   5. Reschedule recurring clocks with toggled value
		function createSimulation() {
			const pages = Math.max(1, Math.ceil(totalSize / 65536));
			const memory = new WebAssembly.Memory({ initial: pages });
			const combModule = new WebAssembly.Module(
				new Uint8Array(handle.combWasmBytes()),
			);
			const combInst = new WebAssembly.Instance(combModule, {
				env: { memory },
			});

			const eventNames = Object.keys(events);
			const eventInsts: Record<number, WebAssembly.Instance> = {};
			for (const name of eventNames) {
				const id: number = events[name];
				try {
					const mod = new WebAssembly.Module(
						new Uint8Array(handle.eventWasmBytes(name)),
					);
					eventInsts[id] = new WebAssembly.Instance(mod, { env: { memory } });
				} catch {}
			}

			const view = new DataView(memory.buffer);

			// Determine edge type for each event from layout type_kind
			const edgeType: Record<number, "posedge" | "negedge"> = {};
			for (const name of eventNames) {
				const id: number = events[name];
				const sig = layout[name];
				if (!sig) continue;
				const tk: string = sig.type_kind ?? "logic";
				if (tk === "reset_async_low" || tk === "reset_sync_low") {
					edgeType[id] = "negedge";
				} else {
					edgeType[id] = "posedge"; // clock, reset_async_high, etc.
				}
			}

			// Event queue entry: carries the signal name, next value, and optional clock reschedule info
			type QueueEntry = {
				time: number;
				eventId: number;
				signalName: string;
				nextVal: number;
				clockHalfPeriod?: number; // if set, reschedule with toggled value
			};
			const queue: QueueEntry[] = [];
			const clocks = new Map<string, { period: number; eventId: number }>();
			let currentTime = 0;
			// Track last signal values for edge detection
			const lastValues: Record<number, number> = {};

			function enqueue(entry: QueueEntry) {
				let lo = 0,
					hi = queue.length;
				while (lo < hi) {
					const mid = (lo + hi) >>> 1;
					if (queue[mid].time <= entry.time) lo = mid + 1;
					else hi = mid;
				}
				queue.splice(lo, 0, entry);
			}

			function writeSignal(name: string, val: number) {
				const sig = layout[name];
				if (sig) view.setUint8(sig.offset, val);
			}

			function resolveEvent(name: string): number {
				const id = events[name];
				if (id === undefined) {
					throw new Error(
						`Unknown event '${name}'. Available: ${eventNames.join(", ")}`,
					);
				}
				return id;
			}

			// Process one time step: pop all events at next time, write values, detect edges, fire FFs
			function processStep(): number | null {
				if (queue.length === 0) return null;
				const t = queue[0].time;
				// Collect all events at this time
				const batch: QueueEntry[] = [];
				while (queue.length > 0 && queue[0].time === t) {
					batch.push(queue.shift()!);
				}
				currentTime = t;

				// Phase 1: Write signal values to memory
				for (const ev of batch) {
					writeSignal(ev.signalName, ev.nextVal);
				}

				// Phase 2: Edge detection + fire triggered FFs
				for (const ev of batch) {
					const prevVal = lastValues[ev.eventId] ?? 0;
					const curVal = ev.nextVal;
					const edge = edgeType[ev.eventId] ?? "posedge";
					const triggered =
						edge === "posedge"
							? prevVal === 0 && curVal !== 0
							: prevVal !== 0 && curVal === 0;
					if (triggered) {
						const inst = eventInsts[ev.eventId];
						if (inst) (inst.exports.run as Function)();
					}
					lastValues[ev.eventId] = curVal;
				}

				// Phase 3: Evaluate combinational logic
				(combInst.exports.run as Function)();

				// Phase 4: Reschedule clocks with toggled value
				for (const ev of batch) {
					if (ev.clockHalfPeriod != null) {
						enqueue({
							time: currentTime + ev.clockHalfPeriod,
							eventId: ev.eventId,
							signalName: ev.signalName,
							nextVal: 1 - ev.nextVal,
							clockHalfPeriod: ev.clockHalfPeriod,
						});
					}
				}

				return currentTime;
			}

			// DUT proxy with lazy evalComb on output reads
			let simDirty = false;
			const dut = new Proxy({} as Record<string, bigint>, {
				get(_, prop: string) {
					const sig = layout[prop];
					if (!sig) return undefined;
					if (simDirty && sig.direction !== "input") {
						(combInst.exports.run as Function)();
						simDirty = false;
					}
					let v = 0n;
					for (let i = sig.byte_size - 1; i >= 0; i--)
						v = (v << 8n) | BigInt(view.getUint8(sig.offset + i));
					if (sig.width < 64) v &= (1n << BigInt(sig.width)) - 1n;
					return v;
				},
				set(_, prop: string, value: bigint) {
					const sig = layout[prop];
					if (!sig) throw new Error(`Signal '${prop}' not found`);
					let v = BigInt(value);
					for (let i = 0; i < sig.byte_size; i++) {
						view.setUint8(sig.offset + i, Number(v & 0xffn));
						v >>= 8n;
					}
					simDirty = true;
					return true;
				},
			});

			const sim = {
				dut,
				warnings: [] as readonly string[],

				addClock(
					name: string,
					opts: { period: number; initialDelay?: number },
				) {
					const eventId = resolveEvent(name);
					clocks.set(name, { period: opts.period, eventId });
					const delay = opts.initialDelay ?? 0;
					const halfPeriod = opts.period / 2;
					// Schedule first edge (0→1) at delay, then toggles every half-period
					enqueue({
						time: currentTime + delay,
						eventId,
						signalName: name,
						nextVal: 1,
						clockHalfPeriod: halfPeriod,
					});
				},

				schedule(name: string, opts: { time: number; value: number }) {
					const eventId = resolveEvent(name);
					enqueue({
						time: opts.time,
						eventId,
						signalName: name,
						nextVal: opts.value,
					});
				},

				step: processStep,

				time(): number {
					return currentTime;
				},

				nextEventTime(): number | null {
					return queue.length > 0 ? queue[0].time : null;
				},

				runUntil(endTime: number, opts?: { maxSteps?: number }) {
					const max = opts?.maxSteps;
					let steps = 0;
					while (queue.length > 0 && queue[0].time <= endTime) {
						processStep();
						steps++;
						if (max != null && steps >= max) {
							throw new Error(
								`runUntil: exceeded ${max} steps at time ${currentTime} (target ${endTime})`,
							);
						}
					}
					currentTime = endTime;
				},

				waitUntil(
					condition: () => boolean,
					opts?: { maxSteps?: number },
				): number {
					const max = opts?.maxSteps ?? 100_000;
					let steps = 0;
					while (!condition()) {
						if (queue.length === 0) break;
						processStep();
						steps++;
						if (steps >= max) {
							throw new Error(
								`waitUntil: condition not met after ${max} steps at time ${currentTime}`,
							);
						}
					}
					return currentTime;
				},

				waitForCycles(
					clock: string,
					count: number,
					opts?: { maxSteps?: number },
				): number {
					const clkInfo = clocks.get(clock);
					if (!clkInfo)
						throw new Error(
							`No clock registered for '${clock}'. Call addClock() first.`,
						);
					const sig = layout[clock];
					if (!sig) throw new Error(`No layout entry for clock '${clock}'.`);
					const readClk = () => view.getUint8(sig.offset);
					let prev = readClk();
					let remaining = count;
					return sim.waitUntil(() => {
						const curr = readClk();
						if (prev === 0 && curr !== 0) remaining--;
						prev = curr;
						return remaining <= 0;
					}, opts);
				},

				reset(
					signal: string,
					opts?: { activeCycles?: number; duration?: number },
				) {
					const sig = layout[signal];
					if (!sig)
						throw new Error(
							`Unknown port '${signal}'. Available: ${Object.keys(layout).join(", ")}`,
						);
					const typeKind: string = sig.type_kind ?? "";
					if (!typeKind.startsWith("reset")) {
						throw new Error(
							`Port '${signal}' is not a reset signal (type_kind: '${typeKind}').`,
						);
					}
					const isActiveLow =
						typeKind === "reset_async_low" || typeKind === "reset_sync_low";
					const activeValue = isActiveLow ? 0n : 1n;
					const inactiveValue = isActiveLow ? 1n : 0n;

					dut[signal] = activeValue;

					if (opts?.duration != null) {
						sim.runUntil(currentTime + opts.duration);
					} else {
						const cycles = opts?.activeCycles ?? 2;
						const firstClock = clocks.keys().next().value;
						if (firstClock) {
							sim.waitForCycles(firstClock, cycles);
						} else {
							// No clock registered — fire the reset event directly
							const eventId = events[signal];
							if (eventId !== undefined) {
								writeSignal(signal, Number(activeValue));
								const inst = eventInsts[eventId];
								if (inst) (inst.exports.run as Function)();
								(combInst.exports.run as Function)();
							}
						}
					}

					dut[signal] = inactiveValue;
				},

				event(name: string): { name: string; id: number } {
					return { name, id: resolveEvent(name) };
				},

				fourState(portName: string): {
					__fourState: true;
					value: bigint;
					mask: bigint;
				} {
					const sig = layout[portName];
					if (!sig)
						throw new Error(
							`Unknown port '${portName}'. Available: ${Object.keys(layout).join(", ")}`,
						);
					let value = 0n;
					for (let i = sig.byte_size - 1; i >= 0; i--)
						value = (value << 8n) | BigInt(view.getUint8(sig.offset + i));
					if (sig.width < 64) value &= (1n << BigInt(sig.width)) - 1n;
					let mask = 0n;
					if (sig.is_4state) {
						for (let i = sig.byte_size - 1; i >= 0; i--)
							mask =
								(mask << 8n) |
								BigInt(view.getUint8(sig.offset + sig.byte_size + i));
						if (sig.width < 64) mask &= (1n << BigInt(sig.width)) - 1n;
					}
					return { __fourState: true, value, mask };
				},

				dump(timestamp: number) {
					if (vcdSignals.length === 0) {
						for (const [name, sig] of Object.entries(layout)) {
							vcdSignals.push({ name, width: sig.width });
						}
					}
					if (simDirty) {
						(combInst.exports.run as Function)();
						simDirty = false;
					}
					const values: bigint[] = [];
					for (const sig of Object.values(layout)) {
						let v = 0n;
						for (let i = sig.byte_size - 1; i >= 0; i--)
							v = (v << 8n) | BigInt(view.getUint8(sig.offset + i));
						if (sig.width < 64) v &= (1n << BigInt(sig.width)) - 1n;
						values.push(v);
					}
					vcdSnapshots.push({ timestamp, values });
				},

				dispose() {},
			};
			return sim;
		}

		const Simulation = {
			create(_module: any) {
				return createSimulation();
			},
		};

		const moduleBindings: Record<string, any> = {};
		for (const name of moduleNames) {
			moduleBindings[name] = { __celox_module: true, name };
		}

		// Proxy console to playground console panel
		const playgroundConsole = {
			log: (...args: any[]) => appendConsole(args.map(String).join(" ")),
			warn: (...args: any[]) =>
				appendConsole(args.map(String).join(" "), "log-warn"),
			error: (...args: any[]) =>
				appendConsole(args.map(String).join(" "), "log-error"),
			info: (...args: any[]) =>
				appendConsole(args.map(String).join(" "), "log-info"),
		};

		// Parse vitest-style args (--grep "pattern")
		const argsStr = runArgsEl.value.trim();
		let grepPattern: RegExp | null = null;
		if (argsStr) {
			const grepMatch = argsStr.match(
				/(?:--grep|-t)\s+(?:"([^"]+)"|'([^']+)'|(\S+))/,
			);
			if (grepMatch) {
				grepPattern = new RegExp(
					grepMatch[1] || grepMatch[2] || grepMatch[3],
					"i",
				);
			}
		}

		// Collect tests from all target test files
		type TestEntry = {
			name: string;
			fn: () => void | Promise<void>;
			suite: string[];
			file: string;
		};
		const tests: TestEntry[] = [];

		statusEl.textContent = "Running…";
		const tsWorker = await monaco.languages.typescript.getTypeScriptWorker();
		const testModels = getTestModels();
		if (testModels.length === 0) throw new Error("No test files found");

		for (const { path: testPath, model: tbMdl } of testModels) {
			const client = await tsWorker(tbMdl.uri);
			const output = await client.getEmitOutput(tbMdl.uri.toString());
			let jsCode = output.outputFiles[0]?.text ?? tbMdl.getValue();

			// Strip import/export/require — we inject all bindings
			jsCode = jsCode
				.replace(
					/^(?:import|export)\s+.*(?:from\s+)?["'][^"']*["'];?\s*$/gm,
					"",
				)
				.replace(
					/^(?:const|let|var)\s+\{[^}]*\}\s*=\s*require\s*\([^)]*\);?\s*$/gm,
					"",
				);

			const suiteStack: string[] = [];
			function _describe(name: string, fn: () => void) {
				suiteStack.push(name);
				fn();
				suiteStack.pop();
			}
			function _it(name: string, fn: () => void | Promise<void>) {
				tests.push({ name, fn, suite: [...suiteStack], file: testPath });
			}
			_it.skip = (_name: string, _fn: () => void | Promise<void>) => {};
			_describe.skip = (_name: string, _fn: () => void) => {};

			const argNames = [
				"describe",
				"it",
				"test",
				"expect",
				"beforeEach",
				"afterEach",
				"Simulator",
				"Simulation",
				"console",
				...moduleNames,
			];
			const argValues = [
				_describe,
				_it,
				_it,
				expect,
				() => {},
				() => {},
				Simulator,
				Simulation,
				playgroundConsole,
				...moduleNames.map((n: string) => moduleBindings[n]),
			];
			new Function(...argNames, jsCode)(...argValues);
		}

		// Filter tests by grep pattern
		const filteredTests = grepPattern
			? tests.filter((t) => {
					const label = [...t.suite, t.name].join(" > ");
					return grepPattern!.test(label);
				})
			: tests;

		// Run collected tests and display vitest-style results
		let passed = 0;
		let failed = 0;
		const skipped = tests.length - filteredTests.length;
		let lastFile = "";
		const t2 = performance.now();

		for (const t of filteredTests) {
			if (t.file !== lastFile) {
				appendConsole(`\n ${t.file}`, "log-info");
				lastFile = t.file;
			}
			const label = [...t.suite, t.name].join(" > ");
			try {
				await t.fn();
				passed++;
				appendConsole(`   PASS  ${label}`, "log-success");
			} catch (e: any) {
				failed++;
				appendConsole(`   FAIL  ${label}`, "log-error");
				appendConsole(`         ${e.message}`, "log-error");
			}
		}

		const t3 = performance.now();
		appendConsole("", "");
		const fileParts: string[] = [];
		const fileSet = new Set(filteredTests.map((t) => t.file));
		if (failed > 0) fileParts.push(`${fileSet.size} files`);
		const testParts: string[] = [];
		if (passed > 0) testParts.push(`${passed} passed`);
		if (failed > 0) testParts.push(`${failed} failed`);
		if (skipped > 0) testParts.push(`${skipped} skipped`);
		const summary = [
			`Test Files  ${fileSet.size} files`,
			`Tests       ${testParts.join(", ")} (${tests.length})`,
			`Time        ${(t3 - t2).toFixed(0)}ms`,
		];
		appendConsole(summary.join("\n"), failed > 0 ? "log-error" : "log-success");

		statusEl.textContent = failed > 0 ? `${failed} failed` : "Done";

		// If VCD data was recorded, show waveform
		if (vcdSnapshots.length > 0) {
			currentVcdTrace = { signals: vcdSignals, snapshots: vcdSnapshots };
			waveformViewer.setTrace(currentVcdTrace);
			wvBadgeEl.textContent = `${vcdSnapshots.length}`;
			wvBadgeEl.classList.add("visible");
			appendConsole(
				`\n[vcd] ${vcdSnapshots.length} snapshots recorded — switch to Waveform tab`,
				"log-info",
			);
			switchRightTab("waveform");
		}
	} catch (e: any) {
		appendConsole(`[error] ${e.message}`, "log-error");
		statusEl.textContent = "Error";
	} finally {
		runBtn.disabled = false;
	}
}

// ── Events ──────────────────────────────────────────────

function loadExample(name: string) {
	const ex = EXAMPLES[name];
	if (!ex) return;

	// Derive module name from the Veryl source
	const topMatch = ex.veryl.match(/module\s+(\w+)/);
	const moduleName =
		topMatch?.[1] ?? name.charAt(0).toUpperCase() + name.slice(1);
	// Use example key for test file name (e.g. "counter_sim" → "counter_sim.test.ts")
	const testFileName = name;

	removeAllFiles();
	createFile("Veryl.toml", ex.verylToml ?? DEFAULT_VERYL_TOML);
	createFile("celox.toml", ex.celoxToml ?? DEFAULT_CELOX_TOML);
	createFile(`src/${moduleName}.veryl`, ex.veryl);
	createFile(`test/${testFileName}.test.ts`, ex.testbench);
	activateFile(`test/${testFileName}.test.ts`);
	onVerylChange();
}

examplesEl.addEventListener("change", () => {
	if (examplesEl.value) loadExample(examplesEl.value);
});
runBtn.addEventListener("click", run);
document.addEventListener("keydown", (e) => {
	if ((e.ctrlKey || e.metaKey) && e.key === "Enter" && !runBtn.disabled) run();
});

loadExample("adder");
onVerylChange(); // Inject types immediately from regex (no WASM needed)
init();
