/**
 * Ticks and clock edges for simulators whose WASM event modules run in
 * JavaScript (browser and WASI builds).
 *
 * These follow the rules of the native `Simulator`, from the same build-time
 * plans (`edgePlansJson()` of the raw handle):
 *
 * - An edge runs the event's own domain when it is the domain's active edge.
 * - Clocks computed combinationally from the event's signal (such as
 *   `clk & en` or `~clk`) are judged from the state before the edge with the
 *   signal at its new level, and their domains evaluate together with the
 *   event's own, against the same state, before any of them commits.
 * - After the commit, every domain whose clock then moved to its active edge
 *   runs, in rounds, until no further edge appears: domains clocked from
 *   state, and gates the commit moved.
 * - `tick` is the event's active edge without moving its signal. An edge
 *   moves the signal and leaves it at its new level.
 *
 * Gates are judged by settling the design with the signal at each level.
 *
 * In a four-state design, levels are 0, 1, x and z, and edges follow
 * IEEE 1800-2023 Table 9-2: a posedge leaves 0 or reaches 1, a negedge
 * leaves 1 or reaches 0, and a change between x and z is neither.
 *
 * @module
 */

/** The memory the kernels share. */
export interface EdgeMemory {
	readonly buffer: ArrayBuffer;
}

/** A one-bit event signal in the packed memory layout. */
export interface EdgePlanSignal {
	readonly offset: number;
	readonly width: number;
	/**
	 * The signal keeps unknown bits; its mask follows the value byte.
	 * Defaults to the plans' `fourState`.
	 */
	readonly fourState?: boolean;
}

/** The active edge of a domain, or `"any"` for a domain of unknown kind. */
export type EdgePlanEdge = "posedge" | "negedge" | "any";

/** A derived event domain. */
export interface EdgePlanDomain {
	readonly event: string;
	readonly signal: EdgePlanSignal;
	readonly edge: EdgePlanEdge;
}

/** What an edge of one event runs besides the event's own domain. */
export interface EdgePlanEvent {
	readonly signal: EdgePlanSignal;
	readonly edge: EdgePlanEdge;
	readonly combRead: boolean;
	readonly domains: readonly EdgePlanDomain[];
	readonly gated: readonly EdgePlanDomain[];
}

/** Per event name, its edge plan (`celox::EdgePlans`). */
export interface EdgePlans {
	readonly fourState: boolean;
	readonly events: Readonly<Record<string, EdgePlanEvent>>;
}

/** One compiled kernel: runs a module's `run` export. */
export type KernelRun = () => void;

/** The kernels of one event domain. */
export interface EdgeKernels {
	/** Evaluate and commit the domain. */
	evalApply(): void;
	/** Evaluate the domain's next state without committing it. */
	evalOnly?(): void;
	/** Commit what `evalOnly()` evaluated. */
	apply?(): void;
}

/**
 * Kernels looked up by event name on first use and cached: `evalApply(name)`
 * runs the event's domain, `phase(name, kind)` one phase of its split
 * kernels. An event without a domain of its own (a clock that only drives a
 * gated clock) has no kernels.
 */
export function cachedEdgeKernels(
	evalApply: (name: string) => KernelRun | undefined,
	phase: (name: string, kind: "evalOnly" | "apply") => KernelRun | undefined,
): (name: string) => EdgeKernels | undefined {
	const cache = new Map<string, EdgeKernels | undefined>();
	return (name) => {
		if (cache.has(name)) return cache.get(name);
		const run = evalApply(name);
		let kernels: EdgeKernels | undefined;
		if (run) {
			const evalOnly = phase(name, "evalOnly");
			const apply = phase(name, "apply");
			kernels =
				evalOnly && apply
					? { evalApply: run, evalOnly, apply }
					: { evalApply: run };
		}
		cache.set(name, kernels);
		return kernels;
	};
}

/**
 * The bit of an edge level that marks an unknown value; the value bit then
 * tells x (1) from z (0). An edge level is 0, 1, x (3) or z (2).
 */
const UNKNOWN = 2;

/** One entry of a cascade round: a domain and the edge level it last had. */
interface RoundEntry {
	readonly domain: EdgePlanDomain;
	was: number;
	fired: boolean;
}

/** A signal moved by an edge, with its edge level before and after. */
interface Lever {
	readonly signal: EdgePlanSignal;
	readonly from: number;
	readonly to: number;
}

/**
 * Whether a change between the edge levels `was` and `is` is an active edge
 * of a domain sensing `edge` (IEEE 1800-2023 Table 9-2).
 */
