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
  readonly ports: Record<string, GenTsPortInfo>;
  readonly events: readonly string[];
}

/** Port metadata from the generator. */
export interface GenTsPortInfo {
  readonly direction: "input" | "output" | "inout";
  readonly type: "clock" | "reset" | "logic" | "bit";
  readonly width: number;
  readonly is4state: boolean;
}

/** Plugin options. */
export interface CeloxPluginOptions {
  /** Explicit path to the `celox-gen-ts` binary. */
  genTsBinary?: string;
  /** Explicit path to the Veryl project root (directory containing Veryl.toml). */
  projectRoot?: string;
}
