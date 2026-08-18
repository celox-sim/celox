import type { CeloxSourceFile } from "./celox-compiler-protocol.js";

export function genTsFromSource(sources: CeloxSourceFile[]): string;

export class NativeSimulatorHandle {
	constructor(
		sources: CeloxSourceFile[],
		top: string,
		options?: { fourState?: boolean },
	);
	readonly layoutJson: string;
	readonly eventsJson: string;
	readonly fourStateInitRegionsJson: string;
	readonly totalSize: number;
	combWasmBytes(): Uint8Array;
	eventWasmBytes(name: string): Uint8Array;
	dispose(): void;
}
