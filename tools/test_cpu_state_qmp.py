"""Mutation regressions for independent FP/ownership/TLS acceptance."""
import struct
import unittest

from cpu_state_qmp import (LAYOUT, SLOT, MASK64, TLS, THREADS, NODES, SPACES,
                           initial_image, decode_image, validate_slot,
                           seed_state, fp_equal, tls_result, normalized_mask,
                           register_state, extended_integer, exception_gate)


def require(ok, message):
    if not ok:
        raise AssertionError(message)


def slot_image(cpu=MASK64):
    row = bytearray(SLOT)
    row[:512] = initial_image(0xFFFF)
    struct.pack_into('<4Q6Q', row, 512, THREADS[0], NODES[0], 0x2000, SPACES[0],
                     TLS[0], 2, 2+(cpu != MASK64), 1, cpu, 0)
    return row


class CpuStateOracleTests(unittest.TestCase):
    def test_full_width_tls_errors_preserve_the_previous_base(self):
        for base in (0x800000000000, 0xFFFF800000000000, MASK64):
            self.assertEqual(tls_result(0x29, [base, 0, 0, 0, 0, 0], TLS[0]),
                             ((MASK64-1, 0), TLS[0]))
        for op in (0x29, 0x2A):
            for index in range(1 if op == 0x29 else 0, 6):
                args = [TLS[0] if op == 0x29 else 0]+[0]*5
                args[index] = 1 << 63
                self.assertEqual(tls_result(op, args, TLS[0]), ((MASK64-1, 0), TLS[0]))
        self.assertEqual(tls_result(0x29, [0x7FFFFFFFFFFF]+[0]*5, 0), ((0, 0), 0x7FFFFFFFFFFF))
        self.assertEqual(tls_result(0x2A, [0]*6, TLS[2]), ((0, TLS[2]), TLS[2]))

    def test_mask_fallback_and_reserved_mxcsr_bits(self):
        self.assertEqual(normalized_mask(0, require), 0xFFBF)
        for mask in (1, 0x10000, MASK64):
            with self.assertRaises(AssertionError):
                normalized_mask(mask, require)
        row = bytearray(initial_image(0xFFFF))
        struct.pack_into('<I', row, 24, 0x10000)
        with self.assertRaisesRegex(AssertionError, 'reserved MXCSR'):
            decode_image(row, 0xFFFF, require)

    def test_uninitialized_reserved_bytes_cannot_pass_an_image_check(self):
        for offset in (5, 42, 158, 416, 511):
            image = bytearray(initial_image(0xFFFF))
            image[offset] = 1
            with self.subTest(offset=offset), self.assertRaisesRegex(AssertionError, 'reserved FXSAVE'):
                decode_image(image, 0xFFFF, require)

    def test_owner_identity_and_balanced_save_load_are_not_guest_scalars(self):
        row = slot_image(1)
        result = validate_slot(row, 0, [0x2000, 0x3000], 0xFFFF, 2, require)
        self.assertEqual(result['cpu'], 1)
        for offset in (512, 520, 528, 536, 552, 560, 568, 576, 584):
            changed = row[:]
            struct.pack_into('<Q', changed, offset, struct.unpack_from('<Q', changed, offset)[0]+1)
            with self.subTest(offset=offset), self.assertRaises(AssertionError):
                validate_slot(changed, 0, [0x2000, 0x3000], 0xFFFF, 2, require)

    def test_retirement_requires_actual_zero_bytes_and_no_live_cpu(self):
        row = bytearray(SLOT)
        struct.pack_into('<QQ', row, 576, MASK64, 1)
        validate_slot(row, 0, [0, 0], 0xFFFF, 2, require, clean=True)
        for offset in (0, 160, 415, 512, 544, 552, 560, 568, 576):
            changed = row[:]
            changed[offset] ^= 1
            with self.subTest(offset=offset), self.assertRaisesRegex(AssertionError, 'retired CPU state'):
                validate_slot(changed, 0, [0, 0], 0xFFFF, 2, require, clean=True)

    def test_new_thread_cannot_inherit_nonzero_fp_or_tls(self):
        row = slot_image()
        with self.assertRaisesRegex(AssertionError, 'inherited'):
            validate_slot(row, 0, [0x2000, 0x3000], 0xFFFF, 2, require, initial=True)
        struct.pack_into('<3Q', row, 544, 0, 0, 0)
        validate_slot(row, 0, [0x2000, 0x3000], 0xFFFF, 2, require, initial=True)
        row[160] = 1
        with self.assertRaisesRegex(AssertionError, 'inherited'):
            validate_slot(row, 0, [0x2000, 0x3000], 0xFFFF, 2, require, initial=True)

    def test_all_sixteen_xmm_and_all_eight_x87_values_are_distinct_and_checked(self):
        expected = seed_state(3)
        self.assertEqual(len(set(expected['xmm'])), 16)
        self.assertEqual(len(set(expected['st'])), 8)
        for bank in ('st', 'xmm'):
            for index in range(len(expected[bank])):
                changed = {**expected, bank: expected[bank][:]}
                changed[bank][index] = '00'*len(bytes.fromhex(changed[bank][index]))
                with self.subTest(bank=bank, index=index), self.assertRaisesRegex(AssertionError, 'FP register bank'):
                    fp_equal(changed, expected, require)
        self.assertEqual(extended_integer(1), '0000000000000080ff3f')
        self.assertEqual(seed_state(1, added=True)['st'][0], extended_integer(209))
        self.assertNotEqual(seed_state(1, toggled=True)['xmm'][0], seed_state(1)['xmm'][0])

    def test_live_register_parser_requires_full_fp_bank_and_fs_base(self):
        text = 'FCW=037f FSW=0000 FTW=00 MXCSR=00001f80 CR4=00000620\nFS =0000 0000000000402200\n'
        text += ' '.join(f'FPR{i}=0000000000000000 0000' for i in range(8))+'\n'
        text += ' '.join(f'XMM{i:02}=0000000000000000 0000000000000000' for i in range(16))
        self.assertEqual(register_state(text, require)['fs_base'], TLS[0])
        for original in ('XMM15=', 'FPR7=', 'FS =', 'CR4='):
            with self.subTest(original=original), self.assertRaises(AssertionError):
                register_state(text.replace(original, 'MISSING='), require)

    def test_sse_exception_gate_is_kernel_only_and_names_the_actual_stub(self):
        raw = bytearray.fromhex('34120800008e00007856341200000000')
        target = 0x1234567800001234
        exception_gate(raw, target, require)
        for offset in (0, 2, 4, 5, 6, 8, 12):
            changed = raw[:]
            changed[offset] ^= 1
            with self.subTest(offset=offset), self.assertRaisesRegex(AssertionError, 'exception gate'):
                exception_gate(changed, target, require)

    def test_a_forged_xm_target_or_user_callable_gate_cannot_pass(self):
        raw = bytearray.fromhex('34120800008e00007856341200000000')
        for target in (0, 0x1234567800001235):
            with self.assertRaises(AssertionError):
                exception_gate(raw, target, require)
        raw[5] = 0xEE
        with self.assertRaises(AssertionError):
            exception_gate(raw, 0x1234567800001234, require)


if __name__ == '__main__':
    unittest.main()
