import type * as monaco from "monaco-editor";

type TypeScriptDefaultsApi = Pick<
	typeof monaco.languages.typescript,
	"ScriptTarget" | "ModuleKind" | "ModuleResolutionKind"
>;

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
		target: ts.ScriptTarget.ES2022,
		module: ts.ModuleKind.ES2022,
		moduleResolution: ts.ModuleResolutionKind.Bundler,
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
