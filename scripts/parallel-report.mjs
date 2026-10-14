// Shared offline viewer and testable trace analysis. No network dependencies.
export function analyzeDispatch(dispatch, runs, waits, compiled) {
	const issues = [],
		points = [],
		byTask = new Map(runs.map((r) => [r.task, r]));
	const laneTasks = new Map();
	for (const t of [...compiled].sort((a, b) => a.task - b.task)) {
		if (!laneTasks.has(t.lane)) laneTasks.set(t.lane, []);
		laneTasks.get(t.lane).push(t.task);
	}
	let work = 0,
		wait = 0;
	for (const r of runs) {
		if (r.skipped) continue;
		if (r.code !== undefined && r.code !== 0)
			issues.push(`Task ${r.task} failed with code ${r.code}`);
		if (r.end_ns < r.start_ns || r.end_ns > dispatch.elapsed_ns)
			issues.push(`Invalid interval: task ${r.task}`);
		work += r.end_ns - r.start_ns;
		points.push([r.start_ns, 1], [r.end_ns, -1]);
		const spec = compiled.find((t) => t.task === r.task);
		if (!spec) issues.push(`Missing compiled metadata: task ${r.task}`);
		for (const [lane, completed] of spec?.waits ?? []) {
			const producer = byTask.get(laneTasks.get(lane)?.[completed - 1]);
			if (
				!producer ||
				producer.skipped ||
				(producer.code !== undefined && producer.code !== 0) ||
				producer.end_ns > r.start_ns
			)
				issues.push(
					`Unsatisfied dependency: lane ${lane} #${completed} → task ${r.task}`,
				);
		}
	}
	for (const task of compiled) {
		if (!byTask.has(task.task)) issues.push(`Missing execution record: task ${task.task}`);
	}
	for (const lane of laneTasks.keys()) {
		const executed = runs
			.filter((r) => r.lane === lane && !r.skipped)
			.sort((a, b) => a.task - b.task);
		for (let i = 1; i < executed.length; i++)
			if (executed[i - 1].end_ns > executed[i].start_ns)
				issues.push(`Lane ${lane} ran tasks out of order`);
	}
	for (const w of waits) wait += w.end_ns - w.start_ns;
	points.sort((a, b) => a[0] - b[0] || a[1] - b[1]);
	let concurrent = 0,
		peak = 0;
	for (const [, delta] of points) {
		concurrent += delta;
		peak = Math.max(peak, concurrent);
	}
	return {
		work_ns: work,
		wait_ns: wait,
		peak,
		average: dispatch.elapsed_ns ? work / dispatch.elapsed_ns : 0,
		utilization: dispatch.elapsed_ns
			? work / (dispatch.elapsed_ns * dispatch.lanes)
			: 0,
		issues,
	};
}

// A missing identity in an old capture is safer than showing another program's SIR.
export function nativeSirForTask(events, schedule, task) {
    return events.find(e => e.event === "native_task_sir" &&
        e.schedule === schedule && e.task === task && schedule !== undefined);
}

// StateBits locations count bits; older captures and envelopes count bytes.
export function stateByteRange(storage, offset, length) {
    if (storage !== "StateBits") return {storage, offset, length};
    const first = Math.floor(offset / 8);
    return {storage, offset:first, length:Math.ceil((offset + length) / 8) - first, bit_offset:offset, bit_length:length};
}

export function signalsForFootprints(footprints, signals, program) {
    const ranges = [...JSON.stringify(footprints).matchAll(
        /object: (StateBits|State|WriteEnvelope), offset: (-?\d+), byte_len: (\d+)/g
    )].map(m => stateByteRange(m[1], Number(m[2]), Number(m[3])));
    const matches = [];
    for (const signal of signals.filter(s => s.event === "signal" && s.program === program)) {
        for (const region of ["offset", "working_offset", "sparse_offset"]) {
            const raw = signal[region];
            const offset = typeof raw === "number" ? raw : Number(raw?.match(/Some\((\d+)\)/)?.[1]);
            if (!Number.isFinite(offset)) continue;
            const overlapping = ranges.filter(r => r.offset < offset + Math.ceil(signal.width / 8)
                && offset < r.offset + r.length);
            if (overlapping.length) matches.push({...signal, matched_region:region, matched_offset:offset, ranges:overlapping});
        }
    }
    return matches;
}

// Tracing span IDs may be recycled after a span closes. Compiler plan IDs
// survive that lifetime and distinguish preliminary units from final tasks.
export function physicalPlanKey(event) {
    const scopes = [...(event.scopes ?? [])].reverse();
    const field = name => event[name] ?? scopes.find(s => s[name] !== undefined)?.[name];
    const program = field("program"), schedule = field("schedule"), level = field("hazard_level");
    return program !== undefined && schedule !== undefined && level !== undefined
        ? JSON.stringify([program, schedule, level])
        : `legacy:${scopeKey(event)}`;
}

// Partition IDs are unique across programs and repeated builds in this process.
export function sourcePlanKey(event) {
    const partition = event.source_partition ?? [...(event.scopes ?? [])].reverse()
        .find(s => s.source_partition !== undefined)?.source_partition;
    return partition !== undefined ? `partition:${partition}` : `legacy:${scopeKey(event)}`;
}

// Resolve each nested record in its own program, including detail bundles
// containing runtime records, compiled SIR and source paths together.
export function createSourceFormatter(events) {
    const field = (e, name) => e?.[name] ?? [...(e?.scopes ?? [])].reverse().find(s => s[name] !== undefined)?.[name];
    const identity = e => field(e, "source_program") !== undefined
        ? `source:${field(e, "source_program")}`
        : field(e, "program") !== undefined ? `program:${field(e, "program")}` : undefined;
    const schedules = new Map(events.filter(e => e.event === "compiled_task").map(e => [e.schedule, identity(e)]));
    const dispatches = new Map(events.filter(e => e.event === "dispatch").map(e => [`${e.pool}/${e.epoch}`, schedules.get(e.schedule)]));
    const key = e => identity(e) ?? schedules.get(e?.schedule) ?? dispatches.get(`${e?.pool}/${e?.epoch}`);
    const maps = new Map();
    for (const s of events.filter(e => e.event === "signal" || e.event === "source_signal")) {
        const id = key(s) ?? "legacy";
        if (!maps.has(id)) maps.set(id, new Map());
        const names = maps.get(id);
        for (const address of [s.address, s.source_address,
            s.object !== undefined ? `instance_id: InstanceId(${s.instance}), var_id: StateObjectId(${s.object})` : undefined,
            s.source_object != null ? `instance_id: InstanceId(${s.instance}), var_id: SourceVarId(${s.source_object})` : undefined]) {
            if (!address) continue;
            // Ambiguous legacy records stay raw instead of acquiring a wrong name.
            names.set(address, names.has(address) && names.get(address) !== s.name ? null : s.name);
        }
    }
    function transform(value, inherited) {
        if (typeof value === "string") {
            const names = maps.get(inherited ?? "legacy");
            return value.replace(/AbsoluteAddr\(inst\d+, (?:state|var)\d+\)|instance_id: InstanceId\(\d+\), var_id: (?:StateObjectId|SourceVarId)\(\d+\)/g,
                address => names?.get(address) ?? address);
        }
        if (!value || typeof value !== "object") return value;
        const id = key(value) ?? inherited;
        if (Array.isArray(value)) return value.map(v => transform(v, id));
        return Object.fromEntries(Object.entries(value).map(([k,v]) => [k, transform(v, id)]));
    }
    return (value, context = value) => JSON.stringify(transform(value, key(context)), null, 2);
}

export function scopeKey(event) {
	return (event.scopes ?? []).map((s) => s.id).join("/");
}

export function dependenciesForPaths(paths) {
	const predecessors = new Map(paths.map((p) => [p.path, []]));
	for (const p of paths)
		for (const user of p.users ?? []) predecessors.get(user)?.push(p.path);
	return predecessors;
}

