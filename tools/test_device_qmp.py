"""Adversarial K11 acceptance regressions; no dependency on the guest policy."""
import unittest

from device_qmp import (DeviceModel, Extension, ARMED, IDENT, PCI_ID, MMIO,
                        register_policy, validate_hardware, validate_checkpoints, validate_scrub)
from smp_scheduler_qmp import NODES, THREADS, MASK64, validate_trace_counts


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def initial():
    def h(owner):
        return dict(slot=1, generation=2, kind=6, target=601,
                    rights=0x314, grant=owner+1, transferable=0, lease=0)
    manifest = [0]*8
    manifest[5] = (2 << 32) | 2
    return dict(handles=[[h(0)], [h(1)]], generations=[[1, 2, 1, 1, 1, 1, 1, 1]]*2,
                bootstrap=[manifest, manifest], grants=[dict(id=1, parent=0), dict(id=2, parent=0)])


class DeviceOracleTests(unittest.TestCase):
    def setUp(self):
        self.model = DeviceModel(initial(), require)
        self.entry = 0
        self.resource = (2 << 32) | 2

    def operation(self, op, args=(), status=0, scalar=0, owner=0):
        self.entry += 1
        row = [0]*32
        row[0] = self.entry
        row[2:7] = [owner, NODES[owner], THREADS[owner], self.entry, op]
        row[7:13] = list(args) + [0]*(6-len(args))
        row[13:15] = [status & MASK64, scalar]
        self.model.operation(row)
        return row

    def claim(self):
        mmio, irq = (2 << 32) | 1, (2 << 32) | 3
        self.operation(0x1C, [self.resource], scalar=mmio)
        self.model.phase = ARMED
        return mmio, irq

    def test_register_oracle_excludes_dma_ack_invalid_width_and_truncation(self):
        for offset in (0x64, 0x80, 0x88, 0x90, 0x98, 0x100000, 1 << 63):
            for width in (1, 2, 4, 8):
                self.assertIsNone(register_policy(offset, width, 1))
        for width in (0, 1, 2, 8, 1 << 32):
            self.assertIsNone(register_policy(0, width))
        self.assertIsNone(register_policy(4, 4, 1 << 32))
        self.assertIsNone(register_policy(8, 4, 13))
        self.assertEqual(register_policy(8, 4, 6), 1)
        self.assertIsNone(register_policy(0x28, 4, 6))

    def test_exclusive_claim_and_foreign_handle_are_independent(self):
        mmio, _ = self.claim()
        self.operation(0x1C, [self.resource], owner=1, status=-40)
        self.operation(0x1E, [mmio, 0, 4], owner=1, status=-17)
        self.assertEqual(self.model.claims, 1)
        self.assertEqual((self.model.reads, self.model.writes), (0, 0))

    def test_fabricated_success_for_dma_or_wrong_capability_is_rejected(self):
        mmio, _ = self.claim()
        with self.assertRaisesRegex(AssertionError, 'independent model'):
            self.operation(0x1E, [mmio, 0x80, 4], scalar=0)
        self.setUp()
        self.claim()
        with self.assertRaisesRegex(AssertionError, 'independent model'):
            self.operation(0x1F, [self.resource, 4, 4, 123])

    def test_denied_register_calls_do_not_consume_budget(self):
        mmio, _ = self.claim()
        for args in ([mmio, 0x80, 4], [mmio, 4, 8], [mmio, 1, 4]):
            self.operation(0x1E, args, status=-45)
        self.operation(0x1F, [mmio, 4, 4, 1 << 32], status=-45)
        self.operation(0x1E, [mmio, 0, 4, 0, 0, 1], status=-2)
        self.assertEqual(self.model.remaining, 64)
        self.operation(0x1E, [mmio, 0, 4], scalar=IDENT)
        self.assertEqual(self.model.remaining, 63)

    def test_incorrect_hardware_read_result_is_rejected(self):
        mmio, _ = self.claim()
        self.operation(0x1F, [mmio, 4, 4, 0x12345678])
        with self.assertRaisesRegex(AssertionError, 'independent model'):
            self.operation(0x1E, [mmio, 4, 4], scalar=0x12345678)

    def test_irq_rights_attenuation_and_strict_descendant_revocation(self):
        _, irq = self.claim()
        child = (2 << 32) | 4
        self.operation(0x19, [irq, 1], scalar=child)
        self.operation(0x21, [child, 1], status=-18)
        self.operation(0x19, [child, 2], status=-18)
        self.operation(0x1A, [irq], scalar=1)
        self.assertIn(irq, self.model.tables[0])
        self.assertNotIn(child, self.model.tables[0])

    def test_raw_state_and_budget_tampering_are_rejected(self):
        row = [0]*32
        row[:7] = [1, 0, 0, NODES[0], THREADS[0], 1, 1]
        row[14] = 0x7000003FF
        row[16] = 1
        self.model.process([row])
        for field in (15, 16, 17, 18, 20, 22, 23, 26, 27, 28, 29, 30):
            model = DeviceModel(initial(), require)
            modified = list(row)
            modified[field] += 1
            with self.assertRaises(AssertionError):
                model.process([modified])

    def test_actual_native_frame_arguments_and_results_must_match(self):
        row = self.operation(1, scalar=0x7000003FF)
        class Observer:
            require = staticmethod(require)
        extension = Extension(Observer())
        extension.model = self.model
        before, after = [0]*20, [0]*20
        before[14], after[11] = 1, 0x7000003FF
        extension.native_result(before, after, 0, 0, row[5])
        after[11] ^= 1
        with self.assertRaisesRegex(AssertionError, 'actual entry/resumed frames'):
            extension.native_result(before, after, 0, 0, row[5])
        with self.assertRaisesRegex(AssertionError, 'omitted or fabricated'):
            extension.complete_traces([[]])

    def test_extended_tls_journal_requires_real_frames_and_continuations(self):
        self.model.tls = [0] * len(THREADS)
        calls = [self.operation(0x29, [0x402200]),
                 self.operation(0x2A, scalar=0x402200)]
        class Observer:
            require = staticmethod(require)
        extension = Extension(Observer())
        extension.model = self.model
        traces = []
        for row in calls:
            before, after = [0]*20, [0]*20
            before[14] = row[6]
            for index, value in zip((8, 9, 11, 5, 7, 6), row[7:13]):
                before[index] = value
            after[14], after[11] = row[13:15]
            extension.native_result(before, after, 0, 0, row[5])
            trace = [0]*40
            trace[0], trace[4], trace[10:30] = row[5], 128, before
            traces.append(trace)
        extension.complete_traces([traces])
        with self.assertRaisesRegex(AssertionError, 'omitted or fabricated'):
            extension.complete_traces([traces[1:]])
        traces[0][18] ^= 1
        with self.assertRaisesRegex(AssertionError, 'omitted or fabricated'):
            extension.complete_traces([traces])
        traces[0][18] ^= 1
        extension.matched.remove((0, calls[1][5]))
        with self.assertRaisesRegex(AssertionError, 'lacks observed continuation'):
            extension.complete_traces([traces])

    def test_tls_journal_rejects_foreign_results_reserved_args_and_legacy_exposure(self):
        self.model.tls = [0] * len(THREADS)
        self.operation(0x29, [0x402200])
        self.operation(0x29, [0x7FFFFFFFFFFF, 1 << 63], status=-2)
        self.operation(0x2A, scalar=0x402200)
        self.operation(0x2A, owner=1)
        with self.assertRaisesRegex(AssertionError, 'independent model'):
            self.operation(0x2A, owner=1, scalar=0x402200)
        self.model.tls = None
        with self.assertRaisesRegex(AssertionError, 'unexpected operation'):
            self.operation(0x29, [0x402200])


