"""Shared tracker tests used by legacy per-variant unittest entry points."""
import json
import unittest
from audit.interpreter import evaluate_record
from audit.veryl_scaling import rv32i_latency_common as common
from audit.veryl_scaling.rv32i_latency_witnesses import initial,val

class TrackerTests(unittest.TestCase):
    variant=None
    def model(self):return common.builders(self.variant)[2]()
    def test_reset_cancels_epoch(self):
        reset=initial(self.model()['implementation'])
        self.assertEqual(val(reset['tag']),0);self.assertEqual(val(reset['age']),0)
        self.assertFalse(any(reset[k] for k in common.builders(self.variant)[0]))
    def test_dut_namespace_noninterference(self):
        m=self.model()['implementation']
        for field in common.builders(self.variant)[0]:
            text=json.dumps(m['next'][field]);self.assertNotIn('s.tag',text);self.assertNotIn('s.age',text);self.assertNotIn('i.choose',text)
    def test_single_chosen_token(self):
        m=self.model()['implementation'];state=initial(m)
        inp=dict(rst=False,stall=False,h=False,f=False,choose=True)
        four=self.variant.family=='fourstage'
        inp['t' if four else 'r']=False
        tags=[]
        for _ in range(9):
            state=evaluate_record(m,'next',state,inp);tags.append(val(state['tag']))
        self.assertEqual(tags,[1,2,3,4,5,5,5,5,5] if four else [1,2,3,4,4,4,4,4,4])