// Keep estimated work intact: an atomic item's cost belongs to its assigned
// lane even when it contains paths from more than one RTL instance.
export function partitionLanes(items, assignments) {
    const lanes = new Map();
    for (const item of items) {
        const lane = assignments[item.item];
        if (!lanes.has(lane)) lanes.set(lane, {lane, items:0, cost:0, pinned:0, crossLaneEdges:0});
        const row = lanes.get(lane);
        row.items++;
        row.cost += item.cost;
        row.pinned += Number(Boolean(item.pinned_lane0));
        row.crossLaneEdges += (item.predecessors ?? []).filter(p => assignments[p] !== lane).length;
    }
    return [...lanes.values()].sort((a,b) => a.lane - b.lane);
}

// Evaluate static task costs with the scheduler's lane and wait recurrence.
export function plannedTimeline(compiled, duration = () => 1, synchronization = 0) {
    const tasks = [...compiled].sort((a, b) => a.task - b.task);
    const lanes = new Map(), result = [], ids = new Set();
    for (const task of tasks) {
        if (ids.has(task.task)) throw new Error(`Duplicate task ${task.task}`);
        ids.add(task.task);
        const own = lanes.get(task.lane) ?? [];
        let start = own.at(-1)?.end ?? 0;
        const predecessors = own.length ? [own.at(-1).task] : [];
        for (const [lane, count] of task.waits ?? []) {
            const producer = lanes.get(lane)?.[count - 1];
            if (lane === task.lane || !Number.isInteger(count) || count < 1 || !producer)
                throw new Error(`Unresolved dependency: lane ${lane} #${count} → task ${task.task}`);
            start = Math.max(start, producer.end + synchronization);
            predecessors.push(producer.task);
        }
        const cost = duration(task);
        if (!Number.isFinite(cost) || cost < 0) throw new Error(`Missing duration: task ${task.task}`);
        const row = {...task, start, end: start + cost, predecessors};
        own.push(row);
        lanes.set(task.lane, own);
        result.push(row);
    }
    return {tasks: result, makespan: Math.max(0, ...result.map(t => t.end))};
}

