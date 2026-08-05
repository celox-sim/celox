import { defineTbComponent } from "@celox-sim/celox";
import { initialValue } from "@fixture/tb-helper";
import { step } from "virtual:tb-step";

const viteStore = defineTbComponent<{ value: bigint }>({
	kind: "method_only",
	create: () => ({ value: initialValue }),
	methods: {
		set: {
			args: [{ name: "value", type: "value" }],
			call: ({ state }, [value]) => {
				state.value = (value as bigint) + step;
			},
		},
		get: {
			returns: { width: 8 },
			call: ({ state }) => ({ returnValue: state.value }),
		},
	},
});

export default { vite_store: viteStore };
