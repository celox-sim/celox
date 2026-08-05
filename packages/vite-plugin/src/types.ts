/** JSON output from `celox-gen-ts --json`. */
export interface GenTsJsonOutput {
	readonly projectPath: string;
	readonly modules: readonly GenTsModule[];
	readonly fileModules: Record<string, readonly string[]>;
}

/** Per-module entry from `celox-gen-ts --json`. */
export interface GenTsModule {
	readonly moduleName: string;
	readonly sourceFile: string;
	readonly dtsContent: string;
	readonly mdContent: string;
	readonly ports: Record<string, GenTsPortInfo>;
	readonly events: readonly string[];
	/** Whether this module is a native testbench (`#[test]` module). */
	readonly isTest: boolean;
}

/** Port metadata from the generator. */
export interface GenTsPortInfo {
	readonly direction: "input" | "output" | "inout";
	readonly type: "clock" | "reset" | "logic" | "bit";
	readonly width: number;
	readonly is4state: boolean;
}

export type TestbenchComponentManifests = Record<
	string,
	{ readonly manifest: string }
>;

export interface CeloxPluginOptions {
	/** Explicit path to the Veryl project root (directory containing Veryl.toml). */
	projectRoot?: string;
	/**
	 * Path to a TypeScript TB component registry module, absolute or relative
	 * to the Veryl project root. Its default export must map `$comp` names to
	 * `defineTbComponent` results. The plugin uses it for manifest generation
	 * and callback injection into generated native Vitest cases.
	 */
	testbenchComponents?: string;
}
