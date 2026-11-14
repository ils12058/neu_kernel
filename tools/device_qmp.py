"""Independent K11 oracle: raw PCI/EDU/I/O APIC, ownership and native frames.

The operation model below does not call the Rust policy. Guest journal requests
and results must reconcile with every actual SYSCALL frame and resumed context.
Hardware checks use QEMU's physical address space and interrupt-controller state.
"""
import copy
import math
import re
import struct

from smp_scheduler_qmp import MASK64, NODES, THREADS, P, W, U, NX, PA, raw32, raw64
from authority_qmp import validate_lineage

MMIO, IOAPIC = 0xFFFFB00000001000, 0xFFFFB00000002000
PCI_ID, IDENT = 0x11E81234, 0x010000ED
ABSENT, FREE, REARM, ARMED, FLIGHT, PENDING, QUARANTINE = range(7)


def register_policy(offset, width, value=None):
    if width != 4 or offset % 4:
        return None
    if value is None:
        return 0 if offset in (0, 4, 8, 0x20, 0x24) else None
    if not 0 <= value <= 0xFFFFFFFF:
        return None
    if offset == 4 or offset == 0x20 and value == 0x80:
        return 0
    if offset == 8 and value <= 12:
        return 1
    if offset == 0x60 and value == 2:
        return 2
    return None


def validate_hardware(config, regs, dma, route, destination, bar, final, require):
    require(config[0] == PCI_ID and config[4] == bar and config[15] >> 8 & 255 == 1,
            'hardware PCI identity/BAR/pin differs from discovery')
    require(config[1] & 0x407 == (0x400 if final else 2),
            'PCI bus mastering, port decoding or interrupt-disable state is unsafe')
    require(config[16] & 0x10000 == 0, 'MSI became enabled')
    require(dma is None if final else not any(dma), 'EDU DMA registers were programmed')
    if not final:
        require(regs[0] == IDENT, 'EDU register identification was not read at 32-bit width')
        require(route >> 56 == destination and route & ~((255 << 56) | 0x14000) == 0xA0F0,
                'I/O APIC destination/vector/trigger/polarity changed')
    else:
        require(route == 0x10000, 'I/O APIC route did not return to its masked baseline')


def validate_checkpoints(case, checkpoints, require):
    required = {('checkpoint', 1, 1), ('checkpoint', 3), ('scrub', 1), ('scrub', 2)}
    if case in ('smp-device-2', 'smp-device-4'):
        required.add(('checkpoint', 2))
    if case in ('smp-device-close', 'smp-device-revoke'):
        required.add(('checkpoint', 1, 2))
    require(required <= checkpoints.keys(), 'missing independent device lifecycle checkpoint')


def validate_scrub(state, hardware, mailbox, epoch, require):
    regs = hardware['registers']
    require(state[0] == QUARANTINE and state[1] in (1, 2) and state[7] == 0
            and regs[4] == 0 and regs[8] == 1 and regs[0x20] == 0x80 and regs[0x24] == 1
            and hardware['route'] & 0x14000 == 0x10000 and mailbox == [0, 0] and epoch == state[1],
            'neutral reset lacks actual masked completion before reassignment')


