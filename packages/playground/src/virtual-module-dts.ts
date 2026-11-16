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

/** Port information as reported by `genTsFromSource` JSON. */
export interface JsonPortInfoLike {
	direction: string;
	type?: string;
	kind?: string;
	width: number;
	is4state?: boolean;
	is_4state?: boolean;
}

/** Convert compiler-reported ports to the virtual-module port shape. */
export function virtualPortsFromJson(
	ports: Record<string, JsonPortInfoLike>,
): Record<string, VirtualPortInfo> {
	const result: Record<string, VirtualPortInfo> = {};
	for (const [name, port] of Object.entries(ports)) {
		const kindName = port.kind ?? port.type ?? "logic";
		const kind: VirtualPortKind =
			kindName === "clock" || kindName === "reset" || kindName === "bit"
				? kindName
				: "logic";
		result[name] = {
			direction:
				port.direction === "output" || port.direction === "inout"
					? port.direction
					: "input",
			kind,
			width: port.width || 1,
			is4state: port.is4state ?? port.is_4state ?? kind !== "bit",
		};
	}
	return result;
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
