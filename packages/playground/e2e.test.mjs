// End-to-end tests for the celox-wasm playground pipeline.
//
// These tests verify the full flow: compile Veryl source -> generate WASM
// simulation modules -> instantiate and run -> verify outputs.
//
// Run with: node packages/playground/e2e.test.mjs

import { strict as assert } from "node:assert";
import { describe, it } from "node:test";
import { SimHandle } from "./pkg-node/celox_wasm.js";

// ── Helpers ──────────────────────────────────────────────────────────

/**
 * Create a WasmSimulation from compiled SimHandle.
 * This mirrors the browser-side WasmSimulation class from main.js.
 */
async function createSimulation(handle) {
    const layout = JSON.parse(handle.layoutJson());
    const events = JSON.parse(handle.eventsJson());
    const totalSize = handle.totalSize();

    // Create shared memory
    const pages = Math.max(1, Math.ceil(totalSize / 65536));
    const memory = new WebAssembly.Memory({ initial: pages });

    // Instantiate eval_comb module
    const combBytes = handle.combWasmBytes();
    const combModule = await WebAssembly.compile(combBytes);
    const combInstance = await WebAssembly.instantiate(combModule, {
        env: { memory },
    });

    // Instantiate event modules
    const eventInstances = {};
    for (const eventName of Object.keys(events)) {
        const eventBytes = handle.eventWasmBytes(eventName);
        const eventModule = await WebAssembly.compile(eventBytes);
        const eventInstance = await WebAssembly.instantiate(eventModule, {
            env: { memory },
        });
        eventInstances[eventName] = eventInstance;
    }

    return { memory, layout, combInstance, eventInstances };
}

/** Set a signal value in memory. */
function setSignal(view, layout, name, value) {
    const sig = layout[name];
    if (!sig) throw new Error(`Signal '${name}' not found in layout`);
    const { offset, byteSize } = sig;
    if (byteSize <= 4) {
        for (let i = 0; i < byteSize; i++) {
            view.setUint8(offset + i, (value >> (i * 8)) & 0xff);
        }
    } else {
        let v = BigInt(value);
        for (let i = 0; i < byteSize; i++) {
            view.setUint8(offset + i, Number(v & 0xffn));
            v >>= 8n;
        }
    }
}

/** Get a signal value from memory. */
function getSignal(view, layout, name) {
    const sig = layout[name];
    if (!sig) throw new Error(`Signal '${name}' not found in layout`);
    const { offset, width, byteSize } = sig;
    let value = 0;
    for (let i = Math.min(byteSize, 4) - 1; i >= 0; i--) {
        value = (value << 8) | view.getUint8(offset + i);
    }
    if (width < 32) {
        value &= (1 << width) - 1;
    }
    return value >>> 0;
}

/** Run eval_comb and check return code. */
function evalComb(combInstance) {
    const rc = combInstance.exports.run();
    if (rc !== 0n && rc !== 0) {
        throw new Error(`eval_comb returned error code ${rc}`);
    }
}

/** Trigger an event and re-evaluate comb. */
function tickEvent(eventInstances, combInstance, name) {
    const inst = eventInstances[name];
    if (!inst) throw new Error(`Event '${name}' not found`);
    const rc = inst.exports.run();
    if (rc !== 0n && rc !== 0) {
        throw new Error(`event '${name}' returned error code ${rc}`);
    }
    evalComb(combInstance);
}

// ── Tests ────────────────────────────────────────────────────────────

