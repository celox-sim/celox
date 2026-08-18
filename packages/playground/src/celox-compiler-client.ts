import type {
	CeloxCompilerRequest,
	CeloxCompilerResponse,
	CeloxSourceFile,
	CompiledSimulator,
} from "./celox-compiler-protocol.js";

type PendingRequest = {
	resolve: (value: unknown) => void;
	reject: (reason: Error) => void;
};

type CompilerRequestBody<T = CeloxCompilerRequest> =
	T extends CeloxCompilerRequest ? Omit<T, "id"> : never;

export class CeloxCompilerClient {
	readonly ready: Promise<void>;

	private readonly worker: Worker;
	private readonly pending = new Map<number, PendingRequest>();
	private nextRequestId = 1;
	private resolveReady!: () => void;
	private rejectReady!: (reason: Error) => void;

	constructor() {
		this.ready = new Promise<void>((resolve, reject) => {
			this.resolveReady = resolve;
			this.rejectReady = reject;
		});
		this.worker = new Worker(
			new URL("./celox-compiler-worker.ts", import.meta.url),
			{ type: "module" },
		);
		this.worker.addEventListener("message", (event) => {
			this.handleMessage(event.data as CeloxCompilerResponse);
		});
		this.worker.addEventListener("error", (event) => {
			this.fail(new Error(event.message || "Celox compiler worker failed"));
		});
	}

	async genTsFromSource(sources: CeloxSourceFile[]): Promise<string> {
		return this.request<string>({ type: "genTsFromSource", sources });
	}

	async compile(
		sources: CeloxSourceFile[],
		top: string,
		options: { fourState: boolean },
	): Promise<CompiledSimulator> {
		return this.request<CompiledSimulator>({
			type: "compile",
			sources,
			top,
			fourState: options.fourState,
		});
	}

	private async request<T>(request: CompilerRequestBody): Promise<T> {
		await this.ready;
		const id = this.nextRequestId++;
		const result = new Promise<T>((resolve, reject) => {
			this.pending.set(id, {
				resolve: resolve as (value: unknown) => void,
				reject,
			});
		});
		this.worker.postMessage({ ...request, id });
		return result;
	}

	private handleMessage(message: CeloxCompilerResponse) {
		if (message.type === "ready") {
			this.resolveReady();
			return;
		}
		if (message.type === "initError") {
			this.fail(new Error(message.error));
			return;
		}

		const pending = this.pending.get(message.id);
		if (!pending) return;
		this.pending.delete(message.id);
		if (message.type === "result") pending.resolve(message.value);
		else pending.reject(new Error(message.error));
	}

	private fail(error: Error) {
		this.rejectReady(error);
		for (const pending of this.pending.values()) pending.reject(error);
		this.pending.clear();
	}
}
