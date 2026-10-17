import { chai, JestAsymmetricMatchers, JestChaiExpect } from "@vitest/expect";
import * as monaco from "monaco-editor";
import { CeloxCompilerClient } from "./celox-compiler-client.js";
import { buildMonacoTestbenchCompilerOptions } from "./monaco-testbench-options.js";
import { celoxDtsLibs } from "./playground-dts.js";
import { FourState, X, Z } from "./playground-runtime-helpers.js";
import {
	createPlaygroundSimulation,
	createPlaygroundSimulator,
	type PlaygroundDumpSink,
} from "./playground-simulator.js";
import { transpileTestbench } from "./testbench-transpile.js";
import {
	buildVirtualModuleDts,
	extractPortsFromSource,
	type VirtualPortInfo,
} from "./virtual-module-dts.js";
import {
	generateVcdText,
	type VcdSignalInfo,
	type VcdSnapshot,
	type VcdTrace,
	WaveformViewer,
} from "./waveform-viewer.js";

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
    it("counts up with enable", () => {
        const sim = Simulation.create(Counter);
        sim.addClock("clk", { period: 10 });

        sim.dump(sim.time());
        sim.reset("rst");
        sim.dump(sim.time());

        sim.dut.en = 1n;
        sim.dump(sim.time());

        for (let i = 0; i < 100; i++) {
            const t = sim.step();
            if (t === null) break;
            sim.dump(t);
        }

        expect(sim.dut.count).toBeGreaterThan(0n);
        sim.dispose();
    });

    it("stays zero with enable off", () => {
        const sim = Simulation.create(Counter);
        sim.addClock("clk", { period: 10 });

        sim.dump(sim.time());
        sim.reset("rst");
        sim.dump(sim.time());

        sim.dut.en = 0n;
        sim.dump(sim.time());

        for (let i = 0; i < 60; i++) {
            const t = sim.step();
            if (t === null) break;
            sim.dump(t);
        }

        expect(sim.dut.count).toBe(0n);
        sim.dispose();
    });
});
`,
	},
	four_state: {
		veryl: `module FourStateDemo (
    a: input logic<8>,
    b: input logic<8>,
    array_input: input logic<1> [8],
    snapshot: output logic<8>,
    array_snapshot: output logic<8>,
) {
    assign snapshot = a;

    for i in 0..8 :g {
        assign array_snapshot[i] = array_input[i];
    }
}`,
		testbench: `import { describe, it, expect } from "vitest";
import { FourState, Simulation, Simulator, X } from "@celox-sim/celox";
import { FourStateDemo } from "../src/FourStateDemo.veryl";

