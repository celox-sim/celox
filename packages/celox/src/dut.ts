/**
 * DUT (Device Under Test) accessor factory.
 *
 * Builds a plain object with Object.defineProperty getter/setters that
 * read and write directly via DataView on a SharedArrayBuffer.
 * No Proxy is used — every port becomes a concrete property whose
 * accessors are fully visible to V8 inline caches.
 */

import type {
  NativeHandle,
  PortInfo,
  SignalLayout,
  FourStateValue,
} from "./types.js";
import type { HierarchyNode } from "./napi-helpers.js";
import { isFourStateValue } from "./types.js";

// ---------------------------------------------------------------------------
// Internal dirty-tracking state shared between DUT and Simulator/Simulation
// ---------------------------------------------------------------------------

/**
 * Mutable state shared between the DUT accessor and its owning
 * Simulator/Simulation instance.  The Simulator clears `dirty` after
 * tick()/runUntil(); the DUT sets it on any input write and checks it
 * before any output read.
 * @internal
 */
export interface DirtyState {
  dirty: boolean;
}

// ---------------------------------------------------------------------------
// DataView helpers
// ---------------------------------------------------------------------------

/** Read an unsigned integer of the given byte-size (little-endian), masked to width. */
function readNumber(view: DataView, offset: number, width: number): number {
  if (width <= 8) {
    const raw = view.getUint8(offset);
    return width === 8 ? raw : raw & ((1 << width) - 1);
  }
  if (width <= 16) {
    const raw = view.getUint16(offset, true);
    return width === 16 ? raw : raw & ((1 << width) - 1);
  }
  if (width <= 32) {
    const raw = view.getUint32(offset, true);
    return width === 32 ? raw : (raw & ((1 << width) - 1)) >>> 0;
  }
  // 33..53 bits — fits safely in a JS number
  const lo = view.getUint32(offset, true);
  const hi = view.getUint32(offset + 4, true) & ((1 << (width - 32)) - 1);
  return lo + hi * 0x1_0000_0000;
}

/** Write an unsigned integer of the given byte-size (little-endian). */
function writeNumber(
  view: DataView,
  offset: number,
  width: number,
  value: number,
): void {
  if (width <= 8) {
    view.setUint8(offset, value & ((1 << width) - 1));
  } else if (width <= 16) {
    view.setUint16(offset, value & ((1 << width) - 1), true);
  } else if (width <= 32) {
    view.setUint32(offset, value >>> 0, true);
  } else {
    // 33..53 bits
    view.setUint32(offset, value >>> 0, true);
    view.setUint32(offset + 4, Math.floor(value / 0x1_0000_0000) >>> 0, true);
  }
}

/** Read a wide value (≥ 54 bits) as BigInt, little-endian. */
function readBigInt(view: DataView, offset: number, byteSize: number): bigint {
  let result = 0n;
  // Read 8 bytes at a time, then remaining bytes
  const fullWords = Math.floor(byteSize / 8);
  for (let i = 0; i < fullWords; i++) {
    const word = view.getBigUint64(offset + i * 8, true);
    result |= word << BigInt(i * 64);
  }
  const remaining = byteSize % 8;
  if (remaining > 0) {
    const base = offset + fullWords * 8;
    for (let i = 0; i < remaining; i++) {
      result |= BigInt(view.getUint8(base + i)) << BigInt(fullWords * 64 + i * 8);
    }
  }
  return result;
}

/** Write a wide value (≥ 54 bits) as BigInt, little-endian. */
function writeBigInt(
  view: DataView,
  offset: number,
  byteSize: number,
  value: bigint,
): void {
  const fullWords = Math.floor(byteSize / 8);
  for (let i = 0; i < fullWords; i++) {
    view.setBigUint64(offset + i * 8, value & 0xFFFF_FFFF_FFFF_FFFFn, true);
    value >>= 64n;
  }
  const remaining = byteSize % 8;
  if (remaining > 0) {
    const base = offset + fullWords * 8;
    for (let i = 0; i < remaining; i++) {
      view.setUint8(base + i, Number(value & 0xFFn));
      value >>= 8n;
    }
  }
}

