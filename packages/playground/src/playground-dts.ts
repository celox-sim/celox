/** A declaration file for Monaco's TypeScript service. */
export interface ExtraLib {
	readonly content: string;
	readonly filePath: string;
}

const CELOX_ROOT = "file:///node_modules/@celox-sim/celox";

/** Rewrite the relative imports of a `@celox-sim/celox` declaration file. */
function fixCeloxDtsImports(content: string): string {
	return content.replace(
		/from\s+["']\.\/([\w-]+)\.js["']/g,
		'from "@celox-sim/celox/$1"',
	);
}

/**
 * Monaco libraries for every declaration file of `@celox-sim/celox`'s
 * `dist`, keyed by path (as `import.meta.glob` returns them): each one under
 * `dist/` and under its bare subpath, and `index.d.ts` as the package root,
 * so that testbenches see the whole public API, explicit edges included.
 */
export function celoxDtsLibs(files: Record<string, string>): ExtraLib[] {
	const libs: ExtraLib[] = [];
	for (const [path, content] of Object.entries(files)) {
		const name = /([\w-]+)\.d\.ts$/.exec(path)?.[1];
		if (!name) continue;
		const fixed = fixCeloxDtsImports(content);
		libs.push({ content: fixed, filePath: `${CELOX_ROOT}/dist/${name}.d.ts` });
		libs.push({ content: fixed, filePath: `${CELOX_ROOT}/${name}.d.ts` });
	}
	return libs;
}

export type VirtualPortDirection = "input" | "output" | "inout";
export type VirtualPortKind = "logic" | "bit" | "clock" | "reset";

export interface VirtualPortInfo {
	direction: VirtualPortDirection;
	kind: VirtualPortKind;
	width: number;
	is4state: boolean;
}

function isFourStatePortKind(kind: VirtualPortKind): boolean {
	return kind === "logic" || kind === "clock" || kind === "reset";
}

function setterTypeForPort(port: VirtualPortInfo): string {
	return port.is4state ? "FourStateSignalValue" : "bigint";
}

function needsFourStateImport(ports: Record<string, VirtualPortInfo>): boolean {
	return Object.values(ports).some(
		(port) =>
			port.kind !== "clock" && port.direction !== "output" && port.is4state,
	);
}

export function buildVirtualModuleDts(
	moduleName: string,
	ports: Record<string, VirtualPortInfo>,
): string {
	const importLine = needsFourStateImport(ports)
		? 'import type { FourStateSignalValue, ModuleDefinition } from "@celox-sim/celox";'
		: 'import type { ModuleDefinition } from "@celox-sim/celox";';

	const portEntries = Object.entries(ports)
		.filter(([, port]) => port.kind !== "clock")
		.map(([name, port]) => {
			if (port.direction === "output") {
				return `  readonly ${name}: bigint;`;
			}
			return [
				`  get ${name}(): bigint;`,
				`  set ${name}(value: ${setterTypeForPort(port)});`,
			].join("\n");
		})
		.join("\n");

	return `${importLine}

export interface ${moduleName}Ports {
${portEntries}
}

export declare const ${moduleName}: ModuleDefinition<${moduleName}Ports>;
`;
}

export function extractPortsFromSource(
	source: string,
): Record<string, VirtualPortInfo> {
	const ports: Record<string, VirtualPortInfo> = {};
	const portRe =
		/(\w+)\s*:\s*(input|output|inout)\s+(?:'[_a-zA-Z]*\s+)?(logic|bit|clock|reset)(?:<(\d+)>)?/g;
	let match: RegExpExecArray | null;

	while ((match = portRe.exec(source)) !== null) {
		const kind = match[3] as VirtualPortKind;
		ports[match[1]] = {
			direction: match[2] as VirtualPortDirection,
			kind,
			width: match[4] ? parseInt(match[4], 10) : 1,
			is4state: isFourStatePortKind(kind),
		};
	}

	return ports;
}
