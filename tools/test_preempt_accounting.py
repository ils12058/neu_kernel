"""Regression checks for the user/kernel timer ledger in the QMP oracle."""

import unittest

from neu import Failure, check_preempt_tick_accounting, preempt_drain_records


class PreemptAccountingTests(unittest.TestCase):
    def events(self, ticks):
        return [{'tick': tick} for tick in ticks]

    def test_user_only_ticks_remain_exact_and_contiguous(self):
        check_preempt_tick_accounting(self.events([1, 2, 3]), [], 3, 3, 3, 0)

    def test_captured_middle_drain_is_separate_from_sixteen_user_frames(self):
        # The retained failing execution had user ticks 1..8, a common-stack
        # drain at 9, then user ticks 10..17. No user context was lost.
        text = 'Preempt deferred drained: before=8 after=9 eois=9 root=0x1000 IF=0 kernel_switch=false'
        drains = preempt_drain_records(text, 0x1000)
        events = self.events([*range(1, 9), *range(10, 18)])
        check_preempt_tick_accounting(events, drains, 17, 17, 16, 1)

    def test_existing_single_final_shutdown_drain_remains_bounded(self):
        check_preempt_tick_accounting(self.events([1, 2]), [], 3, 3, 2, 1)

    def test_an_unexplained_middle_tick_cannot_be_a_final_drain(self):
        with self.assertRaises(Failure):
            check_preempt_tick_accounting(self.events([1, 3]), [], 3, 3, 2, 1)

    def test_duplicate_or_reordered_user_ticks_are_rejected(self):
        for ticks in ([1, 1], [2, 1]):
            with self.subTest(ticks=ticks), self.assertRaises(Failure):
                check_preempt_tick_accounting(self.events(ticks), [], 2, 2, 2, 0)

    def test_foreign_root_missing_eoi_and_extra_tick_do_not_authorize_a_drain(self):
        for before, after, eois, root in [(8, 9, 9, 0xC000), (8, 9, 8, 0x1000), (8, 10, 10, 0x1000)]:
            text = f'Preempt deferred drained: before={before} after={after} eois={eois} root={root:#x} IF=0 kernel_switch=false'
            with self.subTest(text=text), self.assertRaises(Failure):
                preempt_drain_records(text, 0x1000)

    def test_replayed_or_second_drain_exceeds_the_existing_bound(self):
        text = 'Preempt deferred drained: before=8 after=9 eois=9 root=0x1000 IF=0 kernel_switch=false\n'
        for duplicate in (text, 'Preempt deferred drained: before=16 after=17 eois=17 root=0x1000 IF=0 kernel_switch=false'):
            with self.subTest(duplicate=duplicate), self.assertRaises(Failure):
                preempt_drain_records(text + duplicate, 0x1000)

    def test_counter_conservation_and_drain_classification_are_required(self):
        event = self.events([1, 2])
        for counters in [(2, 1, 2, 0), (2, 2, 1, 0), (3, 3, 2, 0), (4, 4, 2, 2)]:
            with self.subTest(counters=counters), self.assertRaises(Failure):
                check_preempt_tick_accounting(event, [], *counters)
        with self.assertRaises(Failure):
            check_preempt_tick_accounting(event, [{'after': 2}], 3, 3, 2, 1)


if __name__ == '__main__':
    unittest.main(verbosity=2)