describe("FourStateDemo", () => {
    it("keeps the default simulator in two-state mode", () => {
        const sim = Simulator.create(FourStateDemo);

        expect(sim.fourState("a").mask).toBe(0n);

        sim.dispose();
    });

    it("starts unassigned simulator signals as X", () => {
        const sim = Simulator.create(FourStateDemo, { fourState: true });

        expect(sim.fourState("a").mask).toBe(0xffn);

        sim.dispose();
    });

    it("starts unassigned simulation signals as X", () => {
        const sim = Simulation.create(FourStateDemo, { fourState: true });

        expect(sim.fourState("a").mask).toBe(0xffn);

        sim.dispose();
    });

    it("starts every unpacked array element as X", () => {
        const sim = Simulator.create(FourStateDemo, { fourState: true });

        expect(sim.fourState("array_snapshot").mask).toBe(0xffn);

        sim.dispose();
    });

    it("writes and reads all-X", () => {
        const sim = Simulator.create(FourStateDemo, { fourState: true });

        sim.dut.a = X;

        const a = sim.fourState("a");
        expect(a.value).toBe(0xffn);
        expect(a.mask).toBe(0xffn);

        sim.dispose();
    });

    it("round-trips partial X and clears mask on defined write", () => {
        const sim = Simulator.create(FourStateDemo, { fourState: true });

        sim.dut.b = FourState(0x05, 0xf0);

        const partial = sim.fourState("b");
        expect(partial.value).toBe(0x05n);
        expect(partial.mask).toBe(0xf0n);

        sim.dut.b = 0x33n;

        const defined = sim.fourState("b");
        expect(defined.value).toBe(0x33n);
        expect(defined.mask).toBe(0n);

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
  #waveform-toolbar select { font-size: 0.7rem; padding: 2px 6px; max-width: 260px; }
  #waveform-toolbar button { font-size: 0.7rem; padding: 2px 8px; background: #1a1a2e; color: #c9d1d9; border-color: #0f3460; font-weight: 400; }
  #waveform-toolbar button:hover { background: #0f3460; }
  #waveform-container { flex: 1; overflow: hidden; }
  .wv-badge { display: none; font-size: 0.55rem; background: #3fb950; color: #0d1117; border-radius: 6px; padding: 0 4px; margin-left: 4px; font-weight: 700; }
  .wv-badge.visible { display: inline; }
</style>
<header>
  <h1>Celox Playground</h1>
  <select id="examples"><option value="">-- Example --</option><option value="adder">Adder</option><option value="counter">Counter (Simulator)</option><option value="counter_sim">Counter (Simulation)</option><option value="counter_vcd">Counter (Waveform)</option><option value="four_state">4-State</option></select>
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
        <select id="wv-trace-select"></select>
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
const wvTraceSelectEl = document.getElementById(
	"wv-trace-select",
) as HTMLSelectElement;

// Per-test VCD traces collected during a run
let vcdTraces: { label: string; trace: VcdTrace }[] = [];
let currentVcdTrace: VcdTrace | null = null;

// VCD recording state — reset before each test
let vcdSignals: VcdSignalInfo[] = [];
let vcdSnapshots: VcdSnapshot[] = [];

/** Flush accumulated snapshots into a named trace entry. */
function flushVcdTrace(label: string) {
	if (vcdSnapshots.length > 0) {
		vcdTraces.push({
			label,
			trace: { signals: [...vcdSignals], snapshots: vcdSnapshots },
		});
		vcdSnapshots = [];
	}
	// Keep vcdSignals — signals are the same across tests for the same module
}

/** Show the trace at the given index in the selector. */
function showVcdTrace(index: number) {
	if (index < 0 || index >= vcdTraces.length) return;
	currentVcdTrace = vcdTraces[index].trace;
	waveformViewer.setTrace(currentVcdTrace);
	wvTraceSelectEl.value = String(index);
}

document
	.getElementById("wv-zoom-in")!
	.addEventListener("click", () => waveformViewer.zoomIn());
document
	.getElementById("wv-zoom-out")!
	.addEventListener("click", () => waveformViewer.zoomOut());
document
	.getElementById("wv-fit")!
	.addEventListener("click", () => waveformViewer.fit());
wvTraceSelectEl.addEventListener("change", () =>
	showVcdTrace(Number(wvTraceSelectEl.value)),
);
document.getElementById("wv-download")!.addEventListener("click", () => {
	if (!currentVcdTrace) return;
	const text = generateVcdText(currentVcdTrace);
	const blob = new Blob([text], { type: "text/plain" });
	const url = URL.createObjectURL(blob);
	const a = document.createElement("a");
	a.href = url;
	const label = wvTraceSelectEl.selectedOptions[0]?.textContent ?? "dump";
	a.download = `${label.replace(/[^a-zA-Z0-9_-]/g, "_")}.vcd`;
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
const monacoTypeScript = monaco.typescript;
monacoTypeScript.typescriptDefaults.setEagerModelSync(true);
const monacoTestbenchCompilerOptions =
	buildMonacoTestbenchCompilerOptions(monacoTypeScript);
monacoTypeScript.typescriptDefaults.setCompilerOptions(
	monacoTestbenchCompilerOptions,
);
monacoTypeScript.javascriptDefaults.setCompilerOptions(
	monacoTestbenchCompilerOptions,
);
monacoTypeScript.typescriptDefaults.setDiagnosticsOptions({
	noSemanticValidation: false,
	noSyntaxValidation: false,
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

type PlaygroundTestMarker = {
	code: string | number | undefined;
	message: string;
	severity: number;
	startLineNumber: number;
	startColumn: number;
	endLineNumber: number;
	endColumn: number;
};

type PlaygroundTestApi = {
	loadExample: (name: string) => void;
	setFileContent: (path: string, content: string) => void;
	getModelMarkers: (path: string) => PlaygroundTestMarker[];
	getTypeScriptDiagnostics: (path: string) => Promise<PlaygroundTestMarker[]>;
	getStatusText: () => string;
};

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

function getFile(path: string): PlaygroundFile | null {
	return files.get(path) ?? null;
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
		activeFilePath =
			remaining.length > 0 ? remaining[remaining.length - 1] : null;
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

monacoTypeScript.typescriptDefaults.addExtraLib(
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

// Every declaration file of the package, so the whole public API resolves.
const celoxDtsSources = import.meta.glob<string>("../../celox/dist/*.d.ts", {
	query: "?raw",
	import: "default",
	eager: true,
});
for (const lib of celoxDtsLibs(celoxDtsSources)) {
	monacoTypeScript.typescriptDefaults.addExtraLib(lib.content, lib.filePath);
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

function snapshotTestSources(
	target: string,
): { path: string; source: string }[] {
	if (target === "*") {
		// All test files
		const result: { path: string; source: string }[] = [];
		for (const [path, f] of files) {
			if (isTestFile(path)) {
				result.push({ path, source: f.model.getValue() });
			}
		}
		return result;
	}
	const f = files.get(target);
	if (f) return [{ path: target, source: f.model.getValue() }];
	return [];
}

function updateDutTypes(ports: Record<string, VirtualPortInfo>) {
	if (currentExtraLib) currentExtraLib.dispose();

	// Extract module name from Veryl source
	const topMatch = getVerylSource().match(/module\s+(\w+)/);
	const moduleName = topMatch?.[1] || "Top";

	// Only generate .veryl module declarations — @celox-sim/celox types are
	// registered once at startup from the real .d.ts files.
	const virtualModuleDts = buildVirtualModuleDts(moduleName, ports);
	const dts = `
declare module "./${moduleName}.veryl" {
${virtualModuleDts}
}

declare module "../src/${moduleName}.veryl" {
${virtualModuleDts}
}
`;
	currentExtraLib = monacoTypeScript.typescriptDefaults.addExtraLib(
		dts,
		"file:///celox-sim.d.ts",
	);

	// Also register the .veryl module as a virtual file so TS can resolve the import
	const verylModuleDts = virtualModuleDts;
	// Register under multiple paths to ensure resolution works
	// test/foo.test.ts imports "../src/Adder.veryl" → resolves to file:///src/Adder.veryl
	monacoTypeScript.typescriptDefaults.addExtraLib(
		verylModuleDts,
		`file:///src/${moduleName}.veryl`,
	);
	monacoTypeScript.typescriptDefaults.addExtraLib(
		verylModuleDts,
		`file:///src/${moduleName}.d.veryl.ts`,
	);
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
let updateRevision = 0;
function onVerylChange() {
	clearTimeout(updateTimer);
	const revision = ++updateRevision;
	updateTimer = setTimeout(async () => {
		const source = getVerylSource();
		const model = getVerylModel();

		// Try WASM-based analysis (accurate types + diagnostics)
		if (celoxCompilerReady) {
			try {
				const result = JSON.parse(
					await celoxCompiler.genTsFromSourceLatest([
						{ content: source, path: "main.veryl" },
					]),
				);
				if (revision !== updateRevision) return;
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
				if (revision !== updateRevision) return;
				// Analysis failed — show diagnostics
				if (model) {
					const markers = parseVerylErrors(e.message || String(e));
					monaco.editor.setModelMarkers(model, "veryl", markers);
				}
				// Still try regex fallback for types
			}
		}

		// Fallback: regex-based port extraction (no WASM needed)
		if (revision !== updateRevision) return;
		const ports = extractPortsFromSource(source);
		if (Object.keys(ports).length > 0) {
			updateDutTypes(ports);
		}
	}, 500);
}

// ── WASM loading ────────────────────────────────────────

const celoxCompiler = new CeloxCompilerClient();
let celoxCompilerReady = false;

async function init() {
	try {
		await celoxCompiler.ready;
		celoxCompilerReady = true;
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
	vcdTraces = [];
	currentVcdTrace = null;
	wvTraceSelectEl.innerHTML = "";
	wvBadgeEl.classList.remove("visible");
	wvBadgeEl.textContent = "";

	const targetPath = runTargetEl.value;
	if (!targetPath) {
		appendConsole("[error] No test file selected", "log-error");
		statusEl.textContent = "Error";
		runBtn.disabled = false;
		return;
	}
	const runArgs = runArgsEl.value.trim();
	const testSources = snapshotTestSources(targetPath);
	if (testSources.length === 0) {
		appendConsole("[error] No test files found", "log-error");
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
		const genTsResult = await (async () => {
			try {
				return JSON.parse(
					await celoxCompiler.genTsFromSource([
						{ content: verylSource, path: "main.veryl" },
					]),
				);
			} catch {
				return null;
			}
		})();

		const sources = [{ content: verylSource, path: "main.veryl" }];
		const [twoStateCompiled, fourStateCompiled] = await Promise.all([
			celoxCompiler.compile(sources, topName, { fourState: false }),
			celoxCompiler.compile(sources, topName, { fourState: true }),
		]);
		const t1 = performance.now();
		appendConsole(
			`[compile] ${(t1 - t0).toFixed(0)}ms — ${Object.keys(twoStateCompiled.layout).length} signals, ${Object.keys(twoStateCompiled.events).length} events`,
			"log-success",
		);

		type PlaygroundSimulatorOptions = { fourState?: boolean };

		// Every dump() lands in the waveform viewer's current trace.
		const recordDump: PlaygroundDumpSink = (timestamp, signals, values) => {
			if (vcdSignals.length === 0) vcdSignals.push(...signals);
			vcdSnapshots.push({ timestamp, values });
		};
		const compiledFor = (options?: PlaygroundSimulatorOptions) =>
			options?.fourState === true ? fourStateCompiled : twoStateCompiled;

		// Build module definitions from genTsFromSource result for .veryl imports
		const moduleNames = (genTsResult?.modules || [])
			.map((m: any) => m.moduleName)
			.filter(Boolean);

		const Simulator = {
			create(_module: any, options?: PlaygroundSimulatorOptions) {
				return createPlaygroundSimulator(compiledFor(options), recordDump);
			},
		};

		const Simulation = {
			create(_module: any, options?: PlaygroundSimulatorOptions) {
				return createPlaygroundSimulation(compiledFor(options), recordDump);
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
		let grepPattern: RegExp | null = null;
		if (runArgs) {
			const grepMatch = runArgs.match(
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

		for (const { path: testPath, source } of testSources) {
			const jsCode = transpileTestbench(source, testPath);

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
				"FourState",
				"X",
				"Z",
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
				FourState,
				X,
				Z,
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
			// Reset per-test VCD accumulator
			vcdSnapshots = [];
			try {
				await t.fn();
				passed++;
				appendConsole(`   PASS  ${label}`, "log-success");
			} catch (e: any) {
				failed++;
				appendConsole(`   FAIL  ${label}`, "log-error");
				appendConsole(`         ${e.message}`, "log-error");
			}
			// Flush any VCD data recorded by this test
			flushVcdTrace(label);
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
		if (vcdTraces.length > 0) {
			// Populate trace selector
			wvTraceSelectEl.innerHTML = "";
			for (let i = 0; i < vcdTraces.length; i++) {
				const opt = document.createElement("option");
				opt.value = String(i);
				opt.textContent = vcdTraces[i].label;
				wvTraceSelectEl.appendChild(opt);
			}
			showVcdTrace(0);

			const totalSnaps = vcdTraces.reduce(
				(s, t) => s + t.trace.snapshots.length,
				0,
			);
			wvBadgeEl.textContent = `${vcdTraces.length}`;
			wvBadgeEl.classList.add("visible");
			appendConsole(
				`\n[vcd] ${vcdTraces.length} trace(s), ${totalSnaps} total snapshots`,
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

function installPlaygroundTestApi() {
	function flattenDiagnosticMessageText(messageText: any): string {
		if (typeof messageText === "string") return messageText;
		if (!messageText || typeof messageText.messageText !== "string") return "";
		const next = Array.isArray(messageText.next)
			? messageText.next
					.map((child: any) => flattenDiagnosticMessageText(child))
					.filter(Boolean)
					.join("\n")
			: "";
		return next
			? `${messageText.messageText}\n${next}`
			: messageText.messageText;
	}

	const api: PlaygroundTestApi = {
		loadExample,
		setFileContent(path: string, content: string) {
			const file = getFile(path);
			if (!file) throw new Error(`File not found: ${path}`);
			file.model.setValue(content);
		},
		getModelMarkers(path: string) {
			const file = getFile(path);
			if (!file) throw new Error(`File not found: ${path}`);
			return monaco.editor
				.getModelMarkers({ resource: file.model.uri })
				.map((marker) => ({
					code:
						typeof marker.code === "object" ? marker.code.value : marker.code,
					message: marker.message,
					severity: marker.severity,
					startLineNumber: marker.startLineNumber,
					startColumn: marker.startColumn,
					endLineNumber: marker.endLineNumber,
					endColumn: marker.endColumn,
				}));
		},
		async getTypeScriptDiagnostics(path: string) {
			const file = getFile(path);
			if (!file) throw new Error(`File not found: ${path}`);
			const workerFactory = await monacoTypeScript.getTypeScriptWorker();
			const worker = await workerFactory(file.model.uri);
			const uri = file.model.uri.toString();
			const [semantic, syntactic, suggestion] = await Promise.all([
				worker.getSemanticDiagnostics(uri),
				worker.getSyntacticDiagnostics(uri),
				worker.getSuggestionDiagnostics(uri),
			]);
			return [...semantic, ...syntactic, ...suggestion].map((diag: any) => {
				const start = typeof diag.start === "number" ? diag.start : 0;
				const length = typeof diag.length === "number" ? diag.length : 0;
				const startPos = file.model.getPositionAt(start);
				const endPos = file.model.getPositionAt(start + length);
				return {
					code: diag.code,
					message: flattenDiagnosticMessageText(diag.messageText),
					severity: monaco.MarkerSeverity.Error,
					startLineNumber: startPos.lineNumber,
					startColumn: startPos.column,
					endLineNumber: endPos.lineNumber,
					endColumn: endPos.column,
				};
			});
		},
		getStatusText() {
			return statusEl.textContent ?? "";
		},
	};

	(
		globalThis as typeof globalThis & {
			__CELOX_PLAYGROUND_TEST_API__?: PlaygroundTestApi;
		}
	).__CELOX_PLAYGROUND_TEST_API__ = api;
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
installPlaygroundTestApi();
init();
