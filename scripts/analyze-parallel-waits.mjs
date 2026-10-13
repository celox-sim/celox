import fs from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";

// Attribute only actual waits to the exact compiled schedule that ran. A
// consumer can also wait for an indirect predecessor; do not invent a direct
// memory conflict when the tracker did not report one.
export function explainWaits(events) {
  const schedules = new Map(), runs = new Map(), dispatches = new Map(), hazards = new Map();
  const signals = events.filter(e => e.event === "signal");
  const scope = e => (e.scopes ?? []).map(s => s.id).join("/");
  const execution = e => `${e.pool}/${e.epoch}`;
  for (const e of events) {
    if (e.event === "compiled_task") {
      if (!schedules.has(e.schedule)) schedules.set(e.schedule, new Map());
      schedules.get(e.schedule).set(e.task, e);
    } else if (e.event === "task_run") {
      runs.set(`${execution(e)}/${e.task}`, e);
    } else if (e.event === "dispatch") dispatches.set(execution(e), e);
    else if (e.event === "hazard") {
      const key = `${scope(e)}/${e.predecessor}/${e.unit}`;
      if (!hazards.has(key)) hazards.set(key, []);
      hazards.get(key).push(e);
    }
  }
  const laneTasks = new Map();
  for (const [id, tasks] of schedules) {
    const lanes = new Map();
    for (const t of [...tasks.values()].sort((a,b) => a.task - b.task)) {
      if (!lanes.has(t.lane)) lanes.set(t.lane, []);
      lanes.get(t.lane).push(t);
    }
    laneTasks.set(id, lanes);
  }
  const rows = new Map(), issues = [];
  for (const w of events.filter(e => e.event === "task_wait")) {
    const d = dispatches.get(execution(w));
    const producer = laneTasks.get(d?.schedule)?.get(w.other)?.[w.completed - 1];
    const consumer = schedules.get(d?.schedule)?.get(w.task);
    const p = producer && runs.get(`${execution(w)}/${producer.task}`);
    if (!d || !producer || !consumer || !p || p.skipped || (p.code ?? 0) !== 0
        || w.end_ns < w.start_ns || p.end_ns > w.end_ns) {
      issues.push({wait:w, reason:"Missing or inconsistent producer, schedule or timing"});
      continue;
    }
    const key = `${d.schedule}/${producer.task}/${consumer.task}`;
    if (!rows.has(key)) rows.set(key, {schedule:d.schedule, producer:producer.task,
      consumer:consumer.task, producer_lane:producer.lane, consumer_lane:consumer.lane,
      producer_units:producer.units ?? [], consumer_units:consumer.units ?? [],
      count:0, wait_ns:0, blocked_ns:0, ready_tail_ns:0, producer_work_ns:0,
      conflicts:hazards.get(`${scope(consumer)}/${producer.task}/${consumer.task}`) ?? []});
    const row = rows.get(key);
    row.count++;
    row.wait_ns += w.end_ns - w.start_ns;
    row.blocked_ns += Math.max(0, p.end_ns - w.start_ns);
    row.ready_tail_ns += w.end_ns - Math.max(w.start_ns, p.end_ns);
    row.producer_work_ns += p.end_ns - p.start_ns;
  }
  const cache = new Map();
  function names(effect) {
    if (cache.has(effect)) return cache.get(effect);
    const m = effect?.match(/object: (State|WriteEnvelope), offset: (\d+), byte_len: (\d+)/);
    const matches = [];
    if (m) {
      const low = Number(m[2]), high = low + Number(m[3]);
      for (const s of signals) for (const region of ["offset", "working_offset", "sparse_offset"]) {
        const raw = s[region];
        const start = typeof raw === "number" ? raw : Number(raw?.match(/Some\((\d+)\)/)?.[1]);
        if (Number.isFinite(start) && start < high && low < start + Math.ceil(s.width / 8))
          matches.push({name:s.name, region, address:s.address, offset:start, width:s.width});
      }
    }
    cache.set(effect, matches);
    return matches;
  }
  const result = [...rows.values()].sort((a,b) => b.blocked_ns - a.blocked_ns);
  for (const row of result) row.conflicts = row.conflicts.map(c => ({type:c.hazard,
    producer_effect:c.producer, consumer_effect:c.consumer,
    representative:c.representative ?? false, signals:names(c.consumer)}));
  return {rows:result, issues,
    note:"Wait sums span lanes and must not be added to wall time. Blocked time ends when the required producer finishes; ready-tail includes notification/check/scheduling overhead. Signal names are overlapping storage candidates, including aliases, not proof of a semantic dependence. Exact byte ranges and the tracker edge are authoritative; a missing direct conflict may be a transitive ordering edge. Array storage can be strided, so names alone are not an exact address map."};
}

