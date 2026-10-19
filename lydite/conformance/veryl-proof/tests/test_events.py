import unittest
from proof_backend import Backend, Unsupported


class EventTests(unittest.TestCase):
    def test_four_state_edge_table(self):
        # Payload/mask levels: 0, 1, Z=2, X=3. IEEE 1800 Table 9-2.
        rising = {(0, 1), (0, 2), (0, 3), (2, 1), (3, 1)}
        falling = {(1, 0), (1, 2), (1, 3), (2, 0), (3, 0)}
        backend = Backend.__new__(Backend)
        for kind, expected in [('ClockPosedge', rising), ('ResetAsyncHigh', rising),
                               ('ClockNegedge', falling), ('ResetAsyncLow', falling)]:
            backend.event_kinds = {'event': kind}
            for before in range(4):
                for after in range(4):
                    with self.subTest(kind=kind, before=before, after=after):
                        self.assertEqual(backend.event_edge('event', before, after),
                                         (before, after) in expected)

    def test_untyped_event_fails_closed(self):
        backend = Backend.__new__(Backend)
        backend.event_kinds = {'event': 'Other'}
        with self.assertRaisesRegex(Unsupported, 'no edge kind'):
            backend.event_edge('event', 0, 1)


if __name__ == '__main__':
    unittest.main()
