import {
	buildNapiOpts,
	loadNativeAddon,
	type NapiInjectedCall,
	type NapiInjectedComponent,
	type NapiInjectedResult,
	type NapiInjectedValue,
	type NapiTestResult,
} from "./napi-helpers.js";
import type { SimulatorOptions, SourceFile } from "./types.js";

export type TbPort = {
	direction: "input" | "output";
	role?: "clock" | "reset";
};

export type TbParameter = {
	type: "value" | "string";
	optional?: boolean;
};

export type TbFourStateValue = { value: bigint; maskXz: bigint };
export type TbOutputValue = bigint | TbFourStateValue;

export interface TbComponentContext<State> {
	readonly instance: string;
	readonly state: State;
	readonly inputs: Readonly<Record<string, bigint>>;
	readonly masks: Readonly<Record<string, bigint>>;
	readonly params: Readonly<Record<string, bigint | string | undefined>>;
	readonly cycle: bigint;
	readonly time: bigint;
	readonly seed: bigint;
	readonly firedClock?: string;
	readonly fourState: boolean;
}

export interface TbComponentEffects {
	outputs?: Record<string, TbOutputValue>;
	failures?: string[];
	logs?: string[];
	finish?: boolean;
}

export type TbMethodArgument = {
	name: string;
	type: "value" | "string";
};

export interface TbMethodResult extends TbComponentEffects {
	returnValue?: bigint;
}

export interface TbMethodDefinition<State> {
	args?: TbMethodArgument[];
	/** Omit for a unit-returning method. */
	returns?: { width: number };
	call(
		context: TbComponentContext<State>,
		args: ReadonlyArray<bigint | string | undefined>,
	): TbMethodResult | undefined;
}

export interface TbComponentDefinition<State> {
	kind: "clocked" | "method_only";
	ports?: Record<string, TbPort>;
	params?: Record<string, TbParameter>;
	methods?: Record<string, TbMethodDefinition<State>>;
	create?(context: Omit<TbComponentContext<never>, "state">): State;
	onInit?(context: TbComponentContext<State>): TbComponentEffects | undefined;
	onClock?(context: TbComponentContext<State>): TbComponentEffects | undefined;
	onReset?(context: TbComponentContext<State>): TbComponentEffects | undefined;
	onFinish?(context: TbComponentContext<State>): TbComponentEffects | undefined;
}

const isPromise = (value: unknown): value is PromiseLike<unknown> =>
	typeof value === "object" &&
	value !== null &&
	"then" in value &&
	typeof (value as { then?: unknown }).then === "function";

function records(values: NapiInjectedValue[]): {
	values: Record<string, bigint | string | undefined>;
	masks: Record<string, bigint>;
} {
	const result: Record<string, bigint | string | undefined> = {};
	const masks: Record<string, bigint> = {};
	for (const item of values) {
		if (item.name === undefined) continue;
		result[item.name] = item.bits ?? item.stringValue;
		masks[item.name] = item.maskXz ?? 0n;
	}
	return { values: result, masks };
}

/**
 * Define a synchronous TypeScript implementation of a Veryl clocked or
 * method-only testbench component. No Rust/Wasm component library is built;
 * the callback is injected into a single `runTest` invocation.
 */
