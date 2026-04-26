import type * as monaco from "monaco-editor";

type TypeScriptDefaultsApi = Pick<
	typeof monaco.languages.typescript,
	"ScriptTarget" | "ModuleKind" | "ModuleResolutionKind"
>;

function pickEnumValue<T extends Record<string, string | number | undefined>>(
	enumObject: T,
	keys: readonly string[],
): string | number {
	for (const key of keys) {
		const value = enumObject[key];
		if (value !== undefined) return value;
	}
	throw new Error(`Missing Monaco enum value for ${keys.join(" / ")}`);
}

export const MONACO_TESTBENCH_LIBS = [
	"es2022",
	"es2020.bigint",
	"dom",
	"dom.iterable",
] as const;

export function buildMonacoTestbenchCompilerOptions(
	ts: TypeScriptDefaultsApi,
): monaco.languages.typescript.CompilerOptions {
	return {
		target: pickEnumValue(ts.ScriptTarget, ["ES2022", "ES2021", "ES2020"]),
		module: pickEnumValue(ts.ModuleKind, ["ES2022", "ES2020", "ESNext"]),
		moduleResolution: pickEnumValue(ts.ModuleResolutionKind, [
			"Bundler",
			"NodeNext",
			"Node16",
			"NodeJs",
			"Node",
		]),
		// Keep BigInt support explicit so bigint literals in examples/tests stay valid.
		lib: [...MONACO_TESTBENCH_LIBS],
		allowArbitraryExtensions: true,
		esModuleInterop: true,
		skipLibCheck: true,
		strict: true,
		noEmit: true,
		rootDirs: ["file:///src", "file:///test", "file:///.celox/src"],
		paths: {
			"@celox-sim/celox": ["file:///node_modules/@celox-sim/celox/index.d.ts"],
			"@celox-sim/celox/*": [
				"file:///node_modules/@celox-sim/celox/*.d.ts",
				"file:///node_modules/@celox-sim/celox/dist/*.d.ts",
			],
		},
	};
}
