// Celox Playground — fully browser-based
//
// Uses napi-rs WASM build with memfs virtual filesystem.
// genTs() reads Veryl.toml and .veryl files from memfs.
// NativeSimulatorHandle compiles Veryl → WASM simulation modules.
// Simulation runs via WebAssembly.instantiate() in browser.

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
    testbench: `sim.set("a", 100);
sim.set("b", 200);
sim.evalComb();
log("a=100, b=200 => sum=" + sim.get("sum"));

sim.set("a", 65535);
sim.set("b", 1);
sim.evalComb();
log("a=65535, b=1 => sum=" + sim.get("sum"));`,
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
sim.set("rst", 0);
sim.evalComb();
sim.tick("clk");
log("After reset: count=" + sim.get("count"));

// Release reset, enable counting
sim.set("rst", 1);
sim.set("en", 1);

for (let i = 0; i < 5; i++) {
    sim.evalComb();
    sim.tick("clk");
    log("Cycle " + (i+1) + ": count=" + sim.get("count"));
}`,
  },
};

// ── UI Setup ────────────────────────────────────────────

const app = document.getElementById("app")!;
app.innerHTML = `
<style>
  * { box-sizing: border-box; margin: 0; padding: 0; }
  body { font-family: system-ui, sans-serif; background: #1a1a2e; color: #e0e0e0; }
  header { background: #16213e; padding: 10px 20px; display: flex; align-items: center; gap: 12px; border-bottom: 1px solid #0f3460; }
  header h1 { font-size: 1.1rem; color: #e94560; }
  select, button { padding: 5px 10px; border-radius: 4px; border: 1px solid #0f3460; background: #1a1a2e; color: #e0e0e0; font-size: 0.85rem; cursor: pointer; }
  button { background: #e94560; color: #fff; border-color: #e94560; font-weight: 600; }
  button:hover { background: #c73e54; }
  button:disabled { opacity: 0.5; cursor: not-allowed; }
  .status { margin-left: auto; font-size: 0.8rem; color: #888; }
  .panels { display: grid; grid-template-columns: 1fr 1fr; grid-template-rows: 1fr 200px; height: calc(100vh - 44px); }
  .panel { display: flex; flex-direction: column; border: 1px solid #0f3460; overflow: hidden; }
  .panel-hdr { background: #16213e; padding: 3px 10px; font-size: 0.7rem; font-weight: 600; color: #666; text-transform: uppercase; letter-spacing: 0.05em; }
  textarea { flex: 1; background: #0d1117; color: #c9d1d9; border: none; padding: 10px; font-family: 'Fira Code', monospace; font-size: 0.85rem; line-height: 1.5; resize: none; tab-size: 4; outline: none; }
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
  <div class="panel"><div class="panel-hdr">Veryl Source</div><textarea id="veryl" spellcheck="false"></textarea></div>
  <div class="panel"><div class="panel-hdr">Testbench (JS)</div><textarea id="tb" spellcheck="false"></textarea></div>
  <div class="panel" style="grid-column:1/-1"><div class="panel-hdr">Console</div><div id="console"></div></div>
</div>
`;

const verylEl = document.getElementById("veryl") as HTMLTextAreaElement;
const tbEl = document.getElementById("tb") as HTMLTextAreaElement;
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

function clearConsole() {
  consoleEl.innerHTML = "";
}

// ── Load WASM addon ─────────────────────────────────────

let celox: any;

async function init() {
  try {
    celox = await import("/celox.wasi-browser.js");
    statusEl.textContent = "Ready";
    runBtn.disabled = false;
  } catch (e: any) {
    statusEl.textContent = "Failed";
    appendConsole("[error] Failed to load WASM: " + e.message, "log-error");
  }
}

// ── Run simulation ──────────────────────────────────────

async function run() {
  clearConsole();
  runBtn.disabled = true;
  statusEl.textContent = "Compiling…";

  try {
    const verylSource = verylEl.value;
    const testbench = tbEl.value;

    // Detect top module name
    const topMatch = verylSource.match(/module\s+(\w+)/);
    if (!topMatch) throw new Error("No module found in Veryl source");
    const topName = topMatch[1];

    const t0 = performance.now();

    // Compile via genTsFromSource (no filesystem needed)
    const tsResult = JSON.parse(
      celox.genTsFromSource([{ content: verylSource, path: "main.veryl" }])
    );
    appendConsole(
      `[compile] Parsed: ${tsResult.modules?.length || 0} module(s)`,
      "log-info"
    );

    // Create simulator handle → WASM bytes
    const handle = new celox.NativeSimulatorHandle(
      [{ content: verylSource, path: "main.veryl" }],
      topName
    );

    const layout = JSON.parse(handle.layoutJson);
    const events = JSON.parse(handle.eventsJson);
    const totalSize = handle.totalSize;
    const stableSize = handle.stableSize;

    const t1 = performance.now();
    appendConsole(
      `[compile] Done in ${(t1 - t0).toFixed(0)}ms — ${Object.keys(layout).length} signals, ${Object.keys(events).length} events, ${totalSize}B memory`,
      "log-success"
    );

    // Instantiate simulation WASM modules
    statusEl.textContent = "Instantiating…";
    const pages = Math.max(1, Math.ceil(totalSize / 65536));
    const memory = new WebAssembly.Memory({ initial: pages });

    const combBytes = new Uint8Array(handle.combWasmBytes());
    const combInst = await WebAssembly.instantiate(combBytes, {
      env: { memory },
    });

    const eventInsts: Record<string, WebAssembly.Instance> = {};
    for (const name of Object.keys(events)) {
      try {
        const bytes = new Uint8Array(handle.eventWasmBytes(name));
        const { instance } = await WebAssembly.instantiate(bytes, {
          env: { memory },
        });
        eventInsts[name] = instance;
      } catch {}
    }

    // Build sim wrapper
    const view = new DataView(memory.buffer);
    const sim = {
      set(name: string, value: number | bigint) {
        const sig = layout[name];
        if (!sig) throw new Error(`Signal '${name}' not found`);
        const v = Number(value);
        for (let i = 0; i < sig.byte_size; i++)
          view.setUint8(sig.offset + i, (v >> (i * 8)) & 0xff);
      },
      get(name: string): number {
        const sig = layout[name];
        if (!sig) throw new Error(`Signal '${name}' not found`);
        let v = 0;
        for (let i = Math.min(sig.byte_size, 4) - 1; i >= 0; i--)
          v = (v << 8) | view.getUint8(sig.offset + i);
        if (sig.width < 32) v &= (1 << sig.width) - 1;
        return v >>> 0;
      },
      evalComb() {
        (combInst.instance.exports.run as Function)();
      },
      tick(eventName: string) {
        const inst = eventInsts[eventName];
        if (inst) (inst.exports.run as Function)();
        (combInst.instance.exports.run as Function)();
      },
    };

    // Execute testbench
    statusEl.textContent = "Running…";
    appendConsole("[run] Executing testbench…", "log-info");
    const log = (msg: any) => appendConsole(String(msg));
    new Function("sim", "log", testbench)(sim, log);

    appendConsole("[run] Done.", "log-success");
    statusEl.textContent = "Done";
  } catch (e: any) {
    appendConsole("[error] " + e.message, "log-error");
    statusEl.textContent = "Error";
  } finally {
    runBtn.disabled = false;
  }
}

// ── Event handlers ──────────────────────────────────────

function loadExample(name: string) {
  const ex = EXAMPLES[name];
  if (ex) {
    verylEl.value = ex.veryl;
    tbEl.value = ex.testbench;
  }
}

examplesEl.addEventListener("change", () => {
  if (examplesEl.value) loadExample(examplesEl.value);
});

runBtn.addEventListener("click", run);

document.addEventListener("keydown", (e) => {
  if ((e.ctrlKey || e.metaKey) && e.key === "Enter" && !runBtn.disabled) run();
});

// Boot
loadExample("adder");
init();
