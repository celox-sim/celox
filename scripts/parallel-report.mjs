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

function startViewer(data) {
	const $ = (id) => document.getElementById(id);
	const events = data.events;
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
	const symbolNames = new Map(
		signals.flatMap((s) => [
			[s.address, s.name],
			[s.source_address, s.name],
		]),
	);
	const objectNames = new Map(
		signals.map((s) => [`${s.instance}/${s.object}`, s.name]),
	);
	const sourceObjectNames = new Map(
		signals
			.filter((s) => s.source_object !== undefined)
			.map((s) => [`${s.instance}/${s.source_object}`, s.name]),
	);
	const sourceNames = (value) =>
		JSON.stringify(value, null, 2)
			.replace(
				/AbsoluteAddr\(inst\d+, (?:state|var)\d+\)/g,
				(address) => `${symbolNames.get(address) ?? address}`,
			)
			.replace(
				/instance_id: InstanceId\((\d+)\), var_id: StateObjectId\((\d+)\)/g,
				(full, instance, object) =>
					`${objectNames.get(`${instance}/${object}`) ?? full}`,
			)
			.replace(
				/instance_id: InstanceId\((\d+)\), var_id: SourceVarId\((\d+)\)/g,
				(full, instance, object) =>
					`${sourceObjectNames.get(`${instance}/${object}`) ?? full}`,
			);

	function details(value) {
		$("details").textContent = sourceNames(value);
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
		const svg = svgElement("svg", { width, height });
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
		return svgElement(
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
		const a = analyzeDispatch(d, runs, waits, compiled);
		$("runtime-stats").textContent =
			`Peak overlap ${a.peak} · mean ${a.average.toFixed(2)} lanes · utilization ${(a.utilization * 100).toFixed(1)}% · ${a.issues.length ? a.issues.join("; ") : "recorded ordering checks passed"}`;
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
				`${((d.elapsed_ns * i) / 5000).toFixed(1)} µs`,
			);
		const byLane = new Map();
		for (const t of compiled) {
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
				if (producer)
					edge(
						svg,
						"timeline",
						left + producer.end_ns * scale,
						44 + lane * row,
						left + consumer.start_ns * scale,
						44 + t.lane * row,
					);
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
			rect.onclick = () => details(w);
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
			const title = svgElement("title", {}, rect);
			title.textContent = `task ${r.task}, ${r.end_ns - r.start_ns} ns`;
			rect.onclick = () =>
				details({
					execution: r,
					waits: waits.filter((w) => w.task === r.task),
					compiled: compiled.find((t) => t.task === r.task),
					final_native_sir: events.find(
						(e) =>
							e.event === "native_task_sir" &&
							e.task === r.task &&
							e.kind === compiled.find((t) => t.task === r.task)?.kind,
					),
				});
			label(svg, left + r.start_ns * scale, 77 + r.lane * row, `t${r.task}`);
		}
	}
	$("dispatch").onchange = runtime;
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
				if (pos) edge(svg, id, pos[0] + 205, pos[1] + 20, x, y + 20);
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
					fill: `hsl(${((n.lane ?? 0) * 71 + 190) % 360} 35% 28%)`,
					stroke: "#658bad",
				},
				group,
			);
			label(group, x + 8, y + 18, n.label.slice(0, 27));
			label(group, x + 8, y + 34, n.subtitle ?? "");
			group.style.cursor = "pointer";
			group.onclick = () => onClick(n);
			const title = svgElement("title", {}, rect);
			title.textContent = n.label;
		}
	}
	const items = events.filter((e) => e.event === "item"),
		paths = events.filter((e) => e.event === "path");
	const candidates = events.filter((e) => e.event === "candidate");
	table(
		$("candidates"),
		["Candidate", "Task budget", "Makespan", "Sequential cost", "Sync cost", "Task overhead"],
		candidates.map((c) => ({
			cells: [
				c.candidate,
                c.task_budget || "unbounded",
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
	const initialCandidate = candidates.find(c => events.some(e =>
        e.event === "partition_decision" && scopeKey(e) === scopeKey(c) && e.estimated_makespan === c.makespan)) ?? candidates[0];
    let currentCandidate = initialCandidate;
    const allocation = document.createElement("div");
    $("candidates").after(allocation);
	function showItems(candidate) {
		currentCandidate = candidate;
		const scopedItems = candidate
			? items.filter((e) => scopeKey(e) === scopeKey(candidate))
			: items;
		const scopedPaths = candidate
			? paths.filter((e) => scopeKey(e) === scopeKey(candidate))
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
                    const name = sourceNames(pathById.get(id)?.target ?? "unknown").replaceAll('"', "");
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
							scopeKey(e) === scopeKey(n.record) &&
							(n.record.work.includes(e.first) ||
								n.record.work.includes(e.second)),
					),
					lowering: events.filter(
						(e) =>
							e.event === "lowered_task" &&
							scopeKey(e) === scopeKey(n.record) &&
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
							label: `p${p.path} ${sourceNames(p.target).replaceAll('"', "")}`,
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
									scopeKey(e) === scopeKey(p.record) &&
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
		const key = scopeKey(e);
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
		const scoped = events.filter((e) => scopeKey(e) === key);
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
				const ranges = [
					...JSON.stringify(footprints).matchAll(
						/offset: (\d+), byte_len: (\d+)/g,
					),
				].map((m) => [Number(m[1]), Number(m[2])]);
				details({
					unit: n.id,
					dependencies: rows.find((e) => e.unit === n.id),
					footprints,
					hazards,
					nearby_signals: signals.filter((s) =>
						ranges.some(
							([offset, len]) =>
								offset < s.offset + Math.ceil(s.width / 8) &&
								s.offset < offset + len,
						),
					),
					note: "Signal matches show stable-state storage; working/sparse offsets are included in the signal records. Aliases can share storage. Some tracker edges summarize earlier ordering rather than a direct range conflict.",
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
