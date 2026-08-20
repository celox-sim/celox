const assert = require("node:assert/strict");

const addon = require(process.argv[2]);
assert.equal(typeof addon.fromMyArtifact, "function");

const handle = addon.fromMyArtifact({ moduleName: "NetAdder", width: 8 });
const layout = JSON.parse(handle.layoutJson);
assert.equal(typeof handle.sharedMemory, "function");
const memory = handle.sharedMemory();
memory[layout.a.offset] = 10;
memory[layout.b.offset] = 23;
handle.evalComb();
assert.equal(memory[layout.y.offset], 33);
handle.dispose();
