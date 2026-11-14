"""Adversarial regressions for the independently implemented K10 QMP oracle."""
import copy
import unittest

from authority_qmp import (AuthorityModel, Extension, validate_lineage,
                           validate_mapping_bindings, validate_invalidation_window)
from smp_scheduler_qmp import MASK64, NODES, THREADS


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def initial(mapped=False):
    def handle(slot, grant, rights=0x30f, kind=5, target=123):
        return dict(slot=slot, generation=1, grant=grant, rights=rights,
                    kind=kind, target=target, transferable=True)
    grants = [dict(id=1, parent=0, object=123, rights=0x30f),
              dict(id=3, parent=0, object=123, rights=0x30f)]
    tables = [[handle(0, 1), handle(2, 0, 224, 4, 401)],
              [handle(0, 3), handle(2, 0, 224, 4, 402)]]
    mappings = []
    if mapped:
        grants.append(dict(id=2, parent=1, object=123, rights=5))
        tables[1].append(handle(1, 2, 5))
        mappings.append(dict(grant=2))
    return dict(handles=tables, grants=grants, mappings=mappings,
                generations=[[1, 0, 1, 0, 0, 0, 0, 0], [1, int(mapped), 1, 0, 0, 0, 0, 0]])


class AuthorityModelTests(unittest.TestCase):
    def setUp(self):
        self.model = AuthorityModel(initial(), require)
        self.sequence = 0
        self.root = (1 << 32) | 1
        self.child = (1 << 32) | 2
        self.endpoint = (1 << 32) | 3

    def operation(self, op, handle=0, rights=0, status=0, scalar=0, owner=0,
                  phase=0, entry=None, extra=(0, 0, 0, 0)):
        self.sequence += 1
        row = [self.sequence, phase, owner, NODES[owner], THREADS[owner], op,
               handle, rights, status & MASK64, scalar,
               entry or self.sequence, 0x10000 + owner*0x10000, *extra]
        self.model.operation(row)
        return row

    def test_attenuation_close_and_rederive_use_fresh_handle_generations(self):
        self.operation(0x19, self.root, 0x105, scalar=self.child)
        self.operation(0x19, self.child, 2, status=-18)
        self.operation(0x11, self.child)
        self.operation(0x19, self.root, 5, scalar=(2 << 32) | 2)
        self.operation(0x19, self.child, 1, status=-17)

    def test_tampered_success_or_handle_is_rejected(self):
        self.operation(0x19, self.root, 0x105, scalar=self.child)
        with self.assertRaisesRegex(AssertionError, 'independent model'):
            self.operation(0x19, self.child, 2, scalar=99)
        self.setUp()
        with self.assertRaisesRegex(AssertionError, 'independent model'):
            self.operation(0x19, self.root, 1, scalar=(1 << 32) | 8)

    def test_every_reserved_argument_and_unknown_right_is_rejected(self):
        for index in range(4):
            extra = [0]*4; extra[index] = 1 << 63
            self.operation(0x19, self.root, 1, status=-2, extra=extra)
            self.operation(0x1a, self.root, status=-2, extra=extra)
        self.operation(0x1a, self.root, 1, status=-2)
        for rights in (0, 0x400, 1 << 63):
            self.operation(0x19, self.root, rights, status=-18)
        self.assertEqual(len(self.model.tables[0]), 2)

    def test_queued_descendant_is_discarded_and_source_can_derive_again(self):
        self.operation(0x19, self.root, 13, scalar=self.child)
        self.operation(0x17, self.endpoint, self.child)
        self.operation(0x1a, self.root, scalar=1)
        self.operation(0x18, self.endpoint, owner=1, status=-32)
        self.assertEqual(self.model.discarded, [0, 1])
        self.assertIn(self.root, self.model.tables[1])
        self.operation(0x19, self.root, 5, scalar=(2 << 32) | 2)

    def test_mapping_revoke_has_one_matching_asynchronous_completion(self):
        self.model = AuthorityModel(initial(mapped=True), require)
        pending = self.operation(0x1a, self.root, status=-100)
        self.assertEqual(self.model.maps, [2])
        self.assertIsNotNone(self.model.pending)
        self.operation(0x19, self.root, 5, status=-23)
        self.operation(0x1a, self.root, phase=1, entry=pending[10], scalar=1)
        self.assertEqual(self.model.maps, [])
        self.assertIn(self.root, self.model.tables[0])
        self.assertIn(self.root, self.model.tables[1])
        with self.assertRaisesRegex(AssertionError, 'no matching outstanding revoke'):
            self.operation(0x1a, self.root, phase=1, entry=pending[10], scalar=1)

    def test_foreign_completion_cannot_release_mapping_ownership(self):
        self.model = AuthorityModel(initial(mapped=True), require)
        pending = self.operation(0x1a, self.root, status=-100)
        with self.assertRaisesRegex(AssertionError, 'no matching outstanding revoke'):
            self.operation(0x1a, self.root, phase=1, entry=pending[10], scalar=1, owner=1)
        self.assertEqual(self.model.maps, [2])

    def test_queued_wait_resumes_revoked_with_zero_scalar(self):
        setup = initial()
        setup['handles'][0][1].update(grant=4, rights=0x3e0)
        setup['grants'].append(dict(id=4, parent=0, object=401, rights=0x3e0))
        self.model = AuthorityModel(setup, require)
        self.operation(0x19, self.endpoint, 64, scalar=self.child)
        wait = self.operation(0x16, self.child, status=-100)
        self.operation(0x1a, self.endpoint, scalar=1)
        self.assertEqual(self.model.results[(wait[2], wait[10])], (-38 & MASK64, 0))
        self.assertEqual(self.model.waits, {})

    def test_exhausted_generations_leave_grants_and_handles_unchanged(self):
        self.model.generations[0] = [0xffffffff]*8
        before = copy.deepcopy((self.model.tables, self.model.forest))
        self.operation(0x19, self.root, 5, status=-22)
        self.assertEqual((self.model.tables, self.model.forest), before)