export function defineTbComponent<State = undefined>(
	definition: TbComponentDefinition<State>,
): InjectedTbComponent {
	const manifest = JSON.stringify({
		kind: definition.kind,
		ports: Object.entries(definition.ports ?? {}).map(([name, port]) => ({
			name,
			dir: port.direction,
			...(port.role === undefined ? {} : { role: port.role }),
		})),
		params: Object.entries(definition.params ?? {}).map(([name, param]) => ({
			name,
			type: param.type,
			optional: param.optional ?? false,
		})),
		methods: Object.entries(definition.methods ?? {}).map(([name, method]) => ({
			name,
			args: method.args ?? [],
			...(method.returns === undefined
				? {}
				: { ret: "value", ret_width: method.returns.width }),
		})),
	});
	const states = new Map<string, State>();

	const handler = (call: NapiInjectedCall): NapiInjectedResult => {
		const inputRecords = records(call.inputs);
		const paramRecords = records(call.params);
		const base = {
			instance: call.instance,
			inputs: inputRecords.values as Record<string, bigint>,
			masks: inputRecords.masks,
			params: paramRecords.values,
			cycle: call.cycle,
			time: call.time,
			seed: call.seed,
			firedClock: call.firedClock,
			fourState: call.fourState,
		};

		if (call.phase === "create") {
			const state = definition.create?.(
				base as Omit<TbComponentContext<never>, "state">,
			) as State;
			if (isPromise(state)) {
				throw new TypeError(
					"testbench component create callback must be synchronous",
				);
			}
			states.set(call.instance, state);
			return {};
		}

		if (!states.has(call.instance)) {
			throw new Error(`unknown testbench component instance: ${call.instance}`);
		}
		const context = { ...base, state: states.get(call.instance) as State };
		const convertEffects = (
			effects: TbComponentEffects,
			returnValue?: NapiInjectedValue,
		): NapiInjectedResult => {
			const widths = new Map(call.ports.map((port) => [port.name, port.width]));
			return {
				outputs: Object.entries(effects.outputs ?? {}).map(([name, raw]) => {
					const width = widths.get(name);
					if (width === undefined)
						throw new Error(`unknown component output: ${name}`);
					const value =
						typeof raw === "bigint" ? { value: raw, maskXz: 0n } : raw;
					return {
						name,
						bits: value.value,
						maskXz: value.maskXz,
						width,
					};
				}),
				returnValue,
				failures: effects.failures,
				logs: effects.logs,
				finish: effects.finish,
			};
		};

		if (call.phase === "method") {
			const method =
				call.method === undefined
					? undefined
					: definition.methods?.[call.method];
			if (method === undefined) {
				throw new Error(`unknown component method: ${String(call.method)}`);
			}
			const args = call.args.map((arg) => arg.bits ?? arg.stringValue);
			const result = method.call(context, args) ?? {};
			if (isPromise(result)) {
				throw new TypeError("testbench component methods must be synchronous");
			}
			if (method.returns === undefined) {
				if (result.returnValue !== undefined) {
					throw new Error(`unit method ${call.method} returned a value`);
				}
				return convertEffects(result);
			}
			if (result.returnValue === undefined) {
				throw new Error(`component method ${call.method} returned no value`);
			}
			return convertEffects(result, {
				bits: result.returnValue,
				maskXz: 0n,
				width: method.returns.width,
			});
		}

		const callback =
			call.phase === "init"
				? definition.onInit
				: call.phase === "clock"
					? definition.onClock
					: call.phase === "reset"
						? definition.onReset
						: call.phase === "finish"
							? definition.onFinish
							: undefined;
		const effects = callback?.(context) ?? {};
		if (isPromise(effects)) {
			throw new TypeError("testbench component callbacks must be synchronous");
		}
		if (call.phase === "finish") states.delete(call.instance);

		return convertEffects(effects);
	};

	return { manifest, handler };
}

export interface InjectedTbComponent {
	/** @internal */ readonly manifest: string;
	/** @internal */ readonly handler: NapiInjectedComponent["handler"];
}

export interface RunTestOptions extends SimulatorOptions {
	components?: Record<string, InjectedTbComponent>;
}

/** Compile Veryl sources and run a native testbench with optional TS components. */
export function runTest(
	sources: ReadonlyArray<SourceFile>,
	top: string,
	options: RunTestOptions = {},
): NapiTestResult {
	const { components, ...nativeOptions } = options;
	const injected: NapiInjectedComponent[] = Object.entries(
		components ?? {},
	).map(([name, component]) => ({ name, ...component }));
	return loadNativeAddon().runTest(
		sources.map(({ content, path }) => ({ content, path })),
		top,
		buildNapiOpts(nativeOptions),
		injected,
	);
}

/** Run a Veryl project testbench with optional TS components. */
export function runTestFromProject(
	projectPath: string,
	top: string,
	options: RunTestOptions = {},
): NapiTestResult {
	const { components, ...nativeOptions } = options;
	const injected: NapiInjectedComponent[] = Object.entries(
		components ?? {},
	).map(([name, component]) => ({ name, ...component }));
	return loadNativeAddon().runTestFromProject(
		projectPath,
		top,
		buildNapiOpts(nativeOptions),
		injected,
	);
}
