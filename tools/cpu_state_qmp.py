"""Independent x87/MMX/SSE2/TLS oracle for stopped x86-64 QEMU CPUs.

The exported layout locates actual owned storage. Expected images, register
banks, TLS results and ownership transitions are computed here, without any
guest CPU-state policy or guest-computed pass bit.
"""
import re
import struct

from smp_scheduler_qmp import (CPU_STATE, DMA, EXTENDED_FAULT, MIGRATE, MASK64,
                               THREADS, NODES, SPACES, VirtualMemory)
from smp_qmp import _user_registers

MAGIC, REQUIRED = 0x4E45555F46505431, 0x07800001
SIZE, SLOT, CPU, BRIDGE, TRACE = 512, 592, 3008, 672, 1152
FALLBACK = 0xFFBF
TLS = [0x402200, 0x402200, 0x402220]
LAYOUT = [MAGIC, 1, SIZE, SLOT, 512, 544, 552, 560, 568, 576, 584,
          CPU, BRIDGE, 928, 1440, 1952, 2464, TRACE, 128, 640,
          256, 4, 3, 32, REQUIRED, MASK64]


def normalized_mask(mask, require):
    mask = mask or FALLBACK
    require(not mask & ~0xFFFF and mask & 0x1F80 == 0x1F80, 'unsupported MXCSR mask')
    return mask


def initial_image(mask):
    image = bytearray(SIZE)
    struct.pack_into('<H', image, 0, 0x37F)
    struct.pack_into('<II', image, 24, 0x1F80, mask)
    return bytes(image)


def decode_image(image, mask, require):
    require(len(image) == SIZE, 'truncated FXSAVE image')
    mxcsr = int.from_bytes(image[24:28], 'little')
    require(not mxcsr & ~mask, 'FXSAVE image has reserved MXCSR bits')
    require(image[5] == 0 and all(not any(image[32+i*16+10:48+i*16]) for i in range(8))
            and not any(image[416:]), 'uninitialized reserved FXSAVE bytes')
    return dict(fcw=int.from_bytes(image[:2], 'little'),
                fsw=int.from_bytes(image[2:4], 'little'), ftw=image[4], mxcsr=mxcsr,
                st=[image[32+i*16:42+i*16].hex() for i in range(8)],
                xmm=[image[160+i*16:176+i*16].hex() for i in range(16)])


def register_state(text, require):
    def field(name):
        match = re.search(r'\b' + name + r'=([0-9a-fA-F]+)', text)
        require(match is not None, 'missing actual register ' + name)
        return int(match[1], 16)
    fs = re.search(r'\bFS\s*=\s*([0-9a-fA-F]+)\s+([0-9a-fA-F]+)', text)
    require(fs is not None, 'missing actual FS selector/base')
    st = re.findall(r'\bFPR(\d)=([0-9a-fA-F]{16})\s+([0-9a-fA-F]{4})', text)
    xmm = re.findall(r'\bXMM(\d\d)=([0-9a-fA-F]{16})\s+([0-9a-fA-F]{16})', text)
    require([int(i) for i, _, _ in st] == list(range(8)) and
            [int(i) for i, _, _ in xmm] == list(range(16)), 'incomplete actual FP register bank')
    return dict(fcw=field('FCW'), fsw=field('FSW'), ftw=field('FTW'), mxcsr=field('MXCSR'),
                st=[struct.pack('<QH', int(low, 16), int(high, 16)).hex() for _, low, high in st],
                xmm=[struct.pack('<QQ', int(low, 16), int(high, 16)).hex() for _, high, low in xmm],
                fs_selector=int(fs[1], 16), fs_base=int(fs[2], 16), cr4=field('CR4'))


def fp_equal(actual, expected, require):
    require(all(actual[key] == expected[key] for key in ('fcw', 'fsw', 'ftw', 'mxcsr', 'st', 'xmm')),
            'actual FP register bank differs from independently expected image')


def exception_gate(raw, target, require):
    address = int.from_bytes(raw[:2], 'little') | (int.from_bytes(raw[6:8], 'little') << 16) | (int.from_bytes(raw[8:12], 'little') << 32)
    require(len(raw) == 16 and raw[2:6] == b'\x08\x00\x00\x8e' and not any(raw[12:])
            and address == target, 'extended exception gate has foreign target, selector, IST or DPL')


