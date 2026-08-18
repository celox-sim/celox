export type CeloxSourceFile = {
	content: string;
	path: string;
};

export type CompiledSimulator = {
	layout: Record<string, any>;
	events: Record<string, number>;
	totalSize: number;
	combModule: WebAssembly.Module;
	eventModules: Record<string, WebAssembly.Module>;
};

export type CeloxCompilerRequest =
	| {
			id: number;
			type: "genTsFromSource";
			sources: CeloxSourceFile[];
	  }
	| {
			id: number;
			type: "compile";
			sources: CeloxSourceFile[];
			top: string;
			fourState: boolean;
	  };

export type CeloxCompilerResponse =
	| { type: "ready" }
	| { type: "initError"; error: string }
	| { type: "result"; id: number; value: unknown }
	| { type: "error"; id: number; error: string };
