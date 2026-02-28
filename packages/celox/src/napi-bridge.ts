/**
 * Backward-compatibility re-exports from napi-helpers.ts.
 *
 * New code should import from "./napi-helpers.js" directly.
 */

export {
  createSimulatorBridge,
  createSimulationBridge,
  type RawNapiAddon,
  type RawNapiSimulatorHandle,
  type RawNapiSimulationHandle,
} from "./napi-helpers.js";
