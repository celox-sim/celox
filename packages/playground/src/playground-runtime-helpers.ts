export const X = Symbol.for("veryl:X");
export const Z = Symbol.for("veryl:Z");

export function FourState(value: number | bigint, mask: number | bigint) {
	return {
		__fourState: true as const,
		value: BigInt(value),
		mask: BigInt(mask),
	};
}

export type PlaygroundSignalLayout = {
	offset: number;
	width: number;
	byte_size: number;
	is_4state: boolean;
};

export type PlaygroundSignalValue =
	| bigint
	| number
	| symbol
	| { __fourState: true; value: bigint; mask: bigint };

export type PlaygroundEventHandle = {
	readonly name: string;
	readonly id: number;
};

export type PlaygroundTickRuntime = {
	readonly defaultEventId: number;
	readonly knownEventIds: ReadonlySet<number>;
	readonly flushDirty: () => void;
	readonly tickOne: (eventId: number) => void;
};

const MAX_U32 = 0xffff_ffff;

function validateU32(value: number, label: "Event ID" | "Tick count"): void {
	if (!Number.isInteger(value) || value < 0 || value > MAX_U32) {
		throw new RangeError(
			`${label} ${value} must be an integer between 0 and ${MAX_U32}`,
		);
	}
}

function validateKnownEventId(
	eventId: number,
	knownEventIds: ReadonlySet<number>,
): void {
	validateU32(eventId, "Event ID");
	if (!knownEventIds.has(eventId)) {
		const availableIds = [...knownEventIds].sort((a, b) => a - b).join(", ");
		throw new RangeError(
			`Unknown event ID ${eventId}. Available IDs: ${availableIds || "(none)"}`,
		);
	}
}

/**
 * Normalize, validate, and execute a Playground `Simulator.tick()` call.
 * Validation completes before either injected callback is invoked.
 */
export function runPlaygroundTicks(
	runtime: PlaygroundTickRuntime,
	eventOrCount?: PlaygroundEventHandle | number,
	count?: number,
): void {
	let eventId: number;
	let ticks: number;
	let hasExplicitEvent = false;

	if (typeof eventOrCount === "object" && eventOrCount !== null) {
		eventId = eventOrCount.id;
		ticks = count ?? 1;
		hasExplicitEvent = true;
	} else if (typeof eventOrCount === "number" && count !== undefined) {
		eventId = eventOrCount;
		ticks = count;
		hasExplicitEvent = true;
	} else {
		eventId = runtime.defaultEventId;
		ticks = typeof eventOrCount === "number" ? eventOrCount : (count ?? 1);
	}

	validateU32(ticks, "Tick count");
	if (hasExplicitEvent) {
		validateKnownEventId(eventId, runtime.knownEventIds);
	} else if (ticks > 0) {
		if (runtime.knownEventIds.size === 0) {
			throw new Error("Simulator has no events to tick");
		}
		validateKnownEventId(eventId, runtime.knownEventIds);
	}

	runtime.flushDirty();
	for (let i = 0; i < ticks; i++) runtime.tickOne(eventId);
}

export function initializeFourStateMemory(
	memory: WebAssembly.Memory,
	regions: Array<[offset: number, byteSize: number]>,
) {
	const bytes = new Uint8Array(memory.buffer);
	for (const [offset, byteSize] of regions) {
		bytes.fill(0xff, offset, offset + byteSize * 2);
	}
}

export function writeSignalValue(
	view: DataView,
	sig: PlaygroundSignalLayout,
	value: PlaygroundSignalValue,
	fourStateEnabled: boolean,
) {
	const isX = value === X;
	const isZ = value === Z;
	const isExplicitFourState =
		typeof value === "object" && value !== null && value.__fourState === true;
	if (
		(isX || isZ || isExplicitFourState) &&
		(!fourStateEnabled || !sig.is_4state)
	) {
		throw new Error("Cannot assign a 4-state value in 2-state mode");
	}

	const widthMask = (1n << BigInt(sig.width)) - 1n;
	let data: bigint;
	let mask: bigint;
	if (isX) {
		data = widthMask;
		mask = widthMask;
	} else if (isZ) {
		data = 0n;
		mask = widthMask;
	} else if (isExplicitFourState) {
		data = value.value & widthMask;
		mask = value.mask & widthMask;
	} else {
		data = BigInt(value as bigint | number) & widthMask;
		mask = 0n;
	}

	for (let i = 0; i < sig.byte_size; i++) {
		view.setUint8(sig.offset + i, Number(data & 0xffn));
		data >>= 8n;
	}
	if (sig.is_4state) {
		for (let i = 0; i < sig.byte_size; i++) {
			view.setUint8(sig.offset + sig.byte_size + i, Number(mask & 0xffn));
			mask >>= 8n;
		}
	}
}
