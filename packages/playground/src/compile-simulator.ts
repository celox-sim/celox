import type { EdgePlans } from "@celox-sim/celox/wasm-edges";
import type { CompiledSimulator } from "./celox-compiler-protocol.js";

/** The parts of the wasm32 Celox handle a compiled simulator is made from. */
export interface CompilerHandle {
	readonly layoutJson: string;
	readonly eventsJson: string;
	readonly fourStateInitRegionsJson: string;
	readonly totalSize: number;
	combWasmBytes(): Uint8Array;
	eventWasmBytes(name: string): Uint8Array;
	edgePlansJson(): string;
	eventPhaseWasmBytes(name: string, phase: "evalOnly" | "apply"): Uint8Array;
}

function compileModule(
	bytes: () => Uint8Array,
): WebAssembly.Module | undefined {
	try {
		return new WebAssembly.Module(new Uint8Array(bytes()));
	} catch {
		// An event without a domain of its own (a clock that only drives a
		// gated clock) has no module.
		return undefined;
	}
}

/** Compile every module of `handle` for the playground runtime. */
export function compiledFromHandle(
	handle: CompilerHandle,
	fourState: boolean,
): CompiledSimulator {
	const events = JSON.parse(handle.eventsJson) as Record<string, number>;
	const eventModules: CompiledSimulator["eventModules"] = {};
	const phaseModules: CompiledSimulator["phaseModules"] = {};
	for (const name of Object.keys(events)) {
		const own = compileModule(() => handle.eventWasmBytes(name));
		if (!own) continue;
		eventModules[name] = own;
		const evalOnly = compileModule(() =>
			handle.eventPhaseWasmBytes(name, "evalOnly"),
		);
		const apply = compileModule(() =>
			handle.eventPhaseWasmBytes(name, "apply"),
		);
		if (evalOnly && apply) phaseModules[name] = { evalOnly, apply };
	}
	return {
		fourState,
		fourStateInitRegions: JSON.parse(handle.fourStateInitRegionsJson) as Array<
			[number, number]
		>,
		layout: JSON.parse(handle.layoutJson),
		events,
		totalSize: handle.totalSize,
		combModule: new WebAssembly.Module(new Uint8Array(handle.combWasmBytes())),
		eventModules,
		phaseModules,
		edgePlans: JSON.parse(handle.edgePlansJson()) as EdgePlans,
	};
}
