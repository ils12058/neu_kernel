"""Independent ABI 0.8 and hardware oracle; no guest policy is imported.

Raw ACPI/PCI configuration, VT-d tables/registers, native frames, host bytes,
IRQ frames and invalidation completions must agree with separate state models.
"""
import copy
import re
import struct

from device_qmp import (DeviceModel, Extension as DeviceExtension, FREE, REARM,
                        ARMED, FLIGHT, PENDING, QUARANTINE, validate_scrub)
from smp_scheduler_qmp import MASK64, NODES, THREADS, PA, U, NX

IOVA, SCRUB, INTERNAL, IRQ = 0x100000, 0x180000, 0x40000, 256
PATTERN, CANARY = 0x1020304050607080, 0xDAD0CAFE55AA33CC


def rejected_isolation(qmp, symbols, failure):
    """Absence of DMAR must leave every PCI requester unable to bus master."""
    from smp_scheduler_qmp import VirtualMemory
    from smp_qmp import PhysicalMemory
    memory, physical = VirtualMemory(qmp, failure), PhysicalMemory(qmp, 0, failure)

    def require(ok, message):
        if not ok:
            raise failure(message)
    pci = memory.words(symbols['neu_pci_layout'], 16)
    require(pci[0] == 1 and pci[11] == 0 and memory.u64(symbols['neu_iommu_layout']) == 0,
            'missing-IOMMU control unexpectedly established DMA isolation')
    dma = memory.words(symbols['neu_dma_layout'], 32)
    require(dma[0] == 0 or not any(memory.words(dma[1], 12)) and not any(memory.words(dma[6], 2)),
            'DMA lease/authority published without an IOMMU')
    ecam, configurations = memory.u64(pci[12]), []
    for i in range(pci[2]):
        raw = memory.read(pci[1] + i*pci[3], pci[3])
        requester = raw[2] << 8 | raw[3]
        config = physical.bytes(ecam+(requester << 12), 64)
        command = int.from_bytes(config[4:6], 'little')
        require(command & 0x404 == 0x400, 'missing IOMMU left a PCI DMA/interrupt requester enabled')
        configurations.append(dict(requester=requester, command=command, raw=config.hex()))
    return dict(no_dmar=True, no_domain=True, requesters=configurations)


def acpi_table(data, signature, require):
    require(len(data) >= 36 and data[:4] == signature and
            int.from_bytes(data[4:8], 'little') == len(data) and not sum(data) % 256,
            'ACPI signature/extent/checksum mismatch')


def walk_domain(memory, root, requester, require):
    """Return the selected RID's domain and exact 4K IOVA->PTE map."""
    roots = memory.words(root, 512)
    bus, function = requester >> 8, requester & 255
    require(all(v == 0 for i, v in enumerate(roots) if i != bus * 2),
            'unassigned requester bus has a root context')
    entry = roots[bus * 2]
    require(entry & ~PA == 1 and entry & PA, 'selected requester has no translated root')
    contexts = memory.words(entry & PA, 512)
    require(all(v == 0 for i, v in enumerate(contexts) if i not in (function * 2, function * 2 + 1)),
            'another requester acquired the selected domain')
    low, high = contexts[function * 2:function * 2 + 2]
    require(low & ~PA == 1 and high & ~0xFFFF07 == 0 and high >> 8 > 0
            and high & 7 in (1, 2), 'context is absent, pass-through or malformed')
    levels, leaves, visited = (high & 7) + 2, {}, set()

    def walk(address, level, prefix):
        require(address and address not in visited, 'cyclic/aliased second-level table')
        visited.add(address)
        for index, value in enumerate(memory.words(address, 512)):
            if not value:
                continue
            require(value & ~(PA | 3) == 0 and value & 3 and value & PA,
                    'unsupported DMA translation flags or physical page')
            iova = prefix | (index << (12 + 9 * (level - 1)))
            if level == 1:
                leaves[iova] = value
            else:
                require(value & 3 == 3, 'intermediate DMA table permissions changed')
                walk(value & PA, level - 1, iova)
    walk(low & PA, levels, 0)
    return dict(id=high >> 8, context=entry & PA, levels=levels,
                root=low & PA, pages=sorted(visited), leaves=leaves)


def invalidations(rows, require):
    for index, row in enumerate(rows):
        require(len(row) == 8 and row[0] == index + 1 and row[4] == 0xA000000000000000
                and row[5] & 0x9800000000000000 == 0x0800000000000000
                and row[6] & ~(3 << 48) == 0x9000000000000000
                and row[7] & 0x8600000000000000 == 0x0200000000000000,
                'context/IOTLB invalidation lacks actual completion granularity')