/** Read a signal value from the DataView. Always returns bigint. */
function readSignal(
  view: DataView,
  sig: SignalLayout,
): bigint {
  if (sig.width <= 53) {
    return BigInt(readNumber(view, sig.offset, sig.width));
  }
  return readBigInt(view, sig.offset, sig.byteSize);
}

/** Write a signal value to the DataView. Accepts bigint (number accepted for compat). */
function writeSignal(
  view: DataView,
  sig: SignalLayout,
  value: bigint | number,
): void {
  const bigVal = typeof value === "bigint" ? value : BigInt(value);
  if (sig.width <= 53) {
    writeNumber(view, sig.offset, sig.width, Number(bigVal));
  } else {
    writeBigInt(view, sig.offset, sig.byteSize, bigVal);
  }
}

/** Write a 4-state value (value + mask) to the DataView. */
function writeFourState(
  view: DataView,
  sig: SignalLayout,
  fsv: FourStateValue,
): void {
  writeSignal(view, sig, fsv.value);
  // Mask is stored immediately after the value bytes
  const maskLayout: SignalLayout = {
    offset: sig.offset + sig.byteSize,
    width: sig.width,
    byteSize: sig.byteSize,
    is4state: false,
    direction: sig.direction,
  };
  writeSignal(view, maskLayout, fsv.mask);
}

/** Write all-X mask for a signal. */
function writeAllX(view: DataView, sig: SignalLayout): void {
  // Value = 0, mask = all 1s
  writeSignal(view, sig, 0n);
  const allOnes = (1n << BigInt(sig.width)) - 1n;
  const maskLayout: SignalLayout = {
    offset: sig.offset + sig.byteSize,
    width: sig.width,
    byteSize: sig.byteSize,
    is4state: false,
    direction: sig.direction,
  };
  writeSignal(view, maskLayout, allOnes);
}

// ---------------------------------------------------------------------------
// DUT factory
// ---------------------------------------------------------------------------

/**
 * Create a DUT accessor object with defineProperty-based getters/setters.
 *
 * @param buffer    SharedArrayBuffer from NAPI create()
 * @param layout    Per-signal byte layout within the buffer
 * @param portDefs  Port metadata from the ModuleDefinition
 * @param handle    Native handle (for evalComb calls)
 * @param state     Shared dirty-tracking state
 * @param hierarchy Optional hierarchy node for child instance access
 */
export function createDut<P>(
  buffer: ArrayBuffer | SharedArrayBuffer,
  layout: Record<string, SignalLayout>,
  portDefs: Record<string, PortInfo>,
  handle: NativeHandle,
  state: DirtyState,
  hierarchy?: HierarchyNode,
): P {
  const view = new DataView(buffer);
  const obj = Object.create(null) as P;

  // Iterate portDefs (not layout) so that interface ports are discovered
  // even though their individual members are the ones that appear in layout.
  for (const [name, port] of Object.entries(portDefs)) {
    // Skip clock ports — they are controlled via tick()/addClock()
    if (port.type === "clock") continue;

    // Check for nested interface
    if (port.interface) {
      const nestedObj = createNestedDut(
        view,
        layout,
        port.interface,
        name,
        handle,
        state,
      );
      Object.defineProperty(obj, name, {
        value: nestedObj,
        enumerable: true,
        configurable: false,
        writable: false,
      });
      continue;
    }

    const sig = layout[name];
    if (!sig) continue;

    // Check for array port
    if (port.arrayDims && port.arrayDims.length > 0) {
      const arrayObj = createArrayDut(view, sig, port, handle, state);
      Object.defineProperty(obj, name, {
        value: arrayObj,
        enumerable: true,
        configurable: false,
        writable: false,
      });
      continue;
    }

    // Scalar port — define getter/setter
    defineSignalProperty(obj as object, name, view, sig, port, handle, state);
  }

  // Attach child instance accessors from hierarchy
  if (hierarchy) {
    for (const [childName, instances] of Object.entries(hierarchy.children)) {
      if (instances.length === 1) {
        const childDut = createChildDut(buffer, instances[0]!, handle, state);
        Object.defineProperty(obj, childName, {
          value: childDut,
          enumerable: true,
          configurable: false,
          writable: false,
        });
      } else if (instances.length > 1) {
        const childDuts = instances.map((inst) =>
          createChildDut(buffer, inst, handle, state),
        );
        Object.defineProperty(obj, childName, {
          value: childDuts,
          enumerable: true,
          configurable: false,
          writable: false,
        });
      }
    }
  }

  return obj;
}

