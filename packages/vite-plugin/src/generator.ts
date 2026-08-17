import { loadNativeAddon } from "@celox-sim/celox";
import type { GenTsJsonOutput, TestbenchComponentManifests } from "./types.js";

/**
 * Run the TypeScript type generator via the NAPI addon and parse the output.
 */
export function runGenTs(
	projectRoot: string,
	testbenchComponents?: TestbenchComponentManifests,
): GenTsJsonOutput {
	const addon = loadNativeAddon();
	const manifests = Object.entries(testbenchComponents ?? {}).map(
		([name, component]) => ({ name, manifest: component.manifest }),
	);
	const json = addon.genTs(projectRoot, manifests);
	return JSON.parse(json) as GenTsJsonOutput;
}
