"""Mutation tests for the independent DMA acceptance oracle."""
import copy
import unittest

from dma_qmp import DmaModel, LeaseModel, acpi_table, invalidations, walk_domain, IOVA
from smp_scheduler_qmp import MASK64, NODES, THREADS
from device_qmp import ARMED
from test_device_qmp import initial, require


class DmaOracleTests(unittest.TestCase):
    def setUp(self):
        self.model, self.entry = DmaModel(initial(), require), 0
        self.resource = (2 << 32) | 2
        self.mmio, self.domain, self.buffer = [(2 << 32) | s for s in (1, 4, 5)]
        self.operation(0x1C, [self.resource], scalar=self.mmio)
        self.model.phase = ARMED
        self.operation(0x22, [self.mmio], scalar=self.domain)
        self.operation(0x23, [self.domain], scalar=self.buffer)

    def operation(self, op, args=(), status=0, scalar=0, owner=0):
        self.entry += 1
        row = [0]*32
        row[0] = self.entry
        row[2:7] = [owner, NODES[owner], THREADS[owner], self.entry, op]
        row[7:13] = list(args)+[0]*(6-len(args))
        row[13:15] = [status & MASK64, scalar]
        self.model.operation(row)

    def map(self, permissions=3):
        self.operation(0x24, [self.domain, self.buffer, permissions], scalar=IOVA)

    def test_wrong_type_owner_reserved_arguments_and_attenuated_allocation(self):
        self.operation(0x22, [self.buffer], status=-19)
        self.operation(0x23, [self.domain], owner=1, status=-17)
        self.operation(0x24, [self.domain, self.buffer, 3, 0, 0, 1], status=-2)
        child = (2 << 32) | 6
        self.operation(0x19, [self.domain, 4], scalar=child)
        self.operation(0x23, [child], status=-18)
        self.assertEqual(self.model.permissions, 0)

    def test_out_of_bounds_overflow_and_directions_never_submit(self):
        self.map()
        for offset, length in ((0, 0), (0, 4097), (8192, 1), (8191, 2), (MASK64, 1)):
            self.operation(0x28, [self.domain, self.buffer, offset, length, 1], status=-49)
        for permission in (0, 3, 4, MASK64):
            self.operation(0x28, [self.domain, self.buffer, 0, 8, permission], status=-50)
        self.assertEqual(self.model.submits, 0)
        self.assertIsNone(self.model.transfer)

    def test_readonly_mapping_and_buffer_cannot_amplify_write_authority(self):
        self.map(1)
        self.operation(0x28, [self.domain, self.buffer, 0, 8, 2], status=-50)
        child = (2 << 32) | 6
        self.operation(0x19, [self.buffer, 1], scalar=child)
        self.operation(0x27, [child, 0, 123], status=-18)
        self.operation(0x24, [self.domain, child, 1], status=-18)
        self.operation(0x19, [child, 7], status=-18)

    def test_in_flight_unmap_cpu_write_and_second_submit_are_denied(self):
        self.map()
        self.operation(0x28, [self.domain, self.buffer, 4095, 2, 1], scalar=1)
        self.operation(0x25, [self.domain, self.buffer], status=-52)
        self.operation(0x27, [self.buffer, 0, 1], status=-52)
        self.operation(0x28, [self.domain, self.buffer, 0, 8, 1], status=-52)
        self.assertEqual(self.model.transfer, (4095, 2, 1))

    def test_cross_page_roundtrip_is_computed_from_bytes(self):
        self.map()
        for offset, value in ((4088, 0x1020304050607080), (4096, 0xFFEEDDCCBBAA9988)):
            self.operation(0x27, [self.buffer, offset, value])
        self.operation(0x28, [self.domain, self.buffer, 4093, 6, 1], scalar=1)
        self.model.complete_dma()
        self.model.phase = ARMED
        self.operation(0x27, [self.buffer, 4088, 0])
        self.operation(0x27, [self.buffer, 4096, 0])
        self.operation(0x28, [self.domain, self.buffer, 4093, 6, 2], scalar=2)
        self.model.complete_dma()
        self.operation(0x26, [self.buffer, 4088], scalar=0x1020300000000000)
        self.operation(0x26, [self.buffer, 4096], scalar=0xAA9988)
        with self.assertRaisesRegex(AssertionError, 'native model mismatch'):
            self.operation(0x26, [self.buffer, 4096], scalar=0)

    def test_forged_dma_success_and_unmapped_submit_are_rejected(self):
        self.operation(0x28, [self.domain, self.buffer, 0, 8, 1], status=-50)
        with self.assertRaisesRegex(AssertionError, 'native model mismatch'):
            self.operation(0x28, [self.domain, self.buffer, 0, 8, 1], scalar=1)


class DmaHardwareOracleTests(unittest.TestCase):
    def test_acpi_checksum_signature_and_declared_extent(self):
        table = bytearray(36)
        table[:4], table[4:8] = b'DMAR', (36).to_bytes(4, 'little')
        table[9] = -sum(table) % 256
        acpi_table(table, b'DMAR', require)
        for offset in (0, 4, 9, 35):
            changed = table[:]
            changed[offset] ^= 1
            with self.assertRaises(AssertionError):
                acpi_table(changed, b'DMAR', require)

    def test_invalidation_request_busy_and_completion_granularity(self):
        row = [1, 0, 1, 1, 0xA000000000000000, 0x2800000000000000,
               0x9000000000000000, 0x1200000000000000]
        invalidations([row], require)
        for field, bit in ((0, 1), (4, 1 << 61), (5, 1 << 63), (5, 1 << 59),
                           (6, 1 << 60), (7, 1 << 63), (7, 1 << 57)):
            changed = row[:]
            changed[field] ^= bit
            with self.assertRaisesRegex(AssertionError, 'invalidation'):
                invalidations([changed], require)

    def test_raw_context_and_page_walk_reject_requester_alias_and_pass_through(self):
        pages = {p: [0]*512 for p in (0x1000, 0x2000, 0x3000, 0x4000, 0x5000)}
        pages[0x1000][0] = 0x2001
        pages[0x2000][80:82] = [0x3001, 0x101]
        pages[0x3000][0], pages[0x4000][0] = 0x4003, 0x5003
        pages[0x5000][256:258] = [0x200003, 0x202003]

        class Memory:
            def words(self, address, count):
                require(count == 512 and address in self.pages, 'foreign table page')
                return self.pages[address]
        memory = Memory()
        memory.pages = pages
        self.assertEqual(walk_domain(memory, 0x1000, 40, require)['leaves'],
                         {IOVA: 0x200003, IOVA+4096: 0x202003})
        for page, index, value in ((0x1000, 2, 0x2001), (0x2000, 82, 0x3001),
                                   (0x2000, 80, 0x3009), (0x4000, 0, 0x3003),
                                   (0x5000, 256, 0x200083)):
            memory.pages = copy.deepcopy(pages)
            memory.pages[page][index] = value
            with self.assertRaises(AssertionError):
                walk_domain(memory, 0x1000, 40, require)

    def test_dma_lifetime_rejects_state_tampering_and_release_without_scrub(self):
        boot = [1, 1]+[0]*18
        claim = [2, 2, NODES[0], 1]+[0]*16
        lease = LeaseModel(require)
        lease.process([boot, claim])
        release = [3, 9, 0, 1]+[0]*16
        with self.assertRaises(AssertionError):
            lease.process([boot, claim, release])
        for index in range(4, 14):
            changed = claim[:]
            changed[index] = 1
            with self.assertRaises(AssertionError):
                LeaseModel(require).process([boot, changed])


if __name__ == '__main__':
    unittest.main()