def extended_integer(value):
    exponent = value.bit_length() - 1
    return struct.pack('<QH', value << (63-exponent), 0x3FFF+exponent).hex()


def seed_state(tag, dma=False, added=False, toggled=False):
    st = [extended_integer(tag*100+8-i + (tag*100+1 if i == 0 and added else 0)) for i in range(8)]
    bank = [(tag << 48) | (i << 32) | 0x11223344 for i in range(16)]
    if toggled:
        bank[0] ^= bank[15]
    return dict(fcw=0x37F if dma else 0x37F+((tag-1) << 10), fsw=0, ftw=255,
                mxcsr=0x1F80 if dma else 0x1F80+((tag-1) << 13), st=st,
                xmm=[struct.pack('<QQ', value, value).hex() for value in bank])


def tls_result(op, args, current):
    valid = len(args) == 6 and (op == 0x29 and args[0] < 0x800000000000
             and all(v == 0 for v in args[1:]) or op == 0x2A and all(v == 0 for v in args))
    if not valid:
        return ((-2 & MASK64), 0), current
    return ((0, 0), args[0]) if op == 0x29 else ((0, current), current)


def validate_slot(row, slot, roots, mask, cpus, require, initial=False, clean=False):
    image = row[:SIZE]
    identity, base, saves, loads, live, cpu, generation = (
        list(struct.unpack_from('<4Q', row, 512)), *struct.unpack_from('<6Q', row, 544))
    if clean or not live:
        require(image == bytes(SIZE) and identity == [0]*4 and base == saves == loads == live == 0
                and cpu == MASK64 and generation in (0, 1), 'retired CPU state retains ownership/data')
    else:
        require(live == 1 and identity == [THREADS[slot], NODES[slot], roots[0 if slot != 1 else 1], SPACES[slot]]
                and base < 0x800000000000 and (cpu == MASK64 or cpu < cpus)
                and loads == saves + (cpu != MASK64) and generation == 0,
                'saved CPU state has foreign identity, TLS or ownership')
        decode_image(image, mask, require)
        if initial:
            require(image == initial_image(mask) and base == saves == loads == 0 and cpu == MASK64,
                    'new Thread inherited FP/TLS state')
    return dict(identity=identity, base=base, saves=saves, loads=loads, live=live,
                cpu=cpu, generation=generation, image=image.hex())


