"""Gate tampering tests; mocked diagnostics confer no proof authority."""
import copy
import unittest
from audit.guarded_expansion.ci import GROUPS, matrix, validate_results
from audit.equality_sharing.ci import AuditFailure


def valid_summaries():
    frozen=iter(matrix());out=[]
    for w,s in GROUPS:
        rows=[]
        for _ in range(8):
            r=next(frozen);name=r['key'].split(':',2)[2]
            negative='_wrong_result' in name or '_complement_wrong' in name or '_guard_escape_wrong' in name
            rows.append({'name':name,'input_sha256':r['input_sha256'],'errors':[],
                         'role':'negative' if negative else 'limitation_positive',
                         'status':'counterexample' if negative else 'stuttering_refinement_verified',
                         'disposition':'validated_original_counterexample' if negative else 'limitation_improved_verified'})
        out.append({'width':w,'alpha_seed':s,'status':'passed','forbidden_solver_invocations':'',
                    'checker_sha256':'fixed','rows':rows})
    return out


class Gate(unittest.TestCase):
    def test_frozen_matrix(self):
        self.assertEqual(len(matrix()),24)
        self.assertEqual(validate_results(valid_summaries()),{'positive_controls_verified':6,'original_formula_sat_mutants':18})

    def reject(self,change):
        summaries=valid_summaries();change(summaries)
        with self.assertRaises(AuditFailure):validate_results(summaries)

    def test_missing_group_and_case_fail(self):
        self.reject(lambda s:s.pop())
        self.reject(lambda s:s[0]['rows'].pop())

    def test_unknown_positive_is_not_control_success(self):
        self.reject(lambda s:s[0]['rows'][0].update(status='unknown',disposition='diagnostic_unknown'))

    def test_auxiliary_sat_and_mutant_verification_fail(self):
        self.reject(lambda s:s[0]['rows'][1].update(disposition='auxiliary_sat'))
        self.reject(lambda s:s[0]['rows'][1].update(disposition='limitation_improved_verified',status='stuttering_refinement_verified'))

    def test_changed_hash_or_order_fail(self):
        self.reject(lambda s:s[0]['rows'][0].update(input_sha256='changed'))
        self.reject(lambda s:s[0]['rows'].reverse())
        self.reject(lambda s:s[1].update(checker_sha256='different'))

    def test_audit_error_or_external_solver_fail(self):
        self.reject(lambda s:s[0]['rows'][0].update(errors=['unaccounted original attempt']))
        self.reject(lambda s:s[0].update(forbidden_solver_invocations='z3'))

if __name__=='__main__':unittest.main()