class DmaModel(DeviceModel):
    info = 0x8000007FF
    irq_statuses = (1, 2, 256)
    lease_kinds = (7, 8, 9, 10)

    def __init__(self, initial, require):
        super().__init__(initial, require)
        self.domain = self.buffer_handle = self.permissions = self.submits = 0
        self.buffer = bytearray(8192)
        self.internal = bytearray(4096)
        self.transfer = None
        self.dma_calls = []

    def budget_exhausted(self):
        return self.submits == 16

    def complete_dma(self):
        self.require(self.transfer is not None, 'DMA completion has no authorized request')
        offset, length, direction = self.transfer
        if direction == 1:
            self.internal[:length] = self.buffer[offset:offset+length]
        else:
            self.buffer[offset:offset+length] = self.internal[:length]
        self.transfer = None

    def before_row(self, row):
        if (row[1] == 1 and row[13] == 0 and self.flight == IRQ or
            row[1] == 3 and not self.deferred_irq and row[8] == IRQ or row[1] == 9):
            self.complete_dma()

    def after_row(self, row):
        if row[1] == 6:
            self.permissions = self.domain = self.buffer_handle = 0
            self.buffer = bytearray(8192)
            self.internal = bytearray(4096)

    def operation(self, row):
        op, args, actual = row[6], row[7:13], tuple(row[13:15])
        owner = 0 if row[3] == NODES[0] else 1
        key = row[2], row[5]
        self.require(op != 2 or args[0] == 128, f"DMA CPL3 fixture exited with {args[0]}; recent calls={self.rows[-8:]}")
        if not 0x22 <= op <= 0x28:
            super().operation(row)
            if op == 0x1C and actual[0] == 0:
                self.domain = self.buffer_handle = self.permissions = self.submits = 0
                self.buffer = bytearray(8192)
                self.transfer = None
            return
        self.require(key not in self.calls and row[3] == NODES[owner] and row[4] in THREADS,
                     'foreign or duplicate native DMA caller')
        self.calls[key] = row
        used = {0x22: 1, 0x23: 1, 0x24: 3, 0x25: 2, 0x26: 2, 0x27: 3, 0x28: 5}[op]
        kind, rights = {0x22: (7, 1), 0x23: (9, 4), 0x24: (9, 4), 0x25: (9, 4),
                        0x26: (10, 1), 0x27: (10, 2), 0x28: (9, 32)}[op]
        error, h = self.lookup(owner, args[0], kind, rights)
        if not error:
            error = self.authorized(owner, h)
        if op == 0x28 and error == -46 and self.reason == 5 and self.submits == 16:
            error = -53
        expected = (error, 0)
        if any(args[used:]):
            expected = -2, 0
        elif error:
            pass
        elif op in (0x22, 0x23):
            anchor = self.mmio if op == 0x22 else self.domain
            field = 'domain' if op == 0x22 else 'buffer_handle'
            if args[0] != anchor:
                expected = -18, 0
            else:
                value = getattr(self, field)
                if not value:
                    error, value = self.allocate(owner, dict(kind=9 if op == 0x22 else 10,
                        rights=0x334 if op == 0x22 else 0x307, target=0, grant=self.grant(),
                        transferable=0, lease=self.generation))
                    if error:
                        expected = error, 0
                    else:
                        setattr(self, field, value)
                if not error:
                    expected = 0, value
        elif op in (0x26, 0x27):
            offset = args[1]
            if offset % 8 or offset >= 8192:
                expected = -49, 0
            elif self.transfer is not None:
                expected = -52, 0
            elif op == 0x26:
                expected = 0, int.from_bytes(self.buffer[offset:offset+8], 'little')
            else:
                self.buffer[offset:offset+8] = args[2].to_bytes(8, 'little')
                expected = 0, 0
        else:
            permission = args[2] if op == 0x24 else args[4] if op == 0x28 else 0
            error, buffer = self.lookup(owner, args[1], 10, 4 | permission)
            if not error:
                error = self.authorized(owner, buffer)
            if op != 0x25 and (permission not in (1, 2, 3) or op == 0x28 and permission == 3):
                expected = -50, 0
            elif error:
                expected = error, 0
            elif op == 0x24:
                if self.permissions or self.transfer is not None:
                    expected = -51, 0
                else:
                    self.permissions = permission
                    expected = 0, IOVA
            elif op == 0x25:
                if self.transfer is not None:
                    expected = -52, 0
                elif not self.permissions:
                    expected = -51, 0
                else:
                    self.permissions = 0
                    expected = 0, 0
            elif self.phase != ARMED or self.transfer is not None:
                expected = -52, 0
            elif not self.permissions & permission:
                expected = -50, 0
            elif not 0 < args[3] <= 4096 or args[2] + args[3] > 8192:
                expected = -49, 0
            elif self.submits == 16:
                expected = -53, 0
            else:
                self.submits += 1
                self.transfer = args[2], args[3], permission
                self.phase, self.flight = FLIGHT, IRQ
                expected = 0, self.submits
        expected = expected[0] & MASK64, expected[1]
        self.require(actual == expected,
                     f'DMA native model mismatch op={op:#x} args={args} actual={actual} expected={expected}')
        self.results[key] = expected
        if expected[0] == 0:
            self.dma_calls.append(row)


