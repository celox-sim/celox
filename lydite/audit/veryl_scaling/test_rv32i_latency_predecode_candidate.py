"""Compatibility tests and concrete witnesses for the predecode variant."""
import unittest
from audit.veryl_scaling import rv32i_latency_witnesses as shared
from audit.veryl_scaling import rv32i_latency_test_support as support
from audit.veryl_scaling.rv32i_latency_variants import VARIANTS
from audit.veryl_scaling.rv32i_latency_witnesses import initial,inputs,val,enc_i,enc_r,enc_s,enc_b
VARIANT=VARIANTS['predecode']

def witness(machine,roots,program,pick=0,stall_edges=()):
    return shared.witness(machine,roots,program,VARIANT,pick,stall_edges)
def actual_witnesses(machine,roots):return shared.actual_witnesses(machine,roots,VARIANT)

class TrackerTests(support.TrackerTests):
    variant=VARIANT

if __name__=='__main__':unittest.main()
