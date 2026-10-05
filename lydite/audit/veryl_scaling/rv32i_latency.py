"""Compatibility entry point for the pinned baseline latency variant."""
from audit.veryl_scaling import rv32i_latency_common as p
from audit.veryl_scaling import rv32i_latency_models as models
from audit.veryl_scaling.rv32i_latency_variants import VARIANTS
c=p.c
s=p.s
VARIANT=VARIANTS['baseline']
SOURCE=p.source_path(VARIANT)
TOP=VARIANT.top
CONTROL=models.BASELINE_CONTROL
equations=models.baseline_equations
source_document=models.baseline_source_document
control_document=models.baseline_control_document
sha=p.sha

def compile_machine(out,frontend,lifter):return p.compile_machine(out,frontend,lifter,VARIANT)
def run(out,checker=None):return p.run(out,VARIANT,checker)

if __name__=='__main__':p.main(VARIANT)
