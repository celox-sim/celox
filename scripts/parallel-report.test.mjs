import test from "node:test";
import assert from "node:assert/strict";
import {
	analyzeDispatch,
    partitionLanes,
	dependenciesForPaths,
	scopeKey,
    sourcePlanKey,
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

test("compiled plan honors lane order and numbered cross-lane completions", async () => {
    const { plannedTimeline } = await import("./parallel-report.mjs");
    const tasks = [
        { task: 3, lane: 1, waits: [[0, 2]] },
        { task: 0, lane: 0, waits: [] },
        { task: 2, lane: 0, waits: [] },
        { task: 1, lane: 1, waits: [[0, 1]] },
    ];
    const plan = plannedTimeline(tasks, t => [9, 2, 3, 1][t.task]);
    assert.deepEqual(plan.tasks.map(t => [t.start, t.end]), [[0,9],[9,11],[9,12],[12,13]]);
    assert.equal(plan.makespan, 13);
    assert.deepEqual(plan.tasks[3].predecessors, [1,2]);
    assert.equal(plannedTimeline(tasks).makespan, 3);
    assert.throws(() => plannedTimeline([{task:0,lane:1,waits:[[0,1]]}]), /Unresolved dependency/);
    assert.throws(() => plannedTimeline(tasks, () => NaN), /Missing duration/);
    assert.throws(() => plannedTimeline([tasks[0], tasks[0]]), /Unresolved dependency|Duplicate/);
});

test("static candidate cost matches scheduler task overhead and synchronization", async () => {
    const { candidateTimeline } = await import("./parallel-report.mjs");
    const candidate = { task_lanes:[0,1,0], task_items:[[0],[1],[2]],
        task_waits:[[],[[0,1]],[]], order:[2,0,1], task_overhead:1, synchronization_cost:5 };
    const plan = candidateTimeline(candidate, [{item:0,cost:2},{item:1,cost:3},{item:2,cost:9}]);
    assert.deepEqual(plan.tasks.map(t => [t.start,t.end]), [[0,10],[15,18],[10,14]]);
    assert.equal(plan.makespan, 18);
    assert.throws(() => candidateTimeline({}, []), /older capture/);
    assert.throws(() => candidateTimeline(candidate, []), /Missing duration/);
});

test("legacy captured scheduler tasks reproduce costs without execution data", async () => {
    const { candidateTimeline } = await import("./parallel-report.mjs");
    const c = {tasks:'[LaneTask { lane: 0, sequence: 0, items: [0], waits: [] }, LaneTask { lane: 1, sequence: 0, items: [1], waits: [LaneWait { lane: 0, completed_tasks: 1 }] }]',
        order:[0,1], synchronization_cost:400, task_overhead:100, makespan:730};
    const items = [{item:0,cost:50},{item:1,cost:80}];
    assert.deepEqual(candidateTimeline(c,items).tasks.map(t=>[t.start,t.end]), [[0,150],[550,730]]);
    assert.deepEqual(candidateTimeline({...c, start_ns:999, end_ns:100000, elapsed_ns:9000},items),candidateTimeline(c,items));
    assert.throws(()=>candidateTimeline({...c,makespan:731},items),/mismatch/);
    assert.throws(()=>candidateTimeline({...c,tasks:'process.exit()'},items),/Invalid captured/);
    assert.throws(()=>candidateTimeline({...c,synchronization_cost:undefined},items),/Missing static/);
});

test("adopted candidate selection respects scope and the actual recorded placement", async () => {
    const {selectedCandidateIndex} = await import('./parallel-report.mjs');
    const c = {makespan:42,lanes:[0,1],order:[0,1],scopes:[{id:'1'}]};
    const choices = [{...c,scopes:[{id:'2'}]}, {...c,lanes:[1,0]}, c];
    assert.equal(selectedCandidateIndex(choices,[{...c,estimated_makespan:42,accepted:true}]),2);
    assert.equal(selectedCandidateIndex(choices,[{...c,estimated_makespan:42,accepted:false}]),-1);
    assert.equal(selectedCandidateIndex([ {...c,task_budget:0}, {...c,task_budget:16} ],
        [{...c,task_budget:16,estimated_makespan:42,accepted:true}]),1);
});

test("export offers static cost comparison with no measured-duration prediction mode", async () => {
    const {renderReport} = await import('./render-parallel-report.mjs');
    const html = renderReport({schema:1,events:[]});
    assert.ok(html.includes('Scheduler estimate · static cost'));
    assert.ok(html.includes('static-candidate'));
    assert.ok(!html.includes('plan-mode'));
    assert.ok(!html.includes('Ideal replay'));
    assert.ok(!html.includes('Expected · compiled schedule'));
});

function lineageFixture() {
    const c = {event:'candidate',scopes:[{id:'s'}],makespan:10,lanes:[1,1],order:[0,1],task_lanes:[1,1],task_items:[[0],[1]],task_waits:[[],[]]};
    const events = [
        {...c,event:'partition_decision',accepted:true,estimated_makespan:10},
        {event:'lowered_task',scopes:c.scopes,task:0,lane:1,items:[0],units:[0,1]},
        {event:'lowered_task',scopes:c.scopes,task:1,lane:1,items:[1],units:[2]},
        {event:'compiled_task',kind:'Comb',schedule:'comb',task:0,lane:1,units:[0]},
        {event:'compiled_task',kind:'Comb',schedule:'comb',task:1,lane:1,units:[1,2]},
    ];
    const compiled = [
        {task:5,kind:'Fused(clk)',lane:1,units:[0]},
        {task:6,kind:'Fused(clk)',lane:1,units:[1,2,3]},
        {task:7,kind:'Fused(clk)',lane:0,units:[4]},
    ];
    return {c,events,compiled};
}

test('lineage follows splitting and merging through units, never matching p/t numbers', async () => {
    const {taskProvenance} = await import('./parallel-report.mjs');
    const {c,events,compiled} = lineageFixture();
    const result = taskProvenance(c,events,compiled);
    assert.equal(result.reason,null);
    assert.deepEqual(result.links,[{p:0,unit:0,t:5},{p:0,unit:1,t:6},{p:1,unit:2,t:6}]);
    assert.deepEqual(result.runtime,[{task:5,planned:[0],ff_units:[]},{task:6,planned:[0,1],ff_units:[3]},{task:7,planned:[],ff_units:[4]}]);
    assert.deepEqual(taskProvenance(c,[...events,{event:'task_run',task:6,start_ns:999,end_ns:1000}],compiled),result);
    const event = taskProvenance(c,events,[{task:0,kind:'Event(clk)',lane:1,units:[0]}]);
    assert.equal(event.reason,null);
    assert.deepEqual(event.links,[]);
    assert.deepEqual(event.runtime[0].ff_units,[0]);
});

test('lineage refuses unadopted candidates and incomplete or ambiguous evidence', async () => {
    const {taskProvenance} = await import('./parallel-report.mjs');
    const {c,events,compiled} = lineageFixture();
    assert.match(taskProvenance({...c,order:[1,0]},events,compiled).reason,/not adopted/);
    assert.match(taskProvenance(c,events.filter(e=>!(e.event==='lowered_task' && e.task===1)),compiled).reason,/Incomplete/);
    assert.match(taskProvenance(c,[...events,{event:'lowered_task',scopes:[{id:'other'}]}],compiled).reason,/Ambiguous/);
    assert.match(taskProvenance(c,events,[compiled[0],compiled[2]]).reason,/missing combinational/);
    assert.match(taskProvenance(c,events,compiled.map(t=>({...t,lane:2}))).reason,/lane differs/);
    assert.match(taskProvenance(c,events.filter(e=>e.kind!=='Comb'),compiled).reason,/Comb schedule/);
    assert.match(taskProvenance(c,events.map(e=>e.event==='lowered_task' && e.task===1 ? {...e,units:[1]}:e),compiled).reason,/duplicate/);
});

test('lineage respects stable partition and program identities despite recycled scopes', async () => {
    const {taskProvenance,selectedCandidateIndex} = await import('./parallel-report.mjs');
    const {c,events,compiled} = lineageFixture();
    const candidate = {...c,source_partition:'partition-a'};
    const scoped = events.map(e => e.event === 'compiled_task'
        ? {...e,program:'program-a'} : {...e,source_partition:'partition-a'});
    const runtime = compiled.map(t => ({...t,program:'program-a'}));
    assert.equal(taskProvenance(candidate,scoped,runtime).reason,null);
    assert.equal(selectedCandidateIndex([{...candidate,source_partition:'partition-b'},candidate],scoped),1);
    assert.match(taskProvenance(candidate,[...scoped,{...scoped[1],source_partition:'partition-b'}],runtime).reason,/Ambiguous/);
    assert.match(taskProvenance(candidate,scoped,compiled.map(t=>({...t,program:'program-b'}))).reason,/programs differ/);
});

test('task focus follows completion waits and optionally same-lane neighbors', async () => {
    const {taskNeighborhood} = await import('./parallel-report.mjs');
    const tasks=[{task:0,lane:0,waits:[]},{task:1,lane:1,waits:[]},
        {task:2,lane:0,waits:[[1,1]]},{task:3,lane:1,waits:[[0,2]]},
        {task:4,lane:0,waits:[]},{task:5,lane:2,waits:[[0,1]]}];
    const focused=taskNeighborhood(tasks,2);
    assert.deepEqual(focused.nodes.map(t=>t.task),[0,1,2,3,4]);
    assert.deepEqual(focused.edges.filter(e=>e.to===2),[{from:0,to:2,kind:'lane order'},{from:1,to:2,kind:'completion wait'}]);
    assert.deepEqual(taskNeighborhood(tasks,2,{laneOrder:false}).nodes.map(t=>t.task),[1,2,3]);
    assert.deepEqual(taskNeighborhood(tasks,2,{direction:'before'}).nodes.map(t=>t.task),[0,1,2]);
    assert.deepEqual(taskNeighborhood(tasks,2,{direction:'after'}).nodes.map(t=>t.task),[2,3,4]);
    // Descendants of an ancestor are siblings, not this task's descendants.
    assert.ok(!taskNeighborhood(tasks,2,{depth:3}).nodes.some(t=>t.task===5));
    assert.deepEqual(taskNeighborhood(tasks,4,{depth:2,direction:'before'}).nodes.map(t=>t.task),[0,1,2,4]);
});

test('task focus keeps isolated and skipped tasks and rejects incomplete ordering evidence', async () => {
    const {taskNeighborhood} = await import('./parallel-report.mjs');
    assert.deepEqual(taskNeighborhood([{task:8,lane:0,waits:[],skipped:true}],8).nodes.map(t=>t.relation),['selected']);
    assert.throws(()=>taskNeighborhood([],0),/Missing/);
    assert.throws(()=>taskNeighborhood([{task:1,lane:0,waits:[[1,1]]}],1),/Unresolved wait/);
    assert.throws(()=>taskNeighborhood([{task:1,lane:0,waits:[]}],1,{depth:0}),/Invalid focus/);
});

test('timeline focus bounds zoom and preserves many-to-one selected counterparts', async () => {
    const {focusWindow,focusRoles} = await import('./parallel-report.mjs');
    assert.deepEqual(focusWindow([],600),{zoom:0,center:575});
    assert.deepEqual(focusWindow([[100,110]],600),{zoom:8,center:105});
    assert.deepEqual(focusWindow([[100,500],[600,1100]],600),{zoom:1,center:600});
    const neighborhood={nodes:[{task:8,relation:'selected'},{task:11,relation:'successor'},{task:7,relation:'predecessor'}]};
    const links=[{p:8,t:8},{p:11,t:8},{p:7,t:6}];
    const roles=focusRoles(neighborhood,links,'static');
    assert.equal(roles.runtime.get(8),'counterpart');
    assert.equal(roles.runtime.get(6),'related');
    assert.equal(roles.static.get(7),'predecessor');
    const reverse=focusRoles({nodes:[{task:8,relation:'selected'}]},links,'runtime');
    assert.deepEqual([...reverse.static],[[8,'counterpart'],[11,'counterpart']]);
    assert.equal(focusRoles(neighborhood,[],'static').runtime.size,0);
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
test('StateBits footprints are matched by the bytes holding their bits',async()=>{
 const {signalsForFootprints,stateByteRange}=await import('./parallel-report.mjs');
 assert.deepEqual(stateByteRange('StateBits',83,7),{storage:'StateBits',offset:10,length:2,bit_offset:83,bit_length:7});
 assert.deepEqual(stateByteRange('State',10,1),{storage:'State',offset:10,length:1});
 const signals=[{event:'signal',program:'p',name:'low',offset:10,width:8},{event:'signal',program:'p',name:'high',offset:11,width:8},{event:'signal',program:'p',name:'far',offset:12,width:8}];
 const matches=signalsForFootprints([{reads:'Exact(MemoryLocation { object: StateBits, offset: 83, byte_len: 7 })'}],signals,'p');
 assert.deepEqual(matches.map(s=>s.name),['low','high']);
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

test('source plans isolate reused spans and local item IDs across builds', () => {
 const candidate={event:'candidate',scopes:[{id:'reused',source_partition:'first'}]};
 const events=['item','path','merge','lowered_task','slt_node','partition_decision'].flatMap(event=>[
  {event,scopes:[{id:'reused',source_partition:'second'}]},
  {event,scopes:[{id:'different',source_partition:'first'}]},
 ]);
 const joined=events.filter(e=>sourcePlanKey(e)===sourcePlanKey(candidate));
 assert.equal(joined.length,6);
 assert.ok(joined.every(e=>e.scopes[0].source_partition==='first'));
 assert.equal(sourcePlanKey({source_partition:'first'}),sourcePlanKey(candidate));
 assert.equal(sourcePlanKey({scopes:[{id:'old'}]}),'legacy:old');
});

test('source names remain scoped across programs, nested details and runtime events', async () => {
 const {createSourceFormatter}=await import('./parallel-report.mjs');
 const events=['first','second'].flatMap((id,i)=>[
  {event:'source_signal',scopes:[{id:'reused',source_program:id}],source_address:'AbsoluteAddr(inst0, var0)',instance:0,source_object:0,name:`${id}.source`},
  {event:'signal',program:id,address:'AbsoluteAddr(inst0, state0)',instance:0,object:0,name:`${id}.state`},
  {event:'compiled_task',schedule:id,program:id},
  {event:'dispatch',pool:id,epoch:1,schedule:id},
 ]);
 const format=createSourceFormatter(events);
 const a={scopes:[{id:'reused',source_program:'first'}],target:'AbsoluteAddr(inst0, var0)'};
 const b={scopes:[{id:'reused',source_program:'second'}],target:'AbsoluteAddr(inst0, var0)'};
 const result=JSON.parse(format({paths:[a,b],sir:{program:'first',text:'instance_id: InstanceId(0), var_id: StateObjectId(0)'},execution:{pool:'second',epoch:1,text:'AbsoluteAddr(inst0, state0)'}}));
 assert.equal(result.paths[0].target,'first.source');assert.equal(result.paths[1].target,'second.source');
 assert.equal(result.sir.text,'first.state');assert.equal(result.execution.text,'second.state');
 assert.equal(JSON.parse(format(a.target,a)),'first.source');
 assert.equal(JSON.parse(format(a.target)),a.target);
 const legacy=createSourceFormatter([{event:'source_signal',source_address:a.target,name:'one'},{event:'source_signal',source_address:a.target,name:'two'}]);
 assert.equal(JSON.parse(legacy(a.target)),a.target);
});