/**
 * Create a child instance DUT accessor from a HierarchyNode.
 * Recursively creates accessors for the child's signals and its own children.
 */
export function createChildDut(
  buffer: ArrayBuffer | SharedArrayBuffer,
  hierarchy: HierarchyNode,
  handle: NativeHandle,
  state: DirtyState,
): object {
  const view = new DataView(buffer);
  const obj = Object.create(null);

  // Define signal properties for this child instance
  for (const [name, port] of Object.entries(hierarchy.ports)) {
    if (port.type === "clock") continue;

    const sig = hierarchy.forDut[name];
    if (!sig) continue;

    if (port.arrayDims && port.arrayDims.length > 0) {
      const arrayObj = createArrayDut(view, sig, port, handle, state);
      Object.defineProperty(obj, name, {
        value: arrayObj,
        enumerable: true,
        configurable: false,
        writable: false,
      });
      continue;
    }

    defineSignalProperty(obj, name, view, sig, port, handle, state);
  }

  // Recursively attach children
  for (const [childName, instances] of Object.entries(hierarchy.children)) {
    if (instances.length === 1) {
      const childDut = createChildDut(buffer, instances[0]!, handle, state);
      Object.defineProperty(obj, childName, {
        value: childDut,
        enumerable: true,
        configurable: false,
        writable: false,
      });
    } else if (instances.length > 1) {
      const childDuts = instances.map((inst) =>
        createChildDut(buffer, inst, handle, state),
      );
      Object.defineProperty(obj, childName, {
        value: childDuts,
        enumerable: true,
        configurable: false,
        writable: false,
      });
    }
  }

  return obj;
}

/** Define a single scalar signal property on the target object. */
function defineSignalProperty(
  target: object,
  name: string,
  view: DataView,
  sig: SignalLayout,
  port: PortInfo | undefined,
  handle: NativeHandle,
  state: DirtyState,
): void {
  const isOutput = port?.direction === "output";
  const isInput = port?.direction === "input";

  Object.defineProperty(target, name, {
    get(): bigint {
      // Output reads: lazy evalComb if dirty
      if (state.dirty && !isInput) {
        handle.evalComb();
        state.dirty = false;
      }
      return readSignal(view, sig);
    },

    set(value: bigint | number | symbol | FourStateValue) {
      if (isOutput) {
        throw new Error(`Cannot write to output port '${name}'`);
      }

      if (value === Symbol.for("veryl:X")) {
        if (!sig.is4state) {
          throw new Error(`Port '${name}' is not 4-state; cannot assign X`);
        }
        writeAllX(view, sig);
      } else if (isFourStateValue(value)) {
        if (!sig.is4state) {
          throw new Error(`Port '${name}' is not 4-state; cannot assign FourState`);
        }
        writeFourState(view, sig, value);
      } else {
        const bigVal = typeof value === "bigint" ? value : BigInt(value as number);
        writeSignal(view, sig, bigVal);
        // Clear mask when writing a defined value to a 4-state signal
        if (sig.is4state) {
          const maskLayout: SignalLayout = {
            offset: sig.offset + sig.byteSize,
            width: sig.width,
            byteSize: sig.byteSize,
            is4state: false,
            direction: sig.direction,
          };
          writeSignal(view, maskLayout, 0n);
        }
      }

      state.dirty = true;
    },

    enumerable: true,
    configurable: false,
  });
}

// ---------------------------------------------------------------------------
// Nested interface accessor
// ---------------------------------------------------------------------------