// Older schema-1 captures retained Rust Debug task tables. Decode only this
// data grammar; never evaluate source or infer tasks from execution records.
export function candidateTasks(candidate) {
    if (Array.isArray(candidate.task_lanes) && Array.isArray(candidate.task_items) && Array.isArray(candidate.task_waits)) {
        return candidate.task_lanes.map((lane, task) => ({task, lane,
            items: candidate.task_items[task], waits: candidate.task_waits[task]}));
    }
    if (typeof candidate.tasks !== "string")
        throw new Error("Static task data unavailable in this older capture; re-capture required.");
    let rows;
    try {
        rows = JSON.parse(candidate.tasks.replace(/\bLane(?:Task|Wait)\s*\{/g, "{")
            .replace(/\b(lane|sequence|items|waits|completed_tasks)\s*:/g, '"$1":'));
    } catch {
        throw new Error("Invalid captured static task table.");
    }
    if (!Array.isArray(rows)) throw new Error("Invalid captured static task table.");
    return rows.map((t, task) => {
        if (!Number.isInteger(t.lane) || t.lane < 0 || !Array.isArray(t.items) ||
            !t.items.every(i => Number.isInteger(i) && i >= 0) || !Array.isArray(t.waits))
            throw new Error("Invalid captured static task table.");
        return {...t, task, waits:t.waits.map(w => [w.lane, w.completed_tasks])};
    });
}

export function candidateTimeline(candidate, items) {
    const tasks = candidateTasks(candidate);
    const costs = new Map(items.map(i => [i.item, i.cost]));
    if (!Number.isFinite(candidate.synchronization_cost) || candidate.synchronization_cost < 0 ||
        !Number.isFinite(candidate.task_overhead) || candidate.task_overhead < 0)
        throw new Error("Missing static overhead or synchronization cost.");
    const plan = plannedTimeline(tasks, t => t.items.reduce((sum, position) =>
        sum + (costs.get(candidate.order[position]) ?? NaN), candidate.task_overhead), candidate.synchronization_cost);
    if (candidate.makespan !== undefined && plan.makespan !== candidate.makespan)
        throw new Error(`Static reconstruction mismatch: ${plan.makespan} vs recorded ${candidate.makespan}.`);
    return plan;
}

export function selectedCandidateIndex(candidates, decisions) {
    return candidates.findIndex(c => decisions.some(d => d.accepted && sourcePlanKey(d) === sourcePlanKey(c) &&
        d.task_budget === c.task_budget &&
        d.estimated_makespan === c.makespan && JSON.stringify(d.lanes) === JSON.stringify(c.lanes) &&
        JSON.stringify(d.order) === JSON.stringify(c.order)));
}

// Unit positions are preserved by the per-unit SIR passes. Comb kernels use
// that vector directly; Fused kernels append FF units after the comb prefix.
// Refuse ambiguous captures instead of matching p/t IDs or source names.
export function taskProvenance(candidate, events, compiled) {
    const fail = reason => ({reason, links:[], runtime:[]});
    if (!candidate) return fail("No static candidate selected.");
    const decisions = events.filter(e => e.event === "partition_decision" && e.accepted);
    if (selectedCandidateIndex([candidate], decisions) < 0)
        return fail("This candidate was not adopted; it has no runtime task mapping.");
    if (new Set(events.filter(e => e.event === "lowered_task").map(sourcePlanKey)).size !== 1)
        return fail("Ambiguous or missing lowering scope; no provenance mapping inferred.");
    const lowered = events.filter(e => e.event === "lowered_task" && sourcePlanKey(e) === sourcePlanKey(candidate));
    let specs;
    try { specs = candidateTasks(candidate); } catch (error) { return fail(error.message); }
    const byTask = new Map(lowered.map(e => [e.task,e]));
    if (lowered.length !== specs.length || byTask.size !== specs.length)
        return fail("Incomplete lowering records.");
    const owners = new Map();
    for (const p of specs) {
        const record = byTask.get(p.task);
        const sourceItems = p.items.map(i => candidate.order[i]);
        if (!record || record.lane !== p.lane || JSON.stringify(record.items) !== JSON.stringify(sourceItems) || !Array.isArray(record.units))
            return fail("Lowering records do not match the selected candidate.");
        for (const unit of record.units) {
            if (!Number.isInteger(unit) || unit < 0 || owners.has(unit)) return fail("Invalid or duplicate lowering unit.");
            owners.set(unit,p.task);
        }
    }
    const count = owners.size;
    if ([...owners.keys()].some(u => u >= count)) return fail("Incomplete lowering unit sequence.");
    const comb = events.filter(e => e.event === "compiled_task" && e.kind === "Comb");
    if (new Set(comb.map(t => t.schedule)).size !== 1) return fail("Missing or ambiguous compiled Comb schedule.");
    if (new Set([...comb, ...compiled].map(t=>t.program).filter(p=>p!==undefined)).size > 1)
        return fail("Compiled Comb and runtime programs differ.");
    const combUnits = new Set();
    for (const t of comb) for (const u of t.units ?? []) {
        if (!owners.has(u) || combUnits.has(u) || byTask.get(owners.get(u)).lane !== t.lane)
            return fail("Compiled Comb units do not match the lowering records.");
        combUnits.add(u);
    }
    if (combUnits.size !== count) return fail("Incomplete compiled Comb units.");
    if (!compiled.length) return fail("No compiled metadata for this dispatch.");
    const kind = compiled[0].kind;
    if (compiled.some(t => t.kind !== kind)) return fail("Mixed runtime phases.");
    const hasComb = kind === "Comb" || kind?.startsWith("Fused(");
    if (!hasComb && !kind?.startsWith("Event(")) return fail("Unknown runtime phase.");
    const runtime = [], links = [], seen = new Set();
    for (const t of compiled) {
        if (!Array.isArray(t.units)) return fail("Missing runtime unit records.");
        const ps = new Set(), ff = [];
        for (const u of t.units) {
            if (!Number.isInteger(u) || u < 0 || seen.has(u)) return fail("Invalid or duplicate runtime unit.");
            seen.add(u);
            if (hasComb && u < count) {
                const p = owners.get(u);
                if (byTask.get(p).lane !== t.lane) return fail("Runtime lane differs from recorded lowering provenance.");
                ps.add(p); links.push({p, unit:u, t:t.task});
            } else {
                if (kind === "Comb") return fail("Unknown compiled Comb unit.");
                ff.push(u);
            }
        }
        runtime.push({task:t.task, planned:[...ps].sort((a,b)=>a-b), ff_units:ff});
    }
    if (hasComb && [...owners.keys()].some(u => !seen.has(u))) return fail("Runtime schedule is missing combinational units.");
    return {reason:null, links, runtime};
}

// Completion-count waits and lane sequence are execution-order edges, not
// an assertion that each edge represents a direct data dependency.
export function taskNeighborhood(tasks, selected, {depth = 1, direction = "both", laneOrder = true} = {}) {
    if (!Number.isInteger(depth) || depth < 1 || depth > 3 || !["both", "before", "after"].includes(direction))
        throw new Error("Invalid focus range.");
    const sorted = [...tasks].sort((a,b) => a.task-b.task);
    const byId = new Map(sorted.map(t=>[t.task,t]));
    if (byId.size !== tasks.length || !byId.has(selected)) throw new Error("Missing or duplicate focus task.");
    const lanes = new Map(), edges = [];
    for (const t of sorted) {
        const own = lanes.get(t.lane) ?? [];
        if (laneOrder && own.length) edges.push({from:own.at(-1),to:t.task,kind:"lane order"});
        for (const [lane,count] of t.waits ?? []) {
            const from = lanes.get(lane)?.[count-1];
            if (lane === t.lane || !Number.isInteger(count) || count < 1 || from === undefined)
                throw new Error(`Unresolved wait for task ${t.task}: lane ${lane} #${count}`);
            edges.push({from,to:t.task,kind:"completion wait"});
        }
        own.push(t.task); lanes.set(t.lane,own);
    }
    const predecessors = new Map(sorted.map(t=>[t.task,[]])), successors = new Map(sorted.map(t=>[t.task,[]]));
    for (const e of edges) {predecessors.get(e.to).push(e.from);successors.get(e.from).push(e.to);}
    const walk = adjacency => {
        const reached = new Set(); let frontier = [selected];
        for (let step=0; step<depth; step++) {
            const next = [];
            for (const task of frontier) for (const neighbor of adjacency.get(task))
                if (!reached.has(neighbor)) {reached.add(neighbor);next.push(neighbor);}
            frontier = next;
        }
        return reached;
    };
    const before = direction === "after" ? new Set() : walk(predecessors);
    const after = direction === "before" ? new Set() : walk(successors);
    const visible = new Set([selected,...before,...after]);
    return {nodes:sorted.filter(t=>visible.has(t.task)).map(t=>({...t,relation:t.task===selected?"selected":before.has(t.task)?"predecessor":"successor"})),
        edges:edges.filter(e=>visible.has(e.from)&&visible.has(e.to))};
}

export function focusWindow(intervals, viewportWidth, fullWidth = 1150) {
    if (!intervals.length) return {zoom:0, center:fullWidth/2};
    const lo = Math.min(...intervals.map(i=>i[0])), hi = Math.max(...intervals.map(i=>i[1]));
    const zoom = Math.min(8, Math.max(1, (viewportWidth-100)/Math.max(hi-lo,1)));
    return {zoom,center:(lo+hi)/2};
}

export function focusRoles(neighborhood, links, side) {
    const roles = {static:new Map(),runtime:new Map()};
    const other = side === "static" ? "runtime" : "static";
    for (const t of neighborhood.nodes) roles[side].set(t.task,t.relation);
    for (const link of links) {
        const from = side === "static" ? link.p : link.t;
        const to = side === "static" ? link.t : link.p;
        if (!roles[side].has(from)) continue;
        const role = roles[side].get(from)==="selected" ? "counterpart" : "related";
        if (roles[other].get(to)!=="counterpart") roles[other].set(to,role);
    }
    return roles;
}

function startViewer(data) {
	const $ = (id) => document.getElementById(id);
	const events = data.events;
    const items = events.filter(e => e.event === "item");
    const candidates = events.filter(e => e.event === "candidate");
	const select = (id, options) => {
		$(id).replaceChildren(
			...options.map(([value, label]) => {
				const o = document.createElement("option");
				o.value = value;
				o.textContent = label;
				return o;
			}),
		);
	};
	const signals = events.filter((e) => e.event === "signal" || e.event === "source_signal");
    const sourceNames = createSourceFormatter(events);
    function details(value, context = value) {
        $("details").textContent = sourceNames(value, context);
    }

	function text(tag, content, parent, className) {
		const el = document.createElement(tag);
		el.textContent = content;
		if (className) el.className = className;
		parent.append(el);
		return el;
	}
	const summary = $("summary"),
		s = data.summary;
	text(
		"p",
		`${data.backend} · ${data.threads} threads · ${data.forced ? "Forced parallel" : "Automatic selection"} · ${events.length} diagnostic records`,
		summary,
	);
	const metrics = document.createElement("div");
	metrics.className = "metrics";
	summary.append(metrics);
	for (const [label, value] of (s.mode === "testbench" ? [
        ["Testbench ticks", s.ticks], ["Captured kernel calls", events.filter(e => e.event === "kernel_call").length],
        ["Recorded tasks", data.counts.task_run ?? 0], ["Recorded waits", data.counts.task_wait ?? 0],
    ] : [
		["Speedup (untraced)", s.speedup == null ? "Not measured" : s.speedup.toFixed(2) + "×"],
		["Parallel build", s.parallel_ns_per_tick == null ? "Testbench capture" : s.parallel_ns_per_tick.toFixed(0) + " ns/tick"],
		["1-thread build", s.sequential_ns_per_tick == null ? "Not run" : s.sequential_ns_per_tick.toFixed(0) + " ns/tick"],
		["Signal comparisons", s.checks],
	])) {
		const item = document.createElement("div");
		text("strong", value, item);
		text("span", label, item);
		metrics.append(item);
	}
	text("p", s.warning, summary, "muted");
    if (data.omit_sir) text("p", "Expanded SIR omitted (--omit-sir). Source expressions, candidate decisions, memory footprints and task dependencies are retained.", summary, "muted");
    if (s.final_signals && Object.keys(s.final_signals).length) text("pre", JSON.stringify(s.final_signals, null, 2), summary);
    if (s.mode === "testbench") text("p", `${s.top}: ${s.ticks} ticks; ${s.tick_limit_reached ? "stopped at requested limit" : "testbench finished"}. Capture calls ${s.capture_after_calls + 1}–${s.capture_after_calls + s.capture_calls} / ${s.kernel_calls} selected-kernel calls.`, summary);
    for (const decision of events.filter(e => e.event === "partition_decision")) {
        text("p", `配置判断: sequential cost ${decision.sequential_cost}; best makespan ${decision.estimated_makespan} + dispatch synchronization = ${decision.parallel_cost}. Required ratio ${decision.minimum_speedup_percent}% → ${decision.accepted ? "並列プランを採用" : "逐次にフォールバック"}. コストは静的推定値です。`, summary);
    }
	if (data.debug_build)
		text(
			"p",
			"Debug build: use --release for performance conclusions.",
			summary,
			"bad",
		);
	const decisions = events.filter((e) => e.event === "selector_decision");
	for (const d of decisions) {
		const slot = events.find(
			(e) => e.event === "selector_slot" && e.selector === d.selector,
		);
		text(
			"p",
			`${slot?.slot ?? "Kernel"}: parallel median ${d.parallel_ns} ns / sequential median ${d.sequential_ns} ns → ${d.choice}`,
			summary,
		);
	}
	if (!s.checks && s.mode !== "testbench")
		text(
			"p",
			"No outputs were specified; functional equivalence was not checked. Use --observe SIGNAL.",
			summary,
			"bad",
		);
	const choices = events.filter((e) => e.event === "kernel_choice");
	text(
		"p",
		`Captured choices: ${choices.filter((c) => c.choice === "Parallel").length} parallel, ${choices.filter((c) => c.choice === "Sequential").length} sequential. Automatic selection can use a fused sequential tick path that bypasses the selector.`,
		summary,
		"muted",
	);
	$("source-text").textContent = data.source;
	for (const button of document.querySelectorAll("[data-tab]"))
		button.onclick = () => {
			for (const section of document.querySelectorAll("section"))
				section.hidden = section.id !== button.dataset.tab;
		};

	const svgNS = "http://www.w3.org/2000/svg";
	function svgElement(tag, attrs, parent) {
		const e = document.createElementNS(svgNS, tag);
		for (const [k, v] of Object.entries(attrs)) e.setAttribute(k, v);
		if (parent) parent.append(e);
		return e;
	}
	function newSvg(id, width, height) {
		const svg = svgElement("svg", { width, height, viewBox: `0 0 ${width} ${height}` });
		$(id).replaceChildren(svg);
		const defs = svgElement("defs", {}, svg);
		const marker = svgElement(
			"marker",
			{
				id: `arrow-${id}`,
				viewBox: "0 0 10 10",
				refX: 9,
				refY: 5,
				markerWidth: 6,
				markerHeight: 6,
				orient: "auto",
			},
			defs,
		);
		svgElement("path", { d: "M0 0L10 5L0 10Z", fill: "#8ba9d1" }, marker);
		return svg;
	}
	function label(svg, x, y, content) {
		const t = svgElement("text", { x, y }, svg);
		t.textContent = content;
		return t;
	}
	function edge(svg, id, x1, y1, x2, y2) {
		const path = svgElement(
			"path",
			{
				d: `M${x1} ${y1} C${(x1 + x2) / 2} ${y1},${(x1 + x2) / 2} ${y2},${x2} ${y2}`,
				fill: "none",
				stroke: "#8ba9d1",
				"stroke-opacity": 0.55,
				"marker-end": `url(#arrow-${id})`,
			},
			svg,
		);
        Object.assign(path.dataset,{x1,y1,x2,y2});
        return path;
	}
	function table(parent, columns, rows, onClick) {
		parent.replaceChildren();
		const t = document.createElement("table");
		parent.append(t);
		const h = document.createElement("tr");
		for (const c of columns) text("th", c, h);
		t.append(h);
		for (const row of rows) {
			const tr = document.createElement("tr");
			tr.dataset.row = "true";
			for (const value of row.cells)
				text(
					"td",
					typeof value === "object"
						? JSON.stringify(value)
						: String(value ?? ""),
					tr,
				);
			tr.onclick = () => onClick(row.value);
			t.append(tr);
		}
	}

	const dispatches = events.filter((e) => e.event === "dispatch");
	select(
		"dispatch",
		dispatches.map((d, i) => [
			i,
			`#${i} · ${d.schedule} · epoch ${d.epoch} · ${(d.elapsed_ns / 1000).toFixed(1)} µs`,
		]),
	);
    let provenance = {reason:"Select a dispatch.", links:[], runtime:[]};
    let focus = null;
    const focusHistory = [];
    const focusData = {static:[],runtime:[],runs:[],waits:[]};
    let activeRoles = {static:new Map(),runtime:new Map()};
    const timelineIds = {static:"planned-timeline",runtime:"timeline"};
    const zoomLevels = {static:0,runtime:0};
    const roleColors = {selected:"#ffd166",counterpart:"#ffd166",predecessor:"#78bfff",successor:"#e3a0ff",related:"#85caca"};
    const baseNumber = (el,attr) => {
        const key = `base${attr[0].toUpperCase()}${attr.slice(1)}`;
        if (el.dataset[key] === undefined) el.dataset[key]=el.getAttribute(attr);
        return Number(el.dataset[key]);
    };
    function updateMinimap(side) {
        const id=timelineIds[side], container=$(id), map=$(id+"-map"), svg=container.querySelector("svg");
        if(!svg)return;
        const height=Number(svg.getAttribute("height")), zoom=zoomLevels[side];
        let mini=map.querySelector("svg");
        if(!mini){mini=svgElement("svg",{viewBox:`0 0 1150 ${height}`,width:1150,height:72,preserveAspectRatio:"none","aria-label":"Full timeline. Click to pan."},map);}
        mini.setAttribute("viewBox", `0 0 1150 ${height}`);
        mini.replaceChildren();
        for(const rect of svg.querySelectorAll("rect[data-task],rect[data-static-task]")) {
            const marker=svgElement("rect",{x:baseNumber(rect,"x"),y:baseNumber(rect,"y"),width:Math.max(2,baseNumber(rect,"width")),height:20,
                fill:rect.getAttribute("fill"),opacity:rect.style.opacity||1},mini);
            if(rect.dataset.focusRole) {marker.setAttribute("stroke",roleColors[rect.dataset.focusRole]);marker.setAttribute("stroke-width","5");}
        }
        const unscale=x=>90+(x-90)/(zoom||1);
        const left=zoom?Math.max(0,unscale(container.scrollLeft)):0;
        const right=zoom?Math.min(1150,unscale(container.scrollLeft+container.clientWidth)):1150;
        svgElement("rect",{x:left,y:0,width:Math.max(1,right-left),height,fill:"#ffffff","fill-opacity":.04,stroke:"#fff","stroke-width":4,"data-viewport":"true"},mini);
        mini.onclick=e=>{
            if(!zoom)return;
            const bounds=mini.getBoundingClientRect(), x=(e.clientX-bounds.left)/bounds.width*1150;
            container.scrollLeft=Math.max(0,90+(x-90)*zoom-container.clientWidth/2);
        };
    }
    function zoomTimeline(side, zoom, center) {
        const id=timelineIds[side], container=$(id), svg=container.querySelector("svg");
        if(!svg)return;
        zoomLevels[side]=zoom;
        const z=zoom||1, sx=x=>x<90?x:90+(x-90)*z;
        for(const el of svg.querySelectorAll("rect,text,line")) {
            for(const attr of ["x","x1","x2"]) if(el.hasAttribute(attr))el.setAttribute(attr,sx(baseNumber(el,attr)));
            if(el.tagName==="rect") el.setAttribute("width",baseNumber(el,"width")*z);
        }
        for(const e of svg.querySelectorAll("path[data-x1]")) {
            const x1=sx(Number(e.dataset.x1)),x2=sx(Number(e.dataset.x2));
            e.setAttribute("d",`M${x1} ${e.dataset.y1} C${(x1+x2)/2} ${e.dataset.y1},${(x1+x2)/2} ${e.dataset.y2},${x2} ${e.dataset.y2}`);
        }
        const width=90+1060*z,height=Number(svg.getAttribute("height"));
        svg.setAttribute("viewBox",`0 0 ${width} ${height}`);
        svg.style.width=zoom?`${width}px`:"100%";
        svg.style.height=zoom?`${height}px`:"auto";
        if(center!==undefined)container.scrollLeft=zoom?Math.max(0,sx(center)-container.clientWidth/2):0;
        updateMinimap(side);
    }
    function paintTimelineFocus(neighborhood) {
        activeRoles=focus&&neighborhood?focusRoles(neighborhood,provenance.links,focus.side):{static:new Map(),runtime:new Map()};
        for(const side of ["static","runtime"]) {
            const id=timelineIds[side],container=$(id),svg=container.querySelector("svg"),roles=activeRoles[side];
            if(!svg)continue;
            for(const el of svg.querySelectorAll("[data-task],[data-static-task],[data-task-label],[data-wait-task]")) {
                const task=Number(el.dataset.staticTask??el.dataset.task??el.dataset.taskLabel??el.dataset.waitTask),role=roles.get(task);
                el.style.opacity=focus?(role?1:.13):1;
                if(role)el.dataset.focusRole=role;else delete el.dataset.focusRole;
                if(el.tagName==="rect" && el.dataset.waitTask===undefined){el.setAttribute("stroke",role?roleColors[role]:"none");el.setAttribute("stroke-width",role==="selected"||role==="counterpart"?4:2);}
                if(el.tagName==="text")el.style.fill=role?roleColors[role]:"";
            }
            for(const arrow of svg.querySelectorAll("path[data-from]")) {
                const relevant=roles.has(Number(arrow.dataset.from))&&roles.has(Number(arrow.dataset.to));
                arrow.style.opacity=focus?(relevant?1:.06):1;
                arrow.setAttribute("stroke",focus&&relevant?"#ffe29b":"#8ba9d1");
                arrow.setAttribute("stroke-width",focus&&relevant?2:1);
            }
            const rects=[...svg.querySelectorAll("rect[data-task],rect[data-static-task]")];
            const visible=rects.filter(r=>roles.has(Number(r.dataset.staticTask??r.dataset.task)));
            const selected=visible.filter(r=>["selected","counterpart"].includes(r.dataset.focusRole));
            const window=focusWindow(visible.map(r=>[baseNumber(r,"x"),baseNumber(r,"x")+baseNumber(r,"width")]),container.clientWidth);
            const setting=$("focus-zoom").value;
            const zoom=!focus||!visible.length||setting==="overview"?0:setting==="auto"?window.zoom:Number(setting);
            // Keep the selected task centered if its neighborhood is too wide.
            const fits=visible.length && (Math.max(...visible.map(r=>baseNumber(r,"x")+baseNumber(r,"width")))-Math.min(...visible.map(r=>baseNumber(r,"x"))))*(zoom||1)<=container.clientWidth-100;
            const center=setting==="auto"&&fits?window.center:selected.length?(Math.min(...selected.map(r=>baseNumber(r,"x")))+Math.max(...selected.map(r=>baseNumber(r,"x")+baseNumber(r,"width"))))/2:window.center;
            zoomTimeline(side,zoom,center);
            if(selected.length)container.scrollTop=Math.max(0,Number(selected[0].getAttribute("y"))-container.clientHeight/2);
            const prefix=side==="static"?"p":"t";
            $(id+"-location").textContent=!focus?"Full timeline":selected.length?`${selected.map(r=>prefix+(r.dataset.staticTask??r.dataset.task)).join(", ")} highlighted · ${zoom?zoom.toFixed(1)+"×":"overview"} · white box above marks the visible range`:
                roles.size?"Related tasks highlighted":"No corresponding task in this timeline";
        }
    }
    function locateFocus() {
        if(!focus)return;
        $(timelineIds[focus.side]).closest(".timeline-panel").scrollIntoView({block:"center"});
    }
    for(const side of ["static","runtime"]) $(timelineIds[side]).addEventListener("scroll",()=>updateMinimap(side));
    $("focus-locate").onclick=()=>{renderFocus();locateFocus();};
    function renderFocus() {
        $("focus-back").disabled = !focusHistory.length;
        $("focus-clear").disabled = !focus;
        $("focus-locate").disabled = !focus;
        $("focus-counterparts").replaceChildren();
        if (!focus) {
            $("focus-status").textContent = "Click a timeline task or table row to focus its predecessors and successors.";
            $("task-focus-graph").replaceChildren();
            $("task-focus-edges").replaceChildren();
            $("focus-neighbors").replaceChildren();
            paintTimelineFocus(null);
            return;
        }
        const prefix = focus.side === "static" ? "p" : "t";
        try {
            const neighborhood = taskNeighborhood(focusData[focus.side], focus.task, {
                depth:Number($("focus-depth").value), direction:$("focus-direction").value, laneOrder:$("focus-lane-order").checked,
            });
            $("focus-status").textContent = `${prefix}${focus.task} · ${neighborhood.nodes.length} tasks in focus. Highlighted in the original timelines. Click a neighbor to follow it. The optional dependency diagram shows ordering (solid: completion wait; dashed: lane order), not time.`;
            paintTimelineFocus(neighborhood);
            $("focus-neighbors").replaceChildren();
            for(const t of neighborhood.nodes.filter(t=>t.task!==focus.task).slice(0,30)) {
                const button=text("button",`${t.relation==="predecessor"?"←":"→"} ${prefix}${t.task}`, $("focus-neighbors"));
                button.style.borderColor=roleColors[t.relation];
                button.onclick=()=>focusTask(focus.side,t.task);
            }
            graph("task-focus-graph", neighborhood.nodes.map(t=>{
                const r = focusData.runs.find(r=>r.task===t.task);
                const duration = focus.side === "static" ? `${t.end-t.start} cost` : !r ? "Missing execution" : r.skipped ? "Skipped" : `${r.end_ns-r.start_ns} ns${r.code ? " · failed" : ""}`;
                return {id:t.task,lane:t.lane,focus:t.task===focus.task,label:`${prefix}${t.task} · ${t.relation}`,
                    subtitle:`Lane ${t.lane} · ${duration}`,predecessors:neighborhood.edges.filter(e=>e.to===t.task).map(e=>e.from),
                    edgeKinds:Object.fromEntries(neighborhood.edges.filter(e=>e.to===t.task).map(e=>[e.from,e.kind]))};
            }), n=>focusTask(focus.side,n.id));
            table($("task-focus-edges"), ["From", "To", "Ordering constraint"], neighborhood.edges.map(e=>({cells:[`${prefix}${e.from}`,`${prefix}${e.to}`,e.kind],value:e})), e=>details(e));
            const ids = new Set(provenance.links.filter(l=>(focus.side==="static"?l.p:l.t)===focus.task).map(l=>focus.side==="static"?l.t:l.p));
            if (!ids.size) text("span", provenance.reason ?? "No counterpart in this phase (for example, FF-only work).", $("focus-counterparts"), "muted");
            for (const id of ids) {
                const side = focus.side === "static" ? "runtime" : "static";
                const button = text("button", `Open counterpart ${side==="static"?"p":"t"}${id}`, $("focus-counterparts"));
                button.onclick = ()=>focusTask(side,id);
            }
        } catch (error) {
            $("focus-status").textContent = error.message;
            $("task-focus-graph").replaceChildren();
            $("task-focus-edges").replaceChildren();
        }
        highlightTask(null);
    }
    function focusTask(side, task, remember = true) {
        if (remember && focus && (focus.side!==side || focus.task!==task)) focusHistory.push({...focus});
        focus = {side,task};
        const spec = focusData[side].find(t=>t.task===task);
        details({selected:`${side==="static"?"p":"t"}${task}`,task:spec,
            ...(side === "runtime" ? {execution:focusData.runs.find(r=>r.task===task), waits:focusData.waits.filter(w=>w.task===task),
                final_native_sir:nativeSirForTask(events, spec?.schedule, task)} : {static_task:spec}),
            provenance:provenanceDetails(task,side)});
        renderFocus();
        locateFocus();
    }
    function clearFocus() {focus=null;focusHistory.length=0;renderFocus();highlightTask(null);}
    $("focus-clear").onclick = clearFocus;
    $("focus-back").onclick = ()=>{const previous=focusHistory.pop();if(previous)focusTask(previous.side,previous.task,false);};
    for (const id of ["focus-depth","focus-direction","focus-lane-order","focus-zoom"]) $(id).onchange = renderFocus;
    renderFocus();
    function highlightTask(task, side = "runtime") {
        if(focus)return;
        const ps = new Set(), ts = new Set();
        if (task === null && focus) { task=focus.task;side=focus.side; }
        if (task !== null) {
            (side === "static" ? ps : ts).add(task);
            for (const link of provenance.links) {
                if ((side === "static" ? link.p : link.t) === task) { ps.add(link.p); ts.add(link.t); }
            }
        }
        for (const rect of document.querySelectorAll("#runtime rect[data-task], #runtime rect[data-static-task]")) {
            const selected = rect.dataset.staticTask !== undefined ? ps.has(Number(rect.dataset.staticTask)) : ts.has(Number(rect.dataset.task));
            rect.setAttribute("stroke", selected ? "#fff" : "none");
            rect.setAttribute("stroke-width", "3");
        }
    }
    function provenanceDetails(task, side) {
        return {status:provenance.reason ?? "Recorded lowering unit lineage", links:provenance.links.filter(l => (side === "static" ? l.p : l.t) === task),
            runtime:provenance.runtime.filter(t => side === "static" ? t.planned.includes(task) : t.task === task)};
    }
    function renderProvenance(compiled) {
        const c = candidates[Number($("static-candidate").value)];
        provenance = taskProvenance(c, events, compiled);
        $("provenance-note").textContent = provenance.reason ?? "Recorded p → optimized comb unit → t lineage. Hover either panel to highlight all counterparts; click for unit-level links. FF-only tasks have no p. A link records origin, not how much work survived optimization.";
        table($("provenance-table"), ["Source task", "Lowered / optimized comb units", "Runtime tasks"], c && !provenance.reason ? candidateTasks(c).map(p => {
            const links = provenance.links.filter(l => l.p === p.task);
            return {cells:[`p${p.task}`, [...new Set(links.map(l=>l.unit))].join(", ") || "—", [...new Set(links.map(l=>`t${l.t}`))].join(", ") || "No task in this phase"], value:{...provenanceDetails(p.task,"static"),source_task:p.task}};
        }) : [], value=>focusTask("static",value.source_task));
        highlightTask(null);
    }
    function drawPlan(id, plan, lanes, unit, onSelect, extent = plan.makespan) {
        const svg = newSvg(id, 1150, 55 + lanes * 66), scale = 1000 / Math.max(extent, 1);
        for (let i = 0; i <= 5; i++) {
            const tick = label(svg, 90 + i * 200, 18, `${(extent * i / 5).toFixed(1)} ${unit}`);
            if (i === 5) tick.setAttribute("text-anchor", "end");
        }
        for (let lane = 0; lane < lanes; lane++) label(svg, 5, 55 + lane * 66, `Lane ${lane}`);
        const byId = new Map(plan.tasks.map(t => [t.task, t]));
        for (const t of plan.tasks) for (const p of t.predecessors) {
            const producer = byId.get(p);
            const arrow=edge(svg, id, 90 + producer.end * scale, 51 + producer.lane * 66, 90 + t.start * scale, 51 + t.lane * 66);
            Object.assign(arrow.dataset,{from:p,to:t.task});
        }
        for (const t of plan.tasks) {
            const rect = svgElement("rect", {x:90 + t.start * scale, y:41 + t.lane * 66,
                width:Math.max(2, (t.end - t.start) * scale), height:20, rx:3, fill:"#739feb"}, svg);
            rect.dataset.staticTask = t.task;
            if (id === "planned-timeline") {
                rect.onmouseenter = () => highlightTask(t.task, "static");
                rect.onmouseleave = () => highlightTask(null);
            }
            rect.onclick = () => onSelect(t);
            svgElement("title", {}, rect).textContent = `p${t.task}: ${t.start}–${t.end} ${unit}`;
            label(svg, 90 + t.start * scale, 77 + t.lane * 66, `p${t.task}`).dataset.taskLabel=t.task;
        }
    }
    const selectedIndex = selectedCandidateIndex(candidates, events.filter(e => e.event === "partition_decision"));
    select("static-candidate", candidates.map((c, i) => [i, `${c.candidate} · scope ${scopeKey(c)} · ${c.makespan} cost${i === selectedIndex ? " · adopted" : ""}`]));
    if (selectedIndex >= 0) $("static-candidate").value = selectedIndex;
    function renderStaticComparison() {
        const c = candidates[Number($("static-candidate").value)];
        try {
            if (!c) throw new Error("No recorded static candidate. A time prediction cannot be derived from this capture.");
            const plan = candidateTimeline(c, items.filter(i => sourcePlanKey(i) === sourcePlanKey(c)));
            focusData.static = plan.tasks;
            drawPlan("planned-timeline", plan, data.threads, "cost", t => focusTask("static",t.task));
            $("plan-note").textContent = `Recorded combinational partition: ${c.candidate}. Static makespan ${plan.makespan} cost; synchronization ${c.synchronization_cost}; task overhead ${c.task_overhead}. No measured durations or conversion to ns. Source tasks pN and compiled runtime tasks tN are different stages; matching numbers do not identify the same task. The selected dispatch may also include FF work.`;
        } catch (error) {
            focusData.static = [];
            $("planned-timeline").replaceChildren();
            $("plan-note").textContent = error.message;
        }
    }
    $("static-candidate").onchange = () => { clearFocus();renderStaticComparison();runtime(); };
    renderStaticComparison();
    function renderComparison(d, runs, compiled) {
        focusData.runtime=compiled;focusData.runs=runs;
        renderProvenance(compiled);
        table($("task-comparison"), ["Runtime task", "Source lineage", "Lane", "Actual start (ns)", "Duration (ns)", "State"], compiled.map(t => {
            const r = runs.find(r => r.task === t.task);
            const origin = provenance.runtime.find(r => r.task === t.task);
            const lineage = !origin ? "Unknown" : [...origin.planned.map(p => `p${p}`), ...(origin.ff_units.length ? ["FF"] : [])].join(", ") || "No source units";
            return {cells:[`t${t.task}`, lineage, t.lane, r && !r.skipped ? r.start_ns : "—", r && !r.skipped ? r.end_ns-r.start_ns : "—", !r ? "Missing" : r.skipped ? "Skipped" : r.code ? `Failed (${r.code})` : "Completed"], value:{compiled:t, execution:r, provenance:provenanceDetails(t.task,"runtime")}};
        }), value=>focusTask("runtime",value.compiled.task));
        const kind = compiled[0]?.kind ?? "";
        const phase = kind.startsWith("Fused") ? "Fused (combinational + FF)" : kind.startsWith("Event") ? "FF event" : kind === "Comb" ? "Combinational" : "Unknown phase";
        $("runtime-phase").textContent = `Measured phase: ${phase} · after optimization`;
    }

	function runtime() {
		const d = dispatches[Number($("dispatch").value)];
		if (!d) {
			$("runtime-stats").textContent =
				"No parallel dispatch captured. Inspect partition decisions and automatic fallback, or run --always to inspect available parallel kernels.";
			return;
		}
		const runs = events.filter(
			(e) => e.event === "task_run" && e.pool === d.pool && e.epoch === d.epoch,
		);
		const waits = events.filter(
			(e) =>
				e.event === "task_wait" && e.pool === d.pool && e.epoch === d.epoch,
		);
		const compiled = [
			...new Map(
				events
					.filter(
						(e) => e.event === "compiled_task" && e.schedule === d.schedule,
					)
					.map((e) => [e.task, e]),
			).values(),
		];
		focusData.waits=waits;
		const a = analyzeDispatch(d, runs, waits, compiled);
		$("runtime-stats").textContent =
			`Peak overlap ${a.peak} · mean ${a.average.toFixed(2)} lanes · utilization ${(a.utilization * 100).toFixed(1)}% · ${a.issues.length ? a.issues.join("; ") : "recorded ordering checks passed"}`;
		renderComparison(d, runs, compiled);
		const width = 1150,
			row = 66,
			left = 90,
			scale = 1000 / Math.max(d.elapsed_ns, 1);
		const svg = newSvg("timeline", width, 55 + d.lanes * row);
		for (let lane = 0; lane < d.lanes; lane++) {
			label(svg, 5, 55 + lane * row, `Lane ${lane}`);
			svgElement(
				"line",
				{
					x1: left,
					y1: 60 + lane * row,
					x2: 1100,
					y2: 60 + lane * row,
					stroke: "#314663",
				},
				svg,
			);
		}
		for (let i = 0; i <= 5; i++)
			label(
				svg,
				left + i * 200,
				18,
				`${((d.elapsed_ns * i) / 5).toFixed(1)} ns`,
			);
		const byLane = new Map();
		for (const t of [...compiled].sort((a, b) => a.task - b.task)) {
			if (!byLane.has(t.lane)) byLane.set(t.lane, []);
			byLane.get(t.lane).push(t);
		}
		for (const t of compiled) {
			const consumer = runs.find((r) => r.task === t.task && !r.skipped);
			if (!consumer) continue;
			for (const [lane, count] of t.waits) {
				const producer = runs.find(
					(r) => r.task === byLane.get(lane)?.[count - 1]?.task && !r.skipped,
				);
				if (producer) {
					const arrow=edge(
						svg,
						"timeline",
						left + producer.end_ns * scale,
						44 + lane * row,
						left + consumer.start_ns * scale,
						44 + t.lane * row,
					);
                    Object.assign(arrow.dataset,{from:producer.task,to:t.task});
                }
			}
		}
		for (const w of waits) {
			const rect = svgElement(
				"rect",
				{
					x: left + w.start_ns * scale,
					y: 30 + w.lane * row,
					width: Math.max(1, (w.end_ns - w.start_ns) * scale),
					height: 9,
					fill: "#e9b95f",
				},
				svg,
			);
			rect.dataset.waitTask=w.task;
            rect.onclick = () => focusTask("runtime",w.task);
			const title = svgElement("title", {}, rect);
			title.textContent = `task ${w.task}: wait for lane ${w.other} completion ${w.completed}, ${w.end_ns - w.start_ns} ns`;
		}
		for (const r of runs) {
			if (r.skipped) continue;
			const rect = svgElement(
				"rect",
				{
					x: left + r.start_ns * scale,
					y: 41 + r.lane * row,
					width: Math.max(2, (r.end_ns - r.start_ns) * scale),
					height: 20,
					fill: "#45ba94",
					rx: 3,
				},
				svg,
			);
			rect.dataset.task = r.task;
            rect.onmouseenter = () => highlightTask(r.task);
            rect.onmouseleave = () => highlightTask(null);
			const title = svgElement("title", {}, rect);
			title.textContent = `task ${r.task}, ${r.end_ns - r.start_ns} ns`;
			rect.onclick = () => focusTask("runtime",r.task);
			label(svg, left + r.start_ns * scale, 77 + r.lane * row, `t${r.task}`).dataset.taskLabel=r.task;
		}
        updateMinimap("runtime");updateMinimap("static");
	}
	$("dispatch").onchange = () => {
        clearFocus();
        $("details").textContent = "Select a task in the current dispatch.";
        runtime();
    };
	runtime();

	function graph(id, nodes, onClick) {
		// Topological levels make all dependency arrows run left to right. Rows
		// are independent nodes. Bound rendered DOM, not retained evidence.
        const total = nodes.length;
        nodes = nodes.slice(0, 300);
		const levels = new Map(),
			byId = new Map(nodes.map((n) => [n.id, n]));
		function level(node, visiting = new Set()) {
			if (levels.has(node.id)) return levels.get(node.id);
			if (visiting.has(node.id)) return 0;
			visiting.add(node.id);
			let depth = 0;
			for (const p of node.predecessors ?? []) {
				const prev = byId.get(p);
				if (prev) depth = Math.max(depth, level(prev, visiting) + 1);
			}
			visiting.delete(node.id);
			levels.set(node.id, depth);
			return depth;
		}
		for (const n of nodes) level(n);
		const rows = new Map(),
			positions = new Map();
		let width = 800,
			height = 150;
		for (const n of nodes) {
			const depth = levels.get(n.id),
				r = rows.get(depth) ?? 0;
			rows.set(depth, r + 1);
			const x = 30 + depth * 230,
				y = 25 + r * 67;
			positions.set(n.id, [x, y]);
			width = Math.max(width, x + 225);
			height = Math.max(height, y + 75);
		}
		const svg = newSvg(id, width, height);
        if (total > nodes.length) {
            const note = document.createElement("p");
            note.className = "muted";
            note.textContent = `Showing ${nodes.length} / ${total} nodes. Refine the source filter to inspect remaining nodes; full records are retained in Event inspector and JSON.`;
            $(id).prepend(note);
        }
		for (const n of nodes) {
			const [x, y] = positions.get(n.id);
			for (const p of n.predecessors ?? []) {
				const pos = positions.get(p);
				if (pos) {
                    const arrow = edge(svg, id, pos[0] + 205, pos[1] + 20, x, y + 20);
                    if (n.edgeKinds?.[p] === "lane order") arrow.setAttribute("stroke-dasharray", "5 4");
                    if (n.edgeKinds?.[p]) svgElement("title", {}, arrow).textContent = n.edgeKinds[p];
                }
			}
		}
		for (const n of nodes) {
			const [x, y] = positions.get(n.id);
			const group = svgElement("g", {}, svg);
			const rect = svgElement(
				"rect",
				{
					x,
					y,
					width: 205,
					height: 43,
					rx: 6,
					fill: n.focus ? "#76551e" : `hsl(${((n.lane ?? 0) * 71 + 190) % 360} 35% 28%)`,
					stroke: n.focus ? "#ffe29b" : "#658bad",
				},
				group,
			);
			label(group, x + 8, y + 18, n.label.slice(0, 27));
			label(group, x + 8, y + 34, n.subtitle ?? "");
			group.style.cursor = "pointer";
			group.onclick = () => onClick(n);
            group.setAttribute("role", "button");
            group.setAttribute("tabindex", "0");
            group.setAttribute("aria-label", n.label);
            group.dataset.node = n.id;
            group.onkeydown = e=>{if(e.key==="Enter" || e.key===" "){e.preventDefault();onClick(n);}};
			const title = svgElement("title", {}, rect);
			title.textContent = n.label;
		}
	}
	const paths = events.filter((e) => e.event === "path");
	table(
		$("candidates"),
		["Candidate", "Task budget", "Makespan", "Sequential cost", "Sync cost", "Task overhead"],
		candidates.map((c) => ({
			cells: [
				c.candidate,
                c.task_budget || "Unbounded",
				c.makespan,
				c.sequential_cost,
				c.synchronization_cost,
				c.task_overhead,
			],
			value: c,
		})),
		(c) => {
			details(c);
			showItems(c);
		},
	);
	const initialCandidate = candidates[selectedIndex] ?? candidates[0];
    let currentCandidate = initialCandidate;
    const allocation = document.createElement("div");
    $("candidates").after(allocation);
	function showItems(candidate) {
		currentCandidate = candidate;
		const scopedItems = candidate
			? items.filter((e) => sourcePlanKey(e) === sourcePlanKey(candidate))
			: items;
        $("candidate-timeline").replaceChildren();
        if (candidate) {
            try {
                const plan = candidateTimeline(candidate, scopedItems);
                drawPlan("candidate-timeline", plan, data.threads, "cost", details);
            } catch (error) { text("p", error.message, $("candidate-timeline"), "muted"); }
        }

		const scopedPaths = candidate
			? paths.filter((e) => sourcePlanKey(e) === sourcePlanKey(candidate))
			: paths;
		const pathById = new Map(scopedPaths.map(p => [p.path, p]));
        allocation.replaceChildren();
        if (candidate) {
            text("h2", `候補 ${candidate.candidate} / task budget ${candidate.task_budget || "unbounded"}: レーン別の仕事と依存`, allocation);
            const lanes = partitionLanes(scopedItems, candidate.lanes);
            const laneTable = text("div", "", allocation);
            table(laneTable, ["Lane", "Items", "Estimated work", "Pinned items", "Cross-lane edges"], lanes.map(l => ({cells:[l.lane, l.items, l.cost, l.pinned, l.crossLaneEdges], value:l})), details);
            const groups = new Map(), blocks = new Map();
            for (const item of scopedItems) {
                for (const id of item.paths) {
                    const name = sourceNames(pathById.get(id)?.target ?? "unknown", pathById.get(id)).replaceAll('"', "");
                    const core = name.match(/.*?u_core\[\d+\]/);
                    const group = core?.[0] ?? name.split(".").slice(0, -1).join(".");
                    if (!groups.has(group)) groups.set(group, Array(data.threads).fill(0));
                    groups.get(group)[candidate.lanes[item.item]]++;
                    const block = name.split(".").slice(0, -1).join(".");
                    if (!blocks.has(block)) blocks.set(block, Array(data.threads).fill(0));
                    blocks.get(block)[candidate.lanes[item.item]]++;
                }
            }
            text("h2", "RTL インスタンス → レーン（出力パス数）", allocation);
            text("p", "CPU の hart とシミュレーターレーンは別物です。数字はソースの書き込みパス数で、実行時間ではありません。行をクリックすると該当する回路に絞り込めます。", allocation, "muted");
            const scopeTable = text("div", "", allocation);
            table(scopeTable, ["RTL scope", ...Array.from({length:data.threads}, (_, i) => `Lane ${i}`)], [...groups].sort((a,b) => b[1].reduce((s,n)=>s+n,0)-a[1].reduce((s,n)=>s+n,0)).map(([name, counts]) => ({cells:[name, ...counts], value:name})), name => {$("item-search").value = name; showItems(candidate);});
            if ([...blocks.keys()].some(b => !groups.has(b))) {
                const expanded = document.createElement("details");
                text("summary", "回路ブロックの内訳を表示", expanded);
                const blockTable = text("div", "", expanded);
                allocation.append(expanded);
                const focus = $("item-search").value.toLowerCase();
                table(blockTable, ["Block", ...Array.from({length:data.threads}, (_, i) => `Lane ${i}`)], [...blocks].filter(([name]) => !focus || name.toLowerCase().includes(focus)).map(([name, counts]) => ({cells:[name, ...counts], value:name})), name => {$("item-search").value = name; showItems(candidate);});
            }
        }
		const query = $("item-search").value.toLowerCase();
		const visible = scopedItems.filter(
			(i) =>
				!query ||
				String(i.item) === query ||
				i.paths.some(path => sourceNames(pathById.get(path) ?? {}).toLowerCase().includes(query)),
		);
		graph(
			"item-graph",
			visible.map((i) => ({
				id: i.item,
				lane: candidate?.lanes[i.item],
				label: `item ${i.item}: ${i.paths.length} paths`,
				subtitle: `cost ${i.cost} · lane ${candidate?.lanes[i.item] ?? "?"}${i.pinned_lane0 ? " · pinned" : ""}`,
				predecessors: i.predecessors,
				record: i,
			})),
			(n) => {
				const selected = scopedPaths.filter((p) =>
					n.record.paths.includes(p.path),
				);
				details({
					item: n.record,
					paths: selected,
					merges: events.filter(
						(e) =>
							e.event === "merge" &&
							sourcePlanKey(e) === sourcePlanKey(n.record) &&
							(n.record.work.includes(e.first) ||
								n.record.work.includes(e.second)),
					),
					lowering: events.filter(
						(e) =>
							e.event === "lowered_task" &&
							sourcePlanKey(e) === sourcePlanKey(n.record) &&
							e.items.includes(n.id),
					),
				});
				// Also expose the original path graph rather than only the atomic item.
				const container = $("item-graph");
				let sub = document.getElementById("path-graph");
				if (!sub) {
					sub = document.createElement("div");
					sub.id = "path-graph";
					container.after(sub);
				}
				const deps = dependenciesForPaths(scopedPaths);
				const neighborhood = new Set(
					selected.flatMap((p) => [
						p.path,
						...(deps.get(p.path) ?? []),
						...(p.users ?? []),
					]),
				);
				graph(
					"path-graph",
					scopedPaths
						.filter((p) => neighborhood.has(p.path))
						.map((p) => ({
							id: p.path,
							label: `p${p.path} ${sourceNames(p.target, p).replaceAll('"', "")}`,
							subtitle: `expression ${p.expression}`,
							predecessors: deps.get(p.path),
							record: p,
						})),
					(p) =>
						details({
							path: p.record,
							expression: events.find(
								(e) =>
									e.event === "slt_node" &&
									sourcePlanKey(e) === sourcePlanKey(p.record) &&
									e.node === p.record.expression,
							),
						}),
				);
			},
		);
	}
	$("item-search").onchange = () => showItems(currentCandidate);
	showItems(initialCandidate);
	const plans = new Map();
	for (const e of events.filter((e) => e.event === "physical_dependencies")) {
		const key = physicalPlanKey(e);
		if (!plans.has(key)) plans.set(key, []);
		plans.get(key).push(e);
	}
	select(
		"plan",
		[...plans].map(([key, es]) => [
			key,
			(es[0].scopes ?? [])
				.map((s) => `${s.span} ${s.kind ?? ""} ${s.codegen ?? ""}`)
				.join(" / "),
		]),
	);
	function physical() {
		const key = $("plan").value,
			rows = plans.get(key) ?? [];
		const scoped = events.filter((e) => physicalPlanKey(e) === key);
		graph(
			"physical-graph",
			rows.map((e) => ({
				id: e.unit,
				label: `unit/task ${e.unit}`,
				subtitle: `${e.predecessors.length} predecessors`,
				predecessors: e.predecessors,
			})),
			(n) => {
				const footprints = scoped.filter(
						(e) => e.event === "footprint" && e.unit === n.id,
					),
					hazards = scoped.filter(
						(e) => e.event === "hazard" && e.unit === n.id,
					);
                const program = (footprints[0]?.scopes ?? []).find(s => s.program)?.program;

				details({
					unit: n.id,
					dependencies: rows.find((e) => e.unit === n.id),
					footprints,
					hazards,
                    nearby_signals: signalsForFootprints(footprints, signals, program),
                    note: "Signal matches include stable, working and sparse storage candidates with their physical object and range. Aliases and strided arrays can make names approximate. Some edges summarize transitive ordering.",
				});
			},
		);
	}
	$("plan").onchange = physical;
	physical();
	select("event-type", [
		["", "All events"],
		...[...new Set(events.map((e) => e.event))].sort().map((e) => [e, e]),
	]);
	function inspect() {
		const query = $("search").value.toLowerCase(),
			type = $("event-type").value;
		const rows = events
			.map((e, i) => ({ e, i }))
			.filter(
				({ e }) =>
					(!type || e.event === type) &&
					(!query || sourceNames(e).toLowerCase().includes(query)),
			);
		table(
			$("event-table"),
			["#", "Event", "Details"],
			rows
				.slice(0, 500)
				.map(({ e, i }) => ({
					cells: [i, e.event, sourceNames(e).slice(0, 200)],
					value: e,
				})),
			details,
		);
		if (rows.length > 500)
			text(
				"p",
				`${rows.length} matches; showing first 500. Narrow the search.`,
				$("event-table"),
			);
	}
	$("search").oninput = inspect;
	$("event-type").onchange = inspect;
	inspect();
}
if (typeof document !== "undefined")
	startViewer(JSON.parse(document.getElementById("report-data").textContent));
