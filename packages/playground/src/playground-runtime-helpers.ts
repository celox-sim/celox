export const X = Symbol.for("veryl:X");
export const Z = Symbol.for("veryl:Z");

export function FourState(value: number | bigint, mask: number | bigint) {
	return {
		__fourState: true as const,
		value: BigInt(value),
		mask: BigInt(mask),
	};
}