function stableEdge(edge: EdgePlanEdge, was: number, is: number): boolean {
	const betweenUnknowns = (was & is & UNKNOWN) !== 0;
	switch (edge) {
		case "posedge":
			return was !== 1 && is !== 0 && !betweenUnknowns;
		case "negedge":
			return was !== 0 && is !== 1 && !betweenUnknowns;
		default:
			return was !== is;
	}
}

/**
 * Edge execution over a shared memory and per-event kernels.
 */
export class WasmEdgeRunner {
	private readonly memory: EdgeMemory;
	private readonly plans: EdgePlans;
	private readonly eventNames: Map<number, string>;
	private readonly kernels: (name: string) => EdgeKernels | undefined;
	private readonly evalComb: () => void;

	constructor(
		memory: EdgeMemory,
		plans: EdgePlans,
		events: Readonly<Record<string, number>>,
		kernels: (name: string) => EdgeKernels | undefined,
		evalComb: () => void,
	) {
		this.memory = memory;
		this.plans = plans;
		this.eventNames = new Map(
			Object.entries(events).map(([name, id]) => [id, name]),
		);
		this.kernels = kernels;
		this.evalComb = evalComb;
	}

	/** Whether ticks of the event can run derived domains. */
	hasDerived(eventId: number): boolean {
		const plan = this.plan(eventId);
		return !!plan && (plan.domains.length > 0 || plan.gated.length > 0);
	}

	/** Whether the event's active edge is a rising one. */
	activeEdgeIsRising(eventId: number): boolean {
		return this.plan(eventId)?.edge !== "negedge";
	}

	/** The event's active edge without moving its signal, as `tick`. */
	tick(eventId: number): void {
		const name = this.name(eventId);
		const plan = this.plan(eventId);
		if (!plan || !this.hasDerived(eventId)) {
			this.evalComb();
			this.kernels(name)?.evalApply();
			this.evalComb();
			return;
		}
		const active = plan.edge === "negedge" ? 0 : 1;
		const levers: Lever[] =
			plan.gated.length > 0
				? [{ signal: plan.signal, from: active ^ 1, to: active }]
				: [];
		this.run(
			[name],
			plan.domains,
			plan.gated.map((g) => [g, 0]),
			levers,
			{
				settle: true,
				persist: false,
			},
		);
	}

	/**
	 * Move event signals at one instant; `rising[i]` gives the direction of
	 * `eventIds[i]`. `dirty` says whether combinational logic has pending
	 * changes. Returns whether combinational logic is settled afterwards:
	 * moving a signal that combinational logic does not read, without running
	 * a domain, leaves it as it was.
	 */
	edges(
		eventIds: readonly number[],
		rising: readonly boolean[],
		dirty = true,
	): boolean {
		const sources: string[] = [];
		const levers: Lever[] = [];
		const domains: EdgePlanDomain[] = [];
		const gated: [EdgePlanDomain, number][] = [];
		let moved = false;
		let combRead = false;
		eventIds.forEach((eventId, index) => {
			const name = this.name(eventId);
			const plan = this.plan(eventId);
			if (!plan) {
				return;
			}
			const to = rising[index] ? 1 : 0;
			const old = this.level(plan.signal);
			if (
				old === to ||
				levers.some((l) => l.signal.offset === plan.signal.offset)
			) {
				return;
			}
			moved = true;
			combRead ||= plan.combRead;
			this.setLevel(plan.signal, to);
			// A gated clock is judged from the lever's real level, x and z
			// included.
			const lever = { signal: plan.signal, from: old, to };
			if (plan.edge === "any" || (plan.edge === "posedge") === (to === 1)) {
				sources.push(name);
			}
			if (plan.gated.length > 0) {
				levers.push(lever);
				for (const domain of plan.gated) {
					if (!gated.some(([known]) => known.event === domain.event)) {
						gated.push([domain, levers.length - 1]);
					}
				}
			}
			for (const domain of plan.domains) {
				if (!domains.some((known) => known.event === domain.event)) {
					domains.push(domain);
				}
			}
		});
		if (!moved) {
			return !dirty;
		}
		const notSource = (domain: EdgePlanDomain) =>
			!sources.includes(domain.event);
		const ran = this.run(
			sources,
			domains.filter(notSource),
			gated.filter(([domain]) => notSource(domain)),
			levers,
			{ settle: dirty || combRead, persist: true },
		);
		return ran || (!dirty && !combRead);
	}