class Extension:
    def __init__(self, observer):
        self.o, self.require = observer, observer.require
        self.dma = observer.case in DMA
        self.info = 0x900001FFF if self.dma else 0x9000018FF
        self.features, self.mask = [], 0
        self.hardware, self.kernel_probes, self.tls = {}, {}, [0]*3
        self.tls_calls, self.rejects = 0, 0
        self.slots = 0

    def layout(self, memory):
        actual = self.o.array(memory, 'neu_cpu_state_layout', 26)
        expected = LAYOUT[:]
        expected[20] = self.o.trace_limit
        self.require(actual == expected, 'unknown extended-state schema')
        storage = self.o.array(memory, 'neu_cpu_state_storage', 8)
        self.require(storage[0] == 1 and storage[1] % 16 == 0 and storage[1] == storage[4]
                     and storage[2:4] == [storage[1]+3*SLOT, storage[1]+3*SLOT+8]
                     and memory.u32(storage[2]) == storage[5] == self.mask
                     and memory.u64(storage[3]) == storage[7] == self.o.cpus,
                     'CPU-state descriptor differs from actual production storage')
        self.slots = storage[1]
        return storage

    def owned(self, memory, scheduler=None, initial=False, clean=False):
        self.layout(memory)
        roots = self.o.prepared['roots'] if self.o.prepared else [0, 0]
        rows = [validate_slot(memory.read(self.slots+i*SLOT, SLOT), i, roots, self.mask,
                              self.o.cpus, self.require, initial, clean) for i in range(3)]
        owners = [r['cpu'] for r in rows if r['cpu'] != MASK64]
        self.require(len(owners) == len(set(owners)), 'one CPU owns two FP contexts')
        if scheduler:
            for i, row in enumerate(rows):
                if scheduler['threads'][i]['state'] in (5, 6):
                    self.require(not row['live'], 'terminal Thread retained FP/TLS data')
        return rows

    def baseline(self, memory, live):
        features = [memory.words(self.o.symbol('neu_cpu_state_features')+i*128, 16) for i in range(self.o.cpus)]
        masks = [normalized_mask(f[7], self.require) for f in features]
        self.require(len(set(masks)) == 1, 'CPUs have incompatible MXCSR masks')
        self.mask, self.features = masks[0], features
        self.layout(memory)
        images = []
        for cpu, f in enumerate(features):
            base = self.o.symbol('neu_smp_sched_cpus') + cpu*CPU
            image = memory.read(base+1952, SIZE)
            regs = register_state(live[cpu]['text'], self.require)
            self.require(f[0] & REQUIRED == REQUIRED and f[1] == self.mask and f[6] == 1
                         and f[2:6] == [regs['fs_base'], regs['fs_selector'], live[cpu]['cr0'], regs['cr4']]
                         and f[10:12] == [base+1952, base+2464]
                         and int.from_bytes(image[28:32], 'little') == f[7],
                         'CPU support/control snapshot differs from actual baseline')
            fp_equal(regs, decode_image(image, self.mask, self.require), self.require)
            if self.o.case == 'cpu-state-kernel':
                expected = dict(fcw=0x37F, fsw=0, ftw=255, mxcsr=0x1F80,
                    st=[extended_integer((cpu+1)*1000+8-i) for i in range(8)],
                    xmm=[struct.pack('<QQ', 0xDEAD000000000000 | ((cpu+1) << 40) | i,
                         (0xDEAD000000000000 | ((cpu+1) << 40) | i) ^ MASK64).hex() for i in range(16)])
                fp_equal(regs, expected, self.require)
                self.require(regs['fs_selector'] == 0x10 and regs['fs_base'] == base,
                             'kernel sentinel profile lacks CPU-specific privileged FS state')
            images.append(image.hex())
        self.o.output.append(dict(stage='cpu-state-baseline', features=features, images=images,
                                  slots=self.owned(memory, clean=True)))

    def prepared(self, memory, physical, scheduler):
        gates = []
        for cpu in range(self.o.cpus):
            idt = memory.u64(self.o.symbol('neu_smp_cpus')+cpu*384+18*8)
            for vector in (6, 7, 16, 19):
                target = self.o.symbol('neu_isr_'+str(vector))
                raw = memory.read(idt+vector*16, 16)
                exception_gate(raw, target, self.require)
                self.require(memory.read(target, 4) == bytes((0x6A, 0, 0x6A, vector)),
                             'extended exception stub has wrong normalized vector/error')
                gates.append(dict(cpu=cpu, vector=vector, raw=raw.hex(), target=target))
        self.o.output.append(dict(stage='cpu-state-initial', slots=self.owned(memory, scheduler, initial=True), gates=gates))

    def observe(self, memory, physical, live, cpus, scheduler):
        rows = self.owned(memory, scheduler)
        for active in scheduler['active']:
            cpu, slot = active['cpu'], THREADS.index(active['thread'])
            r, local = live[cpu], cpus[cpu]
            if r['cpl'] == 3:
                regs = register_state(r['text'], self.require)
                self.require(rows[slot]['cpu'] == cpu and regs['fs_selector'] == 0
                             and regs['cr4'] == (self.features[cpu][5] & ~0x50000) | 0x600
                             and r['cr0'] == (self.features[cpu][4] & ~12) | 34,
                             'Ring 3 lacks exclusive eager FP ownership or exposes unsupported state')
                begin = self.o.site('neu_cpu_dma_seed_done') if self.dma else self.o.site('neu_smp_sched_user_loop_begin')
                end = self.o.site('neu_smp_sched_user_loop_end')
                # DMA's fixture continues with integer-only device operations.
                stable = r['rip'] >= begin if self.dma else begin <= r['rip'] < end
                if stable and (slot, cpu) not in self.hardware:
                    added = not self.dma and r['rip'] == self.o.site('neu_cpu_x87_added')
                    toggled = not self.dma and r['rip'] == self.o.site('neu_cpu_xmm_toggled')
                    fp_equal(regs, seed_state(slot+1, self.dma, added, toggled), self.require)
                    self.require(regs['fs_base'] == TLS[slot], 'Thread restored another TLS base')
                    record = dict(stage='cpu-state-hardware', cpu=cpu, thread=THREADS[slot],
                                  registers=regs, raw=r['text'], slot=rows[slot])
                    self.hardware[(slot, cpu)] = record
                    self.o.output.append(record)
            elif cpu not in self.kernel_probes and (local[51] == 1 or local[61] == 1) and (
                    self.o.symbol('neu_smp_sched_probe_begin') <= r['rip'] < self.o.symbol('neu_smp_sched_probe_end')):
                regs = register_state(r['text'], self.require)
                image = memory.read(self.o.symbol('neu_smp_sched_cpus')+cpu*CPU+1952, SIZE)
                fp_equal(regs, decode_image(image, self.mask, self.require), self.require)
                self.require([regs['fs_base'], regs['fs_selector'], r['cr0'], regs['cr4']] == self.features[cpu][2:6],
                             'Rust handler retained user FS or user FP controls')
                self.kernel_probes[cpu] = dict(stage='cpu-state-kernel-handler', cpu=cpu, registers=regs, raw=r['text'])
                self.o.output.append(self.kernel_probes[cpu])

    def native_result(self, before, after, slot):
        op = before[14]
        if op not in (0x29, 0x2A):
            return False
        args = [before[i] for i in (8, 9, 11, 5, 7, 6)]
        result, base = tls_result(op, args, self.tls[slot])
        self.require((after[14], after[11]) == result, 'TLS status/scalar differs from independent current-Thread model')
        self.tls[slot] = base
        self.tls_calls += 1
        self.rejects += result[0] != 0
        return True

    def rows(self, memory, name, cpu, count):
        base = self.o.symbol(name)+cpu*self.o.trace_limit*TRACE
        output = []
        for i in range(count):
            raw = memory.read(base+i*TRACE, TRACE)
            output.append((list(struct.unpack_from('<16Q', raw)), raw[128:640], raw[640:1152]))
        return output

    def terminal(self, memory, physical, live, cpus, scheduler, data, entries, traces):
        self.require({slot for slot, _ in self.hardware} == {0, 1, 2}, 'missing actual seeded FP register banks on three Threads')
        if self.o.case in MIGRATE:
            self.require((2, 1) in self.hardware and (2, 3) in self.hardware, 'migration lacks actual FP/TLS banks on both CPUs')
        if self.o.case != 'cpu-state-stress':
            self.require(set(self.kernel_probes) == {0, 1}, 'missing actual restored-kernel FP/FS observations')
        saved_entries, saved_traces, vectors = [], [], set()
        mmx = [0]*3
        for cpu in range(self.o.cpus):
            e = self.rows(memory, 'neu_cpu_state_entries', cpu, len(entries[cpu]))
            t = self.rows(memory, 'neu_cpu_state_traces', cpu, len(traces[cpu]))
            baseline = memory.read(self.o.symbol('neu_smp_sched_cpus')+cpu*CPU+1952, SIZE)
            for extended, ordinary in zip(e, entries[cpu]):
                words, image, _ = extended
                self.require(words[:6] == [*ordinary[:4], cpu, 0] and words[14:16] == [1, 1]
                             and words[13] == self.o.symbol('neu_smp_sched_cpus')+cpu*CPU+1440,
                             'extended entry has a foreign native-frame identity')
                decode_image(image, self.mask, self.require)
                self.require([words[8], words[10], words[12]] == [self.features[cpu][2], *self.features[cpu][4:6]],
                             'entry runs Rust with foreign kernel FS/controls')
            for extended, ordinary in zip(t, traces[cpu]):
                words, image, kernel = extended
                vectors.add(ordinary[4])
                self.require(words[:6] == [*ordinary[:4], cpu, ordinary[4]] and words[14:16] == [2, 1]
                             and words[7:13] == [self.features[cpu][2], self.features[cpu][2],
                                self.features[cpu][4], self.features[cpu][4], self.features[cpu][5], self.features[cpu][5]]
                             and words[13] == self.o.symbol('neu_smp_sched_cpus')+cpu*CPU+1440
                             and kernel == baseline,
                             'boundary failed to capture user FP before restoring exact kernel FP/FS/controls')
                decoded = decode_image(image, self.mask, self.require)
                slot = THREADS.index(ordinary[1])
                if not self.dma and ordinary[25] in (self.o.site('neu_cpu_mmx_resume'), self.o.site('neu_cpu_mmx_restored_resume')):
                    # MMX has eight 64-bit registers. The x87 exponent portion
                    # is not a MMX operand; retain its actual bytes in evidence.
                    expected = [struct.pack('<Q', ((slot+1) << 48) | (i << 32) | 0x55667788).hex() for i in range(8)]
                    self.require([value[:16] for value in decoded['st']] == expected and decoded['ftw'] == 255,
                                 'MMX alias registers were not saved/restored exactly')
                    if ordinary[4] == 128:
                        mmx[slot] += 1
                if ordinary[4] not in (128, 224, 241, 240):
                    self.require(self.o.case in EXTENDED_FAULT and words[6] == TLS[slot], 'FP fault captured foreign TLS')
                    if ordinary[4] == 16:
                        self.require(decoded['fcw'] == 0x37B and decoded['fsw'] & 0x84 == 0x84,
                                     'x87 #MF lacks actual unmasked divide-by-zero state')
                    elif self.o.case == 'cpu-state-sse-status':
                        self.require(decoded['mxcsr'] & 0x204 == 4, 'SSE status lacks actual unmasked divide-by-zero state')
                        self.require(int.from_bytes(image[160:164], 'little') == 0x7F800000,
                                     'TCG SSE status lacks its actual positive-infinity result')
                    else:
                        fp_equal(decoded, seed_state(slot+1), self.require)
            saved_entries.append(e)
            saved_traces.append(t)
        self.require({128, 224} <= vectors and (not self.dma or 240 in vectors), 'missing actual native/timer/device FP boundaries')
        if not self.dma:
            self.require(mmx == [2, 2, 2], 'MMX save and post-restore images were not observed on three Threads')
        streams = []
        for slot in range(3):
            order = [1, 3] if slot == 2 and self.o.case in MIGRATE else [1] if slot == 2 else [2] if slot == 1 and self.o.cpus == 4 else [0]
            previous = None
            count = 0
            for cpu in order:
                for i, ordinary in enumerate(entries[cpu]):
                    if ordinary[1] != THREADS[slot]:
                        continue
                    extended = saved_entries[cpu][i]
                    if previous is None:
                        self.require(extended[1] == initial_image(self.mask) and extended[0][6] == 0,
                                     'first dispatch contains another owner\'s FP/TLS state')
                    else:
                        old, old_ordinary = previous
                        base = old[0][6]
                        if old_ordinary[4] == 128 and old_ordinary[24] == 0x29:
                            args = [old_ordinary[10+i] for i in (8, 9, 11, 5, 7, 6)]
                            _, base = tls_result(0x29, args, base)
                        self.require(extended[1] == old[1] and extended[0][6] == base,
                                     'saved/resumed or migrated FP image/TLS changed across a Thread boundary')
                    boundaries = [(t, r) for t, r in zip(saved_traces[cpu], traces[cpu])
                                  if r[0] == ordinary[0] and r[1] == THREADS[slot]]
                    self.require(boundaries, 'extended dispatch has no owned boundary')
                    previous = boundaries[-1]
                    count += 1
            startup = physical.bytes(self.o.prepared['data'][0 if slot != 1 else 1]+0x400+slot*SIZE, SIZE)
            self.require(startup == initial_image(self.features[order[0]][7]), 'Ring 3 startup FXSAVE did not independently prove zero state')
            address = self.o.prepared['data'][0 if slot != 1 else 1]+0x200+(0x20 if slot == 2 else 0)
            self.require(physical.words(address, 2) == [(slot+1)*0x1101, slot+1], 'TLS storage crossed a sibling or unrelated Node')
            if self.o.case not in EXTENDED_FAULT or slot == 1:
                final = physical.bytes(self.o.prepared['data'][0 if slot != 1 else 1]+0xA00+slot*SIZE, SIZE)
                fp_equal(decode_image(final, self.mask, self.require), seed_state(slot+1, self.dma), self.require)
            if not self.dma:
                self.require(data[slot][11] == int.from_bytes(struct.pack('<d', slot+1.25), 'little')
                             and data[slot][12] == 1 and data[slot][14] == TLS[slot]
                             and data[slot][15] == (14 if self.o.case == 'cpu-state-negative' else 0),
                             'SSE2 math, initial state or TLS rejection witnesses changed')
            streams.append(dict(thread=THREADS[slot], cpu_order=order, entries=count))
        self.o.output.append(dict(stage='cpu-state-terminal', vectors=sorted(vectors), mmx_boundaries=mmx,
                                  tls_calls=self.tls_calls, tls_rejections=self.rejects, streams=streams,
                                  entries=[[dict(words=w, image=i.hex()) for w, i, _ in rows] for rows in saved_entries],
                                  traces=[[dict(words=w, image=i.hex(), kernel=k.hex()) for w, i, k in rows] for rows in saved_traces],
                                  slots=self.owned(memory, scheduler, clean=True)))

    def final(self, memory, live, cpus):
        rows = self.owned(memory, clean=True)
        self.require(all(r['generation'] == 1 for r in rows), 'retirement lost the generation watermark')
        for cpu in range(self.o.cpus):
            base = self.o.symbol('neu_smp_sched_cpus')+cpu*CPU
            original, final = memory.read(base+1952, SIZE), memory.read(base+2464, SIZE)
            regs = register_state(live[cpu]['text'], self.require)
            self.require(original == final and memory.read(base+1440, SIZE) == bytes(SIZE)
                         and memory.u64(base+BRIDGE+152) == 0
                         and [regs['fs_base'], regs['fs_selector'], live[cpu]['cr0'], regs['cr4']] == self.features[cpu][2:6],
                         'final CPU retains user FP/TLS or changed original kernel state')
            if cpus[cpu][11]:
                self.require(memory.words(base+BRIDGE+160, 3) == [base+928, base+1440, self.features[cpu][3]],
                             'bridge transport or original FS selector has a foreign CPU owner')
            fp_equal(regs, decode_image(original, self.mask, self.require), self.require)
        self.o.output.append(dict(stage='cpu-state-final', slots=rows, registers=[register_state(r['text'], self.require) for r in live]))


