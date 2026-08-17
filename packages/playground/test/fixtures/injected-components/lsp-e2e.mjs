import assert from "node:assert/strict";
import { existsSync } from "node:fs";
import { spawn } from "node:child_process";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const root = import.meta.dirname;
const sourcePath = join(root, "src", "InjectedTestbench.veryl");
const manifestPath = join(
	root,
	".celox",
	"testbench-components",
	"veryl.manifest.json",
);

assert.ok(
	existsSync(manifestPath),
	"generated manifest is missing; run test:vite-components first",
);

const server = spawn(process.env.VERYL_LS ?? "veryl-ls", [], {
	cwd: root,
	stdio: ["pipe", "pipe", "pipe"],
});
let stderr = "";
let receiveBuffer = Buffer.alloc(0);
const messages = [];
const waiters = [];

server.stderr.setEncoding("utf8");
server.stderr.on("data", (chunk) => {
	stderr += chunk;
});

server.stdout.on("data", (chunk) => {
	receiveBuffer = Buffer.concat([receiveBuffer, chunk]);
	while (true) {
		const headerEnd = receiveBuffer.indexOf("\r\n\r\n");
		if (headerEnd < 0) return;
		const header = receiveBuffer.subarray(0, headerEnd).toString("ascii");
		const match = /(?:^|\r\n)Content-Length: (\d+)/i.exec(header);
		assert.ok(match, `LSP response has no Content-Length header: ${header}`);
		const length = Number(match[1]);
		const bodyStart = headerEnd + 4;
		if (receiveBuffer.length < bodyStart + length) return;
		const body = receiveBuffer
			.subarray(bodyStart, bodyStart + length)
			.toString("utf8");
		receiveBuffer = receiveBuffer.subarray(bodyStart + length);
		dispatch(JSON.parse(body));
	}
});

function dispatch(message) {
	if (message.id !== undefined && message.method !== undefined) {
		send({ jsonrpc: "2.0", id: message.id, result: null });
		return;
	}
	const waiterIndex = waiters.findIndex(({ predicate }) => predicate(message));
	if (waiterIndex >= 0) {
		const [{ resolve, timer }] = waiters.splice(waiterIndex, 1);
		clearTimeout(timer);
		resolve(message);
	} else {
		messages.push(message);
	}
}

function send(message) {
	const body = JSON.stringify(message);
	server.stdin.write(`Content-Length: ${Buffer.byteLength(body)}\r\n\r\n${body}`);
}

function waitFor(predicate, description) {
	const queuedIndex = messages.findIndex(predicate);
	if (queuedIndex >= 0) return Promise.resolve(messages.splice(queuedIndex, 1)[0]);
	return new Promise((resolve, reject) => {
		const timer = setTimeout(() => {
			const index = waiters.findIndex((waiter) => waiter.resolve === resolve);
			if (index >= 0) waiters.splice(index, 1);
			reject(new Error(`timed out waiting for ${description}\n${stderr}`));
		}, 15_000);
		waiters.push({ predicate, resolve, timer });
	});
}

const sourceUri = pathToFileURL(sourcePath).href;
const rootUri = pathToFileURL(root).href;
const completionSource = `#[test(InjectedTestbench)]
module InjectedTestbench {
    var store: $comp::vite_store;

    initial {
        store.
        set(8'h2a);
    }
}
`;

try {
	send({
		jsonrpc: "2.0",
		id: 1,
		method: "initialize",
		params: {
			processId: process.pid,
			rootUri,
			capabilities: {},
			workspaceFolders: [{ uri: rootUri, name: "injected-components" }],
		},
	});
	const initialized = await waitFor(
		(message) => message.id === 1,
		"initialize response",
	);
	assert.equal(initialized.error, undefined, JSON.stringify(initialized.error));
	send({ jsonrpc: "2.0", method: "initialized", params: {} });

	send({
		jsonrpc: "2.0",
		method: "textDocument/didOpen",
		params: {
			textDocument: {
				uri: sourceUri,
				languageId: "veryl",
				version: 1,
				text: completionSource,
			},
		},
	});

	const diagnostics = await waitFor(
		(message) =>
			message.method === "textDocument/publishDiagnostics" &&
			message.params?.uri === sourceUri,
		"component diagnostics",
	);
	assert.deepEqual(diagnostics.params.diagnostics, []);

	send({
		jsonrpc: "2.0",
		id: 2,
		method: "textDocument/completion",
		params: {
			textDocument: { uri: sourceUri },
			position: { line: 5, character: 14 },
			context: { triggerKind: 2, triggerCharacter: "." },
		},
	});
	const completion = await waitFor(
		(message) => message.id === 2,
		"component method completion",
	);
	assert.equal(completion.error, undefined, JSON.stringify(completion.error));
	const items = Array.isArray(completion.result)
		? completion.result
		: completion.result?.items;
	assert.ok(Array.isArray(items), "completion response has no items");
	const labels = new Set(items.map(({ label }) => label));
	assert.ok(labels.has("get"), "get method is missing from LSP completion");
	assert.ok(labels.has("set"), "set method is missing from LSP completion");

	send({ jsonrpc: "2.0", id: 3, method: "shutdown", params: null });
	await waitFor((message) => message.id === 3, "shutdown response");
	send({ jsonrpc: "2.0", method: "exit", params: null });
	console.log("veryl-ls recognized generated testbench components");
} finally {
	server.kill();
}
