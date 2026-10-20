/** Portable helpers for operations without a WebAssembly instruction.
 * Modules cross the compiler worker boundary, so these cannot capture its handle.
 * The opcode/precision encoding is defined by celox-sir/src/float.rs.
 */
export function evaluateWasmFloat(
	lhs: bigint,
	rhs: bigint,
	code: bigint,
): bigint {
	if (code !== 4n && code !== 5n && code !== 260n && code !== 261n) {
		throw new Error(`Unsupported WebAssembly floating-point helper: ${code}`);
	}
	const wide = (code & 256n) !== 0n;
	const view = new DataView(new ArrayBuffer(8));
	const decode = (bits: bigint) => {
		view.setBigUint64(0, BigInt.asUintN(64, bits), true);
		return wide ? view.getFloat64(0, true) : view.getFloat32(0, true);
	};
	const a = decode(lhs);
	const b = decode(rhs);
	// libm pow treats 1**NaN and (+/-1)**(+/-Infinity) as 1.
	const result =
		(code & 255n) === 4n
			? a % b
			: a === 1 ||
					(Math.abs(a) === 1 && !Number.isFinite(b) && !Number.isNaN(b))
				? 1
				: a ** b;
	if (wide) {
		view.setFloat64(0, result, true);
		return view.getBigInt64(0, true);
	}
	view.setFloat32(0, result, true);
	return BigInt(view.getUint32(0, true));
}