class HardwareOracleTests(unittest.TestCase):
    def setUp(self):
        self.config = [0]*17
        self.config[0] = PCI_ID
        self.config[1] = 0x100002
        self.config[4] = 0x81000000
        self.config[15] = 0x10A
        self.config[16] = 0x800005
        self.regs = {0: IDENT}
        self.dma = [0]*4
        self.route = 0x1A0F0

    def check(self):
        validate_hardware(self.config, self.regs, self.dma, self.route, 0, 0x81000000, False, require)

    def test_enabled_bus_master_msi_dma_or_wrong_route_is_rejected(self):
        self.check()
        for field, bit in ((1, 4), (1, 1), (1, 0x400), (16, 0x10000)):
            self.setUp()
            self.config[field] |= bit
            with self.assertRaises(AssertionError):
                self.check()
        for field in range(4):
            self.setUp()
            self.dma[field] = 1 << 32
            with self.assertRaisesRegex(AssertionError, 'DMA'):
                self.check()
        for bit in (1, 0x2000, 0x8000, 1 << 56):
            self.setUp()
            self.route ^= bit
            with self.assertRaisesRegex(AssertionError, 'I/O APIC'):
                self.check()

    def test_eight_byte_edu_read_cannot_be_mistaken_for_identification(self):
        self.regs[0] = 0xFFFFFFFF
        with self.assertRaisesRegex(AssertionError, '32-bit'):
            self.check()

    def test_missing_lifecycle_or_second_blocked_window_cannot_pass(self):
        for case, keys in (
            ('smp-device-2', [('checkpoint', 1, 1), ('checkpoint', 2), ('checkpoint', 3)]),
            ('smp-device-4', [('checkpoint', 1, 1), ('checkpoint', 2), ('checkpoint', 3)]),
            ('smp-device-close', [('checkpoint', 1, 1), ('checkpoint', 1, 2), ('checkpoint', 3)]),
            ('smp-device-revoke', [('checkpoint', 1, 1), ('checkpoint', 1, 2), ('checkpoint', 3)]),
        ):
            keys += [('scrub', 1), ('scrub', 2)]
            complete = dict.fromkeys(keys, True)
            validate_checkpoints(case, complete, require)
            for key in keys:
                incomplete = complete.copy()
                del incomplete[key]
                with self.assertRaisesRegex(AssertionError, 'missing independent'):
                    validate_checkpoints(case, incomplete, require)

    def test_computing_clear_alone_is_not_a_completed_neutral_reset(self):
        state = [6, 1] + [0]*22
        hardware = dict(registers={4: 0, 8: 1, 0x20: 0x80, 0x24: 1}, route=0x1A0F0)
        validate_scrub(state, hardware, [0, 0], 1, require)
        for offset, value in ((4, 123), (8, 0), (0x20, 0), (0x20, 0x81), (0x24, 0), (0x24, 2)):
            changed = dict(hardware, registers=dict(hardware['registers']))
            changed['registers'][offset] = value
            with self.assertRaisesRegex(AssertionError, 'actual masked completion'):
                validate_scrub(state, changed, [0, 0], 1, require)
        for route, mailbox, epoch in ((0xA0F0, [0, 0], 1), (0x1E0F0, [0, 0], 1),
                                       (0x1A0F0, [1, 1], 1), (0x1A0F0, [0, 0], 2)):
            with self.assertRaisesRegex(AssertionError, 'actual masked completion'):
                validate_scrub(state, dict(hardware, route=route), mailbox, epoch, require)

    def test_trace_counts_cannot_cross_either_fixed_allocation(self):
        for limit in (256, 512):
            rows = [[0]*64 for _ in range(4)]
            rows[0][11] = rows[3][25] = limit
            validate_trace_counts(rows, limit, require)
            for cpu, field in ((0, 11), (3, 25)):
                changed = [list(row) for row in rows]
                changed[cpu][field] = limit + 1
                with self.assertRaisesRegex(AssertionError, 'fixed allocation'):
                    validate_trace_counts(changed, limit, require)


if __name__ == '__main__':
    unittest.main()