def rejected_cpu(qmp, case, symbols, failure):
    def require(ok, message):
        if not ok:
            raise failure('CPU-state refusal QMP: ' + message)
    qmp.call('stop')
    require(not qmp.call('query-status')['running'], 'VM did not stop')
    memory = VirtualMemory(qmp, failure)
    layout = memory.words(symbols['neu_cpu_state_layout'], 26)
    require(layout == LAYOUT, 'unsupported CPU has an unknown storage schema')
    features = [memory.words(symbols['neu_cpu_state_features']+i*128, 16) for i in range(2)]
    missing = 1 << (24 if case.endswith('fxsr') else 23 if case.endswith('mmx') else 26)
    require(all(f[6] == 2 and not f[0] & missing and f[0] & REQUIRED != REQUIRED for f in features),
            'control did not use actual unsupported CPUID features')
    storage = memory.words(symbols['neu_cpu_state_storage'], 8)
    require(storage[0] == 1 and storage[1] == storage[4] and storage[6] == 0, 'unsupported CPU published Thread ownership')
    rows = [validate_slot(memory.read(storage[1]+i*SLOT, SLOT), i, [0, 0], FALLBACK, 2, require, clean=True) for i in range(3)]
    registers = [_user_registers(qmp, i, failure) for i in range(2)]
    for i, r in enumerate(registers):
        words = memory.words(symbols['neu_smp_sched_cpus']+i*CPU, 64)
        require(words[11] == words[25] == words[4] == 0 and r['cpl'] == 0,
                'unsupported CPU entered a user Thread or extended-state boundary')
    require(not any(memory.words(symbols['NEU_LAST_FAULT'], 24)), 'unsupported CPU caused an architectural fault')
    return dict(features=features, slots=rows, registers=registers, no_user_execution=True)
