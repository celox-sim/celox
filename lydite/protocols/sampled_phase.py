"""Reusable native-IR guard for one initial reset followed by normal ticks.

The predicate observes settled pre-edge values on the first released tick.
The helper imposes no restriction on that tick's post-edge values. It does not
validate a clock/reset waveform or add repeated-reset support to a replay engine.
"""
import re


def first_release_guard(state_name, required_before):
    """Return a private phase bit and a violation expression in canonical v3 IR.

    Merge state/reset/next into the implementation with collision checks, and
    route violation into a checked obligation. Native IR validates predicate
    types. The caller must use exactly one initial reset and unconditional ticks.
    """
    if not isinstance(state_name, str) or not re.fullmatch(r'[A-Za-z_][A-Za-z_0-9]*', state_name):
        raise ValueError('first-release phase state must be a plain identifier')
    return {'state': {state_name: 'bool'}, 'reset': {state_name: True},
            'next': {state_name: False},
            'violation': ['and', 's.' + state_name, ['not', required_before]]}
