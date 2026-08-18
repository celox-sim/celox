import type {
	CeloxCompilerRequest,
	CeloxCompilerResponse,
	CompiledSimulator,
} from "./celox-compiler-protocol.js";

const workerScope = globalThis as unknown as {
	addEventListener(
		type: "message",
		listener: (event: MessageEvent<CeloxCompilerRequest>) => void,
	): void;
	postMessage(message: CeloxCompilerResponse): void;
};

try {
	const celox = await import("./celox-wasm-loader.js");

	workerScope.addEventListener("message", (event) => {
		const request = event.data;
		try {
			if (request.type === "genTsFromSource") {
				const value = celox.genTsFromSource(request.sources);
				workerScope.postMessage({ type: "result", id: request.id, value });
				return;
			}

			const handle = new celox.NativeSimulatorHandle(
				request.sources,
				request.top,
				{ fourState: request.fourState },
			);
			try {
				const layout = JSON.parse(handle.layoutJson);
				const events = JSON.parse(handle.eventsJson) as Record<string, number>;
				const combModule = new WebAssembly.Module(
					new Uint8Array(handle.combWasmBytes()),
				);
				const eventModules: Record<string, WebAssembly.Module> = {};
				for (const name of Object.keys(events)) {
					try {
						eventModules[name] = new WebAssembly.Module(
							new Uint8Array(handle.eventWasmBytes(name)),
						);
					} catch {
						// An event without generated code is valid and has nothing to run.
					}
				}
				const value: CompiledSimulator = {
					layout,
					events,
					totalSize: handle.totalSize,
					combModule,
					eventModules,
				};
				workerScope.postMessage({ type: "result", id: request.id, value });
			} finally {
				handle.dispose();
			}
		} catch (error) {
			workerScope.postMessage({
				type: "error",
				id: request.id,
				error: error instanceof Error ? error.message : String(error),
			});
		}
	});

	workerScope.postMessage({ type: "ready" });
} catch (error) {
	workerScope.postMessage({
		type: "initError",
		error: error instanceof Error ? error.message : String(error),
	});
}
