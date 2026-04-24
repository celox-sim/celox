import { transform as transformWithSucrase } from "sucrase";

type EmitOutputFile = {
	name: string;
	text: string;
};

type EmitOutputLike = {
	outputFiles?: EmitOutputFile[];
};

function stripInjectedModuleSyntax(jsCode: string): string {
	return jsCode
		.replace(
			/^(?:import|export)\s+.*(?:from\s+)?["'][^"']*["'];?\s*$/gm,
			"",
		)
		.replace(/^\s*export\s*\{[^}]*\};?\s*$/gm, "")
		.replace(
			/^(?:const|let|var)\s+\{[^}]*\}\s*=\s*require\s*\([^)]*\);?\s*$/gm,
			"",
		)
		.replace(/^\s+/, "");
}

export function transpileTestbench(
	tsCode: string,
	testPath: string,
	emitOutput?: EmitOutputLike,
): string {
	let jsCode = emitOutput?.outputFiles?.find((f) => f.name.endsWith(".js"))?.text;

	if (!jsCode) {
		try {
			jsCode = transformWithSucrase(tsCode, {
				transforms: ["typescript"],
				filePath: testPath,
			}).code;
		} catch (e: any) {
			throw new Error(
				`Failed to transpile ${testPath} as TypeScript: ${e.message ?? String(e)}`,
			);
		}
	}

	return stripInjectedModuleSyntax(jsCode);
}
