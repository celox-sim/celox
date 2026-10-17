import type { EdgePlans } from "@celox-sim/celox/wasm-edges";

export type CeloxSourceFile = {
	content: string;
	path: string;
};

export type CompiledSimulator = {
	fourState: boolean;
	fourStateInitRegions: Array<[offset: number, byteSize: number]>;
	layout: Record<string, any>;
	events: Record<string, number>;
	totalSize: number;
	combModule: WebAssembly.Module;
	/** Per event, its own domain; absent for an event without one. */
	eventModules: Record<string, WebAssembly.Module>;
	/** Per event, the phases of its split kernels, where it has them. */
	phaseModules: Record<
		string,
		{ evalOnly: WebAssembly.Module; apply: WebAssembly.Module }
	>;
	/** Derived and gated domains of each event's ticks and edges. */
	edgePlans: EdgePlans;
};

export type CeloxCompilerRequest =
	| {
			id: number;
			type: "genTsFromSource";
			sources: CeloxSourceFile[];
	  }
	| {
			id: number;
			type: "compile";
			sources: CeloxSourceFile[];
			top: string;
			fourState: boolean;
	  };

export type CeloxCompilerResponse =
	| { type: "ready" }
	| { type: "initError"; error: string }
	| { type: "result"; id: number; value: unknown }
	| { type: "error"; id: number; error: string };
