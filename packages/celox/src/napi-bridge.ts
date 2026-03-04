/**
 * Backward-compatibility re-exports from napi-helpers.ts.
 *
 * New code should import from "./napi-helpers.js" directly.
 */

export {
	createSimulationBridge,
	createSimulatorBridge,
	type RawNapiAddon,
	type RawNapiSimulationHandle,
	type RawNapiSimulatorHandle,
} from "./napi-helpers.js";