describe("celox-wasm E2E", () => {
    describe("SimHandle construction", () => {
        it("should compile a simple adder module", () => {
            const source = `module Adder (
    a: input logic<8>,
    b: input logic<8>,
    sum: output logic<9>,
) {
    assign sum = a + b;
}`;
            const handle = new SimHandle(source, "Adder");
            assert.ok(handle);

            const layout = JSON.parse(handle.layoutJson());
            assert.ok(layout.a, "layout should contain signal 'a'");
            assert.ok(layout.b, "layout should contain signal 'b'");
            assert.ok(layout.sum, "layout should contain signal 'sum'");

            assert.equal(layout.a.width, 8);
            assert.equal(layout.b.width, 8);
            assert.equal(layout.sum.width, 9);

            assert.ok(handle.totalSize() > 0, "totalSize should be > 0");
            assert.ok(handle.stableSize() > 0, "stableSize should be > 0");

            handle.free();
        });

        it("should reject invalid Veryl source", () => {
            assert.throws(
                () => new SimHandle("not valid veryl code!!!", "Top"),
                /error|Error/i,
            );
        });

        it("should reject unknown top module", () => {
            const source = `module Foo (
    a: input logic<8>,
) {
    // empty
}`;
            assert.throws(
                () => new SimHandle(source, "NonExistent"),
                /error|Error|not found/i,
            );
        });
    });

    describe("Combinational adder simulation", () => {
        it("should compute a + b correctly", async () => {
            const source = `module Adder (
    a: input logic<8>,
    b: input logic<8>,
    sum: output logic<9>,
) {
    assign sum = a + b;
}`;
            const handle = new SimHandle(source, "Adder");
            const { memory, layout, combInstance } =
                await createSimulation(handle);
            const view = new DataView(memory.buffer);

            // Test case 1: 10 + 20 = 30
            setSignal(view, layout, "a", 10);
            setSignal(view, layout, "b", 20);
            evalComb(combInstance);
            assert.equal(getSignal(view, layout, "sum"), 30);

            // Test case 2: 255 + 1 = 256
            setSignal(view, layout, "a", 255);
            setSignal(view, layout, "b", 1);
            evalComb(combInstance);
            assert.equal(getSignal(view, layout, "sum"), 256);

            // Test case 3: 0 + 0 = 0
            setSignal(view, layout, "a", 0);
            setSignal(view, layout, "b", 0);
            evalComb(combInstance);
            assert.equal(getSignal(view, layout, "sum"), 0);

            // Test case 4: 128 + 128 = 256
            setSignal(view, layout, "a", 128);
            setSignal(view, layout, "b", 128);
            evalComb(combInstance);
            assert.equal(getSignal(view, layout, "sum"), 256);

            handle.free();
        });
    });

    describe("Sequential counter simulation", () => {
        it("should count up after reset", async () => {
            const source = `module Counter (
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
}`;
            const handle = new SimHandle(source, "Counter");
            const { memory, layout, combInstance, eventInstances } =
                await createSimulation(handle);
            const view = new DataView(memory.buffer);

            const events = JSON.parse(handle.eventsJson());
            assert.ok(
                Object.keys(events).length > 0,
                "should have clock/reset events",
            );

            // Find the clock event name
            const clockEventName = Object.keys(eventInstances).find(
                (name) =>
                    name.includes("clk") ||
                    name.includes("clock") ||
                    name === "clk",
            );
            assert.ok(clockEventName, "should find a clock event");

            // Reset sequence (active-low: rst=0 asserts reset)
            setSignal(view, layout, "rst", 0);
            setSignal(view, layout, "clk", 0);
            evalComb(combInstance);
            setSignal(view, layout, "clk", 1);
            evalComb(combInstance);
            tickEvent(eventInstances, combInstance, clockEventName);
            assert.equal(
                getSignal(view, layout, "count"),
                0,
                "count should be 0 after reset",
            );

            // Release reset (active-low: rst=1 de-asserts reset)
            setSignal(view, layout, "rst", 1);
            evalComb(combInstance);

            // Clock 5 times and verify counter increments
            for (let i = 0; i < 5; i++) {
                setSignal(view, layout, "clk", 0);
                evalComb(combInstance);
                setSignal(view, layout, "clk", 1);
                evalComb(combInstance);
                tickEvent(eventInstances, combInstance, clockEventName);
                assert.equal(
                    getSignal(view, layout, "count"),
                    i + 1,
                    `count should be ${i + 1} after ${i + 1} clock cycles`,
                );
            }

            handle.free();
        });
    });

    describe("Wider signals", () => {
        it("should handle 16-bit addition correctly", async () => {
            const source = `module WideAdder (
    a: input logic<16>,
    b: input logic<16>,
    sum: output logic<17>,
) {
    assign sum = a + b;
}`;
            const handle = new SimHandle(source, "WideAdder");
            const { memory, layout, combInstance } =
                await createSimulation(handle);
            const view = new DataView(memory.buffer);

            // Test: 1000 + 2000 = 3000
            setSignal(view, layout, "a", 1000);
            setSignal(view, layout, "b", 2000);
            evalComb(combInstance);
            assert.equal(getSignal(view, layout, "sum"), 3000);

            // Test: 65535 + 1 = 65536
            setSignal(view, layout, "a", 65535);
            setSignal(view, layout, "b", 1);
            evalComb(combInstance);
            assert.equal(getSignal(view, layout, "sum"), 65536);

            handle.free();
        });
    });

    describe("Layout metadata", () => {
        it("should report correct signal widths and offsets", () => {
            const source = `module Multi (
    a: input  logic<1>,
    b: input  logic<8>,
    c: input  logic<16>,
    d: output logic<32>,
) {
    assign d = {c, b, 7'b0, a};
}`;
            const handle = new SimHandle(source, "Multi");
            const layout = JSON.parse(handle.layoutJson());

            assert.equal(layout.a.width, 1);
            assert.equal(layout.b.width, 8);
            assert.equal(layout.c.width, 16);
            assert.equal(layout.d.width, 32);

            // All offsets should be non-negative
            assert.ok(layout.a.offset >= 0);
            assert.ok(layout.b.offset >= 0);
            assert.ok(layout.c.offset >= 0);
            assert.ok(layout.d.offset >= 0);

            // Byte sizes should match width
            assert.equal(layout.a.byteSize, 1);
            assert.equal(layout.b.byteSize, 1);
            assert.equal(layout.c.byteSize, 2);
            assert.equal(layout.d.byteSize, 4);

            handle.free();
        });
    });

    describe("Events metadata", () => {
        it("should list clock events for sequential module", () => {
            const source = `module Seq (
    clk: input '_ clock,
    rst: input '_ reset,
    out: output logic<8>,
) {
    var r_val: logic<8>;
    assign out = r_val;
    always_ff (clk, rst) {
        if_reset {
            r_val = 0;
        } else {
            r_val = r_val + 1;
        }
    }
}`;
            const handle = new SimHandle(source, "Seq");
            const events = JSON.parse(handle.eventsJson());

            assert.ok(
                Object.keys(events).length > 0,
                "sequential module should have events",
            );

            handle.free();
        });

        it("should have no events for purely combinational module", () => {
            const source = `module PureComb (
    a: input logic<8>,
    b: output logic<8>,
) {
    assign b = ~a;
}`;
            const handle = new SimHandle(source, "PureComb");
            const events = JSON.parse(handle.eventsJson());

            assert.equal(
                Object.keys(events).length,
                0,
                "combinational module should have no events",
            );

            handle.free();
        });
    });

    describe("WASM module generation", () => {
        it("should generate valid comb WASM bytes", async () => {
            const source = `module Simple (
    a: input logic<8>,
    b: output logic<8>,
) {
    assign b = a;
}`;
            const handle = new SimHandle(source, "Simple");
            const bytes = handle.combWasmBytes();

            assert.ok(bytes instanceof Uint8Array, "should return Uint8Array");
            assert.ok(bytes.length > 0, "should have non-zero length");

            // WASM magic number: \0asm
            assert.equal(bytes[0], 0x00);
            assert.equal(bytes[1], 0x61); // 'a'
            assert.equal(bytes[2], 0x73); // 's'
            assert.equal(bytes[3], 0x6d); // 'm'

            // Should be a valid WASM module
            const module = await WebAssembly.compile(bytes);
            assert.ok(module, "should compile to valid WASM module");

            handle.free();
        });

        it("should generate valid event WASM bytes", async () => {
            const source = `module WithClock (
    clk: input '_ clock,
    rst: input '_ reset,
    out: output logic<8>,
) {
    var r_val: logic<8>;
    assign out = r_val;
    always_ff (clk, rst) {
        if_reset {
            r_val = 0;
        } else {
            r_val = r_val + 1;
        }
    }
}`;
            const handle = new SimHandle(source, "WithClock");
            const events = JSON.parse(handle.eventsJson());
            const eventNames = Object.keys(events);

            for (const eventName of eventNames) {
                const bytes = handle.eventWasmBytes(eventName);
                assert.ok(
                    bytes instanceof Uint8Array,
                    `event '${eventName}' should return Uint8Array`,
                );
                assert.ok(
                    bytes.length > 0,
                    `event '${eventName}' should have non-zero length`,
                );

                // WASM magic number
                assert.equal(bytes[0], 0x00);
                assert.equal(bytes[1], 0x61);
                assert.equal(bytes[2], 0x73);
                assert.equal(bytes[3], 0x6d);

                const module = await WebAssembly.compile(bytes);
                assert.ok(
                    module,
                    `event '${eventName}' should compile to valid WASM module`,
                );
            }

            handle.free();
        });

        it("should reject unknown event name", () => {
            const source = `module WithClock (
    clk: input '_ clock,
    rst: input '_ reset,
    out: output logic<8>,
) {
    var r_val: logic<8>;
    assign out = r_val;
    always_ff (clk, rst) {
        if_reset {
            r_val = 0;
        } else {
            r_val = r_val + 1;
        }
    }
}`;
            const handle = new SimHandle(source, "WithClock");
            assert.throws(
                () => handle.eventWasmBytes("nonexistent_event"),
                /not found/i,
            );
            handle.free();
        });
    });
});