class ProductionEvidenceTests(unittest.TestCase):
    def test_lineage_rejects_amplification_cycles_stale_owners_and_wrong_counts(self):
        grants = [dict(id=1, parent=0, object=123, rights=0x30f, uses=[1, 0, 0, 0], revoked=0),
                  dict(id=2, parent=1, object=123, rights=5, uses=[0, 0, 0, 1], revoked=0)]
        owners = {1: [1, 0, 0, 0], 2: [0, 0, 0, 1]}
        handles = [[dict(grant=1, target=123, rights=0x30f)]]
        validate_lineage(grants, owners, handles, 0, require)
        for field, value in [('rights', 0x400), ('object', 999), ('parent', 2), ('uses', [0]*4)]:
            bad = copy.deepcopy(grants); bad[1][field] = value
            with self.subTest(field=field), self.assertRaises(AssertionError):
                validate_lineage(bad, owners, handles, 0, require)
        with self.assertRaises(AssertionError):
            validate_lineage(grants, {**owners, 9: [1, 0, 0, 0]}, handles, 0, require)
        bad = copy.deepcopy(grants); bad[0]['revoked'] = 1
        with self.assertRaises(AssertionError):
            validate_lineage(bad, owners, handles, 0, require)

    def test_mapping_evidence_requires_identical_live_provenance_in_both_registries(self):
        binding = dict(space=0x101, root=0x10000, address=0x410000, object=123,
                       grant=2, epoch=1, targets=15)
        record = dict(binding, object=0x101)
        objects = {123: dict(kind=5, live=True, target=0x101)}
        roots = {0x101: 0x10000}
        validate_mapping_bindings([binding], [record], objects, roots, require)
        for field in ('space', 'grant', 'address', 'object', 'epoch', 'targets'):
            bad = dict(record); bad[field] += 1
            with self.subTest(field=field), self.assertRaises(AssertionError):
                validate_mapping_bindings([binding], [bad], objects, roots, require)
        for records in ([], [record, record]):
            with self.assertRaises(AssertionError):
                validate_mapping_bindings([binding], records, objects, roots, require)

    def test_hardware_window_rejects_early_ack_wrong_epoch_root_page_or_mode(self):
        request = [2, 1, 0x10000, 0x410000, 15]
        local = [0]*64; local[32:36] = [1, 0x410000, 0x10000, 1]
        regs = dict(rip=0x8000, cpl=0, rfl=2, cr3=0x10000)
        operands = [0x410000, 0x10000, 1, 1]
        probe = [0x8000, 0x8010]
        validate_invalidation_window(2, 4, request, local, regs, operands, 0, probe, require)
        for i in range(4):
            bad = list(operands); bad[i] += 1
            with self.assertRaises(AssertionError):
                validate_invalidation_window(2, 4, request, local, regs, bad, 0, probe, require)
        with self.assertRaises(AssertionError):
            validate_invalidation_window(2, 4, request, local, regs, operands, 4, probe, require)
        with self.assertRaises(AssertionError):
            validate_invalidation_window(2, 4, request, local, dict(regs, rip=0x7fff), operands, 0, probe, require)
        bad = list(local); bad[36] = 1
        with self.assertRaises(AssertionError):
            validate_invalidation_window(2, 4, request, bad, regs, operands, 0, probe, require)

    def test_missing_one_cpu_or_epoch_cannot_pass_terminal_acceptance(self):
        class Observer:
            cpus = 4
            require = staticmethod(require)
            def array(self, _memory, _name, _count):
                layout = [0]*64; layout[48] = 4
                return layout
        for missing in ((1, 0), (4, 3)):
            extension = Extension(Observer())
            extension.invalidations = {key: {} for key in [(e, c) for e in range(1, 5) for c in range(4)] if key != missing}
            with self.assertRaisesRegex(AssertionError, 'missing per-page per-CPU'):
                extension.terminal(None, None, None, None, None, None, None)


if __name__ == '__main__':
    unittest.main(verbosity=2)