export function renderWaitAnalysis(analysis) {
  const payload = JSON.stringify(analysis).replaceAll("<", "\\u003c");
  return `<!doctype html><html lang="ja"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>並列スケジューラー：待ちの原因</title>
<style>:root{color-scheme:dark;font:15px system-ui;background:#101723;color:#e0e8f4}body{margin:28px}h1{font-size:25px}table{border-collapse:collapse;width:100%}td,th{padding:10px;text-align:left;border-bottom:1px solid #314663}tr[data-row]{cursor:pointer}tr[data-row]:hover{background:#293f5b}.muted{color:#9eafc6}.card{background:#182337;padding:18px;border-radius:9px;margin:16px 0}pre{white-space:pre-wrap;overflow-wrap:anywhere;max-height:65vh;overflow:auto;font:12px ui-monospace,monospace}input{font:inherit;padding:8px;background:#22324b;color:inherit;border:1px solid #55718f}.scroll{overflow:auto;max-height:65vh}a{color:#93c6ff}</style>
<h1>依存待ちを、元の回路とメモリ範囲まで逆引きする</h1>
<p>待った側 → 待ち先タスクを特定し、実際の依存トラッカーの RAW / WAR / WAW と信号名を対応づけています。行をクリックすると詳細を表示します。</p>
<p class="muted">「結果待ち」は必要な producer の完了まで。「完了後」は通知・確認・スケジューリングを含みます。待ち時間の合計は複数レーンにまたがり、実行時間の短縮可能量ではありません。信号名は物理範囲の重なりから得た候補で、配列のストライドや別名もあるため、生のバイト範囲を併記します。</p>
<div class="card"><label>信号名・タスク番号 <input id="filter"></label><p id="status"></p><div class="scroll"><table id="waits"></table></div></div>
<div class="card"><h2>依存の詳細</h2><pre id="details">行を選択してください。</pre></div><p><a href="report.html">全イベント・ソース・タイムライン</a></p>
<script type="application/json" id="data">${payload}</script><script>
const a=JSON.parse(document.getElementById('data').textContent),$=id=>document.getElementById(id),us=x=>(x/1000).toFixed(3)+' µs';
function row(parent,values,tag='td'){const r=document.createElement('tr');for(const v of values){const c=document.createElement(tag);c.textContent=v;r.append(c);}parent.append(r);return r;}
function show(){const q=$('filter').value.toLowerCase(),t=$('waits');t.replaceChildren();row(t,['待ったタスク ← 待ち先','Lane','平均の結果待ち','平均の完了後','待ち先の平均実行','信号・理由'],'th');for(const r of a.rows.filter(r=>JSON.stringify(r).toLowerCase().includes(q))){const names=[...new Set(r.conflicts.flatMap(c=>c.signals.map(s=>s.name)))];const tr=row(t,[r.consumer+' ← '+r.producer,r.consumer_lane+' ← '+r.producer_lane,us(r.blocked_ns/r.count),us(r.ready_tail_ns/r.count),us(r.producer_work_ns/r.count),names.slice(0,4).join(', ')||'直接の衝突範囲は未特定']);tr.dataset.row='true';tr.onclick=()=>{$('details').textContent=JSON.stringify(r,null,2);};}$('status').textContent=a.rows.length+' 依存辺 / '+a.issues.length+' 記録の不整合';}show();$('filter').oninput=show;
</script></html>`;
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  const [input, output = path.dirname(input ?? "report.json")] = process.argv.slice(2);
  if (!input) throw new Error("Usage: node scripts/analyze-parallel-waits.mjs report.json [output-directory]");
  const report = JSON.parse(fs.readFileSync(input, "utf8"));
  if (report.schema !== 1 || !Array.isArray(report.events)) throw new Error("Expected report schema 1");
  const analysis = explainWaits(report.events);
  fs.mkdirSync(output, {recursive:true});
  fs.writeFileSync(path.join(output,"wait-analysis.json"), JSON.stringify(analysis,null,2));
  fs.writeFileSync(path.join(output,"wait-analysis.html"), renderWaitAnalysis(analysis));
  console.log(JSON.stringify({edges:analysis.rows.length, issues:analysis.issues.length,
    top:analysis.rows.slice(0,5).map(r=>({producer:r.producer,consumer:r.consumer,blocked_ns:r.blocked_ns/r.count,signals:r.conflicts.flatMap(c=>c.signals.map(s=>s.name))}))}));
  if (analysis.issues.length) process.exitCode = 1;
}
