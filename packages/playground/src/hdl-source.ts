import type { VirtualPortInfo } from "./virtual-module-dts.js";

/** Whether `path` names a SystemVerilog source (`.sv` / `.svh`). */
export function isSvPath(path: string): boolean {
	return /\.svh?$/i.test(path);
}

/** Whether `path` names a Veryl source. */
export function isVerylPath(path: string): boolean {
	return path.endsWith(".veryl");
}

/** Whether `path` names any HDL source the playground can compile. */
export function isHdlPath(path: string): boolean {
	return isSvPath(path) || isVerylPath(path);
}

/** Monaco language id for an HDL source path. */
export function hdlLanguageForPath(path: string): string {
	if (isSvPath(path)) return "systemverilog";
	if (isVerylPath(path)) return "veryl";
	return "plaintext";
}

/** Module names declared in an HDL source, in declaration order. */
export function extractModuleNames(source: string): string[] {
	const names: string[] = [];
	const re = /(?:^|\s)(?:module|macromodule)\s+([A-Za-z_][\w]*)/gm;
	const clean = stripComments(source);
	let match: RegExpExecArray | null;
	while ((match = re.exec(clean)) !== null) names.push(match[1]);
	return names;
}

/**
 * Paths at which the virtual declaration of `moduleName` must be registered
 * so a testbench import resolves to it: the actual path of every open file
 * declaring the module (and of the compiler-reported source file), each with
 * its `.<ext>` declaration sibling used by `allowArbitraryExtensions`, plus
 * the conventional `src/<moduleName>.<ext>` locations of the examples.
 */
export function hdlDeclarationPaths(
	moduleName: string,
	files: { path: string; content: string }[],
	reportedSourceFile?: string,
): string[] {
	const paths = new Set<string>();
	const addSourcePath = (path: string) => {
		const normalized = path.replace(/\\/g, "/").replace(/^\.\//, "");
		paths.add(normalized);
		const dot = normalized.lastIndexOf(".");
		if (dot > 0) {
			const ext = normalized.slice(dot + 1);
			paths.add(`${normalized.slice(0, dot)}.d.${ext}.ts`);
		}
	};
	if (reportedSourceFile) addSourcePath(reportedSourceFile);
	for (const file of files) {
		if (extractModuleNames(file.content).includes(moduleName)) {
			addSourcePath(file.path);
		}
	}
	for (const ext of ["veryl", "sv", "svh"]) {
		addSourcePath(`src/${moduleName}.${ext}`);
	}
	return [...paths];
}

function stripComments(source: string): string {
	return source.replace(/\/\*[\s\S]*?\*\//g, " ").replace(/\/\/[^\n]*/g, " ");
}

/** Split `text` on commas that are not nested in `()`, `[]` or `{}`. */
function splitTopLevel(text: string): string[] {
	const parts: string[] = [];
	let depth = 0;
	let start = 0;
	for (let i = 0; i < text.length; i++) {
		const ch = text[i];
		if (ch === "(" || ch === "[" || ch === "{") depth++;
		else if (ch === ")" || ch === "]" || ch === "}") depth--;
		else if (ch === "," && depth === 0) {
			parts.push(text.slice(start, i));
			start = i + 1;
		}
	}
	parts.push(text.slice(start));
	return parts.map((p) => p.trim()).filter(Boolean);
}

/** The balanced `(...)` group starting at `openIndex`, and its end index. */
function balancedGroup(
	source: string,
	openIndex: number,
): { text: string; end: number } | null {
	let depth = 0;
	for (let i = openIndex; i < source.length; i++) {
		if (source[i] === "(") depth++;
		else if (source[i] === ")") {
			depth--;
			if (depth === 0) return { text: source.slice(openIndex + 1, i), end: i };
		}
	}
	return null;
}

function packedWidth(packed: string | undefined): number {
	if (!packed) return 1;
	const range = packed.match(/^\[\s*(-?\d+)\s*:\s*(-?\d+)\s*\]$/);
	if (range) return Math.abs(Number(range[1]) - Number(range[2])) + 1;
	return 1;
}

const SV_DIRECTIONS = ["input", "output", "inout", "ref"] as const;
type SvDirection = (typeof SV_DIRECTIONS)[number];

interface PortShape {
	direction: SvDirection;
	kind: "logic" | "bit";
	width: number;
}

function parsePortDeclaration(
	decl: string,
	inherited: PortShape | null,
): { name: string; shape: PortShape } | null {
	let rest = decl.trim();
	if (!rest) return null;
	// Drop a default value (`= expr`).
	const eq = rest.indexOf("=");
	if (eq >= 0) rest = rest.slice(0, eq).trim();

	const shape: PortShape = inherited
		? { ...inherited }
		: { direction: "input", kind: "logic", width: 1 };

	const dirMatch = rest.match(/^(input|output|inout|ref)\b\s*/);
	if (dirMatch) {
		shape.direction = dirMatch[1] as SvDirection;
		rest = rest.slice(dirMatch[0].length);
	}
	rest = rest.replace(/^(?:wire|tri|uwire|var)\b\s*/, "");
	const typeMatch = rest.match(
		/^(logic|reg|bit|integer|int|time)\b\s*(?:signed|unsigned)?\s*/,
	);
	if (typeMatch) {
		shape.kind =
			typeMatch[1] === "bit" || typeMatch[1] === "int" ? "bit" : "logic";
		rest = rest.slice(typeMatch[0].length);
	} else {
		rest = rest.replace(/^(?:signed|unsigned)\b\s*/, "");
	}
	const packedMatch = rest.match(/^(\[[^\]]*\](?:\s*\[[^\]]*\])?)\s*/);
	if (packedMatch) {
		shape.width = packedWidth(packedMatch[1].trim().split(/\s+/)[0]);
		rest = rest.slice(packedMatch[0].length);
	}
	const nameMatch = rest.match(/^([A-Za-z_][\w]*)/);
	if (!nameMatch) return null;
	return { name: nameMatch[1], shape };
}

