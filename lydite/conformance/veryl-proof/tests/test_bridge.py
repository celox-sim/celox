import json,os,pathlib,subprocess,tempfile,unittest
ROOT=pathlib.Path(__file__).resolve().parents[1]
BINARY=ROOT/'../../../target/debug/lydite-celox-suite'
REJECTION='hierarchy::test_dynamic_minus_colon_output_port_rmw'
class BridgeTests(unittest.TestCase):
 def test_unknown_and_duplicate_selection_fail(self):
  with tempfile.TemporaryDirectory() as td:
   for selection in [['not-a-case'],['basic::test_simple_assignment']*2]:
    out=pathlib.Path(td)/str(len(selection))
    p=subprocess.run([str(BINARY),str(out),*selection],capture_output=True,text=True)
    self.assertEqual(p.returncode,2);self.assertFalse(out.exists())
 def test_compiler_crash_and_empty_failure_are_not_rejection_passes(self):
  with tempfile.TemporaryDirectory() as td:
   root=pathlib.Path(td)
   for name,script in [('panic','printf "thread main panicked at injected\\n" >&2\nexit 101'),('empty','exit 1'),('invalid_json','printf "not JSON\\n"\nexit 0')]:
    compiler=root/name;compiler.write_text('#!/bin/sh\n'+script+'\n');compiler.chmod(0o700)
    out=root/(name+'-out')
    p=subprocess.run([str(BINARY),str(out),REJECTION],env=dict(os.environ,SIR_EXPORTER_BIN=str(compiler)),capture_output=True,text=True,timeout=30)
    self.assertEqual(p.returncode,1)
    rows=json.load(open(out/'summary.json'));self.assertEqual(rows[0]['status'],'failed')
if __name__=='__main__':unittest.main()
