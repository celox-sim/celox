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

type AnalysisBatch = {
	sources: CeloxSourceFile[];
	waiters: Array<{
		resolve: (value: string) => void;
		reject: (reason: Error) => void;
	}>;
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
	private terminalError?: Error;
	private analysisInFlight = false;
	private queuedAnalysis?: AnalysisBatch;

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
		this.worker.addEventListener("messageerror", () => {
			this.fail(new Error("Celox compiler worker sent an invalid message"));
		});
	}

	async genTsFromSource(sources: CeloxSourceFile[]): Promise<string> {
		return this.request<string>({ type: "genTsFromSource", sources });
	}

	genTsFromSourceLatest(sources: CeloxSourceFile[]): Promise<string> {
		return new Promise<string>((resolve, reject) => {
			const waiter = { resolve, reject };
			if (!this.analysisInFlight) {
				this.analysisInFlight = true;
				void this.runAnalysis({ sources, waiters: [waiter] });
				return;
			}

			if (this.queuedAnalysis) {
				this.queuedAnalysis.sources = sources;
				this.queuedAnalysis.waiters.push(waiter);
			} else {
				this.queuedAnalysis = { sources, waiters: [waiter] };
			}
		});
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
		if (this.terminalError) throw this.terminalError;
		await this.ready;
		if (this.terminalError) throw this.terminalError;
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

	private async runAnalysis(batch: AnalysisBatch) {
		try {
			const value = await this.genTsFromSource(batch.sources);
			for (const waiter of batch.waiters) waiter.resolve(value);
		} catch (error) {
			const reason = error instanceof Error ? error : new Error(String(error));
			for (const waiter of batch.waiters) waiter.reject(reason);
		} finally {
			const next = this.queuedAnalysis;
			this.queuedAnalysis = undefined;
			if (next) void this.runAnalysis(next);
			else this.analysisInFlight = false;
		}
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
		if (this.terminalError) return;
		this.terminalError = error;
		this.rejectReady(error);
		for (const pending of this.pending.values()) pending.reject(error);
		this.pending.clear();
	}
}