function toPortInfo(shape: PortShape): VirtualPortInfo {
	const is4state = shape.kind === "logic";
	return {
		direction: shape.direction === "ref" ? "inout" : shape.direction,
		kind: shape.kind,
		width: shape.width,
		is4state,
	};
}

/**
 * Ports of the first module in a SystemVerilog source.
 *
 * Handles ANSI headers (`module M(input logic [7:0] a, ...);`), including
 * ports that inherit the direction/type of the previous port, and non-ANSI
 * headers whose ports are declared in the body. This is the editor-side
 * fallback used until the compiler reports the analyzed ports; widths of
 * parameterized ranges fall back to 1.
 */
export function extractSvPortsFromSource(
	source: string,
): Record<string, VirtualPortInfo> {
	const ports: Record<string, VirtualPortInfo> = {};
	const clean = stripComments(source);
	const header = clean.match(/(?:module|macromodule)\s+[A-Za-z_][\w]*/);
	if (!header || header.index === undefined) return ports;

	let cursor = header.index + header[0].length;
	// Skip a parameter port list `#(...)`.
	const afterName = clean.slice(cursor).match(/^\s*#\s*\(/);
	if (afterName) {
		const group = balancedGroup(clean, cursor + afterName[0].length - 1);
		if (group) cursor = group.end + 1;
	}
	const open = clean.indexOf("(", cursor);
	if (open < 0) return ports;
	const group = balancedGroup(clean, open);
	if (!group) return ports;

	let inherited: PortShape | null = null;
	for (const decl of splitTopLevel(group.text)) {
		const parsed = parsePortDeclaration(decl, inherited);
		if (!parsed) continue;
		inherited = parsed.shape;
		ports[parsed.name] = toPortInfo(parsed.shape);
	}

	// Non-ANSI body declarations (`input logic [7:0] a;`), and any ANSI port
	// the header only named, fill in or refine the header entries.
	const body = clean.slice(group.end + 1);
	const declRe =
		/\b(input|output|inout)\s+(?:(?:wire|tri|var)\s+)?(logic|reg|bit|integer|int|time)?\s*(?:signed|unsigned)?\s*(\[[^\]]*\])?\s*([^;]+);/g;
	let match: RegExpExecArray | null;
	while ((match = declRe.exec(body)) !== null) {
		for (const nameDecl of splitTopLevel(match[4])) {
			const name = nameDecl.match(/^([A-Za-z_][\w]*)/)?.[1];
			if (!name) continue;
			const kind = match[2] === "bit" || match[2] === "int" ? "bit" : "logic";
			ports[name] = toPortInfo({
				direction: match[1] as SvDirection,
				kind,
				width: packedWidth(match[3]),
			});
		}
	}
	return ports;
}
