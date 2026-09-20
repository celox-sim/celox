import assert from "node:assert/strict";
import { createRequire } from "node:module";

const addon = createRequire(import.meta.url)("@celox-sim/celox-napi");
const [handleKind, mode] = process.argv.slice(2);

assert.ok(
	handleKind === "default" ||
		handleKind === "tiered" ||
		handleKind === "simulation",
	`unknown handle kind: ${String(handleKind)}`,
);
assert.ok(mode === "dispose" || mode === "gc", `unknown mode: ${String(mode)}`);

const Handle =
	handleKind === "simulation"
		? addon.NativeSimulationHandle
		: addon.NativeSimulatorHandle;
assert.equal(typeof Handle, "function", `${handleKind} handle is not available`);

const SOURCE = `
module Lifetime (
    value: input logic<64>,
) {}
`;
const SOURCES = [{ content: SOURCE, path: "" }];
const INITIAL_BYTE = 0xa5;
const PRESSURE_BYTE = 0x5a;
const UPDATED_BYTE = 0x3c;

function createHandle() {
	if (handleKind === "tiered") {
		assert.equal(
			typeof Handle.newTiered,
			"function",
			"newTiered is not available",
		);
		const handle = Handle.newTiered(SOURCES, "Lifetime", { optLevel: "O0" });
		assert.equal(handle.isTiered, true);
		assert.equal(handle.tierCompiled, false);
		return handle;
	}
	return new Handle(SOURCES, "Lifetime");
}

async function promoteTiered(handle) {
	if (handleKind !== "tiered") {
		return;
	}

	const deadline = Date.now() + 60_000;
	while (!handle.tierCompiled && Date.now() < deadline) {
		handle.evalComb();
		await new Promise((resolve) => setImmediate(resolve));
	}
	assert.equal(handle.tierCompiled, true, "tiered backend did not promote");
}

function signalRange(handle, view) {
	const layout = JSON.parse(handle.layoutJson);
	const signal = layout.value;
	assert.ok(signal, "expected Lifetime.value in layout metadata");
	const { offset, byte_size: size } = signal;
	assert.ok(Number.isInteger(offset) && offset >= 0, "invalid signal offset");
	assert.ok(Number.isInteger(size) && size > 0, "invalid signal size");
	assert.ok(offset + size <= view.byteLength, "signal is outside shared memory");
	return { offset, size };
}

function createView(handle) {
	const nativeView = handle.sharedMemory();
	return new Uint8Array(
		nativeView.buffer,
		nativeView.byteOffset,
		nativeView.byteLength,
	);
}

async function createRetainedView() {
	let handle = createHandle();
	const view = createView(handle);
	const { offset, size } = signalRange(handle, view);
	view.fill(INITIAL_BYTE, offset, offset + size);
	await promoteTiered(handle);
	assert.deepEqual(
		[...view.subarray(offset, offset + size)],
		Array(size).fill(INITIAL_BYTE),
		"promotion moved or replaced the shared memory image",
	);
	const weakHandle = new WeakRef(handle);

	if (mode === "dispose") {
		handle.dispose();
	}
	handle = null;

	return { view, offset, size, weakHandle };
}

async function collectHandle(weakHandle) {
	assert.equal(typeof globalThis.gc, "function", "--expose-gc is required");
	for (let i = 0; i < 32; i++) {
		await new Promise((resolve) => setImmediate(resolve));
		globalThis.gc();
		if (weakHandle.deref() === undefined) {
			await new Promise((resolve) => setImmediate(resolve));
			globalThis.gc();
			await new Promise((resolve) => setImmediate(resolve));
			return;
		}
	}
	assert.fail("simulator handle was not garbage-collected");
}

const retained = await createRetainedView();
if (mode === "gc") {
	await collectHandle(retained.weakHandle);
}

// Keep same-sized backend allocations alive so an unsafe dangling view is
// reliably exposed through allocator reuse rather than passing by accident.
const pressure = [];
const pressureCount = handleKind === "tiered" ? 16 : 64;
for (let i = 0; i < pressureCount; i++) {
	const handle = createHandle();
	const view = createView(handle);
	const { offset, size } = signalRange(handle, view);
	view.fill(PRESSURE_BYTE, offset, offset + size);
	pressure.push({ handle, view });
}

const retainedRange = () => [
	...retained.view.subarray(retained.offset, retained.offset + retained.size),
];
assert.deepEqual(retainedRange(), Array(retained.size).fill(INITIAL_BYTE));

retained.view.fill(
	UPDATED_BYTE,
	retained.offset,
	retained.offset + retained.size,
);
assert.deepEqual(retainedRange(), Array(retained.size).fill(UPDATED_BYTE));

for (const { handle } of pressure) {
	handle.dispose();
}

console.log("ok");
