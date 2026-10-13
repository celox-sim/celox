import test from "node:test";
import assert from "node:assert/strict";
import {
	analyzeDispatch,
    partitionLanes,
	dependenciesForPaths,
	scopeKey,
} from "./parallel-report.mjs";

test("measures concurrency and checks cross-lane producer completion", () => {
	const d = { elapsed_ns: 100, lanes: 2 };
	const runs = [
		{ task: 0, lane: 0, start_ns: 0, end_ns: 50 },
		{ task: 1, lane: 1, start_ns: 10, end_ns: 40 },
		{ task: 2, lane: 1, start_ns: 60, end_ns: 90 },
	];
	const tasks = [
		{ task: 0, lane: 0, waits: [] },
		{ task: 1, lane: 1, waits: [] },
		{ task: 2, lane: 1, waits: [[0, 1]] },
	];
	const a = analyzeDispatch(d, runs, [{ start_ns: 40, end_ns: 60 }], tasks);
	assert.equal(a.peak, 2);
	assert.equal(a.average, 1.1);
	assert.equal(a.utilization, 0.55);
	assert.equal(a.wait_ns, 20);
	assert.deepEqual(a.issues, []);
	runs[2].start_ns = 45;
	assert.match(
		analyzeDispatch(d, runs, [], tasks).issues[0],
		/Unsatisfied dependency/,
	);
});
test("failure/skipped predecessors cannot satisfy a running dependent", () => {
	const tasks = [
		{ task: 0, lane: 0, waits: [] },
		{ task: 1, lane: 1, waits: [[0, 1]] },
	];
	const runs = [
		{ task: 0, lane: 0, skipped: true, start_ns: 0, end_ns: 0 },
		{ task: 1, lane: 1, start_ns: 5, end_ns: 10 },
	];
	assert.match(
		analyzeDispatch({ elapsed_ns: 10, lanes: 2 }, runs, [], tasks).issues[0],
		/Unsatisfied/,
	);
});
test("back-to-back tasks do not falsely overlap; invalid same-lane order is flagged", () => {
	const tasks = [
		{ task: 0, lane: 0, waits: [] },
		{ task: 1, lane: 0, waits: [] },
	];
	const runs = [
		{ task: 0, lane: 0, start_ns: 0, end_ns: 5 },
		{ task: 1, lane: 0, start_ns: 5, end_ns: 10 },
	];
	assert.equal(
		analyzeDispatch({ elapsed_ns: 10, lanes: 1 }, runs, [], tasks).peak,
		1,
	);
	runs[1].start_ns = 4;
	assert.match(
		analyzeDispatch({ elapsed_ns: 10, lanes: 1 }, runs, [], tasks).issues[0],
		/out of order/,
	);
});
test("source path edges and scope IDs preserve graph identity", () => {
	assert.deepEqual(
		[
			...dependenciesForPaths([
				{ path: 0, users: [1] },
				{ path: 1, users: [] },
			]),
		],
		[
			[0, []],
			[1, [0]],
		],
	);
	assert.notEqual(
		scopeKey({ scopes: [{ id: "1" }] }),
		scopeKey({ scopes: [{ id: "2" }] }),
	);
});

test("incomplete task metadata cannot report successful ordering verification", () => {
	const run = { task: 0, lane: 0, start_ns: 1, end_ns: 2, code: 0 };
	assert.match(analyzeDispatch({ elapsed_ns: 3, lanes: 1 }, [run], [], []).issues[0], /Missing compiled metadata/);
	assert.match(analyzeDispatch({ elapsed_ns: 3, lanes: 1 }, [], [], [{ task: 0, lane: 0, waits: [] }]).issues[0], /Missing execution record/);
});

test("HTML export preserves source literals and cannot terminate its data script", async () => {
	const { renderReport } = await import("./render-parallel-report.mjs");
	const report = { schema: 1, events: [], source: '</script><script>alert(1)</script> __REPORT_JS__ $& $`' };
	const html = renderReport(report);
	const payload = html.split('<script type="application/json" id="report-data">')[1].split('</script>')[0];
	assert.deepEqual(JSON.parse(payload), report);
	assert.ok(!html.includes('<script>alert(1)</script>'));
	assert.throws(() => renderReport({ schema: 2, events: [] }), /schema 1/);
});

test("lane costs retain mixed-source items and count incoming cross-lane edges", () => {
    assert.deepEqual(partitionLanes([
        {item:0, cost:7, paths:[0,1], predecessors:[], pinned_lane0:true},
        {item:1, cost:5, paths:[2], predecessors:[0]},
        {item:2, cost:3, paths:[3], predecessors:[0,1]},
    ], [0,1,1]), [
        {lane:0, items:1, cost:7, pinned:1, crossLaneEdges:0},
        {lane:1, items:2, cost:8, pinned:0, crossLaneEdges:2},
    ]);
});

test('physical signals include all state regions and preserve storage and program identity',async()=>{
 const {signalsForFootprints}=await import('./parallel-report.mjs');
 const signals=[{event:'signal',program:'p',name:'actual',offset:10,working_offset:'Some(100)',sparse_offset:'Some(200)',width:8},{event:'signal',program:'other',name:'wrong',offset:100,width:8}];
 const matches=signalsForFootprints([{reads:'Exact(MemoryLocation { object: State, offset: 100, byte_len: 1 })',writes:'Exact(MemoryLocation { object: WriteEnvelope, offset: 200, byte_len: 1 })'}],signals,'p');
 assert.deepEqual(matches.map(s=>s.matched_region),['working_offset','sparse_offset']);
 assert.deepEqual(matches.map(s=>s.ranges[0].storage),['State','WriteEnvelope']);
 assert.ok(matches.every(s=>s.name==='actual'));
 assert.equal(signalsForFootprints([{reads:'Exact(MemoryLocation { object: Scratch, offset: 10, byte_len: 1 })'}],signals,'p').length,0);
});
test('final native SIR is selected by schedule even when phase and task are identical',async()=>{
 const {nativeSirForTask}=await import('./parallel-report.mjs');
 const records=['first','second'].map(schedule=>({event:'native_task_sir',schedule,kind:'Comb',task:0,sir:schedule}));
 assert.equal(nativeSirForTask(records,'second',0).sir,'second');
 assert.equal(nativeSirForTask(records,undefined,0),undefined);
});

test('physical plan grouping ignores recycled span IDs and distinguishes programs and stages',async()=>{
 const {physicalPlanKey}=await import('./parallel-report.mjs');
 const event=(program,schedule,hazard_level,id)=>({event:'physical_dependencies',unit:0,scopes:[{id,program,schedule,hazard_level}]});
 const first=event('p','s','units','recycled');
 const second=event('p','t','units','recycled');
 const final=event('p','s','tasks','recycled');
 const other=event('other','s','units','recycled');
 assert.equal(new Set([first,second,final,other].map(physicalPlanKey)).size,4);
 const footprint={...event('p','s','units','different'),event:'footprint'};
 const hazard={event:'hazard',program:'p',schedule:'s',hazard_level:'units'};
 assert.deepEqual([second,footprint,final,hazard,other].filter(e=>physicalPlanKey(e)===physicalPlanKey(first)),[footprint,hazard]);
});
