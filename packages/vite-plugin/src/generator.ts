import { loadNativeAddon } from "@celox-sim/celox";
import type { GenTsJsonOutput } from "./types.js";

/**
 * Run the TypeScript type generator via the NAPI addon and parse the output.
 */
export function runGenTs(projectRoot: string): GenTsJsonOutput {
  const addon = loadNativeAddon();
  const json = addon.genTs(projectRoot);
  return JSON.parse(json) as GenTsJsonOutput;
}
