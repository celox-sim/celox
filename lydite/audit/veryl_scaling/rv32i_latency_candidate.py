"""Compatibility entry point for the pinned split latency variant."""
from audit.veryl_scaling import rv32i_latency_common as p
from audit.veryl_scaling import rv32i_latency_models as models
from audit.veryl_scaling.rv32i_latency_variants import VARIANTS
c=p.c
s=p.s
VARIANT=VARIANTS['split']
SOURCE=p.source_path(VARIANT)
TOP=VARIANT.top
CONTROL=models.FOURSTAGE_CONTROL
equations=models.fourstage_equations
source_document=models.fourstage_source_document
control_document=models.fourstage_control_document
sha=p.sha

def compile_machine(out,frontend,lifter):return p.compile_machine(out,frontend,lifter,VARIANT)
def run(out,checker=None):return p.run(out,VARIANT,checker)

if __name__=='__main__':p.main(VARIANT)
