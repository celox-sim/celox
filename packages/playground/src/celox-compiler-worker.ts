import type {
	CeloxCompilerRequest,
	CeloxCompilerResponse,
} from "./celox-compiler-protocol.js";
import { compiledFromHandle } from "./compile-simulator.js";

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
				const value = compiledFromHandle(handle, request.fourState);
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
