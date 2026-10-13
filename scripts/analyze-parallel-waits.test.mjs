import test from 'node:test';
import assert from 'node:assert/strict';
import { explainWaits, renderWaitAnalysis } from './analyze-parallel-waits.mjs';
function fixture(){return [
 {event:'compiled_task',schedule:'s',task:0,lane:0,scopes:[{id:'a'}]},
 {event:'compiled_task',schedule:'s',task:1,lane:1,scopes:[{id:'a'}]},
 {event:'dispatch',schedule:'s',pool:'p',epoch:1},
 {event:'task_run',task:0,pool:'p',epoch:1,start_ns:10,end_ns:30,code:0},
 {event:'task_run',task:1,pool:'p',epoch:1,start_ns:35,end_ns:40,code:0},
 {event:'task_wait',task:1,pool:'p',epoch:1,lane:1,other:0,completed:1,start_ns:15,end_ns:35},
 {event:'signal',name:'dut.control',address:'a',offset:100,width:8,working_offset:'Some(200)'},
 {event:'hazard',hazard:'RAW',predecessor:0,unit:1,scopes:[{id:'a'}],producer:'Exact(MemoryLocation { object: State, offset: 200, byte_len: 1 })',consumer:'Exact(MemoryLocation { object: State, offset: 200, byte_len: 1 })'},
];}
test('separates producer blocking from ready-tail and resolves working storage',()=>{
 const a=explainWaits(fixture());assert.deepEqual(a.issues,[]);const r=a.rows[0];
 assert.equal(r.blocked_ns,15);assert.equal(r.ready_tail_ns,5);assert.equal(r.producer_work_ns,20);
 assert.equal(r.conflicts[0].signals[0].name,'dut.control');assert.equal(r.conflicts[0].signals[0].region,'working_offset');
});
test('keeps already-ready waits distinct and never assigns conflicts from another scope',()=>{
 const es=fixture();es.find(x=>x.event==='task_wait').start_ns=31;
 es.find(x=>x.event==='hazard').scopes=[{id:'other'}];
 const r=explainWaits(es).rows[0];assert.equal(r.blocked_ns,0);assert.equal(r.ready_tail_ns,4);assert.deepEqual(r.conflicts,[]);
});
test('missing or failed producer is not treated as a valid dependency explanation',()=>{
 const es=fixture();es.find(x=>x.event==='task_run').code=1;assert.equal(explainWaits(es).issues.length,1);
 assert.equal(explainWaits(es.filter(x=>x.event!=='compiled_task')).rows.length,0);
});
test('signal literals cannot close the embedded report script',()=>{
 const a={rows:[],issues:[],note:'</script><script>alert(1)</script>'};
 const html=renderWaitAnalysis(a);assert.ok(!html.includes(a.note));
 const payload=html.split('id="data">')[1].split('</script>')[0];assert.deepEqual(JSON.parse(payload),a);
});
test('identical task numbers in another pool do not change attribution',()=>{
 const es=fixture();es.push({event:'task_run',task:0,pool:'different',epoch:1,start_ns:0,end_ns:999});
 assert.equal(explainWaits(es).rows[0].blocked_ns,15);
});
test('stable plan identity connects out-of-span Cranelift metadata through unit IDs',()=>{
 const es=fixture(),tasks=es.filter(e=>e.event==='compiled_task');
 for(const [i,t] of tasks.entries()){t.hazard_level='units';t.units=[10+i];t.scopes=[];}
 const h=es.find(e=>e.event==='hazard');h.scopes=[{id:'planning-only',schedule:'s',hazard_level:'units'}];h.predecessor=10;h.unit=11;
 const unrelated=structuredClone(h);unrelated.scopes=[{id:'planning-only',schedule:'other',hazard_level:'units'}];unrelated.hazard='WAW';es.push(unrelated);
 const a=explainWaits(es);assert.deepEqual(a.issues,[]);assert.equal(a.rows[0].conflicts.length,1);assert.equal(a.rows[0].conflicts[0].type,'RAW');
});
test('native final task hazards are isolated from the preliminary unit plan',()=>{
 const es=fixture();for(const t of es.filter(e=>e.event==='compiled_task')){t.hazard_level='tasks';t.units=[t.task+10];t.scopes=[];}
 const h=es.find(e=>e.event==='hazard');h.scopes=[{id:'final',schedule:'s',hazard_level:'tasks'}];
 const pre=structuredClone(h);pre.scopes=[{id:'early',schedule:'s',hazard_level:'units'}];pre.hazard='WAW';es.push(pre);
 assert.deepEqual(explainWaits(es).rows[0].conflicts.map(c=>c.type),['RAW']);
});
test('multiple programs with identical topology and storage do not mix tasks or signal names',()=>{
 const a=fixture(),b=fixture();
 for(const [events,schedule,program,pool,name] of [[a,'first','program1','pool1','first.signal'],[b,'second','program2','pool2','second.signal']]){
  for(const e of events){if(e.schedule)e.schedule=schedule;if(e.pool)e.pool=pool;if(e.event==='compiled_task'){e.program=program;e.hazard_level='tasks';e.scopes=[];}if(e.event==='hazard')e.scopes=[{id:'reused',schedule,hazard_level:'tasks'}];if(e.event==='signal'){e.program=program;e.name=name;}}
 }
 const r=explainWaits([...a,...b]);assert.equal(r.rows.length,2);assert.deepEqual(r.rows.map(r=>r.conflicts[0].signals.map(s=>s.name)),[['first.signal'],['second.signal']]);
});

test('aborted or missing consumers do not contribute dependency totals',()=>{
 for (const missing of [false,true]) {
  let es=fixture();
  const consumer=es.find(e=>e.event==='task_run'&&e.task===1);
  consumer.skipped=true;
  if(missing) es=es.filter(e=>e!==consumer);
  const a=explainWaits(es);
  assert.deepEqual(a.rows,[]);
  assert.equal(a.issues.length,1);
  assert.match(a.issues[0].reason,/consumer/);
 }
});
test('consumer records from another epoch or pool cannot validate a wait',()=>{
 for(const field of ['epoch','pool']) {
  const es=fixture();es.find(e=>e.event==='task_run'&&e.task===1)[field]='other';
  const a=explainWaits(es);assert.deepEqual(a.rows,[]);assert.equal(a.issues.length,1);
 }
});
