// Generate `.celox/` sidecars so `tsc` can resolve `*.veryl` imports without
// running Vite first.
import { resolve } from "node:path";
import { runGenTs } from "../../vite-plugin/dist/generator.js";
import { generateSidecars } from "../../vite-plugin/dist/sidecar.js";
import { withSidecarLock } from "../../vite-plugin/dist/sidecar-lock.js";

const root = resolve(import.meta.dirname, "..");
await withSidecarLock(root, () => generateSidecars(runGenTs(root), root));
