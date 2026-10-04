"""Pinned latency variants: source identity, top, family and reset projection."""
from dataclasses import dataclass

@dataclass(frozen=True)
class Variant:
    name: str
    filename: str
    top: str
    source_sha256: str
    family: str
    wrapper: str
    nonzero_resets: tuple = ()

VARIANTS = {
 'baseline': Variant('baseline','rv32i_pipeline.veryl','RV32IPipeline',
  'a5e4cffe59180026172d346fc4318c92e4bc6cad34b62cac5f16808c39da2ca3','baseline','rv32i_latency'),
 'split': Variant('split','rv32i_pipeline_split_candidate.veryl','RV32IPipelineSplitCandidate',
  '7a369b82acc1b9a98da693e9fa4b8fcc26403a39984b3c42a408912f72afbffe','fourstage','rv32i_latency_candidate'),
 'rhs': Variant('rhs','rv32i_pipeline_split_rhs_candidate.veryl','RV32IPipelineSplitRhsCandidate',
  '2dc012b1e74b7e4801575897f04664c67e5072546fc49072a2fdf9cbcc19b557','fourstage','rv32i_latency_rhs_candidate'),
 'predecode': Variant('predecode','rv32i_pipeline_split_predecode_candidate.veryl','RV32IPipelineSplitPredecodeCandidate',
  'd33cc2c43c61c39cf5977efe562bc30d4cb1d2bc5777e2479038f1a7db4749dc','fourstage','rv32i_latency_predecode_candidate'),
 'onehot': Variant('onehot','rv32i_pipeline_split_onehot_candidate.veryl','RV32IPipelineSplitOnehotCandidate',
  'b5c634c60b31c07313f5c54b069b5428013fa162b63f96e24cbb0f56f74722ab','fourstage','rv32i_latency_onehot_candidate',
  (('d_sel1',32,1),('d_sel2',32,1))),
}
