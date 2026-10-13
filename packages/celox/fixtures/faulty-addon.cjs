const stats = {created: 0, disposed: 0};
const settings = {stage: "ok", disposeThrows: false};
const originalError = new Error("memory acquisition failed");
function makeHandle() {
    stats.created++;
    const raw = {
        layoutJson: settings.stage === "layout" ? "invalid json" : "{}",
        eventsJson: settings.stage === "events" ? "invalid json" : "{}",
        hierarchyJson: settings.stage === "hierarchy" ? "invalid json" : "{}",
        warningsJson: settings.stage === "warnings" ? "invalid json" : "[]",
        stableSize: 64, totalSize: 64,
        sharedMemory() {
            if (settings.stage === "memory") throw originalError;
            if (settings.stage === "dut") return {buffer: {}};
            return new Uint8Array(new SharedArrayBuffer(64));
        },
        tick() {}, tickN() {}, evalComb() {}, dump() {},
        addClock() {}, schedule() {}, runUntil() {}, step: () => null,
        time: () => 0, nextEventTime: () => null,
        dispose() {
            stats.disposed++;
            if (settings.disposeThrows) throw new Error("cleanup failed");
        },
    };
    if (settings.stage === "wasm") {
        delete raw.tick; delete raw.tickN;
        raw.initialMemoryBytes = () => new Uint8Array(64);
        raw.combWasmBytes = () => new Uint8Array([0, 1, 2]);
        raw.eventWasmBytes = () => new Uint8Array();
    }
    return raw;
}
class NativeHandle {
    constructor() { Object.assign(this, makeHandle()); }
    static newTiered() { return new NativeHandle(); }
    static fromProject() { return new NativeHandle(); }
    static newTieredFromProject() { return new NativeHandle(); }
    static fromFrontendArtifact() { return new NativeHandle(); }
}
module.exports = {NativeSimulatorHandle: NativeHandle, NativeSimulationHandle: NativeHandle,
    __stats: stats, __settings: settings, __error: originalError};