class LeaseModel:
    """Separate finite lifetime model, including asynchronous retirement."""
    def __init__(self, require):
        self.require, self.rows = require, []
        self.state, self.scrub = [0]*12, 0

    def process(self, rows):
        for index in range(len(self.rows), len(rows)):
            r = rows[index]
            self.require(len(r) == 20 and r[0] == index + 1, 'DMA lifetime journal ordering')
            s, k = self.state[:], r[1]
            owner, generation, rights, busy, quarantine, epoch, invalidated, submissions, completions, offset, length, direction = s
            if k == 1:
                self.require(index == 0 and not any(s), 'DMA initial domain is not empty')
            elif k == 2:
                self.require(not owner and not rights and not busy and epoch == invalidated
                             and r[2] in NODES and r[3] == generation + 1, 'DMA claim preceded safe retirement')
                s[0:2] = r[2:4]
                s[4], s[7], s[8], self.scrub = 0, 0, 0, 0
            elif k == 3:
                self.require(owner and not quarantine, 'duplicate/unowned DMA quarantine')
                s[4] = 1
            elif k == 4:
                self.require(owner and not quarantine and not rights and not busy and epoch == invalidated
                             and r[14] == IOVA and r[15] in (1, 2, 3), 'unauthorized DMA map')
                s[2], s[5], s[6] = r[15], epoch + 1, epoch + 1
            elif k == 5:
                self.require(rights and not busy and epoch == invalidated, 'DMA unmap while busy/uninvalidated')
                s[2], s[5], s[6] = 0, epoch + 1, epoch + 1
            elif k == 6:
                o, n, direction = r[11:14]
                self.require(owner and not quarantine and not busy and epoch == invalidated and submissions < 16
                             and direction in (1, 2) and rights & direction and 0 < n <= 4096 and o+n <= 8192
                             and r[14:16] == [IOVA+o, n], 'DMA submit violated bounds, direction or ownership')
                s[3], s[7], s[9:12] = 1, submissions + 1, [o, n, direction]
            elif k == 7:
                self.require(busy and r[14] == IRQ, 'DMA completion without an in-flight command')
                s[3], s[8] = 0, completions + 1
            elif k == 8:
                self.require(owner and quarantine and not busy and r[14] == self.scrub + 1,
                             'DMA reset skipped a masked completion')
                if r[14] == 1 and rights:
                    s[2], s[5], s[6] = 0, epoch + 1, epoch + 1
                self.scrub = r[14]
            elif k == 9:
                self.require(owner and quarantine and not busy and not rights and epoch == invalidated
                             and self.scrub == 3, 'DMA backing reassigned before scrub/invalidation')
                s[0], s[9:12] = 0, [0, 0, 0]
            elif k == 10:
                self.require(owner and not busy and not quarantine, 'probe interfered with a live DMA command')
            else:
                self.require(False, 'unknown DMA lifetime event')
            self.require(r[2:14] == s, f'raw DMA lifetime transition disagrees with model: {r} != {s}')
            self.state = s
            self.rows.append(r)


