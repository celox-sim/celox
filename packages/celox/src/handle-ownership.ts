/** Release a factory-created handle if ownership cannot reach its wrapper. */
export function withOwnedHandle<T>(
	handle: { dispose(): void },
	construct: () => T,
): T {
	try {
		return construct();
	} catch (error) {
		// Cleanup must not replace the construction error.
		try {
			handle.dispose();
		} catch {
			/* Preserve the original failure. */
		}
		throw error;
	}
}
