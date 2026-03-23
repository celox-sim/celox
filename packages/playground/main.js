// Celox Playground - main entry point
//
// Loads the celox-wasm module, compiles Veryl source, instantiates the
// generated WASM simulation modules, and runs the user's testbench.

import init, { SimHandle } from "./pkg/celox_wasm.js";

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

/**
 * Wraps the raw WASM modules + memory into a simple simulation API
 * that the testbench can call.
 */
class WasmSimulation {
    constructor(memory, layout, combInstance, eventInstances) {
        this._memory = memory;
        this._layout = layout;
        this._combRun = combInstance.exports.run;
        this._events = eventInstances;
        this._view = new DataView(memory.buffer);
    }

    /** Set a signal value (up to 32 bits). */
    set(name, value) {
        const sig = this._layout[name];
        if (!sig) throw new Error(`Signal '${name}' not found in layout`);
        const { offset, byteSize } = sig;
        // Write little-endian
        if (byteSize <= 4) {
            for (let i = 0; i < byteSize; i++) {
                this._view.setUint8(offset + i, (value >> (i * 8)) & 0xff);
            }
        } else {
            // BigInt path for wider signals
            let v = BigInt(value);
            for (let i = 0; i < byteSize; i++) {
                this._view.setUint8(offset + i, Number(v & 0xffn));
                v >>= 8n;
            }
        }
    }

    /** Get a signal value (up to 32 bits returned as Number). */
    get(name) {
        const sig = this._layout[name];
        if (!sig) throw new Error(`Signal '${name}' not found in layout`);
        const { offset, width, byteSize } = sig;
        let value = 0;
        for (let i = Math.min(byteSize, 4) - 1; i >= 0; i--) {
            value = (value << 8) | this._view.getUint8(offset + i);
        }
        // Mask to actual width
        if (width < 32) {
            value &= (1 << width) - 1;
        }
        return value >>> 0; // unsigned
    }

    /** Evaluate combinational logic. */
    evalComb() {
        const rc = this._combRun();
        if (rc !== 0n && rc !== 0) {
            throw new Error(`eval_comb returned error code ${rc}`);
        }
    }

    /** Trigger a clock/reset event by name. */
    tickEvent(name) {
        const inst = this._events[name];
        if (!inst) throw new Error(`Event '${name}' not found`);
        const rc = inst.exports.run();
        if (rc !== 0n && rc !== 0) {
            throw new Error(`event '${name}' returned error code ${rc}`);
        }
        // Re-evaluate comb after event
        this.evalComb();
    }
}

// ── Main initialization ─────────────────────────────────────────────

const statusEl = document.getElementById("status");
const runBtn = document.getElementById("run-btn");
const examplesSelect = document.getElementById("examples");
const verylSource = document.getElementById("veryl-source");
const testbench = document.getElementById("testbench");

async function main() {
    try {
        await init();
        statusEl.textContent = "Ready";
        runBtn.disabled = false;
    } catch (e) {
        statusEl.textContent = "Failed to load WASM";
        appendConsole("Failed to initialize: " + e.message, "log-error");
        return;
    }

    // Load default example
    loadExample("adder");

    examplesSelect.addEventListener("change", () => {
        if (examplesSelect.value) {
            loadExample(examplesSelect.value);
        }
    });

    runBtn.addEventListener("click", runSimulation);
}

function loadExample(name) {
    const ex = EXAMPLES[name];
    if (ex) {
        verylSource.value = ex.veryl;
        testbench.value = ex.testbench;
    }
}

async function runSimulation() {
    clearConsole();
    runBtn.disabled = true;
    statusEl.textContent = "Compiling...";

    try {
        const source = verylSource.value;
        const tb = testbench.value;

        // 1. Compile Veryl to WASM
        appendConsole("[compile] Compiling Veryl source...", "log-info");
        const t0 = performance.now();
        const handle = new SimHandle(source, verylSource.value.match(/module\s+(\w+)/)?.[1] || "Top");
        const t1 = performance.now();
        appendConsole(`[compile] Done in ${(t1 - t0).toFixed(1)}ms`, "log-success");

        // 2. Get layout and metadata
        const layout = JSON.parse(handle.layoutJson());
        const events = JSON.parse(handle.eventsJson());
        const totalSize = handle.totalSize();
        appendConsole(`[info] Memory: ${totalSize} bytes, Signals: ${Object.keys(layout).length}, Events: ${Object.keys(events).length}`, "log-info");

        // 3. Create shared WASM memory
        const pages = Math.max(1, Math.ceil(totalSize / 65536));
        const memory = new WebAssembly.Memory({ initial: pages });

        // 4. Instantiate eval_comb WASM module
        const combBytes = handle.combWasmBytes();
        const combModule = await WebAssembly.compile(combBytes);
        const combInstance = await WebAssembly.instantiate(combModule, {
            env: { memory },
        });

        // 5. Instantiate event WASM modules
        const eventInstances = {};
        for (const eventName of Object.keys(events)) {
            try {
                const eventBytes = handle.eventWasmBytes(eventName);
                const eventModule = await WebAssembly.compile(eventBytes);
                const eventInstance = await WebAssembly.instantiate(eventModule, {
                    env: { memory },
                });
                eventInstances[eventName] = eventInstance;
            } catch (e) {
                appendConsole(`[warn] Could not compile event '${eventName}': ${e.message}`, "log-warn");
            }
        }

        // 6. Create simulation wrapper
        const sim = new WasmSimulation(memory, layout, combInstance, eventInstances);

        // 7. Run testbench
        statusEl.textContent = "Running...";
        appendConsole("[run] Executing testbench...", "log-info");

        // The testbench gets `sim` and a `log` function
        const log = (msg) => appendConsole(String(msg));
        const fn = new Function("sim", "log", tb);
        fn(sim, log);

        appendConsole("[run] Testbench complete.", "log-success");
        statusEl.textContent = "Done";
    } catch (e) {
        appendConsole("[error] " + e.message, "log-error");
        if (e.stack) {
            appendConsole(e.stack, "log-error");
        }
        statusEl.textContent = "Error";
    } finally {
        runBtn.disabled = false;
    }
}

main();
