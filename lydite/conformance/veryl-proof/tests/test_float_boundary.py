import json,os,pathlib,subprocess,tempfile,unittest
from proof_backend import has_float_operations
from coverage import collect,GateError
ROOT=pathlib.Path(__file__).resolve().parents[1]
BINARY=ROOT/'../../../target/debug/lydite-celox-suite'
MESSAGE='proof backend unsupported: floating-point SIR operations'
class FloatBoundaryTests(unittest.TestCase):
 def test_recognizes_only_typed_float_operations(self):
  op={'Float':{'op':'Add','precision':'F64'}}
  instruction={'Binary':[1,2,op,3]}
  self.assertTrue(has_float_operations({'blocks':[instruction]}))
  self.assertTrue(has_float_operations({'Unary':[1,{'Float':{'op':{'ToSigned':{'width':32,'round':True}},'precision':'F32'}},2]}))
  for bad in [None,{},'Float',{'Float':None},{'Float':{'op':'Unknown','precision':'F64'}},{'Float':{'op':'Add','precision':'invalid'}}]:
   with self.subTest(bad=bad):self.assertFalse(has_float_operations({'Binary':[1,2,bad,3]}))
  self.assertFalse(has_float_operations({'Binary':[1,2,'Add',3]}))
 def test_coverage_requires_exact_typed_exception(self):
  with tempfile.TemporaryDirectory() as temp:
   raw=pathlib.Path(temp);name='floating_point::case';d=raw/'floating_point__case/design-1';d.mkdir(parents=True)
   panic='compile '+name+': '+MESSAGE
   row={'case':name,'status':'failed','expectation':'Simulation','errors':[],'panic':panic,'designs':1}
   listed=[{'case':name,'expectation':'Simulation','category':'Test','script_identity':'fp script','source':{'file':'fp.vtest'}}]
   record={'status':'failed','compilation_rejected':False,'proof_backend_unsupported':True,'error':MESSAGE,'reads':0,'operations':0,'commands':2,'design_sha256':'source','protocol_sha256':'flow','diagnostic':{'stage':'proof_backend','detail':MESSAGE}}
   exception={name:{'kind':'proof_backend_unsupported','reason':'FP lacks a proof model','panic':panic}}
   def save():
    (raw/'summary.json').write_text(json.dumps([row]));(d/'backend-result.json').write_text(json.dumps(record))
   save();actual,counts=collect(raw,listed,exception,1)
   self.assertEqual(counts['actual_passes'],0);self.assertEqual(actual['cases'][0]['disposition'],'proof_backend_unsupported')
   for label,exceptions in [('missing',{}),('wrong_kind',{name:dict(exception[name],kind='known_celox_failure')}),('wrong_message',{name:dict(exception[name],panic='other')})]:
    with self.subTest(label=label),self.assertRaises(GateError):collect(raw,listed,exceptions,1)
   for key,value in [('proof_backend_unsupported',False),('reads',1),('error','other failure')]:
    old=record[key];record[key]=value;save()
    with self.subTest(key=key),self.assertRaises(GateError):collect(raw,listed,exception,1)
    record[key]=old
   row['errors']=['broken pipe'];save()
   with self.assertRaises(GateError):collect(raw,listed,exception,1)
   row['errors']=[];save();(d/'proof').mkdir()
   with self.assertRaises(GateError):collect(raw,listed,exception,1)
 def test_malformed_float_is_still_a_backend_error(self):
  with tempfile.TemporaryDirectory() as temp:
   root=pathlib.Path(temp);compiler=root/'exporter'
   compiler.write_text('#!/bin/sh\nprintf \'%s\\n\' \'{"units":[{"Binary":[1,2,{"Float":{"op":"invalid","precision":"F64"}},3]}]}\'\n')
   compiler.chmod(0o700);out=root/'results'
   run=subprocess.run([str(BINARY),str(out),'floating_point::arithmetic_and_rounding'],env=dict(os.environ,SIR_EXPORTER_BIN=str(compiler)),capture_output=True,text=True,timeout=30)
   self.assertEqual(run.returncode,1)
   row=json.loads((out/'summary.json').read_text())[0]
   self.assertTrue(row['errors'])
   record=json.loads((out/'floating_point__arithmetic_and_rounding/design-1/backend-result.json').read_text())
   self.assertFalse(record['proof_backend_unsupported'])
 def test_compile_refusal_is_typed_and_has_no_proof(self):
  with tempfile.TemporaryDirectory() as temp:
   root=pathlib.Path(temp);compiler=root/'exporter'
   compiler.write_text('#!/bin/sh\nprintf \'%s\\n\' \'{"units":[{"Binary":[1,2,{"Float":{"op":"Add","precision":"F64"}},3]}]}\'\n')
   compiler.chmod(0o700);out=root/'results'
   run=subprocess.run([str(BINARY),str(out),'floating_point::arithmetic_and_rounding'],env=dict(os.environ,SIR_EXPORTER_BIN=str(compiler)),capture_output=True,text=True,timeout=30)
   self.assertEqual(run.returncode,1,run.stderr)
   row=json.loads((out/'summary.json').read_text())[0]
   self.assertEqual(row['errors'],[]);self.assertEqual(row['status'],'failed')
   self.assertIn(MESSAGE,row['panic'])
   design=out/'floating_point__arithmetic_and_rounding/design-1'
   record=json.loads((design/'backend-result.json').read_text())
   self.assertTrue(record['proof_backend_unsupported']);self.assertFalse(record['compilation_rejected'])
   self.assertEqual(record['reads'],0);self.assertFalse((design/'proof').exists())
if __name__=='__main__':unittest.main()
