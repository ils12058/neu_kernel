"""Independent stopped-VM acceptance for K9 scheduled SMP user execution.

The exported schemas locate production storage. They do not substitute for
CPU registers, hardware frames, raw queue/Thread fields, translations or the
allocator bitmap. This observer has its own scheduler/ABI expectations and
does not use the Phase 9C-1 observer's static placement/resource assumptions.
"""

import hashlib
import re
import struct
import time

from smp_qmp import _words, _apic_identities, _user_registers, PhysicalMemory, _walk

CASES = {
    "smp-schedule-2": 2,
    "smp-schedule-4": 4,
    "smp-schedule-shared": 2,
    "smp-schedule-migrate": 4,
    "smp-schedule-fault": 2,
    "smp-schedule-reject": 2,
    "smp-schedule-idle": 4,
    "smp-schedule-ipc": 2,
    "smp-schedule-stress": 4,
    "smp-tlb-2": 2,
    "smp-tlb-4": 4,
    "smp-tlb-invalid": 2,
    "smp-authority-disabled": 2,
}
AUTHORITY = {"smp-authority-2": 2, "smp-authority-4": 4, "smp-authority-queued": 4,
             "smp-authority-close": 2, "smp-authority-wait": 4, "smp-authority-stress": 4,
             "smp-authority-terminate": 4}
CASES.update(AUTHORITY)
DEVICE = {"smp-device-2": 2, "smp-device-4": 4, "smp-device-fault": 4, "smp-device-flight-fault": 4,
          "smp-device-close": 2, "smp-device-revoke": 4, "smp-device-timeout": 4,
          "smp-device-budget": 2}
CASES.update(DEVICE)
CASES["smp-device-disabled"] = 2
AUTHORITY_FAULT = {"smp-authority-2", "smp-authority-4", "smp-authority-queued", "smp-authority-terminate"}
MIGRATE = {"smp-schedule-migrate", "smp-schedule-stress"}
IPC = {"smp-schedule-idle", "smp-schedule-ipc"}
TLB = {"smp-tlb-2", "smp-tlb-4", "smp-tlb-invalid"}
CPU_FAULT = TLB | {"smp-schedule-fault", "smp-device-fault", "smp-device-flight-fault"} | AUTHORITY_FAULT
FAULT = CPU_FAULT | {"smp-schedule-reject"}
MAGIC = 0x4E45555F53434831
LAYOUT = [MAGIC, 4, 64, 832, 512, 672, 256, 32, 256]


def validate_trace_counts(cpus, limit, require):
    require(all(0 <= cpu[11] <= limit and 0 <= cpu[25] <= limit for cpu in cpus),
            "native entry or IRQ trace exceeds its fixed allocation")
MASK64 = (1 << 64) - 1
PA = 0x000FFFFFFFFFF000
P, W, U, NX = 1, 2, 4, 1 << 63
AD = (1 << 5) | (1 << 6)
THREADS = [0x100000001, 0x100000002, 0x100000003]
NODES = [0x100000001, 0x100000002, 0x100000001]
SPACES = [0x101, 0x102, 0x101]
USTACK = [0x404000, 0x408000, 0x40C000]
KSTACK = [0xFFFFC00000001000, 0xFFFFC00000005000, 0xFFFFC00000009000]
CODE, DATA, ALIAS, BARRIER = 0x400000, 0x402000, 0x410000, 0x417000
CS, SS = 0x3B, 0x33
USER_FLAGS = 0x200 | 0x8D7 | (1 << 16)
REGISTERS = ("R15", "R14", "R13", "R12", "R11", "R10", "R9", "R8",
             "RDI", "RSI", "RBP", "RDX", "RCX", "RBX", "RAX")
NEGATIVE_ERRORS = [0, 0, 0, 0, 1, 1, 1, 2, 3, 4, 4, 6, 5, 3, 7, 7,
                   9, 6, 3, 9, 9, 9]


def number(text, name, failure):
    match = re.search(r"\b" + name + r"\s*=\s*([0-9a-fA-F]+)", text)
    if not match:
        raise failure("SMP scheduler QMP is missing register " + name)
    return int(match[1], 16)


class VirtualMemory:
    """Snapshot-scoped cache of raw virtual pages; VM stays stopped."""

    def __init__(self, qmp, failure, cpu=0):
        self.qmp, self.failure, self.cpu, self.pages = qmp, failure, cpu, {}

    def read(self, address, length):
        output = bytearray()
        while length:
            page = address & ~0xFFF
            if page not in self.pages:
                self.pages[page] = struct.pack("<512Q", *_words(self.qmp, self.cpu, page, 512, self.failure))
            offset = address & 0xFFF
            count = min(length, 4096 - offset)
            output += self.pages[page][offset:offset + count]
            address += count
            length -= count
        return bytes(output)

    def words(self, address, count):
        return list(struct.unpack("<" + "Q" * count, self.read(address, count * 8)))

    def u64(self, address):
        return int.from_bytes(self.read(address, 8), "little")

    def u32(self, address):
        return int.from_bytes(self.read(address, 4), "little")

    def byte(self, address):
        return self.read(address, 1)[0]


def raw64(row, offset=0):
    return struct.unpack_from("<Q", row, offset)[0]


def raw32(row, offset=0):
    return struct.unpack_from("<I", row, offset)[0]


def space_raw(row, offset=0):
    slot, generation = struct.unpack_from("<QQ", row, offset)
    return (generation << 8) | (slot + 1) if generation else 0


def thread_raw(row, offset=0):
    slot = raw64(row, offset)
    generation = raw32(row, offset + 8)
    return (generation << 32) | (slot + 1)