class DeviceModel:
    def __init__(self, initial, require):
        self.require = require
        self.tables = [{(h['generation'] << 32) | (h['slot'] + 1): copy.deepcopy(h)
                        for h in table} for table in initial['handles']]
        self.generations = copy.deepcopy(initial['generations'])
        self.bootstrap = copy.deepcopy(initial['bootstrap'])
        self.forest = {g['id']: g['parent'] for g in initial['grants']}
        self.next_grant = -1
        self.phase, self.owner, self.generation = FREE, 0, 0
        self.sequence = self.status = self.flight = self.receiver = 0
        self.waiter = None
        self.source = self.mmio = self.irq = 0
        self.reads = self.writes = self.remaining = 0
        self.claims = self.interrupts = self.acks = self.quarantines = self.releases = 0
        self.blocks = self.delivered = self.cancelled = 0
        self.liveness = self.factorial = self.fact_status = 0
        self.rows, self.results, self.calls, self.cancelled_calls = [], {}, {}, set()
        self.deferred_irq = False
        self.reason = self.deadline = self.tick = 0
        self.scrubs = set()

    def grant(self, parent=0):
        result = self.next_grant
        self.next_grant -= 1
        self.forest[result] = parent
        return result

    def allocate(self, owner, prototype):
        busy = {h['slot'] for h in self.tables[owner].values()}
        free = [s for s in range(8) if s not in busy and self.generations[owner][s] < 0xFFFFFFFF]
        if not free:
            return -22, 0
        slot = free[0]
        self.generations[owner][slot] += 1
        handle = self.generations[owner][slot] << 32 | (slot + 1)
        self.tables[owner][handle] = dict(prototype, slot=slot, generation=self.generations[owner][slot])
        return 0, handle

    def lookup(self, owner, handle, kind=None, right=0):
        slot, generation = (handle & 0xFFFFFFFF) - 1, handle >> 32
        if slot not in range(8) or not generation or not self.generations[owner][slot]:
            return -16, None
        h = self.tables[owner].get(handle)
        if h is None:
            return -17, None
        if kind is not None and kind != h['kind']:
            return -19, None
        if right and right & h['rights'] != right:
            return -18, None
        return 0, h

    def below(self, grant, source):
        for _ in range(64):
            grant = self.forest.get(grant, 0)
            if not grant:
                return False
            if grant == source:
                return True
        self.require(False, 'cycle in independent device authority model')

    @staticmethod
    def rights(kind, value):
        access = value & 255
        masks = {1: 3, 2: 1, 3: 16, 4: 224, 5: 15, 6: 20, 7: 3, 8: 3}
        return bool(value and not value & ~1023 and kind in masks and not access & ~masks[kind]
                    and (access or kind == 3))

    def authorized(self, owner, h):
        if self.owner != NODES[owner] or h.get('lease') != self.generation:
            return -42
        return -46 if self.phase == QUARANTINE else 0

    def operation(self, row):
        _, _, cpu, node, thread, entry, op, *tail = row
        args, actual_status, actual_scalar = tail[:6], tail[6], tail[7]
        owner = 0 if node == NODES[0] else 1
        self.require(node == NODES[owner] and thread in THREADS, 'foreign native device caller')
        key = cpu, entry
        self.require(key not in self.calls, 'duplicate device syscall journal entry')
        self.calls[key] = row
        handle, arg = args[:2]
        expected = (0, 0)
        used = {0x19: 2, 0x1A: 1, 0x1B: 2, 0x1C: 1, 0x1D: 1,
                0x1E: 3, 0x1F: 4, 0x20: 1, 0x21: 2}.get(op)
        if used and any(args[used:]):
            expected = -2, 0
        elif op == 1:
            expected = 0, 0x7000003FF
        elif op == 2:
            expected = (-101, handle) if handle <= 255 else (-2, 0)
        elif op == (MASK64 - 255):
            expected = 0, 0
        elif op == 0x10:
            wanted = self.bootstrap[owner][handle] if handle < 8 else 0
            expected = (0, wanted) if wanted in self.tables[owner] else (-16, 0)
        elif op in (0x11, 0x12, 0x19, 0x1A):
            error, h = self.lookup(owner, handle)
            if error:
                expected = error, 0
            elif op == 0x11:
                del self.tables[owner][handle]
            elif op == 0x12:
                self.require(arg == 0x400000, 'unexpected device fixture inspection')
                error, _ = self.lookup(owner, handle, 3, 16)
                expected = (error, 0) if error else (0, 11)
            elif not h['rights'] & (256 if op == 0x19 else 512):
                expected = -18, 0
            elif op == 0x19:
                if not self.rights(h['kind'], arg) or arg & ~h['rights']:
                    expected = -18, 0
                else:
                    expected = self.allocate(owner, dict(h, rights=arg, grant=self.grant(h['grant'])))
            else:
                selected = {g for g in self.forest if self.below(g, h['grant'])}
                removed = 0
                for table in self.tables:
                    for candidate in list(table):
                        if table[candidate]['grant'] in selected:
                            del table[candidate]
                            removed += 1
                expected = 0, removed
        elif 0x1B <= op <= 0x21:
            kind, right = {0x1B: (6, 16), 0x1C: (6, 4), 0x1D: (7, 1),
                           0x1E: (7, 1), 0x1F: (7, 2), 0x20: (8, 1), 0x21: (8, 2)}[op]
            error, h = self.lookup(owner, handle, kind, right)
            if not error and op >= 0x1D:
                error = self.authorized(owner, h)
                # A budget rejection quarantines in the same atomic syscall;
                # its preceding quarantine journal record is checked below.
                if error == -46 and op in (0x1E, 0x1F) and self.reason == 5 and self.remaining == 0:
                    error = -48
            if error:
                expected = error, 0
            elif op == 0x1B:
                expected = (0, [PCI_ID, 1 << 20, 7, self.phase][arg]) if arg < 4 else (-2, 0)
            elif op == 0x1C:
                if self.phase != FREE:
                    expected = (-46 if self.phase == QUARANTINE else -40), 0
                else:
                    self.require(len(self.tables[owner]) <= 6, 'claim lacks two free slots')
                    self.generation += 1
                    self.owner = node
                    self.phase = REARM
                    self.reason = 0
                    self.remaining = 64
                    self.source = handle
                    _, self.mmio = self.allocate(owner, dict(kind=7, rights=0x303,
                        target=0, grant=self.grant(), transferable=0, lease=self.generation))
                    _, self.irq = self.allocate(owner, dict(kind=8, rights=0x303,
                        target=0, grant=self.grant(), transferable=0, lease=self.generation))
                    self.claims += 1
                    expected = 0, self.mmio
            elif op == 0x1D:
                expected = (0, self.irq) if handle == self.mmio else (-18, 0)
            elif op in (0x1E, 0x1F):
                write = op == 0x1F
                trigger = register_policy(arg, args[2], args[3] if write else None)
                if trigger is None:
                    expected = -45, 0
                elif write and self.phase != ARMED:
                    expected = -44, 0
                elif not self.remaining:
                    expected = -48, 0
                elif write and trigger == 1 and self.fact_status != 0x80:
                    expected = -44, 0
                else:
                    self.remaining -= 1
                    if write:
                        self.writes += 1
                        if arg == 4:
                            self.liveness = ~args[3] & 0xFFFFFFFF
                        elif arg == 0x20:
                            self.fact_status = 0x80
                        elif arg == 8:
                            self.factorial = math.factorial(args[3]) & 0xFFFFFFFF
                        if trigger:
                            self.flight = trigger
                            self.phase = FLIGHT
                    else:
                        self.reads += 1
                        expected = 0, {0: IDENT, 4: self.liveness, 8: self.factorial,
                                       0x20: self.fact_status, 0x24: self.status}[arg]
            elif op == 0x20:
                if self.waiter or self.receiver:
                    expected = -40, 0
                elif self.phase == PENDING:
                    self.receiver = thread
                    self.delivered += 1
                    expected = 0, self.sequence
                else:
                    self.waiter = dict(key=key, thread=thread, handle=handle, grant=h['grant'])
                    self.blocks += 1
                    expected = -100, 0
            elif op == 0x21:
                if self.phase != PENDING or self.receiver != thread or arg != self.sequence or not arg:
                    expected = -43, 0
                else:
                    self.receiver = self.status = 0
                    self.phase = REARM
                    self.acks += 1
        else:
            self.require(False, 'unexpected operation in device syscall journal')
        expected = expected[0] & MASK64, expected[1]
        self.require((actual_status, actual_scalar) == expected,
                     f'device syscall differs from independent model: op={op:#x}, args={args}, actual={(actual_status, actual_scalar)}, expected={expected}')
        self.results[key] = expected

    def process(self, rows):
        for index in range(len(self.rows), len(rows)):
            row = rows[index]
            self.require(len(row) == 32 and row[0] == index + 1, 'missing/reordered device transaction')
            kind = row[1]
            old_deadline = self.deadline
            if kind == 0:
                self.operation(row)
            elif kind in (1, 2):
                key = row[2], row[5]
                self.require(self.waiter and self.waiter['key'] == key and
                             self.calls[key][2:13] == row[2:13], 'wake lacks its original native waiter')
                if row[13] == 0:
                    self.require(kind == 1 and self.phase == FLIGHT and self.flight and
                                 row[14] == self.sequence + 1, 'interrupt wake has wrong sequence or phase')
                    self.sequence += 1
                    self.status = self.flight
                    self.flight = 0
                    self.phase = PENDING
                    self.receiver = self.waiter['thread']
                    self.delivered += 1
                    self.deferred_irq = True
                    # interrupt() increments before settling its waiter.
                    self.interrupts += 1
                else:
                    self.require(index + 1 < len(rows) and rows[index+1][1] == 5,
                                 'terminal waiter lacks an atomic quarantine cause')
                    why = rows[index+1][7]
                    expected = (-47 if why == 4 else -46) & MASK64
                    self.require(row[13:15] == [expected, 0], 'wrong terminal wait result')
                    self.phase, self.reason, self.receiver = QUARANTINE, why, 0
                    self.cancelled += 1
                    if kind == 2:
                        self.cancelled_calls.add(key)
                self.waiter = None
                self.results[key] = row[13], row[14]
            elif kind == 3:
                self.require(row[7] == self.generation and row[8] in (1, 2), 'foreign IRQ generation/status')
                if self.deferred_irq:
                    self.require(self.status == row[8], 'wake status differs from hardware event')
                    self.deferred_irq = False
                elif self.phase == QUARANTINE:
                    self.require(self.flight == row[8], 'late IRQ lacks old generation command')
                    self.flight = 0
                    self.interrupts += 1
                else:
                    self.require(self.phase == FLIGHT and self.flight == row[8], 'IRQ has no submitted device operation')
                    self.sequence += 1
                    self.status, self.flight, self.phase = row[8], 0, PENDING
                    self.interrupts += 1
            elif kind == 4:
                self.require(self.phase == REARM and not self.flight and row[7] == self.generation,
                             'device rearmed before acknowledged ownership')
                self.phase = ARMED
            elif kind == 5:
                why = row[7]
                self.require(why in range(1, 7) and self.owner, 'unowned quarantine')
                if why == 4:
                    self.require(row[24] >= old_deadline and old_deadline > 0, 'watchdog fired before its deadline')
                if why == 5:
                    self.require(self.remaining == 0, 'operation budget quarantined prematurely')
                self.phase, self.reason, self.receiver = QUARANTINE, why, 0
                self.quarantines += 1
            elif kind == 6:
                self.require(self.phase == QUARANTINE and not self.flight and not self.waiter
                             and row[7] == self.generation and self.generation in self.scrubs,
                             'reassigned device before retiring pending work and neutral reset')
                owner = 0 if self.owner == NODES[0] else 1
                for handle in list(self.tables[owner]):
                    if self.tables[owner][handle]['kind'] in (7, 8):
                        del self.tables[owner][handle]
                self.owner = self.receiver = self.status = self.remaining = 0
                self.phase = FREE
                self.fact_status = self.liveness = 0
                self.factorial = 1
                self.releases += 1
            elif kind == 7:
                self.require(self.phase == QUARANTINE and not self.flight and not self.waiter
                             and row[7:9] == [self.generation, 1] and self.generation not in self.scrubs,
                             'neutral reset lacks an exclusive quiescing generation')
                self.scrubs.add(self.generation)
            elif kind == 8:
                self.require(row[7] in NODES[:2], 'foreign Node authority teardown')
                self.tables[0 if row[7] == NODES[0] else 1].clear()
            else:
                self.require(False, 'unknown device transaction category')
            state = [self.generation, self.phase, self.owner, self.sequence, self.status,
                     self.waiter['thread'] if self.waiter else 0, self.receiver, self.reads, self.writes]
            self.require(row[15:24] == state, f'device transition differs from independent state: kind={kind}, row={row[15:24]}, model={state}')
            self.require(row[26:32] == [self.remaining, self.flight, self.claims, self.interrupts, self.acks, self.reason],
                         'device counter/budget/completion bookkeeping differs from model')
            self.require(row[24] >= self.tick, 'watchdog tick moved backwards')
            if row[25] and row[25] != old_deadline:
                self.require(self.tick + 80 <= row[25] <= row[24] + 80, 'watchdog interval was extended or shortened')
            self.tick, self.deadline = row[24], row[25]
            self.rows.append(row)