	private run(
		sources: readonly string[],
		domains: readonly EdgePlanDomain[],
		gated: readonly (readonly [EdgePlanDomain, number])[],
		levers: readonly Lever[],
		options: { settle: boolean; persist: boolean },
	): boolean {
		const rounds: RoundEntry[] = domains.map((domain) => ({
			domain,
			was: this.level(domain.signal),
			fired: false,
		}));
		if (gated.length === 0) {
			if (sources.length === 0) {
				return false;
			}
			this.fire(sources, options.settle);
			this.evalComb();
			this.rounds(rounds);
			return true;
		}
		const saved = options.persist
			? []
			: levers.map((lever) => this.saveLevel(lever.signal));
		let moved = false;
		for (const lever of levers) {
			if (this.level(lever.signal) !== lever.from) {
				this.setLevel(lever.signal, lever.from);
				moved = true;
			}
		}
		if (moved || options.settle) {
			this.evalComb();
		}
		const before = gated.map(([domain]) => this.level(domain.signal));
		for (const lever of levers) {
			this.setLevel(lever.signal, lever.to);
		}
		this.evalComb();
		const gatedEntries: RoundEntry[] = gated.map(([domain], index) => {
			const now = this.level(domain.signal);
			return {
				domain,
				was: now,
				fired: stableEdge(domain.edge, before[index] ?? now, now),
			};
		});
		this.fire(
			[
				...sources,
				...gatedEntries
					.filter((entry) => entry.fired)
					.map((entry) => entry.domain.event),
			],
			false,
		);
		this.evalComb();
		this.rounds([...rounds, ...gatedEntries]);
		if (!options.persist) {
			levers.forEach((lever, index) => {
				this.restoreLevel(lever.signal, saved[index] ?? [0, 0]);
			});
			this.evalComb();
		}
		return true;
	}

	/** Cascade rounds: run every domain whose clock moved to its active edge. */
	private rounds(entries: RoundEntry[]): void {
		for (;;) {
			const ready = entries.filter(
				(entry) =>
					!entry.fired &&
					stableEdge(
						entry.domain.edge,
						entry.was,
						this.level(entry.domain.signal),
					),
			);
			if (ready.length === 0) {
				return;
			}
			this.fire(
				ready.map((entry) => entry.domain.event),
				false,
			);
			for (const entry of ready) {
				entry.fired = true;
			}
			this.evalComb();
		}
	}

	/** Evaluate `events` against one state, then commit all of them. */
	private fire(events: readonly string[], settle: boolean): void {
		if (settle) {
			this.evalComb();
		}
		// An event without a domain of its own runs nothing.
		const kernels = events
			.map((name) => this.kernels(name))
			.filter((kernel): kernel is EdgeKernels => kernel !== undefined);
		const [only] = kernels;
		if (kernels.length === 1 && only !== undefined) {
			only.evalApply();
			return;
		}
		for (const kernel of kernels) {
			if (kernel.evalOnly) {
				kernel.evalOnly();
			} else {
				kernel.evalApply();
			}
		}
		for (const kernel of kernels) {
			kernel.apply?.();
		}
	}

	private bytes(): Uint8Array {
		return new Uint8Array(this.memory.buffer);
	}

	private byte(offset: number): number {
		return this.bytes()[offset] ?? 0;
	}

	private fourState(signal: EdgePlanSignal): boolean {
		return signal.fourState ?? this.plans.fourState;
	}

	/** The signal's edge level: 0, 1, or x (3) and z (2) while unknown. */
	private level(signal: EdgePlanSignal): number {
		const value = this.byte(signal.offset) & 1;
		if (this.fourState(signal) && (this.byte(signal.offset + 1) & 1) !== 0) {
			return UNKNOWN | value;
		}
		return value;
	}

	/** Move the signal to the edge level `level`. */
	private setLevel(signal: EdgePlanSignal, level: number): void {
		const bytes = this.bytes();
		bytes[signal.offset] = (this.byte(signal.offset) & ~1) | (level & 1);
		if (this.fourState(signal)) {
			const unknown = (level & UNKNOWN) !== 0 ? 1 : 0;
			bytes[signal.offset + 1] = (this.byte(signal.offset + 1) & ~1) | unknown;
		}
	}

	private saveLevel(signal: EdgePlanSignal): [number, number] {
		return [
			this.byte(signal.offset),
			this.fourState(signal) ? this.byte(signal.offset + 1) : 0,
		];
	}

	private restoreLevel(signal: EdgePlanSignal, saved: [number, number]): void {
		const bytes = this.bytes();
		bytes[signal.offset] = saved[0];
		if (this.fourState(signal)) {
			bytes[signal.offset + 1] = saved[1];
		}
	}

	private name(eventId: number): string {
		const name = this.eventNames.get(eventId);
		if (name === undefined) {
			throw new RangeError(`Unknown event ID ${eventId}`);
		}
		return name;
	}

	private plan(eventId: number): EdgePlanEvent | undefined {
		return this.plans.events[this.name(eventId)];
	}
}
