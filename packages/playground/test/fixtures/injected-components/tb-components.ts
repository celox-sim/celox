import { defineTbComponent } from "@celox-sim/celox";

const viteStore = defineTbComponent<{ value: bigint }>({
	kind: "method_only",
	create: () => ({ value: 0n }),
	methods: {
		set: {
			args: [{ name: "value", type: "value" }],
			call: ({ state }, [value]) => {
				state.value = value as bigint;
			},
		},
		get: {
			returns: { width: 8 },
			call: ({ state }) => ({ returnValue: state.value }),
		},
	},
});

export default { vite_store: viteStore };