class Extension:
    def __init__(self, observer):
        self.o = observer
        self.require = observer.require
        self.initial = self.model = None
        self.checkpoints = {}
        self.irqs = {}
        self.matched = set()

    def layout(self, memory):
        return self.o.array(memory, 'neu_device_layout', 64)

    def authority(self, memory, scheduler, phase, d, result):
        x = self.layout(memory)
        self.require(x[0] == 1 and x[2] == 192 and x[14] == 512 and x[20] == 1 and x[23] == 64,
                     'unknown device ownership schema')
        owners, grants = {}, []
        result['generations'], result['bootstrap'] = [], []
        object_by_id = {(o['kind'] << 40) | (o['generation'] << 8) | (o['slot'] + 1): o for o in result['objects']}
        for owner, handles in enumerate(result['handles']):
            t = d[24+owner*20:44+owner*20]
            result['generations'].append([memory.u32(t[3] + i*t[4] + t[6]) for i in range(8)] if any(t) else [])
            manifest = [0]*8
            for h in handles:
                if h['slot'] == 0 or h['kind'] == 6:
                    manifest[0 if h['slot'] == 0 else 5] = h['generation'] << 32 | h['slot']+1
            result['bootstrap'].append(manifest)
            for h in handles:
                if h['grant']:
                    owners.setdefault(h['grant'], [0]*4)[0] += 1
                if h['kind'] in (6, 7, 8):
                    self.require(not h['transferable'], 'device authority became transferable')
                    obj = object_by_id[h['target']]
                    h['lease'] = obj['target']
                    self.require(h['kind'] == 6 and not h['lease'] or h['kind'] != 6 and h['lease'],
                                 'device object lacks exact claim generation')
        line = x[20:34]
        for slot in range(64):
            raw = memory.read(line[1] + slot*line[2], line[2])
            if raw[line[5]]:
                grants.append(dict(id=(raw32(raw, line[4]) << 32) | (slot+1), revoked=raw[line[6]],
                    parent=raw64(raw, line[7]), object=raw64(raw, line[8]),
                    rights=int.from_bytes(raw[line[9]:line[9]+2], 'little'),
                    uses=list(struct.unpack_from('<IIII', raw, line[10]))))
        validate_lineage(grants, owners, result['handles'], line[13], self.require)
        self.require(not line[12] and not line[13] and not result['waits'], 'unsettled revoke or unrelated Channel wait')
        result['grants'] = grants
        if self.initial is None:
            self.require(phase == 1 and all(len(h) == 4 for h in result['handles']) and len(grants) == 2,
                         'wrong device bootstrap authority')
            self.initial = copy.deepcopy(result)
            self.model = DeviceModel(self.initial, self.require)
        if phase == 4:
            self.require(not grants, 'device lineage leaked at final cleanup')

    def physical(self, address, width):
        # HMP may combine multi-element requests into 8-byte accesses. EDU
        # requires exactly 4 bytes below 0x80, so request ONE element per call.
        fmt = 'wx' if width == 4 else 'gx'
        raw = self.o.qmp.call('human-monitor-command', {'command-line': f'xp /1{fmt} {address:#x}'})
        values = re.findall(r'0x([0-9a-fA-F]+)', raw)
        self.require(len(values) == 1, 'incomplete hardware register read: '+raw)
        return int(values[0], 16)

    def hardware(self, x, final=False):
        ecam = x[9] + (x[8] << 12)
        self.require(x[3] % (1 << 20) == 0 and x[4:9] == [1 << 20, MMIO, IOAPIC, 21, 0x28]
                     and x[16] == 0x40, 'unexpected Q35 device resource')
        config = [0]*17
        for index in (0, 1, 4, 15, 16):
            config[index] = self.physical(ecam + index*4, 4)
        self.require(self.physical(x[9]+0x60, 4) == x[9] | 1,
                     'ECAM aperture disagrees with actual Q35 PCIEXBAR')
        regs = {offset: self.physical(x[3]+offset, 4) for offset in (0, 4, 8, 0x20, 0x24)} if not final else {}
        dma = [self.physical(x[3]+offset, 8) for offset in (0x80, 0x88, 0x90, 0x98)] if not final else None
        pic = self.o.qmp.call('human-monitor-command', {'command-line': 'info pic'})
        match = re.search(r'pin\s+21\s+0x([0-9a-fA-F]+)', pic)
        self.require(match is not None, 'missing actual I/O APIC route')
        route = int(match[1], 16)
        validate_hardware(config, regs, dma, route, x[43], x[3], final, self.require)
        self.require(all(int(value, 16) == 0x10000 for pin, value in re.findall(r'pin\s+(\d+)\s+0x([0-9a-fA-F]+)', pic) if int(pin) != 21),
                     'unrelated I/O APIC route changed')
        return dict(config=config, registers=regs, dma=dma, route=route, pic=pic)

    def observe(self, memory, physical, live, cpus, scheduler, authority, phase, evidence):
        x = self.layout(memory)
        d = memory.words(x[1], 24)
        count = memory.u64(x[17])
        self.require(count <= 512, 'device transaction journal overflow')
        rows = [memory.words(x[13] + i*256, 32) for i in range(count)]
        self.model.process(rows)
        self.require(d[0:9] == [self.model.phase, self.model.generation, self.model.owner,
                     self.model.waiter['thread'] if self.model.waiter else 0, self.model.receiver,
                     self.model.sequence, self.model.status, self.model.flight, self.model.deadline]
                     and d[9:14] == [self.model.claims, self.model.interrupts, self.model.acks,
                                    self.model.quarantines, self.model.releases]
                     and d[15:17] == [self.model.reads, self.model.writes] and d[18] == self.model.remaining
                     and d[19:22] == [self.model.blocks, self.model.delivered, self.model.cancelled],
                     'raw device state disagrees with independent syscall/event model')
        self.require(memory.u64(x[11]) == cpus[0][13], 'watchdog ticks differ from real BSP timer EOIs')
        for owner, actual in enumerate(authority['handles']):
            expected = sorted(self.model.tables[owner].values(), key=lambda h: h['slot'])
            self.require(len(expected) == len(actual) and all(
                all(a[k] == e[k] for k in ('slot', 'generation', 'kind', 'rights')) and
                (a.get('lease') == e.get('lease') if a['kind'] in (7, 8) else a['target'] == e['target'])
                for a, e in zip(actual, expected)), 'actual capability table diverges from independent model')
        if d[3]:
            self.require(d[3] in THREADS and scheduler['threads'][THREADS.index(d[3])]['state'] == 4,
                         'device waiter is not actually blocked')
            handles = memory.words(x[18], 4)
            owner = 0 if d[2] == NODES[0] else 1
            held = next((h for h in authority['handles'][owner] if (h['generation'] << 32 | h['slot']+1) == handles[3]), None)
            self.require(held and held['kind'] == 8 and held['rights'] & 1 and held['lease'] == d[1],
                         'blocked IRQ lacks a live generation-bound wait capability')
        checkpoint = memory.u64(self.o.symbol('neu_device_checkpoint'))
        in_probe = self.o.symbol('neu_smp_sched_probe_begin') <= live[0]['rip'] < self.o.symbol('neu_smp_sched_probe_end')
        key = (('irq', x[46]) if checkpoint == 10 else ('scrub', d[1]) if checkpoint == 4 else
               ('checkpoint', 1, d[19]) if checkpoint == 1 else ('checkpoint', checkpoint))
        if phase == 1 and self.o.prepared is None:
            roots = [evidence[3], evidence[6], evidence[7]]
            for root in roots:
                for address, backing in ((MMIO, x[3]), (IOAPIC, 0xFEC00000)):
                    leaf = self.o.leaf(physical, root, address)
                    self.require(leaf['present'] and leaf['leaf'] & (0x9F | NX) == 0x1B | NX
                                 and leaf['leaf'] & PA == backing and all(not entry & U for _, _, entry in leaf['chain']),
                                 'device page is not exact supervisor RW/NX/UC')
                    self.require(not self.o.leaf(physical, root, address+0x2000)['present'], 'extra device-page alias')
            self.checkpoints['prepared'] = dict(state=d, hardware=self.hardware(x))
            self.o.output.append(dict(stage='device-prepared', **self.checkpoints['prepared']))
        elif checkpoint and in_probe and key not in self.checkpoints:
            hw = self.hardware(x)
            if checkpoint == 10:
                index = x[46]-1
                self.require(0 <= index < 16, 'invalid IRQ capture index')
                record = self.o.array(memory, 'neu_device_irq', 16*40)[index*40:(index+1)*40]
                frame = memory.words(record[8], 22)
                self.require(record[0] == index+1 and record[1] == 0 and record[6:8] == [1, 0]
                             and record[10:32] == frame and frame[15:17] == [240, 0]
                             and record[9] == live[0]['cr3'] and frame[18] in (8, 0x3B),
                             'IRQ capture differs from actual hardware frame/root/vector')
                self.require(record[4] == 0xE0F0 and record[5] == hw['route'] & 0xFFFFFFFF == 0x1A0F0
                             and hw['registers'][0x24] == record[3] in (1, 2)
                             and memory.words(x[10], 2) == [record[2], record[3]],
                             'IRQ mask/remote-IRR/status/mailbox sequencing is incorrect')
                lapic = self.o.qmp.call('human-monitor-command', {'command-line': 'info lapic', 'cpu-index': 0})
                self.require(not re.search(r'ISR[^\n]*\b240\b', lapic), 'device vector remains in service after EOI')
                self.irqs[index+1] = dict(frame=frame, record=record, hardware=hw, registers=live[0], lapic=lapic)
                self.o.output.append(dict(stage='device-irq', **self.irqs[index+1]))
            elif checkpoint == 1:
                self.require(d[3] in THREADS and scheduler['threads'][THREADS.index(d[3])]['state'] == 4
                             and hw['registers'][0x24] == 0, 'wait-before-interrupt checkpoint was not blocked')
                self.o.output.append(dict(stage='device-blocked', state=d, hardware=hw, scheduler=scheduler))
            elif checkpoint == 2:
                self.require(d[0:2] == [PENDING, 1] and d[3:5] == [0, 0] and d[5:8] == [2, 1, 0]
                             and hw['registers'][8] == 720 and hw['registers'][0x20] == 0x80
                             and hw['registers'][0x24] == 1 and hw['route'] & 0x14000 == 0x10000,
                             'factorial completion was not pending before its wait')
                self.o.output.append(dict(stage='device-pending', state=d, hardware=hw))
            elif checkpoint == 3:
                self.require(d[0] == FREE and d[1:5] == [1, 0, 0, 0] and d[7:9] == [0, 0]
                             and hw['registers'][0x20] == hw['registers'][0x24] == hw['registers'][4] == 0
                             and hw['registers'][8] == 1
                             and hw['route'] & 0x14000 == 0x10000 and memory.u64(x[12]) == 0,
                             'device reassignment preceded hardware quiescence')
                self.o.output.append(dict(stage='device-retired', state=d, hardware=hw, authority=authority))
            elif checkpoint == 4:
                mailbox, epoch = memory.words(x[10], 2), memory.u64(x[12])
                validate_scrub(d, hw, mailbox, epoch, self.require)
                self.o.output.append(dict(stage='device-scrub-completion', state=d, hardware=hw,
                                          mailbox=mailbox, epoch=epoch))
            self.checkpoints[key] = True
        if phase == 4:
            self.require(x[44] == 1 and memory.words(x[10], 2) == [0, 0] and memory.u64(x[12]) == 0,
                         'final hardware mailbox/epoch not retired')
            for address in (MMIO, IOAPIC):
                self.require(not self.o.leaf(physical, evidence[3], address)['present'], 'device view survived cleanup')
            self.o.output.append(dict(stage='device-final', state=d, hardware=self.hardware(x, final=True)))

    def terminal(self, memory, physical, cpus, scheduler, authority, inventory, evidence):
        d = memory.words(self.layout(memory)[1], 24)
        normal = self.o.case in ('smp-device-2', 'smp-device-4')
        flight_fault = self.o.case == "smp-device-flight-fault"
        irqs = 4 if normal or flight_fault else 3
        acks = 4 if normal else (2 if self.o.case in ('smp-device-fault', 'smp-device-timeout') else 3)
        first_reason = {'smp-device-fault': 3, 'smp-device-flight-fault': 3, 'smp-device-close': 1, 'smp-device-revoke': 2,
                        'smp-device-timeout': 4, 'smp-device-budget': 5}.get(self.o.case, 1)
        reasons = [r[7] for r in self.model.rows if r[1] == 5]
        self.require(set(range(0x1B, 0x22)) <= {r[6] for key, r in self.model.calls.items()
                                             if self.model.results[key][0] == 0},
                     'an advertised device operation lacks a successful real syscall')
        self.require(d[0:5] == [FREE, 2, 0, 0, 0] and d[9:14] == [2, irqs, acks, 2, 2]
                     and d[23] == 2 and reasons == [first_reason, 1], 'wrong complete device lifecycle')
        self.require(set(self.irqs) == set(range(1, irqs+1)) and
                     cpus[0][56:58] == [irqs, irqs] and all(c[56:59] == [0, 0, 0] for c in cpus[1:]),
                     'missing real IRQ/EOI or foreign CPU delivered the device vector')
        validate_checkpoints(self.o.case, self.checkpoints, self.require)
        self.require(d[19] >= 1 and d[21] == int(self.o.case in ('smp-device-close', 'smp-device-revoke')),
                     'device wait/cancel accounting incorrect')
        hw = self.hardware(self.layout(memory))
        self.require(hw['registers'][4] == hw['registers'][0x20] == hw['registers'][0x24] == 0 and hw['registers'][8] == 1, 'device data registers were not scrubbed')
        self.require(physical.words(self.o.prepared['data'][1] + 128, 16)[14] == 120,
                     'new owner did not complete its own factorial operation')
        if flight_fault:
            self.require(any(r[1] == 5 and r[7] == 3 and r[27] == 1 for r in self.model.rows) and any(r[1] == 3 and r[7:9] == [1, 1] and r[16] == QUARANTINE for r in self.model.rows), 'missing actual in-flight fault and late completion containment')
        self.o.output.append(dict(stage='device-terminal', state=d, transactions=self.model.rows,
                                  real_irqs=irqs, acknowledgements=acks, quarantine_reasons=reasons))

    def native_result(self, before, after, slot, cpu, entry):
        key = cpu, entry
        self.require(key in self.model.results, 'native continuation has no device operation')
        row = self.model.calls[key]
        args = [before[i] for i in (8, 9, 11, 5, 7, 6)]
        self.require(row[3:5] == [NODES[slot], THREADS[slot]] and row[6:13] == [before[14], *args]
                     and self.model.results[key] == (after[14], after[11]),
                     'native device arguments/results differ from actual entry/resumed frames')
        self.matched.add(key)

    def complete_traces(self, traces):
        actual = {}
        for cpu, rows in enumerate(traces):
            for row in rows:
                if row[4] == 128:
                    frame = row[10:30]
                    actual[(cpu, row[0])] = [frame[14], *[frame[i] for i in (8, 9, 11, 5, 7, 6)]]
        self.require(set(actual) == set(self.model.calls) and all(actual[k] == row[6:13] for k, row in self.model.calls.items()),
                     'device journal omitted or fabricated an actual SYSCALL')
        returning = {key for key, row in self.model.calls.items() if row[6] != 2} - self.model.cancelled_calls
        self.require(returning == self.matched, 'a returning device syscall lacks observed continuation')
