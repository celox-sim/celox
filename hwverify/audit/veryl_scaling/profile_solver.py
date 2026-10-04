#!/usr/bin/env python3
"""Build a throwaway timing-instrumented checker; production solver unchanged."""
import argparse,pathlib,shutil,subprocess
ROOT=pathlib.Path(__file__).resolve().parents[2]
p=argparse.ArgumentParser();p.add_argument('--out',type=pathlib.Path,required=True);p.add_argument('--source-ref');a=p.parse_args();a.out=a.out.resolve();a.out.mkdir(parents=True,exist_ok=False)
# The checker builds inside the repository-root Cargo workspace.
REPO=ROOT.parent;WORKSPACE=('crates','vendor','examples')
if a.source_ref:
    names=['Cargo.toml','Cargo.lock','clippy.toml',*subprocess.check_output(['git','ls-tree','-r','--name-only',a.source_ref,'--',*WORKSPACE],cwd=REPO,text=True).splitlines()]
    for name in names:
        destination=a.out/name;destination.parent.mkdir(parents=True,exist_ok=True);destination.write_bytes(subprocess.check_output(['git','show',a.source_ref+':'+name],cwd=REPO))
else:
    for name in ('Cargo.toml','Cargo.lock','clippy.toml'):shutil.copy2(REPO/name,a.out/name)
    for name in WORKSPACE:shutil.copytree(REPO/name,a.out/name,ignore=shutil.ignore_patterns('node_modules','target'))
f=a.out/'crates/hwverify-solver/src/finite.rs';s=f.read_text()
s=s.replace('pub struct Stats {','pub struct Stats {\n    pub encoding_seconds: f64,\n    pub search_seconds: f64,\n    pub validation_seconds: f64,\n    pub encoding_work: u64,')
s=s.replace('"solver_result":self.verdict.as_str(),','"encoding_seconds":self.stats.encoding_seconds,"search_seconds":self.stats.search_seconds,"validation_seconds":self.stats.validation_seconds,"encoding_work":self.stats.encoding_work,"timing_scope":"encoding=aliases+bitblast+context; search=split planning+CNF copies+CDCL; validation=SAT witness checks", "solver_result":self.verdict.as_str(),')
s=s.replace('    let compile = (|| -> Res<()> {','    let encoding_started = Instant::now();\n    let compile = (|| -> Res<()> {')
s=s.replace('    outcome.stats.terms = blast.memo.len();','    outcome.stats.encoding_seconds = encoding_started.elapsed().as_secs_f64();\n    outcome.stats.encoding_work = budget.work;\n    outcome.stats.terms = blast.memo.len();')
s=s.replace('    let choices = match split_choices(', '    let search_started = Instant::now();\n    let mut search_finished = false;\n    let mut validation_started = None;\n    let choices = match split_choices(')
s=s.replace('        budget.check_time()?;\n        if verdict != Verdict::Sat {','        outcome.stats.search_seconds = search_started.elapsed().as_secs_f64();\n        search_finished = true;\n        budget.check_time()?;\n        if verdict != Verdict::Sat {')
s=s.replace('        // Validate every original CNF clause', '        validation_started = Some(Instant::now());\n        // Validate every original CNF clause')
s=s.replace('    outcome.stats.clauses = cumulative_clauses;', '    if !search_finished { outcome.stats.search_seconds = search_started.elapsed().as_secs_f64(); }\n    if let Some(started) = validation_started { outcome.stats.validation_seconds = started.elapsed().as_secs_f64(); }\n    outcome.stats.clauses = cumulative_clauses;')
f.write_text(s)
(a.out/'instrumentation.patch').write_text(subprocess.run(['diff','-u',str(REPO/'crates/hwverify-solver/src/finite.rs'),str(f)],capture_output=True,text=True).stdout)
subprocess.run(['cargo','build','--release','--locked','-p','hwverify-rs','--manifest-path',str(a.out/'Cargo.toml')],check=True)
print(a.out/'target/release/hwverify-rs')
