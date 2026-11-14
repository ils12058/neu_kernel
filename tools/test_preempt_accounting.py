"""Regression checks for the user/kernel timer ledger in the QMP oracle."""

import unittest
from unittest.mock import Mock, patch

from neu import Failure, check_preempt_tick_accounting, check_wait_tick_accounting, preempt_drain_records
import smp_scheduler_qmp as smp
from smp_scheduler_qmp import observation_delay


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


class WaitAccountingTests(unittest.TestCase):
    def test_quiesced_waits_allow_zero_ticks_or_the_captured_kernel_only_drain(self):
        # K10's retained second full attempt: all three Threads blocked with
        # zero user IRQs/captures; tick 1 drained after A1 restored root 0x1000.
        for drains, count in [([], 0), ([(0, 1, 1, 0x1000)], 1)]:
            with self.subTest(drains=drains):
                check_wait_tick_accounting([], drains, count, count, 0, count,
                                           {1: 0, 2: 0, 3: 0}, 0x1000, True)

    def test_each_blocked_native_slice_can_drain_its_own_one_shot(self):
        drains = [(0, 1, 1, 0x1000), (1, 2, 2, 0x1000), (2, 3, 3, 0x1000)]
        check_wait_tick_accounting([], drains, 3, 3, 0, 3,
                                   {1: 0, 2: 0, 3: 0}, 0x1000, True)
        with self.assertRaises(Failure):
            check_wait_tick_accounting([], drains + [(3, 4, 4, 0x1000)], 4, 4, 0, 4,
                                       {1: 0, 2: 0, 3: 0}, 0x1000, True)

    def test_quiescence_rejects_user_irqs_captures_and_counter_mismatches(self):
        for events, captures, counters in [
                ([{'tick': 1}], {1: 1, 2: 0, 3: 0}, (1, 1, 1, 0)),
                ([], {1: 0, 2: 1, 3: 0}, (0, 0, 0, 0)),
                ([], {1: 0, 2: 0, 3: 0}, (1, 0, 0, 1)),
                ([], {1: 0, 2: 0, 3: 0}, (1, 1, 0, 0)),
                ([], {1: 0, 2: 0, 3: 0}, (2, 2, 0, 2))]:
            with self.subTest(events=events, captures=captures, counters=counters), self.assertRaises(Failure):
                check_wait_tick_accounting(events, [], *counters, captures, 0x1000, True)

    def test_quiescence_rejects_foreign_replayed_and_unaccounted_drains(self):
        for drains, total in [
                ([(0, 1, 1, 0xc000)], 1),
                ([(0, 1, 0, 0x1000)], 1),
                ([(0, 2, 2, 0x1000)], 2),
                ([(0, 1, 1, 0x1000), (0, 1, 1, 0x1000)], 2),
                ([(1, 2, 2, 0x1000)], 2)]:
            with self.subTest(drains=drains), self.assertRaises(Failure):
                check_wait_tick_accounting([], drains, total, total, 0, total,
                                           {1: 0, 2: 0, 3: 0}, 0x1000, True)


class SchedulerObservationTests(unittest.TestCase):
    def test_pending_overlap_samples_both_threads_in_a_two_quantum_cycle(self):
        # The captured K10 attempt missed A1/A2 overlap while CPU0 alternated
        # A1/B1 on 10 ms quanta. Fixed 20 ms polling can see only B1 forever.
        self.assertEqual({((15 + 20 * i) % 20) // 10 for i in range(12)}, {1})
        for initial in range(20):
            phase = initial
            seen = set()
            for attempt in range(12):
                phase += round(observation_delay(attempt, False) * 1000)
                seen.add((phase % 20) // 10)
            with self.subTest(initial=initial):
                self.assertEqual(seen, {0, 1})

    def test_witness_completion_restores_the_existing_poll_interval(self):
        for attempt in range(12):
            self.assertEqual(observation_delay(attempt, True), 0.02)
            self.assertGreater(observation_delay(attempt, False), 0)
            self.assertLess(observation_delay(attempt, False), 0.02)


class SchedulerCheckpointTests(unittest.TestCase):
    def observer(self, phase):
        symbols = {'neu_smp_sched_probe_begin': 0x1000, 'neu_smp_sched_probe_end': 0x1100,
                   'neu_smp_sched_cpus': 0x2000, 'neu_smp_cpus': 0x4000}
        observer = smp.Observer(None, 'smp-schedule-2', symbols, 0, Failure, [])
        evidence = [0] * 64
        evidence[:3] = [smp.MAGIC, phase, 2]
        arrays = {'neu_smp_sched_layout': smp.LAYOUT, 'neu_smp_sched_evidence': evidence,
                  'neu_smp_sched_inventory': [0] * 64}
        observer.array = lambda _memory, name, _count: arrays[name]
        observer.apics = {0: 0, 1: 1}
        observer.baseline = None if phase == 0 else {}
        observer.inventory = Mock(side_effect=Failure('invalid actual inventory'))
        observer.scheduler = Mock(return_value={'threads': []})
        observer.authority = Mock(return_value={})
        memory = Mock()
        memory.byte.return_value = memory.u64.return_value = 0

        def words(address, count):
            row = [0] * count
            if address in (0x4000, 0x4000 + 384):
                row[1] = row[3] = (address - 0x4000) // 384
            return row

        memory.words.side_effect = words
        return observer, memory

    def snapshot(self, observer, memory, rip):
        with patch.object(smp, 'VirtualMemory', return_value=memory), \
                patch.object(smp, 'PhysicalMemory'), \
                patch.object(smp, '_user_registers', return_value={'rip': rip}):
            return observer.snapshot()

    def test_published_checkpoint_waits_for_actual_rip_without_accepting_inventory(self):
        for phase in (0, 1):
            for rip in (0x0fff, 0x1100):
                observer, memory = self.observer(phase)
                with self.subTest(phase=phase, rip=rip):
                    self.assertIsNone(self.snapshot(observer, memory, rip))
                    observer.inventory.assert_not_called()
                    observer.scheduler.assert_not_called()
                    self.assertEqual(observer.output, [])
                    self.assertIsNone(observer.prepared)

    def test_entered_checkpoint_still_runs_and_rejects_invalid_inventory(self):
        for phase in (0, 1):
            observer, memory = self.observer(phase)
            self.assertIsNone(self.snapshot(observer, memory, 0x0fff))
            with self.subTest(phase=phase), self.assertRaisesRegex(Failure, 'invalid actual inventory'):
                self.snapshot(observer, memory, 0x1000)
            observer.inventory.assert_called_once()
            self.assertEqual(observer.output, [])
            self.assertIsNone(observer.prepared)

    def test_later_phase_cannot_replace_a_missing_baseline_or_prepared_checkpoint(self):
        for phase in (1, 2, 3, 4, 5):
            observer, memory = self.observer(phase)
            observer.baseline = None
            with self.subTest(phase=phase), self.assertRaisesRegex(Failure, 'independent baseline'):
                self.snapshot(observer, memory, 0x1000)
        for phase in (2, 3, 4):
            observer, memory = self.observer(phase)
            with self.subTest(phase=phase), self.assertRaisesRegex(Failure, 'unobserved prepared checkpoint'):
                self.snapshot(observer, memory, 0x1000)


if __name__ == '__main__':
    unittest.main(verbosity=2)