class Extension(DeviceExtension):
    model_class = DmaModel
    lease_kinds = (6, 7, 8, 9, 10)

    def __init__(self, observer):
        super().__init__(observer)
        self.lease = LeaseModel(self.require)
        self.hardware_checks = {}
        self.pool = None

    def hardware(self, x, final=False):
        ecam = x[9] + (x[8] << 12)
        config = {o: self.physical(ecam+o, 4) for o in (0, 4, 0x10, 0x3C, x[16])}
        self.require(config[0] == 0x11E81234 and config[0x10] == x[3]
                     and config[0x3C] >> 8 & 255 == 1 and config[x[16]] & 0x10000 == 0
                     and config[4] & 0x407 == (0x400 if final else 6),
                     'PCI DMA identity/BAR/MSI/decode/bus-master authority mismatch')
        pic = self.o.qmp.call('human-monitor-command', {'command-line': 'info pic'})
        routes = {int(pin): int(value, 16) for pin, value in re.findall(r'pin\s+(\d+)\s+0x([0-9a-fA-F]+)', pic)}
        self.require(x[7] in routes and all(value == 0x10000 for pin, value in routes.items() if pin != x[7]),
                     'DMA routing changed an unrelated I/O APIC pin')
        route = routes[x[7]]
        self.require(route == 0x10000 if final else route >> 56 == x[43]
                     and route & ~((255 << 56) | 0x14000) == 0xA0F0, 'DMA IRQ route/destination changed')
        regs = {o: self.physical(x[3]+o, 4) for o in (0, 4, 8, 0x20, 0x24)} if not final else {}
        dma = [self.physical(x[3]+o, 8) for o in (0x80, 0x88, 0x90, 0x98)] if not final else None
        if not final:
            self.require(regs[0] == 0x010000ED and dma[3] & ~7 == 0, 'EDU register identity/command')
        return dict(config=config, registers=regs, dma=dma, route=route, pic=pic)

    def firmware(self, memory, physical):
        x = self.o.array(memory, 'neu_pci_layout', 16)
        self.require(x[0] == 1 and 1 <= x[2] <= 128 and x[3] == 48 and x[14] == 16,
                     'unknown PCI inventory schema')
        rsdp = physical.bytes(x[8], 36)
        self.require(rsdp[:8] == b'RSD PTR ' and not sum(rsdp[:20]) % 256 and rsdp[15] >= 2
                     and int.from_bytes(rsdp[20:24], 'little') == 36 and not sum(rsdp) % 256
                     and int.from_bytes(rsdp[24:32], 'little') == x[9], 'raw RSDP differs from copied firmware')
        tables = {}
        for name, address in (('root', x[9]), ('mcfg', x[10]), ('dmar', x[11])):
            header = physical.bytes(address, 36)
            length = int.from_bytes(header[4:8], 'little')
            self.require(36 <= length <= 4096, 'ACPI table length is not bounded')
            data = physical.bytes(address, length)
            acpi_table(data, {'root': b'XSDT', 'mcfg': b'MCFG', 'dmar': b'DMAR'}[name], self.require)
            tables[name] = data
        roots = struct.unpack('<' + 'Q' * ((len(tables['root']) - 36)//8), tables['root'][36:])
        self.require(x[10] in roots and x[11] in roots and len(roots) == len(set(roots)), 'MCFG/DMAR not owned by XSDT')
        mcfg = tables['mcfg']
        self.require(not any(mcfg[36:44]) and (len(mcfg)-44)//16 == x[13], 'MCFG region count/reserved fields')
        ecams = []
        for i in range(x[13]):
            region = mcfg[44+i*16:60+i*16]
            base, segment, first, last, reserved = struct.unpack('<QHBBI', region)
            actual = memory.read(x[12]+i*16, 12)
            self.require(actual == region[:12] and not reserved and first <= last and base % (1 << 20) == 0,
                         'copied ECAM aperture differs from ACPI')
            ecams.append((base, segment, first, last))
        units, offset = [], 48
        while offset < len(tables['dmar']):
            data = tables['dmar']
            kind, length = struct.unpack_from('<HH', data, offset)
            self.require(length >= 4 and offset+length <= len(data), 'DMAR structure extent')
            if kind == 0:
                flags, segment, base = data[offset+4], int.from_bytes(data[offset+6:offset+8], 'little'), int.from_bytes(data[offset+8:offset+16], 'little')
                units.append((flags, segment, base))
            offset += length
        raw_inventory = self.o.qmp.call('query-pci')
        qmp_functions = {}
        def collect(value):
            if isinstance(value, list):
                for item in value: collect(item)
            elif isinstance(value, dict):
                if all(k in value for k in ('bus', 'slot', 'function', 'id')) and isinstance(value['bus'], int):
                    qmp_functions[value['bus'] << 8 | value['slot'] << 3 | value['function']] = value['id']['vendor'] | value['id']['device'] << 16
                for item in value.values(): collect(item)
        collect(raw_inventory)
        inventory = []
        for i in range(x[2]):
            raw = memory.read(x[1]+i*48, 48)
            segment, bus, devfn, ident, klass, command = struct.unpack_from('<HBBIIH', raw)
            region = next(e for e in ecams if e[1] == segment and e[2] <= bus <= e[3])
            cfg = physical.bytes(region[0]+((bus << 8 | devfn) << 12), 4096)
            self.require(int.from_bytes(cfg[:4], 'little') == ident and int.from_bytes(cfg[8:12], 'little') == klass
                         and cfg[14] & 127 == raw[14] and cfg[0x3D] == raw[15], 'PCI inventory differs from hardware')
            header = raw[14]
            bars = 6 if header == 0 else 2
            self.require(raw[24:24+4*bars] == cfg[16:16+4*bars], 'BAR probe did not restore configuration')
            caps, pointer = {}, cfg[0x34] if cfg[6] & 16 else 0
            while pointer:
                self.require(pointer not in caps and 0x40 <= pointer <= 0xFC and pointer % 4 == 0,
                             'malformed hardware standard capability chain')
                caps[pointer] = cfg[pointer]
                pointer = cfg[pointer+1]
            for kind, field in ((5, 19), (0x11, 20), (0x10, 21)):
                found = [p for p, k in caps.items() if k == kind]
                self.require(found == ([raw[field]] if raw[field] else []), 'PCI capability identity/count differs from configuration')
                if kind in (5, 0x11) and found:
                    control = int.from_bytes(cfg[found[0]+2:found[0]+4], 'little')
                    self.require(not control & (1 if kind == 5 else 0x8000), 'unassigned message interrupt enabled')
            extended, pointer = {}, 0x100 if raw[21] else 0
            while pointer:
                self.require(pointer not in extended and 0x100 <= pointer <= 0xFFC and pointer % 4 == 0,
                             'malformed hardware extended capability chain')
                cap = int.from_bytes(cfg[pointer:pointer+4], 'little')
                if cap in (0, 0xFFFFFFFF): break
                self.require(cap & 65535 and cap >> 16 & 15 and len(extended) < 64, 'invalid extended capability header')
                extended[pointer] = cap
                pointer = cap >> 20
            self.require(raw[22:24] == bytes((len(caps), len(extended))), 'PCI capability chain omitted entries')
            inventory.append(dict(segment=segment, bus=bus, devfn=devfn, id=ident, command=command,
                                  header=header, pin=raw[15], secondary=raw[17], subordinate=raw[18],
                                  configuration=cfg[:256].hex(), capabilities=caps, extended_capabilities=extended))
        self.require({f['bus'] << 8 | f['devfn']: f['id'] for f in inventory if f['segment'] == 0} == qmp_functions,
                     'PCI enumeration omitted or fabricated a QEMU function')
        selected = next(f for f in inventory if (f['segment'] << 16 | f['bus'] << 8 | f['devfn']) == x[4])
        self.require(selected['id'] == 0x11E81234 and x[4] == x[15], 'selected PCI requester identity')
        child, pin = selected, selected['pin']
        bridges = []
        while True:
            parents = [f for f in inventory if f['header'] == 1 and f['secondary'] <= child['bus'] <= f['subordinate']]
            if not parents:
                break
            parent = min(parents, key=lambda f: f['subordinate']-f['secondary'])
            pin = ((pin - 1 + (child['devfn'] >> 3)) % 4) + 1
            bridges.append(parent['bus'] << 8 | parent['devfn'])
            child = parent
        self.require(child['bus'] == 0 and x[7] == 20 + (((child['devfn'] >> 3) + pin - 1) % 4),
                     'PCI bridge interrupt swizzling is incorrect')
        for f in inventory:
            command = int.from_bytes(bytes.fromhex(f['configuration'])[4:6], 'little')
            allowed = (f['bus'] << 8 | f['devfn']) in [x[4], *bridges]
            self.require(bool(command & 4) == allowed, 'unassigned PCI bus mastering is enabled')
        if self.o.case == 'smp-dma-slot':
            self.require(x[4] == 0x38 and x[7] == 23, 'alternative slot test used the legacy placement')
        if self.o.case == 'smp-dma-bridge':
            self.require(selected['bus'] != 0 and bridges, 'secondary-bus DMA was not exercised')
        return dict(rsdp=rsdp.hex(), tables={k: v.hex() for k, v in tables.items()},
                    ecam=ecams, units=units, inventory=inventory, selected=x[4], bridges=bridges, qmp_inventory=raw_inventory)

    def isolation(self, memory, physical, final=False):
        x = self.o.array(memory, 'neu_iommu_layout', 32)
        d = self.o.array(memory, 'neu_dma_layout', 32)
        self.require(x[:2] == [1, 1] and x[3] == 16 and x[9:11] == [8, 8]
                     and x[13:15] == [128, 8], 'unknown bounded VT-d schema')
        count = memory.u64(x[12])
        self.require(0 < count <= 128, 'invalid invalidation journal count')
        inv = [memory.words(x[11]+i*64, 8) for i in range(count)]
        invalidations(inv, self.require)
        units, domains = [], []
        for index in range(x[1]):
            u = memory.words(x[2]+index*128, 16)
            regs = {o: self.physical(u[0]+o, width) for o, width in ((0, 4), (8, 8), (0x10, 8),
                     (0x1C, 4), (0x20, 8), (0x28, 8), (0x34, 4), (0x38, 4), (u[6], 8))}
            self.require([regs[0], regs[8], regs[0x10]] == u[2:5] and regs[0x1C] & 0x86000000 == 0x80000000
                         and regs[0x20] == u[5] and regs[0x38] & (1 << 31) and not regs[0x34] & 0x11,
                         'VT-d hardware translation/fault/queue state differs from contract')
            last = next(r for r in reversed(inv) if r[1] == index)
            self.require(regs[0x28] == last[5] and regs[u[6]] == last[7], 'journal is not backed by invalidation register readback')
            faults = [[self.physical(u[0]+u[7]+i*16+o, 8) for o in (0, 8)] for i in range(u[8])]
            active_faults = [f for f in faults if f[1] >> 63]
            if active_faults:
                self.require(self.o.case in ('smp-dma-unmapped', 'smp-dma-readonly', 'smp-dma-requester', 'smp-dma-revoked')
                             and memory.u64(d[16]) in (1, 2, 3), 'unexpected DMA remapping fault')
            if final:
                self.require(not any(physical.words(u[5], 512)) and not active_faults and x[15] == 1,
                             'final IOMMU does not retain empty default-deny roots')
            else:
                domain = walk_domain(physical, u[5], self.layout(memory)[8], self.require)
                lease, scrub = memory.words(d[1], 12), memory.u64(d[8])
                expected = {IOVA: self.pool[0] | lease[2], IOVA+4096: self.pool[2] | lease[2]} if lease[2] else {}
                if scrub in (1, 2, 3, 4):
                    expected.update({SCRUB: self.pool[3] | 1, SCRUB+4096: self.pool[4] | 2})
                self.require(domain['leaves'] == expected, 'actual DMA translations exceed live buffer authority')
                domains.append(domain)
            units.append(dict(descriptor=u, registers=regs, faults=faults))
        return dict(units=units, domains=domains, invalidations=inv)

    def observe(self, memory, physical, live, cpus, scheduler, authority, phase, evidence):
        x, a = self.layout(memory), self.o.array(memory, 'neu_dma_layout', 32)
        self.require(a[0] == 1 and a[2] == 96 and a[5] == 5 and a[11:15] == [128, 20, IOVA, SCRUB],
                     'unknown DMA pool/lease schema')
        self.pool = memory.words(a[4], 5)
        self.require(len(set(self.pool)) == 5 and all(p and not p % 4096 for p in self.pool)
                     and self.pool[2] != self.pool[0] + 4096, 'DMA buffer is not independently backed across pages')
        self.require(physical.words(self.pool[1], 512) == [CANARY]*512, 'unmapped guard was modified by DMA')
        count = memory.u64(x[17])
        self.require(count <= 512, 'device transaction capacity')
        self.model.process([memory.words(x[13]+i*256, 32) for i in range(count)])
        count = memory.u64(a[10])
        self.require(count <= 128, 'DMA transaction capacity')
        self.lease.process([memory.words(a[9]+i*160, 20) for i in range(count)])
        self.require(memory.words(a[1], 12) == self.lease.state, 'actual DMA lease differs from independent lifetime model')
        d, m = memory.words(x[1], 24), self.model
        self.require(d[:9] == [m.phase, m.generation, m.owner, m.waiter['thread'] if m.waiter else 0,
                              m.receiver, m.sequence, m.status, m.flight, m.deadline]
                     and d[9:14] == [m.claims, m.interrupts, m.acks, m.quarantines, m.releases]
                     and d[15:17] == [m.reads, m.writes] and d[18] == m.remaining
                     and d[19:22] == [m.blocks, m.delivered, m.cancelled], 'device state diverged from native DMA model')
        self.require(memory.u64(x[11]) == cpus[0][13], 'DMA watchdog lacks actual BSP timer EOIs')
        for owner, actual in enumerate(authority['handles']):
            expected = sorted(m.tables[owner].values(), key=lambda h: h['slot'])
            self.require(len(expected) == len(actual) and all(
                all(a[k] == e[k] for k in ('slot', 'generation', 'kind', 'rights')) and
                (a.get('lease') == e.get('lease') if a['kind'] in self.lease_kinds else a['target'] == e['target'])
                for a, e in zip(actual, expected)), 'actual DMA capability table diverges from native model')
        if d[3]:
            self.require(d[3] in THREADS and scheduler['threads'][THREADS.index(d[3])]['state'] == 4,
                         'DMA completion waiter is not a blocked Thread')
        checkpoint = memory.u64(self.o.symbol('neu_device_checkpoint'))
        in_probe = self.o.symbol('neu_smp_sched_probe_begin') <= live[0]['rip'] < self.o.symbol('neu_smp_sched_probe_end')
        key = ('irq', x[46]) if checkpoint == 10 else (checkpoint, d[1], d[19] if checkpoint == 1 else 0)
        if phase == 1 and self.o.prepared is None:
            fw, hw, iommu = self.firmware(memory, physical), self.hardware(x), self.isolation(memory, physical)
            self.require(fw['units'][0][2] == iommu['units'][0]['descriptor'][0], 'IOMMU MMIO differs from DMAR')
            for root in (evidence[3], evidence[6], evidence[7]):
                for i, backing in enumerate(self.pool):
                    leaf = self.o.leaf(physical, root, a[3]+i*4096)
                    self.require(leaf['present'] and leaf['leaf'] & PA == backing and leaf['leaf'] & (3 | U | NX) == 3 | NX
                                 and all(not e & U for _, _, e in leaf['chain']), 'DMA pool has userspace CPU authority')
                for virtual, backing in ((x[5], x[3]), (x[6], 0xFEC00000),
                                         (iommu['units'][0]['descriptor'][1], fw['units'][0][2])):
                    leaf = self.o.leaf(physical, root, virtual)
                    self.require(leaf['present'] and leaf['leaf'] & PA == backing
                                 and leaf['leaf'] & (0x9F | NX) == 0x1B | NX,
                                 'DMA platform registers are not supervisor RW/NX/UC')
            self.hardware_checks['prepared'] = dict(firmware=fw, hardware=hw, iommu=iommu, pool=self.pool)
            self.o.output.append(dict(stage='dma-prepared', **self.hardware_checks['prepared']))
        elif checkpoint and in_probe and key not in self.checkpoints:
            hw, isolation = self.hardware(x), self.isolation(memory, physical)
            buffer = physical.bytes(self.pool[0], 4096) + physical.bytes(self.pool[2], 4096)
            if checkpoint == 10:
                index = x[46]-1
                self.require(0 <= index < 64 and x[45] == 64, 'DMA IRQ capture capacity')
                record = memory.words(self.o.symbol('neu_device_irq')+index*320, 40)
                frame = memory.words(record[8], 22)
                self.require(record[0:2] == [index+1, 0] and record[3] == IRQ
                             and record[6:8] == [1, 0] and record[10:32] == frame and frame[15:17] == [240, 0]
                             and record[9] == live[0]['cr3'] and frame[18] in (8, 0x3B)
                             and record[4] == 0xE0F0 and record[5] == hw['route'] & 0xFFFFFFFF == 0x1A0F0
                             and hw['registers'][0x24] == IRQ and memory.words(x[10], 2) == [record[2], IRQ],
                             'DMA IRQ differs from actual frame/controller/mailbox state')
                lease = memory.words(a[1], 12)
                offset, length, direction = lease[9:12]
                self.require(lease[3] == 1 and hw['dma'] == ([IOVA+offset, INTERNAL, length, 4] if direction == 1
                                                                           else [INTERNAL, IOVA+offset, length, 6]),
                             'actual completed DMA command differs from authorized native submission')
                expected = m.buffer[:]
                if direction == 2:
                    expected[offset:offset+length] = m.internal[:length]
                self.require(buffer == expected, 'physical DMA result differs from the independently computed bytes')
                lapic = self.o.qmp.call('human-monitor-command', {'command-line': 'info lapic', 'cpu-index': 0})
                self.require(not re.search(r'ISR[^\n]*\b240\b', lapic), 'DMA vector remains in service after EOI')
                self.irqs[index+1] = dict(record=record, frame=frame, hardware=hw, iommu=isolation,
                                         buffer=buffer.hex(), lapic=lapic)
                self.o.output.append(dict(stage='dma-irq', **self.irqs[index+1]))
            elif checkpoint == 30:
                self.require(d[0:2] == [ARMED, 1] and memory.words(a[1], 12)[2:4] == [3, 0]
                             and not any(buffer), 'initial authorized mapping was not observed before DMA')
            elif checkpoint == 31:
                self.require(d[0] == QUARANTINE and memory.u64(a[8]) == 3
                             and hw['dma'] == [INTERNAL, SCRUB+4096, 4096, 6] and hw['registers'][0x24] == IRQ
                             and hw['route'] & 0x14000 == 0x10000 and memory.words(x[10], 2) == [0, 0]
                             and not any(buffer) and not any(physical.bytes(self.pool[4], 4096)),
                             'DMA reassignment scrub lacks actual masked completion and full zero readback')
            elif checkpoint == 32:
                faults = [f for u in isolation['units'] for f in u['faults'] if f[1] >> 63]
                self.require(len(faults) == 1, 'isolation probe lacks one actual VT-d fault')
                low, high = faults[0]
                expected_rid = a[17] if self.o.case == 'smp-dma-requester' else x[8]
                self.require(low & ~4095 == a[19] & ~4095 and high & 65535 == expected_rid
                             and high >> 62 == 2 and (high >> 32) & 255 == (2 if a[17] else 5)
                             and a[20:24] == [expected_rid, low & ~4095, (high >> 32) & 255, 0]
                             and buffer == m.buffer, 'real DMA isolation fault or victim-memory integrity mismatch')
                if self.o.case == 'smp-dma-revoked':
                    self.require(any(e[1] == 7 for e in self.lease.rows) and any(e[1] == 5 for e in self.lease.rows),
                                 'stale IOVA test never primed and invalidated a real translation')
            elif checkpoint == 33:
                self.require(d[0:2] == [QUARANTINE, 1] and memory.words(a[1], 12)[3:5] == [1, 1]
                             and hw['dma'][3] & 1 and hw['registers'][0x24] == 0
                             and hw['route'] & 0x14000 == 0x10000,
                             'DMA authority cutoff was not observed while hardware RUN remained active')
            elif checkpoint == 1:
                self.require(d[3] in THREADS and scheduler['threads'][THREADS.index(d[3])]['state'] == 4,
                             'DMA blocking checkpoint lost its real waiter')
            elif checkpoint == 3:
                self.require(d[0:5] == [FREE, 1, 0, 0, 0] and not any(buffer) and memory.u64(x[12]) == 0,
                             'DMA reassignment preceded complete retirement')
            elif checkpoint == 4:
                validate_scrub(d, hw, memory.words(x[10], 2), memory.u64(x[12]), self.require)
                self.require((31, d[1], 0) in self.checkpoints, 'device reset preceded DMA buffer scrub observation')
            else:
                self.require(False, 'unexpected DMA fixture checkpoint')
            self.checkpoints[key] = dict(state=d, lease=memory.words(a[1], 12), hardware=hw, iommu=isolation)
            if checkpoint != 10:
                self.o.output.append(dict(stage='dma-checkpoint', checkpoint=checkpoint, **self.checkpoints[key]))
        if phase == 4:
            isolation = self.isolation(memory, physical, final=True)
            self.require(a[31] == 1 and x[44] == 1 and not any(memory.words(x[10], 2)) and memory.u64(x[12]) == 0,
                         'final DMA/device authority was not retired')
            for address in (x[5], x[6], isolation['units'][0]['descriptor'][1]):
                self.require(not self.o.leaf(physical, evidence[3], address)['present'], 'DMA register alias survived cleanup')
            self.o.output.append(dict(stage='dma-final', hardware=self.hardware(x, final=True), iommu=isolation,
                                      lease=memory.words(a[1], 12), events=self.lease.rows))

    def terminal(self, memory, physical, cpus, scheduler, authority, inventory, evidence):
        x, a = self.layout(memory), self.o.array(memory, 'neu_dma_layout', 32)
        d = memory.words(x[1], 24)
        case, m = self.o.case, self.model
        reason = {'smp-dma-close': 1, 'smp-dma-revoke': 2, 'smp-dma-submit-revoke': 2, 'smp-dma-fault': 3,
                  'smp-dma-timeout': 4, 'smp-dma-quota': 5}.get(case, 1)
        reasons = [r[7] for r in m.rows if r[1] == 5]
        self.require(d[0:5] == [FREE, 2, 0, 0, 0] and d[9] == d[12] == d[13] == d[23] == 2
                     and reasons == [reason, 1], 'DMA did not complete both owner generations')
        self.require(len(self.irqs) == d[10] and cpus[0][56:58] == [d[10], d[10]]
                     and all(c[56:59] == [0, 0, 0] for c in cpus[1:]), 'missing DMA IRQ/EOI capture')
        self.require({0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28} <= {r[6] for r in m.dma_calls},
                     'an advertised DMA operation lacks a successful CPL3 call')
        required = {(30, 1, 0), (31, 1, 0), (31, 2, 0), (4, 1, 0), (4, 2, 0), (3, 1, 0)}
        if case in ('smp-dma-unmapped', 'smp-dma-readonly', 'smp-dma-requester', 'smp-dma-revoked'):
            required.add((32, 1, 0))
        if case in ('smp-dma-close', 'smp-dma-revoke', 'smp-dma-submit-revoke', 'smp-dma-fault'):
            required.add((33, 1, 0))
        self.require(required <= self.checkpoints.keys(), 'missing raw DMA mapping/fault/retirement checkpoint')
        self.require(d[19] >= 2 and d[21] == int(case in ('smp-dma-close', 'smp-dma-revoke', 'smp-dma-submit-revoke')),
                     'DMA wait/cancellation accounting')
        if case in ('smp-dma-close', 'smp-dma-revoke', 'smp-dma-submit-revoke', 'smp-dma-fault'):
            self.require(any(r[1] == 5 and r[27] == IRQ for r in m.rows),
                         'terminal authority test did not interrupt real DMA in flight')
        hw = self.hardware(x)
        self.require(hw['dma'] == [0, 0, 0, 6] and hw['registers'][0x24] == 0
                     and hw['registers'][0x20] == 0 and hw['registers'][8] == 1,
                     'DMA command/address/IRQ state survived final retirement')
        for page in (0, 2, 3, 4):
            self.require(not any(physical.bytes(self.pool[page], 4096)), 'DMA pool data leaked across owner lifetimes')
        for op, kind in ((0x24, 4), (0x25, 5), (0x28, 6)):
            calls = [r for r in m.dma_calls if r[6] == op]
            events = [r for r in self.lease.rows if r[1] == kind]
            self.require(len(calls) == len(events), 'native DMA call lacks its matching hardware lifetime transaction')
            if op == 0x28:
                self.require(all(c[9:12] == e[11:14] and c[3] == e[2] for c, e in zip(calls, events)),
                             'DMA hardware submission differs from native buffer bounds/direction/owner')
        self.o.output.append(dict(stage='dma-terminal', state=d, lease=memory.words(a[1], 12),
                                  transactions=m.rows, events=self.lease.rows, hardware=hw))