function createNestedDut(
  view: DataView,
  layout: Record<string, SignalLayout>,
  members: Record<string, PortInfo>,
  prefix: string,
  handle: NativeHandle,
  state: DirtyState,
): object {
  const obj = Object.create(null);

  for (const [memberName, memberPort] of Object.entries(members)) {
    const qualifiedName = `${prefix}.${memberName}`;
    const sig = layout[qualifiedName];
    if (!sig) continue;

    if (memberPort.interface) {
      const nested = createNestedDut(
        view,
        layout,
        memberPort.interface,
        qualifiedName,
        handle,
        state,
      );
      Object.defineProperty(obj, memberName, {
        value: nested,
        enumerable: true,
        configurable: false,
        writable: false,
      });
    } else {
      defineSignalProperty(obj, memberName, view, sig, memberPort, handle, state);
    }
  }

  return obj;
}

// ---------------------------------------------------------------------------
// Array port accessor
// ---------------------------------------------------------------------------

function createArrayDut(
  view: DataView,
  baseSig: SignalLayout,
  port: PortInfo,
  handle: NativeHandle,
  state: DirtyState,
): object {
  const dims = port.arrayDims!;
  const elementWidth = port.width;
  const elementByteSize = Math.ceil(elementWidth / 8);
  const totalElements = dims.reduce((a, b) => a * b, 1);
  const isOutput = port.direction === "output";
  const isInput = port.direction === "input";
  const baseOffset = baseSig.offset;
  const is4state = baseSig.is4state;

  return {
    length: totalElements,

    at(i: number): bigint {
      if (state.dirty && !isInput) {
        handle.evalComb();
        state.dirty = false;
      }
      const offset = baseOffset + i * elementByteSize;
      if (elementWidth <= 53) {
        return BigInt(readNumber(view, offset, elementWidth));
      }
      return readBigInt(view, offset, elementByteSize);
    },

    set(i: number, value: bigint | number | symbol | FourStateValue): void {
      if (isOutput) {
        throw new Error("Cannot write to output array port");
      }
      const offset = baseOffset + i * elementByteSize;
      if (value === Symbol.for("veryl:X")) {
        if (!is4state) {
          throw new Error("Array port is not 4-state; cannot assign X");
        }
        const elemSig: SignalLayout = {
          offset, width: elementWidth, byteSize: elementByteSize,
          is4state, direction: baseSig.direction,
        };
        writeAllX(view, elemSig);
      } else if (isFourStateValue(value)) {
        if (!is4state) {
          throw new Error("Array port is not 4-state; cannot assign FourState");
        }
        const elemSig: SignalLayout = {
          offset, width: elementWidth, byteSize: elementByteSize,
          is4state, direction: baseSig.direction,
        };
        writeFourState(view, elemSig, value);
      } else {
        const bigVal = typeof value === "bigint" ? value : BigInt(value as number);
        const elemSig: SignalLayout = {
          offset, width: elementWidth, byteSize: elementByteSize,
          is4state, direction: baseSig.direction,
        };
        writeSignal(view, elemSig, bigVal);
        if (is4state) {
          const maskSig: SignalLayout = {
            offset: offset + elementByteSize, width: elementWidth,
            byteSize: elementByteSize, is4state: false, direction: baseSig.direction,
          };
          writeSignal(view, maskSig, 0n);
        }
      }
      state.dirty = true;
    },
  };
}

// ---------------------------------------------------------------------------
// 4-state read helper (exported for advanced use)
// ---------------------------------------------------------------------------

/**
 * Read the raw 4-state (value, mask) pair for a signal.
 * Mask bits set to 1 indicate X/Z.
 */
export function readFourState(
  buffer: ArrayBuffer | SharedArrayBuffer,
  sig: SignalLayout,
): [value: bigint, mask: bigint] {
  if (!sig.is4state) {
    throw new Error("Signal is not 4-state");
  }
  const view = new DataView(buffer);
  const value = readSignal(view, sig);
  const maskSig: SignalLayout = {
    offset: sig.offset + sig.byteSize,
    width: sig.width,
    byteSize: sig.byteSize,
    is4state: false,
    direction: sig.direction,
  };
  const mask = readSignal(view, maskSig);
  return [value, mask];
}