class Observer:
    def __init__(self, qmp, case, symbols, deadline, failure, output):
        self.qmp, self.case, self.symbols = qmp, case, symbols
        self.deadline, self.failure, self.output = deadline, failure, output
        self.cpus = CASES[case]
        self.trace_limit = 512 if case in DEVICE else 256
        self.trace_stride = self.trace_limit * 256
        self.baseline = self.prepared = None
        self.native, self.irq, self.invalidation = {}, {}, {}
        self.concurrent = self.blocked = self.migrated = self.quiesced = False
        self.negatives = False
        self.runtime_checks = 0
        self.device_extension = None
        if case in DEVICE:
            from device_qmp import Extension
            self.device_extension = Extension(self)
        self.authority_extension = None
        if case in AUTHORITY:
            from authority_qmp import Extension
            self.authority_extension = Extension(self)

    def require(self, condition, message):
        if not condition:
            raise self.failure("SMP scheduler QMP: " + message)

    def symbol(self, name):
        self.require(name in self.symbols, "missing symbol " + name)
        return self.symbols[name]

    def array(self, memory, name, count):
        return memory.words(self.symbol(name), count)

    def leaf(self, physical, root, address):
        return _walk(physical, root, address, self.failure)

    def mapped(self, physical, root, address, flags, backing=None):
        walk = self.leaf(physical, root, address)
        self.require(walk["present"], f"absent required mapping {root:#x}:{address:#x}")
        leaf = walk["leaf"]
        self.require(leaf & (P | W | U | NX) == flags,
                     f"wrong leaf permissions {root:#x}:{address:#x}: {leaf:#x}")
        self.require(all(raw & P and (not flags & U or raw & U)
                         and (not flags & W or raw & W)
                         and (flags & NX or not raw & NX)
                         for _, _, raw in walk["chain"]), "ancestor permissions deny the expected leaf")
        if backing is not None:
            self.require(leaf & PA == backing & ~0xFFF, "mapping points to foreign backing")
        return (leaf & PA) + (address & 0xFFF)

    def inventory(self, memory, physical, descriptor):
        d = descriptor
        self.require(d[0] == 1 and d[3] == (18 if self.authority_extension else 16) and d[11] == 64 and d[20] == 4
                     and d[28] == 2 and d[34:38] == [16, 16, 24, 24], "unknown paging schema")
        frames = []
        for index in range(d[3]):
            row = memory.read(d[1] + index * d[2], d[2])
            if row[d[6]]:
                frames.append({"slot": index, "physical": raw64(row, d[4]),
                               "generation": raw64(row, d[5]), "initialized": row[d[7]],
                               "owner_kind": raw64(row, d[8]), "owner": space_raw(row, d[8] + 8)})
        mappings = []
        authority_offsets = self.authority_extension.mapping_offsets(memory) if self.authority_extension else None
        for index in range(d[11]):
            row = memory.read(d[9] + index * d[10], d[10])
            if row[d[17]]:
                mappings.append({"slot": index, "space": space_raw(row, d[12]),
                                 "address": raw64(row, d[13]), "frame": list(struct.unpack_from("<QQ", row, d[14])),
                                 "write": row[d[15]], "execute": row[d[15] + 1],
                                 "object": raw64(row, d[16])})
                if authority_offsets is not None:
                    mappings[-1].update({"object_id": space_raw(row, d[16] + 8),
                                         "grant": raw64(row, authority_offsets[0]),
                                         "revoke_epoch": raw64(row, authority_offsets[1]),
                                         "revoke_targets": raw64(row, authority_offsets[2])})
        spaces = []
        for index in range(d[28]):
            row = memory.read(d[26] + index * d[27], d[27])
            spaces.append({"slot": index, "generation": raw64(row, d[29]), "live": row[d[30]],
                           "root": raw64(row, d[31]), "pins": row[d[32]], "refs": row[d[33]]})
        memory_objects = []
        if self.authority_extension:
            memory_objects = self.authority_extension.memory_objects(memory, d)
        else:
            for index in range(d[20]):
                row = memory.read(d[18] + index * d[19], d[19])
                self.require(not row[d[22]] and not row[d[23]] and not row[d[38]], "unexpected MemoryObject lifetime")
        allocator = d[40:48]
        total, free = memory.u64(allocator[0]), memory.u64(allocator[1])
        bitmap = memory.read(allocator[2], ((total + 63) // 64) * 8)
        self.require(len(bitmap) <= allocator[3] * 8 and free <= total
                     and sum(byte.bit_count() for byte in bitmap) == total - free,
                     "allocator bitmap disagrees with raw counters")
        regions = memory.read(allocator[4], allocator[5] * 24)
        kernel_start, kernel_end = memory.u64(allocator[6]), memory.u64(allocator[7])
        spans = []
        for offset in range(0, len(regions), 24):
            start, length, kind = struct.unpack_from("<QQQ", regions, offset)
            if kind == 0:
                end, start = start + length, max(start, 4096)
                spans += [(a, b) for a, b in ((start, min(end, kernel_start)),
                                             (max(start, kernel_end), end)) if a < b]
        self.require(sum((end - start) // 4096 for start, end in spans) == total,
                     "allocator total disagrees with physical memory map")

        def allocated(address):
            ordinal = 0
            for start, end in spans:
                if start <= address < end:
                    ordinal += (address - start) // 4096
                    return bool(bitmap[ordinal // 8] & (1 << (ordinal % 8)))
                ordinal += (end - start) // 4096
            return False

        table_layout = d[48:54]
        tables = []
        for index in range(table_layout[2]):
            row = memory.read(table_layout[0] + index * table_layout[1], table_layout[1])
            address = raw64(row, table_layout[3])
            if address:
                tables.append({"slot": index, "physical": address,
                               "level": row[table_layout[4]], "pinned": row[table_layout[5]]})
        addresses = [f["physical"] for f in frames] + [t["physical"] for t in tables]
        self.require(len(set(addresses)) == len(addresses) and all(allocated(a) for a in addresses),
                     "duplicate or unallocated frame/table ownership")
        self.require(total - free == len(addresses), "allocated page has no raw frame/table owner")
        self.require(all(f["initialized"] == 1 and (f["owner_kind"] == 0 or
                         self.authority_extension and f["owner_kind"] == 1 and
                         any(o["id"] == f["owner"] for o in memory_objects)) for f in frames),
                     "uninitialized or foreign private frame")
        for mapping in mappings:
            owner = next((f for f in frames if f["slot"] == mapping["frame"][0]
                          and f["generation"] == mapping["frame"][1]), None)
            private = owner is not None and owner["owner_kind"] == 0 and owner["owner"] == mapping["space"] and mapping["object"] == 0
            shared = self.authority_extension and owner is not None and owner["owner_kind"] == 1 and mapping["object"] == 1 and owner["owner"] == mapping["object_id"]
            self.require(private or shared,
                         "mapping refers to stale or foreign backing")
            root = spaces[(mapping["space"] & 0xFF) - 1]["root"]
            flags = P | (W if mapping["write"] else 0) | (0 if mapping["execute"] else NX)
            walk = self.leaf(physical, root, mapping["address"])
            if mapping.get("revoke_epoch"):
                self.require(shared and mapping["grant"] and mapping["revoke_targets"] == (1 << self.cpus) - 1
                             and not walk["present"], "pending revocation does not retain a removed mapping")
            else:
                self.require(walk["present"] and walk["leaf"] & PA == owner["physical"]
                             and walk["leaf"] & (P | W | NX) == flags, "registry mapping disagrees with the actual PTE")
        for frame in frames:
            self.require(frame["owner_kind"] == 1 or any(m["frame"] == [frame["slot"], frame["generation"]] for m in mappings),
                         "live private frame has no mapping owner")
        table_by_address = {t["physical"]: t for t in tables}
        reachable = set()

        def visit(address, level):
            self.require(address in table_by_address and table_by_address[address]["level"] == level,
                         "hardware table walk reaches an unowned or incorrectly typed table")
            if address in reachable:
                return
            reachable.add(address)
            if level > 1:
                for entry in physical.words(address, 512):
                    if entry & P:
                        self.require(not entry & (1 << 7), "unexpected huge page in SMP root")
                        visit(entry & PA, level - 1)

        visit(self.baseline["kernel_root"] if self.baseline else memory.u64(self.symbol("neu_smp_sched_evidence") + 24), 4)
        for space in spaces:
            if space["live"] and space["root"]:
                visit(space["root"], 4)
        self.require(reachable == set(table_by_address), "allocated page-table inventory is not reachable from owned roots")
        return {"total": total, "free": free, "allocated": total - free,
                "bitmap": bitmap.hex(), "bitmap_sha256": hashlib.sha256(bitmap).hexdigest(),
                "frames": frames, "mappings": mappings, "spaces": spaces, "tables": tables,
                "memory_objects": memory_objects}

    def frame_valid(self, frame, slot, interrupted=False):
        self.require(len(frame) == 20 and CODE <= frame[15] < CODE + 4096
                     and frame[16] == CS and frame[19] == SS
                     and USTACK[slot] <= frame[18] <= USTACK[slot] + 4088 and frame[18] % 8 == 0
                     and frame[17] & 0x202 == 0x202
                     and frame[17] & ~(0x8D7 | 0x200 | ((1 << 16) if interrupted else 0)) == 0,
                     "invalid user return frame/flags/stack")
        if not interrupted and frame[15] != CODE:
            self.require(frame[12] == frame[15] and frame[4] == frame[17], "native continuation lost RCX/R11")

    def stack(self, memory, address, layout, slot, root):
        self.require(address != 0, "live Thread has no owned stack binding")
        row = memory.read(address, layout[1])
        self.require(thread_raw(row, layout[2]) == THREADS[slot]
                     and space_raw(row, layout[3]) == SPACES[slot]
                     and [raw64(row, layout[i]) for i in (4, 5, 6)] == [root, KSTACK[slot], KSTACK[slot] + 8192],
                     "raw stack binding belongs to a different Thread/root")

    def scheduler(self, memory, cpu_words, live, phase):
        queues = self.array(memory, "neu_smp_sched_queues", 72)
        contexts = self.array(memory, "neu_smp_sched_contexts", 36)
        threads = [self.array(memory, "neu_smp_sched_threads", 96)[i * 32:(i + 1) * 32] for i in range(3)]
        values = self.array(memory, "neu_smp_sched_value_layout", 22)
        self.require(values[0] == 1 and contexts[:4] == [1, 4, 8, 0]
                     and queues[0] == 1 and queues[2:4] == [4, 3]
                     and memory.u64(queues[1]) == self.cpus, "invalid scheduler/context schema")
        roots = [self.prepared["roots"][0], self.prepared["roots"][1], self.prepared["roots"][0]] if self.prepared else None
        seen, running, parsed_queues, owners, active_slots = set(), set(), [], [], []
        for cpu in range(4):
            q = queues[16 + cpu * 7:23 + cpu * 7]
            length = memory.u64(q[0])
            self.require(length <= 3 and (cpu < self.cpus or length == 0), "invalid raw runnable queue length")
            ids = []
            for index in range(3):
                slot_addr, gen_addr = q[1 + index * 2:3 + index * 2]
                if index < length:
                    self.require(slot_addr and gen_addr, "hole in runnable FIFO")
                    thread = (memory.u32(gen_addr) << 32) | (memory.u64(slot_addr) + 1)
                    self.require(thread in THREADS and thread not in seen, "duplicate/stale queued Thread")
                    seen.add(thread)
                    ids.append(thread)
                else:
                    self.require(slot_addr == gen_addr == 0, "queue has an entry beyond its length")
            parsed_queues.append(ids)
            a = contexts[4 + cpu * 8:12 + cpu * 8]
            if a[0]:
                thread = (memory.u32(a[1]) << 32) | (memory.u64(a[0]) + 1)
                node = (memory.u32(a[3]) << 32) | (memory.u64(a[2]) + 1)
                self.require(cpu < self.cpus and thread in THREADS and thread not in running and thread not in seen,
                             "duplicate or foreign CPU execution slot")
                slot = THREADS.index(thread)
                root = memory.u64(a[4])
                self.require(node == NODES[slot] and memory.u64(a[5]) == USTACK[slot] + 4088
                             and memory.u64(a[6]) == CODE and space_raw(memory.read(a[7], 16)) == SPACES[slot],
                             "raw CPU slot has a foreign Node/context")
                if roots:
                    self.require(root == roots[slot], "CPU slot uses a foreign root")
                running.add(thread)
                active_slots.append({"cpu": cpu, "thread": thread, "node": node, "root": root})
                if live[cpu]["cpl"] == 3:
                    self.require(live[cpu]["cr3"] == root and cpu_words[cpu][4] == thread
                                 and cpu_words[cpu][5] == node and live[cpu]["gs"] == 0
                                 and live[cpu]["cs"] == CS and live[cpu]["ss"] == SS
                                 and live[cpu]["rfl"] & 0x200 and live[cpu]["cr0"] & 12 == 12
                                 and live[cpu]["efer"] & 1, "actual Ring 3 CPU disagrees with its production slot")
                    self.require(CODE <= live[cpu]["rip"] < CODE + 4096, "Ring 3 CPU escaped its executable payload")
                    self.require(KSTACK[slot] == cpu_words[cpu][9] and KSTACK[slot] + 8192 == cpu_words[cpu][10]
                                 and USTACK[slot] <= live[cpu]["rsp"] < USTACK[slot] + 4096,
                                 "user CPU has a foreign kernel/user stack")
            else:
                self.require(not any(a), "partial empty CPU slot")
                if cpu < self.cpus:
                    self.require(live[cpu]["cpl"] == 0, "Ring 3 CPU has no owner")
        for slot, row in enumerate(threads):
            self.require(row[0] == THREADS[slot] and row[1] == NODES[slot], "wrong Thread publication identity")
            raw_id = (memory.u32(row[12]) << 32) | (memory.u64(row[11]) + 1)
            node = (memory.u32(row[27]) << 32) | (memory.u64(row[26]) + 1)
            root = memory.u64(row[15])
            state, queued = memory.byte(row[13]), memory.byte(row[14])
            self.require(raw_id == THREADS[slot] and node == NODES[slot]
                         and space_raw(memory.read(row[28], 16)) == SPACES[slot]
                         and memory.u64(row[29]) == CODE and memory.u64(row[30]) == USTACK[slot] + 4088,
                         "actual Thread fields differ from its Node/Space/entry")
            if roots:
                self.require(root == roots[slot], "actual Thread root differs from its owning Node")
            owner = queues[44 + slot * 4:48 + slot * 4]
            owned_cpu = None
            if owner[0]:
                identity = (memory.u32(owner[1]) << 32) | (memory.u64(owner[0]) + 1)
                owned_cpu, affinity = memory.u64(owner[2]), memory.u64(owner[3])
                self.require(identity == THREADS[slot] and owned_cpu < self.cpus
                             and affinity and affinity >> self.cpus == 0 and affinity & (1 << owned_cpu),
                             "invalid raw owner or affinity")
                owners.append([identity, owned_cpu, affinity])
            else:
                self.require(not any(owner), "partial empty owner slot")
            stopping = queues[56 + (0 if slot != 1 else 1) * 3:59 + (0 if slot != 1 else 1) * 3]
            stopped = stopping[0] != 0
            if state in (2, 3, 4):
                self.require(owned_cpu is not None, "runnable/running/blocked Thread has no scheduler owner")
                self.stack(memory, row[31], values, slot, root)
            if state == 2:
                self.require(queued == 1 and row[16] and THREADS[slot] not in running
                             and ((THREADS[slot] in seen) == (not stopped)), "runnable Thread/FIFO disagreement")
                if not stopped:
                    self.require(THREADS[slot] in parsed_queues[owned_cpu], "queued Thread on a foreign CPU")
            elif state == 3:
                self.require(not queued and not row[16] and THREADS[slot] in running
                             and any(a["thread"] == THREADS[slot] and a["cpu"] == owned_cpu for a in active_slots),
                             "running Thread/context disagreement")
            elif state == 4:
                self.require(not queued and row[16] and THREADS[slot] not in seen | running,
                             "blocked Thread remains runnable")
            elif state in (5, 6):
                self.require(not queued and not row[16] and owned_cpu is None and THREADS[slot] not in seen | running,
                             "terminal Thread still owns execution/queue/context")
            else:
                self.require(phase == 5 and state == 1 and owned_cpu is None,
                             "unexpected unadmitted Thread state")
            if row[16]:
                base = row[16] - values[10]
                saved = memory.read(base, values[7])
                self.require(thread_raw(saved, values[8] + values[16]) == THREADS[slot]
                             and thread_raw(saved, values[8] + values[15]) == NODES[slot],
                             "saved context has a foreign active identity")
                context = values[8] + values[17]
                self.require(space_raw(saved, context + values[18]) == SPACES[slot]
                             and [raw64(saved, context + values[i]) for i in (19, 20, 21)]
                             == [root, CODE, USTACK[slot] + 4088], "saved ActiveContext lost its owning root/entry/stack")
                self.stack(memory, base + values[9], values, slot, root)
                flags = [saved[values[i]] for i in (11, 12, 13, 14)]
                self.require(flags[1] == 1 and not (flags[0] and (flags[2] or flags[3]))
                             and not (flags[2] and flags[3]) and (state != 4 or flags[3]),
                             "saved context has inconsistent fresh/IRQ/continuation modes")
                saved_frame = memory.words(row[16], 20)
                self.frame_valid(saved_frame, slot, bool(flags[2]))
                if flags[0]:
                    self.require(saved_frame == [0] * 15 + [CODE, CS, 0x202, USTACK[slot] + 4088, SS],
                                 "fresh saved context already contains execution state")
            row_record = {"id": raw_id, "node": node, "root": root, "state": state,
                          "queued": queued, "owner": owned_cpu, "saved_frame": row[16]}
            threads[slot] = {"words": row, **row_record}
        for index in range(2):
            stop = queues[56 + index * 3:59 + index * 3]
            if stop[0]:
                origin = (memory.u32(stop[1]) << 32) | (memory.u64(stop[0]) + 1)
                self.require(origin in running and any(a["thread"] == origin and a["root"] == memory.u64(stop[2])
                                                       for a in active_slots), "stop origin no longer owns a live CPU")
        counters = [memory.u64(a) for a in queues[4:10]]
        return {"queues": parsed_queues, "owners": owners, "active": active_slots, "threads": threads,
                "counters": counters, "stop": queues[56:62]}

    def authority(self, memory, scheduler, phase):
        if self.authority_extension:
            return self.authority_extension.authority(memory, scheduler, phase)
        d = self.array(memory, "neu_smp_sched_authority", 128)
        self.require(d[0] == 1 and d[3] == (9 if self.device_extension else 6) and d[13:17] == [8, 8, 1, 2], "unknown capability object schema")
        objects = []
        for slot in range(d[3]):
            row = memory.read(d[4] + slot * d[5], d[5])
            objects.append({"slot": slot, "generation": raw32(row, d[6]), "references": raw32(row, d[7]),
                            "pins": raw32(row, d[8]), "live": bool(row[d[9]]), "kind": row[d[10]],
                            "target": raw64(row, d[11])})
        handles, node_states = [], []
        for index in range(2):
            table = d[24 + index * 20:44 + index * 20]
            self.require((phase == 4 and not any(table)) or
                         (table[0] == 1 and table[5] == 8 and table[18] == NODES[index]), "foreign capability table")
            entries = []
            for slot in range(8 if any(table) else 0):
                row = memory.read(table[3] + slot * table[4], table[4])
                if row[table[7]]:
                    entries.append({"slot": slot, "generation": raw32(row, table[6]),
                                    "kind": row[table[8]], "target": raw64(row, table[9]),
                                    "rights": int.from_bytes(row[table[10]:table[10]+2], "little"), "transferable": row[table[11]]})
                    if self.device_extension:
                        entries[-1]["grant"] = raw64(row, self.device_extension.layout(memory)[15])
            handles.append(entries)
            node_fields = d[120 + index * 4:124 + index * 4]
            raw_id = (memory.u32(node_fields[1]) << 32) | (memory.u64(node_fields[0]) + 1)
            state = memory.byte(node_fields[2])
            self.require(raw_id == NODES[index], "actual capability owner Node identity mismatch")
            members = (0, 2) if index == 0 else (1,)
            has_live_thread = any(scheduler["threads"][slot]["state"] in (1, 2, 3, 4) for slot in members)
            self.require(state in (2, 3) if has_live_thread else state in (4, 6),
                         "Node lifecycle disagrees with its actual Thread states")
            if phase in (3, 4):
                self.require(state == (6 if phase == 4 else 4) and not entries, "terminal Node retains live authority")
            node_states.append(state)
        channel = d[64:96]
        self.require(channel[0] == 1 and channel[28:30] == [2, 2], "unknown Channel schema")
        channel_queues = []
        references = {}
        for table in handles:
            for handle in table:
                references[handle["target"]] = references.get(handle["target"], 0) + 1
        for side in range(2):
            base = channel[1] + channel[8] + side * channel[12]
            head, tail, length = [memory.u64(base + channel[i]) for i in (13, 14, 15)]
            self.require(head < 2 and tail < 2 and length <= 2 and (head + length) % 2 == tail,
                         "corrupt actual Channel FIFO")
            transfers = []
            for slot in range(2):
                transfer = base + channel[17] + slot * channel[18]
                is_live = memory.byte(transfer + channel[19])
                target = memory.u64(transfer + channel[20])
                rights = int.from_bytes(memory.read(transfer + channel[21], 2), "little")
                owned = memory.byte(transfer + channel[22])
                if is_live:
                    self.require((slot + 2 - head) % 2 < length and target and owned,
                                 "unowned capability transfer or transfer outside FIFO")
                    references[target] = references.get(target, 0) + 1
                    transfers.append([target, rights])
                else:
                    self.require(not target and not rights and not owned, "stale transfer in empty Channel slot")
            channel_queues.append({"head": head, "tail": tail, "len": length, "transfers": transfers})
        waits = []
        pinned = {}
        for slot in range(3):
            row = d[96 + slot * 8:104 + slot * 8]
            if row[0]:
                thread = (memory.u32(row[1]) << 32) | (memory.u64(row[0]) + 1)
                waiter = {"thread": thread, "token": memory.u64(row[2]), "state": memory.byte(row[3]),
                          "pinned": memory.byte(row[4]), "root": memory.u64(row[5]),
                          "kind": memory.byte(row[6]), "object": memory.u64(row[7])}
                self.require(waiter["token"] and waiter["pinned"] == 1 and thread in THREADS
                             and scheduler["threads"][THREADS.index(thread)]["state"] == 4,
                             "waiter lacks an actual blocked/pinned Thread")
                waits.append(waiter)
                pinned[waiter["object"]] = pinned.get(waiter["object"], 0) + 1
            else:
                self.require(not any(row), "partial empty waiter slot")
        for obj in objects:
            raw_id = (obj["kind"] << 40) | (obj["generation"] << 8) | (obj["slot"] + 1)
            self.require(obj["references"] == references.get(raw_id, 0) and obj["pins"] == pinned.get(raw_id, 0),
                         "object references/pins disagree with actual authority/Channel/wait owners")
            pending_space = obj["kind"] == 3 and obj["target"] in (0x101, 0x102) and phase != 4
            self.require(obj["live"] == bool(obj["references"] or obj["pins"] or pending_space),
                         "orphaned live capability object")
        counters = {name: memory.words(channel[1] + channel[i], 2)
                    for name, i in (("sent", 9), ("received", 10), ("discarded", 11))}
        if phase in (3, 4):
            self.require(not waits and not references and not pinned and not any(q["len"] for q in channel_queues),
                         "terminal authority/Channel/wait storage is not empty")
        result = {"objects": objects, "handles": handles, "queues": channel_queues,
                  "waits": waits, "node_states": node_states, "counters": counters}
        if self.device_extension:
            self.device_extension.authority(memory, scheduler, phase, d, result)
        return result

    def descriptors(self, memory, physical, blocks, live, cpu_words, phase, root):
        records = []
        for cpu, block in enumerate(blocks):
            gdt, tss, idt = block[14], block[16], block[18]
            text = live[cpu]["text"]
            for name, address, limit in (("GDT", gdt, 0x3F), ("IDT", idt, 0xFFF)):
                match = re.search(r"\b" + name + r"=\s*([0-9a-fA-F]+)\s+([0-9a-fA-F]+)", text)
                self.require(match and int(match[1], 16) == address and int(match[2], 16) == limit,
                             "live descriptor register has a foreign CPU table")
            tr = re.search(r"\bTR\s*=\s*([0-9a-fA-F]+)\s+([0-9a-fA-F]+)\s+([0-9a-fA-F]+)", text)
            self.require(tr and int(tr[1], 16) == 0x18 and int(tr[2], 16) == tss and int(tr[3], 16) == 103,
                         "live task register does not name its private TSS")
            entries = memory.words(gdt, 8)
            expected = [0, 0x00AF9A000000FFFF, 0x00CF92000000FFFF,
                        103 | ((tss & 0xFFFFFF) << 16) | (0x8B << 40) | (((tss >> 24) & 0xFF) << 56),
                        tss >> 32, 0, 0x00CFF2000000FFFF, 0x00AFFA000000FFFF]
            self.require([v & ~(1 << 40) if i in (1, 2, 6, 7) else v for i, v in enumerate(entries)] == expected,
                         "private GDT descriptors differ from the documented kernel/user segments")
            rsp0, ist = memory.u64(tss + 4), memory.u64(tss + 36)
            self.require(ist == block[13] and memory.read(tss + 102, 2) == b"\x68\x00", "wrong double-fault IST/I/O bitmap")
            if phase in (0, 1, 3, 4, 5):
                self.require(rsp0 == block[10], "unarmed/stopped CPU still names a reclaimable kernel stack")
            elif live[cpu]["cpl"] == 3 or cpu_words[cpu][51] == 1 or cpu_words[cpu][61] == 1:
                self.require(rsp0 == cpu_words[cpu][10], "user entry TSS RSP0 is not its owned Thread stack")
            gates = {}
            for vector, name in ((224, "timer"), (241, "ipi"), (255, "spurious")) + (((240, "device"),) if self.device_extension else ()):
                gate = memory.read(idt + vector * 16, 16)
                handler = int.from_bytes(gate[:2], "little") | (int.from_bytes(gate[6:8], "little") << 16) | (int.from_bytes(gate[8:12], "little") << 32)
                self.require(gate[2:6] == b"\x08\x00\x00\x8e" and not any(gate[12:]), "unsafe scheduler IDT gate")
                if phase != 0:
                    self.require(handler == self.symbol("neu_smp_sched_" + name), "IDT points at a different interrupt bridge")
                gates[name] = handler
            for base, top in ((block[9], block[10]), (block[12], block[13])):
                self.require(base < top and self.leaf(physical, root, base)["present"]
                             and not self.leaf(physical, root, base - 4096)["present"]
                             and not self.leaf(physical, root, top)["present"], "unguarded private CPU stack/IST")
            records.append({"gdt": gdt, "tss": tss, "idt": idt, "rsp0": rsp0, "ist": ist, "gates": gates})
        self.require(all(len({r[key] for r in records}) == self.cpus for key in ("gdt", "tss", "idt", "ist")),
                     "CPUs share mutable descriptor/IST storage")
        return records

    def prepared_roots(self, memory, physical, inventory, evidence, threads):
        roots = evidence[6:8]
        kernel = evidence[3]
        self.require(len(set([kernel, *roots])) == 3 and all(r and r % 4096 == 0 for r in roots), "user roots are not private")
        self.require([s["live"] for s in inventory["spaces"]] == [1, 1]
                     and [s["root"] for s in inventory["spaces"]] == roots
                     and [s["pins"] for s in inventory["spaces"]] == [2, 1]
                     and [s["refs"] for s in inventory["spaces"]] == [1, 1], "prepared root pins/references are not owned")
        payload = memory.read(self.symbol("neu_smp_sched_payload"), self.symbol("neu_smp_sched_payload_end") - self.symbol("neu_smp_sched_payload"))
        self.require(memory.read(self.symbol("neu_smp_sched_invlpg_done") - 3, 3) == b"\x0f\x01\x3f"
                     and memory.read(self.symbol("neu_smp_sched_root_flush_done") - 3, 3) == b"\x0f\x22\xde",
                     "observed invalidation window does not follow INVLPG/CR3 instructions")
        data_pages = []
        for index, root in enumerate(roots):
            code = self.mapped(physical, root, CODE, P | U)
            self.require(physical.bytes(code, len(payload)) == payload, "Ring 3 code differs from the linked payload")
            self.mapped(physical, root, 0x401000, P | U | NX)
            data_pages.append(self.mapped(physical, root, DATA, P | W | U | NX))
            self.mapped(physical, root, BARRIER, P | W | U | NX, evidence[10])
            members = (0, 2) if index == 0 else (1,)
            directory = root
            for shift in (39, 30, 21):
                entries = physical.words(directory, 512)
                directory = entries[(CODE >> shift) & 511] & PA
                if shift == 39:
                    self.require(entries[0] & (P | W | U) == P | W | U, "user directory has unsafe permissions")
                else:
                    self.require({i for i, e in enumerate(entries) if e & P} == {(CODE >> shift) & 511},
                                 "unexpected user-directory branch")
            leaves = physical.words(directory, 512)
            expected_leaves = {0, 1, 2, 23} | {(USTACK[slot] - CODE) // 4096 for slot in members}
            if index == 0 and self.case in TLB:
                expected_leaves.add(16)
            if self.authority_extension:
                expected_leaves |= {16, 17} if index == 0 else {32, 33, 48, 49}
            self.require({i for i, e in enumerate(leaves) if e & P} == expected_leaves,
                         "user root contains an unregistered page or extra alias")
            device = self.leaf(physical, root, 0xFFFFB00000000000)
            self.require(device["present"] and device["leaf"] & (0x9F | NX) == 0x1B | NX
                         and device["leaf"] & PA == 0xFEE00000, "LAPIC is not supervisor RW/NX/UC")
            for slot in range(3):
                if slot in members:
                    self.mapped(physical, root, USTACK[slot], P | W | U | NX)
                    for address in range(KSTACK[slot], KSTACK[slot] + 8192, 4096):
                        self.mapped(physical, root, address, P | W | NX)
                else:
                    self.require(not self.leaf(physical, root, USTACK[slot])["present"]
                                 and not self.leaf(physical, root, KSTACK[slot])["present"], "foreign Thread stack mapped in Node")
                self.require(not self.leaf(physical, root, USTACK[slot] - 4096)["present"]
                             and not self.leaf(physical, root, USTACK[slot] + 4096)["present"]
                             and not self.leaf(physical, root, KSTACK[slot] - 4096)["present"]
                             and not self.leaf(physical, root, KSTACK[slot] + 8192)["present"], "Thread stack guard is mapped")
            self.require(not self.leaf(physical, root, 0x440000 if self.authority_extension else 0x420000)["present"], "unmapped user probe is present")
            kernel_text = self.leaf(physical, root, self.symbol("__text_start"))
            self.require(kernel_text["present"] and all(not raw & U for _, _, raw in kernel_text["chain"]), "kernel image exposed to Ring 3")
        self.require(data_pages[0] != data_pages[1], "unrelated Nodes share private DATA backing")
        for slot, record in enumerate(threads):
            self.require(record["words"][10] == data_pages[0 if slot != 1 else 1] + slot * 128,
                         "Thread DATA record has foreign physical backing")
        if self.case in TLB:
            self.mapped(physical, roots[0], ALIAS, P | W | U | NX, data_pages[0])
            self.require(not self.leaf(physical, roots[1], ALIAS)["present"], "private TLB alias mapped in unrelated Node")
        return {"roots": roots, "data": data_pages, "barrier": evidence[10], "inventory": inventory}

    def probes(self, memory, physical, live, cpu_words, scheduler, evidence):
        for cpu in range(self.cpus):
            local = cpu_words[cpu]
            for name, probe, pointer, dest, vector in (("native", 61, 62, self.native, 128),
                                                      ("irq", 51, 52, self.irq, 224)):
                if local[probe] != 1 or cpu in dest:
                    continue
                regs = live[cpu]
                # Publication precedes the call by a few instructions. Only
                # accept the real wait window; terminal acceptance still
                # requires every mandatory hardware-frame observation.
                if not self.symbol("neu_smp_sched_probe_begin") <= regs["rip"] < self.symbol("neu_smp_sched_probe_end"):
                    continue
                self.require(self.symbol("neu_smp_sched_probe_begin") <= regs["rip"] < self.symbol("neu_smp_sched_probe_end")
                             and regs["cpl"] == 0 and not regs["rfl"] & 0x600
                             and regs["cr3"] == local[7] and regs["gs"] == 0,
                             "hardware frame probe is not stopped inside the real handler")
                slot = THREADS.index(local[4])
                self.require(local[pointer] == KSTACK[slot] + 8192 - 176
                             and KSTACK[slot] <= regs["rsp"] < local[pointer], "actual normalized frame is outside its owned stack")
                raw = VirtualMemory(self.qmp, self.failure, cpu).words(local[pointer], 22)
                frame = raw[:15] + raw[17:22]
                self.require(raw[15:17] == [vector, 0], "wrong normalized hardware vector/error")
                self.frame_valid(frame, slot, name == "irq")
                trace = memory.words(self.symbol("neu_smp_sched_traces") + cpu * self.trace_stride + (local[25] - 1) * 256, 32)
                self.require(trace[10:30] == frame and trace[4] == vector and trace[6] == local[pointer]
                             and trace[7] == cpu and trace[30:32] == ([1, 0] if name == "irq" else [0, 0]),
                             "saved trace is not the independently read hardware frame")
                if name == "native":
                    self.require(frame[14] == 1 and frame[15] == self.site("neu_smp_sched_query_resume"), "first SYSCALL was not the ABI query")
                dest[cpu] = {"cpu": cpu, "thread": local[4], "root": regs["cr3"], "registers": regs,
                             "address": local[pointer], "normalized": raw, "frame": frame, "trace": trace}
                self.output.append({"stage": name + "-probe", **dest[cpu]})
            if self.case in TLB and local[54] == 1 and cpu not in self.invalidation:
                regs = live[cpu]
                self.require(self.symbol("neu_smp_sched_tlb_probe_begin") <= regs["rip"] < self.symbol("neu_smp_sched_tlb_probe_end")
                             and regs["cpl"] == 0 and not regs["rfl"] & 0x600, "invalidation not observed after its actual instruction")
                self.require(number(regs["text"], "RDI", self.failure) == ALIAS
                             and number(regs["text"], "RSI", self.failure) == regs["cr3"]
                             and number(regs["text"], "RDX", self.failure) == local[35]
                             and number(regs["text"], "R8", self.failure) == 1
                             and local[32:35] == [1, ALIAS, regs["cr3"]] and local[36] == 0,
                             "invalidation CPU/root/epoch/instruction arguments disagree")
                self.require(local[35] == (1 if regs["cr3"] == evidence[6] else 2), "wrong INVLPG/root-switch choice")
                tlb = self.array(memory, "neu_smp_sched_tlb", 16)
                self.require(memory.u64(tlb[12]) & (1 << cpu) == 0, "CPU acknowledged before the observed invalidation window")
                self.mapped(physical, evidence[6], ALIAS, P | U | NX, self.prepared["data"][0])
                self.invalidation[cpu] = {"cpu": cpu, "mode": local[35], "root": regs["cr3"], "epoch": local[32], "registers": regs}
                self.output.append({"stage": "tlb-invalidation", **self.invalidation[cpu]})
        if self.case in FAULT:
            for cpu in range(self.cpus):
                if cpu_words[cpu][53] == 2 and not self.quiesced:
                    if not self.symbol("neu_smp_sched_probe_begin") <= live[cpu]["rip"] < self.symbol("neu_smp_sched_probe_end"):
                        continue
                    self.require(self.symbol("neu_smp_sched_probe_begin") <= live[cpu]["rip"] < self.symbol("neu_smp_sched_probe_end"),
                                 "quiescence checkpoint is not at the pre-commit fault wait")
                    origin = THREADS[2]
                    active = scheduler["active"]
                    self.require(any(a["thread"] == origin and a["cpu"] == cpu for a in active)
                                 and all(a["thread"] != THREADS[0] for a in active)
                                 and scheduler["threads"][0]["state"] == (4 if self.case == "smp-authority-terminate" else 5)
                                 and scheduler["threads"][2]["state"] == 3,
                                 "fault commits while a sibling still owns an execution slot")
                    self.quiesced = True
                    self.output.append({"stage": "fault-quiesced", "registers": live, "scheduler": scheduler})

    def site(self, name):
        return CODE + self.symbol(name) - self.symbol("neu_smp_sched_payload")

    def terminal(self, memory, physical, live, cpu_words, scheduler, authority, inventory, evidence):
        if self.device_extension:
            self.device_extension.terminal(memory, physical, cpu_words, scheduler, authority, inventory, evidence)
        if self.authority_extension:
            self.authority_extension.terminal(memory, physical, cpu_words, scheduler, authority, inventory, evidence)
        self.require(scheduler["counters"][0] >= 40 and scheduler["counters"][1] >= 40
                     and scheduler["counters"][5] == (2 if self.case == "smp-authority-terminate" else 3) and not scheduler["active"]
                     and all(not q for q in scheduler["queues"]), "scheduler did not preempt and terminate three Threads")
        probes_required = self.case != "smp-schedule-stress"
        self.require((not probes_required or set(self.native) == set(self.irq) == {0, 1}) and self.concurrent,
                     "missing direct native/IRQ/same-Node concurrent evidence")
        self.require(not any(self.array(memory, "NEU_LAST_FAULT", 24)), "contained user fault touched the fatal kernel record")
        if self.case in MIGRATE:
            self.require(self.migrated and scheduler["counters"][4] == 1, "migration did not reach CPU3")
        if self.case in IPC:
            self.require(self.blocked and scheduler["counters"][2:4] == [1, 1], "blocking/wakeup did not commit exactly once")
            self.require(authority["counters"]["sent"] == [2, 0] and authority["counters"]["received"] == [0, 2]
                         and authority["counters"]["discarded"] == [0, 0], "scalar/capability messages were lost or duplicated")
        if self.case in FAULT:
            self.require(self.quiesced and (evidence[56] == 0 if self.case == "smp-authority-terminate" else evidence[56] != 0)
                         and evidence[57:59] == [THREADS[2], 1], "missing cross-CPU fault containment")
        data = [physical.words(self.prepared["data"][0 if slot != 1 else 1] + slot * 128, 16) for slot in range(3)]
        self.require([s["pins"] for s in inventory["spaces"]] == [2, 1]
                     and [s["refs"] for s in inventory["spaces"]] == [1, 1],
                     "terminal observation lost execution pins before all CPUs were stopped")
        for slot, row in enumerate(data):
            self.require(row[0] > 0 and row[1] == 0x1122334455667788
                         and row[2] == (0x7000003FF if self.device_extension else (0x6000001FF if self.authority_extension else 0x5000000FF))
                         and (self.authority_extension or row[10] == 11) and row[7] != 0,
                         "Thread lost progress/register canary/ABI/inspection result")
            if self.case not in FAULT or slot == 1:
                self.require(row[4] == slot + 1 and scheduler["threads"][slot]["words"][7:9] == [1, 128], "wrong normal Thread outcome")
        if self.case == "smp-device-disabled":
            self.require(all(row[13] == 7 for row in data), "legacy ABI accepted a device operation")
        if self.case == "smp-authority-disabled":
            self.require(all(row[13] == 2 for row in data), "legacy ABI did not reject both authority operations on every Thread")
        if self.case == "smp-schedule-shared":
            shared = physical.word(self.prepared["data"][0] + 0x300)
            self.require(data[0][5] == data[2][5] == 8 and shared == data[0][0] + data[2][0],
                         "concurrent same-Node atomic increments were lost or fabricated")
        if self.case in IPC:
            self.require(data[1][3] == 0x1101 and data[0][11] == (-17 & MASK64)
                         and data[1][11] == 1 and physical.word(self.prepared["barrier"] + 40) == 1,
                         "scalar receipt, stale moved handle or transferred Flag result missing")
        if self.case in FAULT:
            fault_vector = 128 if self.case == "smp-schedule-reject" else 14
            self.require(scheduler["threads"][0]["words"][7:9] == [3, THREADS[2]]
                         and scheduler["threads"][2]["words"][7:9] == [2, fault_vector]
                         and physical.word(self.prepared["barrier"] + 32) == 1,
                         "wrong fault/sibling/survivor outcome")
        if self.case == "smp-schedule-reject":
            self.require([row[5] for row in data] == [6, 5, 10]
                         and scheduler["threads"][2]["words"][9] == 0,
                         "malformed-return fixture or outcome changed")
        if self.case in TLB:
            self.require(set(self.invalidation) == set(range(self.cpus)) and evidence[43:45] == [1, (1 << self.cpus) - 1],
                         "not every target actually invalidated before commit")
            self.require(data[0][8] == 1 and data[2][8] == 3
                         and physical.word(self.prepared["data"][0] + 0x200) == 1
                         and physical.word(self.prepared["data"][0] + 0x210) == 3,
                         "warmed alias or denied store changed scratch backing")
            self.mapped(physical, evidence[6], ALIAS, P | U | NX, self.prepared["data"][0])
            self.require(evidence[41] & ~W & ~AD == evidence[42] & ~AD and evidence[41] & W and not evidence[42] & W
                         and physical.word(evidence[40]) & ~AD == evidence[42] & ~AD, "actual permission transaction changed more than write access")
            tlb = self.array(memory, "neu_smp_sched_tlb", 16)
            self.require([memory.u64(a) for a in tlb[1:6]] == [1, evidence[6], ALIAS, (1 << self.cpus) - 1, (1 << self.cpus) - 1]
                         and memory.byte(tlb[6]) == memory.byte(tlb[7]) == 0, "actual shootdown coordinator did not commit cleanly")
        traces, entries, rejected = [], [], []
        self.require(sum(local[17] for local in cpu_words) == int(self.case in CPU_FAULT)
                     and sum(local[55] for local in cpu_words) == int(self.case == "smp-schedule-reject"),
                     "CPU faults and rejected user returns were not contained exactly once")
        for cpu, local in enumerate(cpu_words):
            self.require(local[3] == 5 and local[37] == 0 and local[12] + local[24] == local[13]
                         and local[14] == local[15] and live[cpu]["cr3"] == evidence[3]
                         and live[cpu]["gs"] == self.symbol("neu_smp_cpus") + cpu * 384
                         and not live[cpu]["efer"] & 1, "CPU did not restore its root/GS/MSR or pair IRQ acknowledgements")
            bridges = memory.words(self.symbol("neu_smp_sched_cpus") + cpu * 832 + 672, 16)
            if local[11]:
                self.require(bridges[3] == evidence[3] and bridges[4] == live[cpu]["cr0"]
                             and bridges[5] == live[cpu]["efer"] and bridges[6] == live[cpu]["gs"], "bridge did not restore saved kernel controls")
            boundary_rows = memory.words(self.symbol("neu_smp_sched_traces") + cpu * self.trace_stride, local[25] * 32)
            entry_rows = memory.words(self.symbol("neu_smp_sched_entries") + cpu * self.trace_stride, local[11] * 32)
            cpu_traces = [boundary_rows[i:i + 32] for i in range(0, len(boundary_rows), 32)]
            cpu_entries = [entry_rows[i:i + 32] for i in range(0, len(entry_rows), 32)]
            self.require(len(cpu_traces) >= len(cpu_entries), "an entered slice lacks an actual exit/IRQ trace")
            for index, row in enumerate(cpu_entries, 1):
                self.require(row[0] == index and row[4] == cpu and row[1] in THREADS
                             and row[9] != cpu + 1 and row[8] == self.symbol("neu_smp_cpus") + cpu * 384,
                             "entry held its CPU's lock or used foreign CPU-local storage")
                slot = THREADS.index(row[1])
                self.require(row[2:4] == [NODES[slot], self.prepared["roots"][0 if slot != 1 else 1]]
                             and row[6:8] == [KSTACK[slot], KSTACK[slot] + 8192], "entry had foreign Thread/Node/root/stack")
                self.frame_valid(row[10:30], slot, bool(row[5]))
            for row in cpu_traces:
                # The generic exception stub explicitly clears both GS MSRs
                # before calling Rust. Native/IRQ stubs retain KERNEL_GS_BASE.
                expected_gs = self.symbol("neu_smp_sched_cpus") + cpu * 832 + 672 if row[4] in (128, 224, 241) or self.device_extension and row[4] == 240 else 0
                self.require(row[0] > 0 and row[1] in THREADS and row[7] == cpu and row[8] == expected_gs,
                             "exit trace has foreign CPU/GS bridge: " + repr(row[:10]))
                slot = THREADS.index(row[1])
                self.require(row[2:4] == [NODES[slot], self.prepared["roots"][0 if slot != 1 else 1]], "exit trace has foreign Node/root")
                if row[4] in (224, 241) or self.device_extension and row[4] == 240:
                    self.require(row[9] == 0 and row[30:32] == [1, 0], "IRQ lacks ISR-to-EOI evidence")
                elif row[4] == 128:
                    self.require(row[9] == 0 and row[30:32] == [0, 0], "SYSCALL was counted as hardware IRQ")
                    if self.case == "smp-schedule-reject" and slot == 2 and row[25] == self.site("neu_smp_sched_reject_resume"):
                        self.require(cpu == 1 and row[0] == local[11] and row[5] == 0
                                     and row[24] == 1 and row[28] == USTACK[slot] + 4089
                                     and row[27] & 0x400 and row[22] == row[25] and row[14] == row[27],
                                     "rejected SYSCALL lacks the actual unsafe user RSP/DF/return site")
                        address = KSTACK[slot] + 8192 - 176
                        walked = self.leaf(physical, row[3], address)
                        self.require(walked["present"] and row[6] == address,
                                     "rejected return's actual normalized stack frame was reclaimed early")
                        normalized = physical.words((walked["leaf"] & PA) + (address & 4095), 22)
                        self.require(normalized[:15] + normalized[17:] == row[10:30]
                                     and normalized[15:17] == [128, 0],
                                     "rejection trace differs from the actual malformed normalized frame")
                        rejected.append({"cpu": cpu, "address": address, "normalized": normalized, "trace": row})
                else:
                    wanted_error = 6 if self.authority_extension else 7
                    wanted_address = 0xffffb00000001000 if self.case in ("smp-device-fault", "smp-device-flight-fault") else 0x440000 if self.case == "smp-authority-terminate" else (0x410010 if self.authority_extension else (0x410210 if self.case in TLB else self.symbol("__text_start")))
                    wanted_site = "neu_authority_pending_fault_site" if self.case == "smp-authority-terminate" else ("neu_authority_fault_site" if self.authority_extension else ("neu_smp_sched_tlb_fault_site" if self.case in TLB else "neu_smp_sched_fault_site"))
                    self.require(self.case in CPU_FAULT and slot == 2 and row[4] == 14
                                 and row[9] == wanted_error and row[5] == wanted_address
                                 and row[25] == self.site(wanted_site),
                                 "fault vector/error/CR2/site differs from the actual denied user access")
                    address = KSTACK[slot] + 8192 - 176
                    walked = self.leaf(physical, row[3], address)
                    self.require(walked["present"], "terminal fault's actual stack frame was reclaimed early")
                    normalized = physical.words((walked["leaf"] & PA) + (address & 4095), 22)
                    self.require(normalized[:15] + normalized[17:] == row[10:30]
                                 and normalized[15:17] == [14, wanted_error]
                                 and KSTACK[slot] <= row[6] < address,
                                 "fault trace differs from its actual normalized hardware frame")
                    self.frame_valid(row[10:30], slot, True)
            traces.append(cpu_traces)
            entries.append(cpu_entries)
        self.require(len(rejected) == int(self.case == "smp-schedule-reject"),
                     "malformed return was not independently observed exactly once")
        # Reconstruct every per-Thread context boundary across CPU ownership.
        # Migration is bounded to one CPU1 -> CPU3 transfer, so each Thread's
        # ordered execution stream is unambiguous without a guest global clock.
        streams = {}
        for slot, thread in enumerate(THREADS):
            cpu_order = [1, 3] if slot == 2 and self.case in MIGRATE else ([1] if slot == 2 else ([2] if slot == 1 and self.cpus == 4 else [0]))
            stream = [(cpu, row) for cpu in cpu_order for row in entries[cpu] if row[1] == thread]
            self.require(stream, "Thread never entered Ring 3")
            self.require(stream[0][1][10:25] == [0] * 15 and stream[0][1][25:30] == [CODE, CS, 0x202, USTACK[slot] + 4088, SS], "initial frame was synthesized incorrectly")
            previous = None
            for cpu, entry in stream:
                if previous is not None:
                    allowed = {11, 14} if previous[4] == 128 else set()
                    self.require(all(entry[10 + index] == previous[10 + index] for index in range(20) if index not in allowed),
                                 "full saved/resumed context changed outside ABI result registers")
                    self.require(entry[5] == int(previous[4] != 128), "resumed context lost IRQ/native mode")
                    if previous[4] == 128:
                        self.native_result(previous[10:30], entry[10:30], slot, cpu, previous[0])
                boundaries = [row for row in traces[cpu] if row[0] == entry[0] and row[1] == thread]
                self.require(boundaries, "entered user context has no actual boundary")
                previous = boundaries[-1]
            streams[str(thread)] = {"cpus": cpu_order, "entries": len(stream), "last_boundary": previous}
        if self.device_extension:
            self.device_extension.complete_traces(traces)
        self.output.append({"stage": "terminal", "registers": live, "scheduler": scheduler, "authority": authority,
                            "data": data, "entries": entries, "traces": traces, "streams": streams,
                            "rejected_returns": rejected,
                            "inventory": inventory, "evidence": evidence})

    def native_result(self, before, after, slot, cpu=None, entry=None):
        if self.device_extension:
            return self.device_extension.native_result(before, after, slot, cpu, entry)
        if self.authority_extension:
            return self.authority_extension.native_result(before, after, slot, cpu, entry)
        operation = before[14]
        status, value = after[14], after[11]
        if operation == 1:
            wanted = (0, 0x5000000FF)
        elif operation == 0x12:
            wanted = (0, 11)
        elif operation in (0x11, 0x13, 0x17, (-256 & MASK64)):
            wanted = (0, 0)
        elif operation == 0x16:
            wanted = (0, 0x1101)
        elif operation == (-1024 & MASK64):
            wanted = ((-17 & MASK64), 0) if slot == 0 else (0, 1)
        elif operation == 0x10 or operation == 0x18 and status == 0:
            self.require(status == 0 and value != 0, "bootstrap/received capability did not return a live handle")
            return
        elif (operation in (0x19, 0x1a) and self.case == "smp-authority-disabled") or (0x1b <= operation <= 0x21 and self.case == "smp-device-disabled"):
            wanted = ((-1 & MASK64), 0)
        elif operation == 0x18:
            wanted = ((-32 & MASK64), 0)
        else:
            raise self.failure("SMP scheduler QMP: unexpected resumed native operation " + hex(operation))
        self.require((status, value) == wanted, "resumed native operation returned the wrong status/scalar")

    def snapshot(self):
        memory = VirtualMemory(self.qmp, self.failure)
        physical = PhysicalMemory(self.qmp, 0, self.failure)
        self.require(self.array(memory, "neu_smp_sched_layout", 9) == LAYOUT[:6] + [self.trace_limit] + LAYOUT[7:],
                     "unknown scheduler schema")
        evidence = self.array(memory, "neu_smp_sched_evidence", 64)
        descriptor = self.array(memory, "neu_smp_sched_inventory", 64)
        self.require(evidence[0] == MAGIC and evidence[2] == self.cpus, "missing SMP controller identity")
        locked, owner = memory.byte(descriptor[56]), memory.u64(descriptor[57])
        if locked:
            return None
        self.require(owner == 0, "unlocked scheduler retained a lock owner")
        phase = evidence[1]
        if phase != 0:
            self.require(self.baseline is not None, "user execution began before an independent baseline")
        if phase in (2, 3, 4):
            self.require(self.prepared is not None, "execution passed an unobserved prepared checkpoint")
        if phase == 0 and self.baseline is not None:
            return phase
        if phase == 3 and any(item["stage"] == "terminal" for item in self.output):
            return phase
        live = [_user_registers(self.qmp, cpu, self.failure) for cpu in range(self.cpus)]
        # The controller publishes baseline/prepared state before serial output
        # and the wait call. Observe the actual boundary before reading its
        # inventory; a later phase still fails if this checkpoint was missed.
        if (phase == 0 or phase == 1 and self.prepared is None) and not (
                self.symbol("neu_smp_sched_probe_begin") <= live[0]["rip"] <
                self.symbol("neu_smp_sched_probe_end")):
            return None
        cpu_words = [memory.words(self.symbol("neu_smp_sched_cpus") + cpu * 832, 64) for cpu in range(self.cpus)]
        validate_trace_counts(cpu_words, self.trace_limit, self.require)
        blocks = [memory.words(self.symbol("neu_smp_cpus") + cpu * 384, 48) for cpu in range(self.cpus)]
        self.require(all(b[1] == cpu and b[3] == self.apics[cpu] for cpu, b in enumerate(blocks)), "actual CPU blocks disagree with QEMU APIC identities")
        if phase == 0:
            self.require(self.symbol("neu_smp_sched_probe_begin") <= live[0]["rip"] < self.symbol("neu_smp_sched_probe_end"), "baseline missed the allocator checkpoint")
            inv = self.inventory(memory, physical, descriptor)
            samples = sorted(evidence[53:56])
            divided_hz = samples[1] * 1_193_182 // 11_932
            initial = (divided_hz * 10_000 + 500_000) // 1_000_000
            self.require(all(100 <= v <= 100_000_000 for v in samples)
                         and (samples[2] - samples[0]) * 4 <= samples[1]
                         and evidence[8:10] == [divided_hz, initial], "timer quantum does not follow actual PIT calibration samples")
            self.require(not inv["frames"] and not inv["mappings"] and not any(s["live"] for s in inv["spaces"]), "baseline has private user backing")
            self.baseline = {"inventory": inv, "registers": live, "kernel_root": evidence[3]}
            self.output.append({"stage": "baseline", **self.baseline})
            return phase
        self.require(self.baseline is not None, "user execution began before an independent baseline")
        scheduler = self.scheduler(memory, cpu_words, live, phase)
        authority = self.authority(memory, scheduler, phase)
        if self.authority_extension:
            self.authority_extension.observe(memory, physical, live, cpu_words, scheduler, authority, phase, evidence)
        if self.device_extension:
            self.device_extension.observe(memory, physical, live, cpu_words, scheduler, authority, phase, evidence)
        self.runtime_checks += 1
        if phase == 1 and self.prepared is None:
            self.require(self.symbol("neu_smp_sched_probe_begin") <= live[0]["rip"] < self.symbol("neu_smp_sched_probe_end"), "prepared resources missed the checkpoint")
            inv = self.inventory(memory, physical, descriptor)
            self.prepared = self.prepared_roots(memory, physical, inv, evidence, scheduler["threads"])
            descriptors = self.descriptors(memory, physical, blocks, live, cpu_words, phase, evidence[3])
            self.output.append({"stage": "prepared", "scheduler": scheduler, "authority": authority,
                                "descriptors": descriptors, "registers": live, "evidence": evidence, **self.prepared})
        elif phase == 5 and not self.negatives:
            inv = self.inventory(memory, physical, descriptor)
            rows = self.array(memory, "neu_smp_sched_negatives", 176)
            records = []
            for index, expected in enumerate(NEGATIVE_ERRORS):
                row = rows[index * 8:(index + 1) * 8]
                self.require(row[:4] == [index + 1, 1 if index < 16 else 2, expected, expected]
                             and row[5] == row[6] and physical.word(row[4]) == row[5], "negative transaction mutated actual alias or returned wrong rejection")
                records.append(row)
            self.require(evidence[47] == 22, "incomplete deterministic rejection matrix")
            tlb = self.array(memory, "neu_smp_sched_tlb", 16)
            self.require(memory.u64(tlb[1]) == 0 and memory.u64(tlb[5]) == 0 and memory.byte(tlb[6]) == 0,
                         "simulated negative acknowledgements reached the hardware coordinator")
            self.negatives = True
            self.output.append({"stage": "negatives", "cases": records, "inventory": inv, "scheduler": scheduler})
        elif phase in (1, 2) and self.prepared is not None:
            self.probes(memory, physical, live, cpu_words, scheduler, evidence)
            user_a = [a for a in scheduler["active"] if a["node"] == NODES[0] and live[a["cpu"]]["cpl"] == 3]
            if len(user_a) == 2 and not self.concurrent:
                self.require(len({a["root"] for a in user_a}) == 1 and len({a["thread"] for a in user_a}) == 2,
                             "same-Node concurrent ownership is not distinct")
                self.concurrent = True
                record = {"stage": "concurrent", "registers": live, "scheduler": scheduler}
                if self.case == "smp-schedule-shared":
                    record["shared_counter"] = physical.word(self.prepared["data"][0] + 0x300)
                self.output.append(record)
            if self.case in MIGRATE and not self.migrated:
                a2 = next((a for a in scheduler["active"] if a["thread"] == THREADS[2] and a["cpu"] == 3), None)
                if a2 and live[3]["cpl"] == 3:
                    self.require(scheduler["counters"][4] == 1 and cpu_words[1][11] > 0 and cpu_words[3][11] > 0,
                                 "migrated Thread did not execute first on CPU1")
                    self.migrated = True
                    self.output.append({"stage": "migrated", "registers": live, "scheduler": scheduler})
            if self.case in IPC and not self.blocked and authority["waits"]:
                waiter = authority["waits"][0]
                cpu = 2 if self.cpus == 4 else 0
                if self.cpus == 2 or (live[cpu]["hlt"] and live[cpu]["cpl"] == 0 and live[cpu]["rfl"] & 0x200):
                    self.require(waiter["thread"] == THREADS[1] and scheduler["threads"][1]["state"] == 4,
                                 "Channel waiter did not block B1")
                    if self.cpus == 4:
                        self.require(cpu_words[2][3] == 4 and not any(a["cpu"] == 2 for a in scheduler["active"])
                                     and self.symbol("neu_smp_sched_idle_begin") <= live[2]["rip"] <= self.symbol("neu_smp_sched_idle_end"),
                                     "blocked owner CPU did not enter the actual STI/HLT idle path")
                    self.blocked = True
                    self.output.append({"stage": "blocked-idle", "registers": live, "scheduler": scheduler, "authority": authority})
            if self.quiesced and not any(item["stage"] == "fault-resources" for item in self.output):
                inv = self.inventory(memory, physical, descriptor)
                self.require([s["pins"] for s in inv["spaces"]] == [2, 1] and [s["refs"] for s in inv["spaces"]] == [1, 1]
                             and authority["handles"][0], "fault reclaimed authority/backing before origin commit")
                self.output.append({"stage": "fault-resources", "inventory": inv, "authority": authority})
        elif phase == 3 and not any(item["stage"] == "terminal" for item in self.output):
            inv = self.inventory(memory, physical, descriptor)
            self.descriptors(memory, physical, blocks, live, cpu_words, phase, evidence[3])
            self.terminal(memory, physical, live, cpu_words, scheduler, authority, inv, evidence)
        elif phase == 4:
            self.require(any(item["stage"] == "terminal" for item in self.output), "cleanup preceded a terminal execution observation")
            inv = self.inventory(memory, physical, descriptor)
            base = self.baseline["inventory"]
            self.require(inv["total"] == base["total"] and inv["free"] == base["free"] and inv["bitmap"] == base["bitmap"]
                         and inv["tables"] == base["tables"] and not inv["frames"] and not inv["mappings"]
                         and all(not s["live"] and not s["pins"] and not s["refs"] and not s["root"] for s in inv["spaces"]),
                         "final allocator bitmap/table/root/reference inventory differs from exact baseline")
            descriptors = self.descriptors(memory, physical, blocks, live, cpu_words, phase, evidence[3])
            for cpu in range(self.cpus):
                self.require(live[cpu]["cpl"] == 0 and not live[cpu]["rfl"] & 0x600 and live[cpu]["cr3"] == evidence[3]
                             and live[cpu]["cr0"] == self.baseline["registers"][cpu]["cr0"]
                             and live[cpu]["efer"] == self.baseline["registers"][cpu]["efer"], "final CPU control registers differ from baseline")
                lapic = self.qmp.call("human-monitor-command", {"command-line": "info lapic", "cpu-index": cpu})
                self.require(re.search(r"ISR\s+\(none\)", lapic) and re.search(r"IRR\s+\(none\)", lapic), "final LAPIC has a pending/in-service interrupt")
            if self.case == "smp-tlb-invalid":
                self.require(self.negatives, "missing independently checked negative matrix")
            self.output.append({"stage": "final", "inventory": inv, "registers": live, "descriptors": descriptors,
                                "scheduler": scheduler, "authority": authority, "evidence": evidence})
            return phase
        return phase

    def run(self):
        fast = self.qmp.call("query-cpus-fast")
        identities = _apic_identities(self.qmp, fast, self.failure)
        self.require(len(fast) == self.cpus and set(identities.values()) == set(range(self.cpus)), "wrong QEMU CPU topology")
        self.apics = {cpu: apic for apic, cpu in identities.items()}
        self.require(self.qmp.call("query-status")["running"], "QEMU was not running before observation")
        attempts = 0
        while time.monotonic() < self.deadline:
            self.qmp.call("stop")
            self.require(not self.qmp.call("query-status")["running"], "QMP did not stop every CPU")
            phase = self.snapshot()
            self.qmp.call("cont")
            if phase == 4:
                return {"cpu_count": self.cpus, "runtime_invariant_snapshots": self.runtime_checks,
                        "native_cpus": sorted(self.native), "irq_cpus": sorted(self.irq),
                        "invalidation_cpus": sorted(self.invalidation), "concurrent": self.concurrent,
                        "migration": self.migrated, "blocked_idle": self.blocked, "fault_quiescence": self.quiesced,
                        "negative_checks": 22 if self.negatives else 0,
                        "baseline_bitmap_sha256": self.baseline["inventory"]["bitmap_sha256"],
                        "checkpoints": self.output}
            time.sleep(observation_delay(attempts, self.concurrent))
            attempts += 1
        raise self.failure("SMP scheduler QMP exceeded the unchanged guest test deadline")


def observation_delay(attempt, concurrent_seen):
    # CPU0 alternates two runnable Threads on 10 ms quanta. A fixed 20 ms
    # sampling interval can repeatedly stop the unrelated Thread and miss
    # actual same-Node overlap. Vary the phase until both CPL3 owners are seen;
    # the required witness and the overall guest deadline remain unchanged.
    return 0.02 if concurrent_seen else (0.003, 0.007, 0.011)[attempt % 3]


def observe(qmp, case, symbols, deadline, failure, output):
    return Observer(qmp, case, symbols, deadline, failure, output).run()
