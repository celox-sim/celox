import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";

import { CeloxCompilerClient } from "../src/celox-compiler-client";

type WorkerListener = (event: any) => void;

class FakeWorker {
	static instance: FakeWorker;

	readonly messages: any[] = [];
	private readonly listeners = new Map<string, WorkerListener[]>();

	constructor() {
		FakeWorker.instance = this;
	}

	addEventListener(type: string, listener: WorkerListener) {
		const listeners = this.listeners.get(type) ?? [];
		listeners.push(listener);
		this.listeners.set(type, listeners);
	}

	postMessage(message: any) {
		this.messages.push(message);
	}

	emit(type: string, event: any) {
		for (const listener of this.listeners.get(type) ?? []) listener(event);
	}
}

async function createReadyClient() {
	const client = new CeloxCompilerClient();
	FakeWorker.instance.emit("message", { data: { type: "ready" } });
	await client.ready;
	return { client, worker: FakeWorker.instance };
}

describe("CeloxCompilerClient", () => {
	beforeEach(() => {
		vi.stubGlobal("Worker", FakeWorker);
	});

	afterEach(() => {
		vi.unstubAllGlobals();
	});

	test("coalesces queued diagnostics to the latest source", async () => {
		const { client, worker } = await createReadyClient();
		const first = client.genTsFromSourceLatest([
			{ path: "main.veryl", content: "first" },
		]);
		const second = client.genTsFromSourceLatest([
			{ path: "main.veryl", content: "second" },
		]);
		const latest = client.genTsFromSourceLatest([
			{ path: "main.veryl", content: "latest" },
		]);
		await Promise.resolve();

		expect(worker.messages).toHaveLength(1);
		worker.emit("message", {
			data: { type: "result", id: worker.messages[0].id, value: "first-result" },
		});

		await vi.waitFor(() => expect(worker.messages).toHaveLength(2));
		expect(worker.messages[1].sources[0].content).toBe("latest");
		worker.emit("message", {
			data: { type: "result", id: worker.messages[1].id, value: "latest-result" },
		});

		await expect(first).resolves.toBe("first-result");
		await expect(second).resolves.toBe("latest-result");
		await expect(latest).resolves.toBe("latest-result");
	});

	test("rejects later requests after a terminal worker failure", async () => {
		const { client, worker } = await createReadyClient();
		const pending = client.genTsFromSource([
			{ path: "main.veryl", content: "module Top {}" },
		]);
		await Promise.resolve();
		const messageCount = worker.messages.length;
		worker.emit("error", { message: "worker crashed" });

		await expect(pending).rejects.toThrow("worker crashed");
		await expect(
			client.genTsFromSource([
				{ path: "main.veryl", content: "module Next {}" },
			]),
		).rejects.toThrow("worker crashed");
		expect(worker.messages).toHaveLength(messageCount);
	});
});
