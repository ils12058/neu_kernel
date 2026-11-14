"""Independent K10 ownership and syscall oracle over stopped production state.

The observer obtains IDs/offsets from diagnostic schemas, then reads the actual
tables, lineage, FIFO tokens, mapping owners, PTEs and hardware register frames.
The operation model below has no dependency on the Rust implementation.
"""
import copy
import struct

from smp_scheduler_qmp import (MASK64, NODES, THREADS, P, W, U, NX, PA,
                               raw32, raw64, space_raw, number)


def validate_lineage(grants, owners, handles, active_root, require):
    ids = {g['id']: g for g in grants}
    require(len(ids) == len(grants) and set(owners) <= set(ids), 'owner names a freed or duplicate lineage generation')
    require(not active_root or active_root in ids, 'active revoke has no retained source')
    for g in grants:
        require(g['uses'] == owners.get(g['id'], [0]*4), 'lineage has lost or duplicated an owner')
        if g['parent']:
            parent = ids.get(g['parent'])
            require(parent and g['object'] == parent['object'] and not g['rights'] & ~parent['rights'],
                    'derived grant amplifies rights or changes objects')
        require(any(g['uses']) or any(c['parent'] == g['id'] for c in grants) or active_root == g['id'],
                'unowned lineage was not collected')
        visited = {g['id']}; parent = g['parent']
        while parent:
            require(parent not in visited and parent in ids, 'lineage cycle or absent ancestor')
            visited.add(parent); parent = ids[parent]['parent']
    for table in handles:
        for h in table:
            if h['grant']:
                require(h['grant'] in ids and ids[h['grant']]['object'] == h['target']
                        and ids[h['grant']]['rights'] == h['rights'] and not ids[h['grant']]['revoked'],
                        'live handle retains revoked or foreign authority')
    return ids


def validate_mapping_bindings(bindings, registry, objects, roots, require):
    key = lambda m: (m['space'], m['address'])
    actual = {key(m): m for m in registry}
    require(len(actual) == len(registry) == len(bindings) == len({key(m) for m in bindings}),
            'mapping ownership is missing or duplicated across registries')
    for binding in bindings:
        record = actual.get(key(binding))
        obj = objects.get(binding['object'])
        require(record and obj and obj['kind'] == 5 and obj['live'] and
                record['object'] == obj['target'] and roots.get(binding['space']) == binding['root']
                and all(record[name] == binding[name] for name in ('grant', 'epoch', 'targets')),
                'mapping provenance differs between capability and paging registries')


def validate_invalidation_window(cpu, cpus, request, local, regs, operands, acks, probe, require):
    require(request and request[0] == 2 and request[4] == (1 << cpus)-1,
            'hardware invalidation has no pending mapping transaction')
    require(probe[0] <= regs['rip'] < probe[1] and regs['cpl'] == 0 and not regs['rfl'] & 0x600,
            'missed actual invalidation instruction window')
    require(operands == [request[3], regs['cr3'], local[35], request[1]]
            and local[32:35] == [request[1], request[3], regs['cr3']] and local[36] < request[1],
            'wrong invalidation arguments or premature local acknowledgement')
    require(local[35] == (1 if regs['cr3'] == request[2] else 2), 'wrong local invalidation/root-flush choice')
    require(not acks & (1 << cpu), 'CPU acknowledged before observed instruction completion')


class AuthorityModel:
    def __init__(self, initial, require):
        self.require = require
        self.tables = [{(h['generation'] << 32) | (h['slot'] + 1): copy.deepcopy(h)
                        for h in table} for table in initial['handles']]
        self.generations = copy.deepcopy(initial['generations'])
        self.forest = {g['id']: copy.deepcopy(g) for g in initial['grants']}
        self.maps = [m['grant'] for m in initial['mappings']]
        self.queues = [[], []]
        self.waits = {}
        self.pending = None
        self.next_grant = 1 << 63
        self.results = {}
        self.sent, self.received, self.discarded = [1, 0], [0, 1], [0, 0]

    def below(self, grant, root):
        for _ in range(64):
            if grant not in self.forest:
                return False
            grant = self.forest[grant]['parent']
            if grant == root:
                return True
            if not grant:
                return False
        self.require(False, 'cycle in independent authority model')

    def gc(self):
        while True:
            owners = [h['grant'] for t in self.tables for h in t.values()]
            owners += [h['grant'] for q in self.queues for h in q]
            owners += self.maps + [w['grant'] for w in self.waits.values()]
            if self.pending:
                owners.append(self.pending['root'])
            removable = [g for g in self.forest if g not in owners and
                         not any(c['parent'] == g for c in self.forest.values())]
            if not removable:
                return
            for g in removable:
                del self.forest[g]

    @staticmethod
    def valid_rights(kind, rights):
        access = rights & 255
        return bool(rights and not rights & ~1023 and
                    {1: access != 0 and not access & ~3, 2: access == 1,
                     3: not access & ~16, 4: access != 0 and not access & ~224,
                     5: access != 0 and not access & ~15}.get(kind, False))

    def lookup(self, owner, handle):
        slot, generation = (handle & 0xFFFFFFFF) - 1, handle >> 32
        if slot not in range(8) or not generation or not self.generations[owner][slot]:
            return -16, None
        if handle not in self.tables[owner]:
            return -17, None
        return 0, self.tables[owner][handle]

    def allocate(self, owner, source):
        occupied = {h['slot'] for h in self.tables[owner].values()}
        free = [s for s in range(8) if s not in occupied and self.generations[owner][s] < 0xFFFFFFFF]
        if not free:
            return -22, 0
        slot = free[0]
        self.generations[owner][slot] += 1
        handle = (self.generations[owner][slot] << 32) | (slot + 1)
        self.tables[owner][handle] = dict(source, slot=slot, generation=self.generations[owner][slot])
        return 0, handle

    def operation(self, row):
        _, phase, cpu, node, thread, op, handle, rights, status, scalar, entry, _, *extra = row
        key = (cpu, entry)
        owner = 0 if node == NODES[0] else 1
        self.require(node == NODES[owner] and thread in THREADS, 'foreign operation owner')
        if phase == 1:
            self.require(self.pending is not None and self.pending['key'] == key,
                         'completion has no matching outstanding revoke')
            selected = self.pending['selected']
            expected = (0, self.pending['count'])
            self.maps = [g for g in self.maps if g not in selected]
            self.pending = None
            self.gc()
        elif op == 1:
            expected = (0, 0x6000001FF)
        elif op == 0x10:
            self.require(handle == 3, 'unexpected K10 bootstrap request')
            expected = (0, (1 << 32) | 4)
        elif op in (0x19, 0x1A):
            if any(extra) or op == 0x1A and rights:
                expected = (-2, 0)
            elif op == 0x19 and (not rights or rights & ~1023):
                expected = (-18, 0)
            else:
                error, source = self.lookup(owner, handle)
                if error:
                    expected = (error, 0)
                elif not source['rights'] & (256 if op == 0x19 else 512):
                    expected = (-18, 0)
                elif op == 0x19:
                    if rights & ~source['rights'] or not self.valid_rights(source['kind'], rights):
                        expected = (-18, 0)
                    elif self.pending and (source['grant'] == self.pending['root'] or
                                           self.below(source['grant'], self.pending['root'])):
                        expected = (-23, 0)
                    else:
                        self.next_grant += 1
                        grant = self.next_grant
                        expected = self.allocate(owner, dict(source, rights=rights, grant=grant))
                        if expected[0] == 0:
                            self.forest[grant] = {'id': grant, 'parent': source['grant'],
                                                  'object': source['target'], 'rights': rights}
                elif self.pending:
                    expected = (-23, 0)
                else:
                    root = source['grant']
                    selected = {g for g in self.forest if self.below(g, root)}
                    for table in self.tables:
                        for h in list(table):
                            if table[h]['grant'] in selected:
                                del table[h]
                    for side in range(2):
                        kept = [h for h in self.queues[side] if h['grant'] not in selected]
                        self.discarded[side] += len(self.queues[side]) - len(kept)
                        self.queues[side] = kept
                    for waiting in list(self.waits):
                        if self.waits[waiting]['grant'] in selected:
                            self.results[waiting] = (-38 & MASK64, 0)
                            del self.waits[waiting]
                    if any(g in selected for g in self.maps):
                        self.pending = {'root': root, 'selected': selected, 'count': len(selected), 'key': key}
                        expected = (-100, 0)
                    else:
                        expected = (0, len(selected))
                    self.gc()
        elif op == 0x11:
            error, _ = self.lookup(owner, handle)
            expected = (error, 0)
            if not error:
                del self.tables[owner][handle]
                self.gc()
        elif op == 0x17:
            error, endpoint = self.lookup(owner, handle)
            source_error, source = self.lookup(owner, rights)
            self.require(not error and endpoint['kind'] == 4 and endpoint['rights'] & 128
                         and not source_error and source['transferable'] and source['rights'] & 8,
                         'invalid move in authority fixture')
            self.require(len(self.queues[1-owner]) < 2, 'unexpected full authority queue')
            self.queues[1-owner].append(self.tables[owner].pop(rights))
            self.sent[owner] += 1
            expected = (0, 0)
        elif op == 0x18:
            self.require(not self.queues[owner], 'revoked transfer survived to delivery')
            expected = (-32, 0)
        elif op == 0x16:
            error, source = self.lookup(owner, handle)
            self.require(not error and source['kind'] == 4 and source['rights'] & 64
                         and not self.queues[owner], 'wait was not authorized and empty')
            self.waits[key] = source
            expected = (-100, 0)
        else:
            self.require(False, f'unmodelled K10 syscall {op:#x}')
        expected = (expected[0] & MASK64, expected[1])
        self.require((status, scalar) == expected,
                     f'K10 operation differs from independent model: row={row}, expected={expected}')
        if expected[0] != (-100 & MASK64):
            self.results[key] = expected


class Extension:
    def __init__(self, observer):
        self.o = observer
        self.initial = None
        self.invalidations = {}
        self.wait_observed = False
        self.model = None

    def require(self, condition, message):
        self.o.require(condition, 'authority: ' + message)

    def layout(self, memory):
        return self.o.array(memory, 'neu_authority_layout', 64)

    def mapping_offsets(self, memory):
        return self.layout(memory)[29:32]

    def memory_objects(self, memory, d):
        objects = []
        for slot in range(4):
            row = memory.read(d[18] + slot * d[19], d[19])
            if row[d[22]]:
                self.require(row[d[23]] == 0 and row[d[38]] == 1 and raw64(row, d[25]) == 2,
                             'MemoryObject lost its capability lifetime reference')
                objects.append({'id': (raw64(row, d[21]) << 8) | (slot + 1),
                                'frames': [list(struct.unpack_from('<QQ', row, d[24] + i * 16)) for i in range(2)],
                                'capability_refs': row[d[38]]})
            else:
                self.require(not row[d[23]] and not row[d[38]], 'dead MemoryObject retained owners')
        return objects

    def authority(self, memory, scheduler, phase):
        x = self.layout(memory)
        d = self.o.array(memory, 'neu_smp_sched_authority', 128)
        self.require(d[0] == 1 and d[3] == 6 and d[13:17] == [8, 8, 1, 2]
                     and x[0] == x[14] == 1 and x[3] == x[17] == 64, 'unknown authority schema')
        objects, handles, generations, node_states, waits, mappings = [], [], [], [], [], []
        references, pins, grant_owners = {}, {}, {}

        def owner(grant, category):
            if grant:
                grant_owners.setdefault(grant, [0, 0, 0, 0])[category] += 1

        for slot in range(6):
            row = memory.read(d[4] + slot * d[5], d[5])
            objects.append({'slot': slot, 'generation': raw32(row, d[6]), 'references': raw32(row, d[7]),
                            'pins': raw32(row, d[8]), 'live': bool(row[d[9]]), 'kind': row[d[10]],
                            'target': raw64(row, d[11])})
        for index in range(2):
            t = d[24+index*20:44+index*20]
            self.require(phase == 4 and not any(t) or t[0] == 1 and t[5] == 8 and t[18] == NODES[index],
                         'foreign authority table')
            entries, gens = [], []
            for slot in range(8 if any(t) else 0):
                row = memory.read(t[3] + slot * t[4], t[4])
                gens.append(raw32(row, t[6]))
                if row[t[7]]:
                    h = {'slot': slot, 'generation': gens[-1], 'kind': row[t[8]],
                         'target': raw64(row, t[9]), 'rights': int.from_bytes(row[t[10]:t[10]+2], 'little'),
                         'transferable': row[t[11]], 'grant': raw64(row, x[27])}
                    entries.append(h)
                    references[h['target']] = references.get(h['target'], 0) + 1
                    owner(h['grant'], 0)
            handles.append(entries); generations.append(gens)
            nf = d[120+index*4:124+index*4]
            self.require((memory.u32(nf[1]) << 32) | (memory.u64(nf[0]) + 1) == NODES[index],
                         'Node table owner generation changed')
            state = memory.byte(nf[2]); node_states.append(state)
            members = (0, 2) if index == 0 else (1,)
            has_live = any(scheduler['threads'][s]['state'] in (1, 2, 3, 4) for s in members)
            self.require(state in (2, 3) if has_live else state in (4, 6), 'Node/Thread lifetime mismatch')
            if phase in (3, 4):
                self.require(not entries and state == (6 if phase == 4 else 4), 'terminal Node retains handles')
        channel = d[64:96]
        self.require(channel[0] == 1 and channel[28:30] == [2, 2], 'unknown Channel schema')
        queues = []
        for side in range(2):
            q = channel[1] + channel[8] + side * channel[12]
            head, tail, length = [memory.u64(q + channel[i]) for i in (13, 14, 15)]
            self.require(head < 2 and tail < 2 and length <= 2 and (head + length) % 2 == tail, 'invalid actual FIFO')
            transfers = []
            for slot in range(2):
                ptr = q + channel[17] + slot * channel[18]
                live = memory.byte(ptr + channel[19]); target = memory.u64(ptr + channel[20])
                rights = int.from_bytes(memory.read(ptr + channel[21], 2), 'little')
                owned = memory.byte(ptr + channel[22]); grant = memory.u64(ptr + x[28])
                if live:
                    self.require((slot+2-head) % 2 < length and target and owned and grant, 'unowned queued authority')
                    references[target] = references.get(target, 0) + 1
                    owner(grant, 1)
                    transfers.append({'target': target, 'rights': rights, 'grant': grant})
                else:
                    self.require(not target and not rights and not owned and not grant, 'stale authority in empty FIFO slot')
            queues.append({'head': head, 'tail': tail, 'len': length, 'transfers': transfers})
        for slot in range(3):
            row = d[96+slot*8:104+slot*8]
            if row[0]:
                thread = (memory.u32(row[1]) << 32) | (memory.u64(row[0]) + 1)
                w = {'thread': thread, 'token': memory.u64(row[2]), 'state': memory.byte(row[3]),
                     'pinned': memory.byte(row[4]), 'root': memory.u64(row[5]),
                     'kind': memory.byte(row[6]), 'object': memory.u64(row[7]),
                     'grant': memory.u64(x[52+slot])}
                self.require(w['pinned'] == 1 and thread in THREADS and
                             scheduler['threads'][THREADS.index(thread)]['state'] == 4, 'wait has no blocked pinned owner')
                pins[w['object']] = pins.get(w['object'], 0) + 1
                owner(w['grant'], 2); waits.append(w)
            else:
                self.require(not any(row) and x[52+slot] == 0, 'partial empty authority wait')
        for slot in range(64):
            row = memory.read(x[15] + slot * x[16], x[16])
            if row[x[18]]:
                m = {'grant': raw64(row, x[19]), 'object': raw64(row, x[20]), 'space': space_raw(row, x[21]),
                     'root': raw64(row, x[22]), 'address': raw64(row, x[23]), 'epoch': raw64(row, x[24]),
                     'targets': raw64(row, x[25])}
                self.require(m['grant'] and m['space'] in (0x101, 0x102), 'mapping lacks provenance')
                pins[m['object']] = pins.get(m['object'], 0) + 1
                owner(m['grant'], 3); mappings.append(m)
        self.require(len(mappings) == x[26], 'mapping count differs from raw records')
        grants = []
        for slot in range(64):
            row = memory.read(x[1] + slot * x[2], x[2])
            if row[x[5]]:
                g = {'id': (raw32(row, x[4]) << 32) | (slot + 1), 'revoked': row[x[6]],
                     'parent': raw64(row, x[7]), 'object': raw64(row, x[8]),
                     'rights': int.from_bytes(row[x[9]:x[9]+2], 'little'),
                     'uses': list(struct.unpack_from('<IIII', row, x[10]))}
                grants.append(g)
        ids = validate_lineage(grants, grant_owners, handles, x[13], self.require)
        for mapping in mappings:
            self.require(ids[mapping['grant']]['object'] == mapping['object'], 'mapping names another grants backing')
        for queue in queues:
            for transfer in queue['transfers']:
                grant = ids[transfer['grant']]
                self.require(grant['object'] == transfer['target'] and grant['rights'] == transfer['rights']
                             and not grant['revoked'], 'queued transfer retains revoked or foreign authority')
        for obj in objects:
            object_id = (obj['kind'] << 40) | (obj['generation'] << 8) | (obj['slot'] + 1)
            self.require(obj['references'] == references.get(object_id, 0) and obj['pins'] == pins.get(object_id, 0),
                         'actual object lifetime differs from handle/queue/wait/mapping ownership')
            pending_root = obj['kind'] == 3 and phase != 4
            self.require(obj['live'] == bool(obj['references'] or obj['pins'] or pending_root), 'orphaned capability object')
        p = self.o.array(memory, 'neu_smp_sched_inventory', 64)
        registry_mappings, roots = [], {}
        for slot in range(p[11]):
            row = memory.read(p[9] + slot*p[10], p[10])
            if row[p[17]] and raw64(row, p[16]) == 1:
                registry_mappings.append({'space': space_raw(row, p[12]), 'address': raw64(row, p[13]),
                                          'object': space_raw(row, p[16]+8), 'grant': raw64(row, x[29]),
                                          'epoch': raw64(row, x[30]), 'targets': raw64(row, x[31])})
        for slot in range(p[28]):
            row = memory.read(p[26] + slot*p[27], p[27])
            if row[p[30]]:
                roots[(raw64(row, p[29]) << 8) | (slot+1)] = raw64(row, p[31])
        validate_mapping_bindings(mappings, registry_mappings,
            {(obj['kind'] << 40) | (obj['generation'] << 8) | (obj['slot']+1): obj for obj in objects},
            roots, self.require)
        counters = {name: memory.words(channel[1] + channel[i], 2)
                    for name, i in (('sent', 9), ('received', 10), ('discarded', 11))}
        result = {'objects': objects, 'handles': handles, 'generations': generations, 'node_states': node_states,
                  'queues': queues, 'waits': waits, 'mappings': mappings, 'grants': grants, 'counters': counters}
        if phase == 1 and self.initial is None:
            self.require(len(mappings) == 6 and len(grants) == (5 if self.o.case == 'smp-authority-wait' else 4),
                         'wrong initial derivation/mapping inventory')
            self.require(x[33] != x[36] and ids[x[34]]['parent'] == x[33] and ids[x[35]]['parent'] == x[34],
                         'independent roots or nested provenance absent')
            self.initial = copy.deepcopy(result)
        if phase == 4:
            self.require(not grants and not mappings and not waits and not references and not pins
                         and not any(q['len'] for q in queues), 'authority ownership leaked at final cleanup')
        return result

    def observe(self, memory, physical, live, cpu_words, scheduler, authority, phase, evidence):
        if phase not in (1, 2) or self.o.prepared is None:
            return
        x = self.layout(memory)
        if authority['waits'] and not self.wait_observed:
            self.require(self.o.case == 'smp-authority-wait' and authority['waits'][0]['thread'] == THREADS[2],
                         'unexpected blocked authority waiter')
            self.wait_observed = True
            self.o.output.append({'stage': 'authority-blocked', 'authority': authority, 'registers': live})
        rows = [self.o.array(memory, 'neu_authority_invalidations', 128)[i*16:(i+1)*16] for i in range(x[48])]
        for cpu, local in enumerate(cpu_words):
            key = (local[32], cpu)
            if local[54] != 1 or key in self.invalidations:
                continue
            regs = live[cpu]
            request = next((r for r in rows if r[1] == local[32]), None)
            if not self.o.symbol('neu_smp_sched_tlb_probe_begin') <= regs['rip'] < self.o.symbol('neu_smp_sched_tlb_probe_end'):
                continue
            tlb = self.o.array(memory, 'neu_smp_sched_tlb', 16)
            validate_invalidation_window(cpu, self.o.cpus, request, local, regs,
                [number(regs['text'], name, self.o.failure) for name in ('RDI', 'RSI', 'RDX', 'R8')],
                memory.u64(tlb[12]), [self.o.symbol('neu_smp_sched_tlb_probe_begin'),
                                       self.o.symbol('neu_smp_sched_tlb_probe_end')], self.require)
            self.require(physical.word(request[7]) == 0 and not self.o.leaf(physical, request[2], request[3])['present'],
                         'revoked leaf remains present')
            retained = [m for m in authority['mappings'] if m['root'] == request[2] and m['address'] == request[3]]
            self.require(len(retained) == 1 and retained[0]['epoch'] == request[1] and retained[0]['grant'] == request[5],
                         'mapping owner released before hardware acknowledgement')
            self.invalidations[key] = {'cpu': cpu, 'epoch': request[1], 'request': request,
                                       'registers': regs, 'mapping': retained[0]}
            self.o.output.append({'stage': 'authority-invalidation', **self.invalidations[key]})

    def terminal(self, memory, physical, cpu_words, scheduler, authority, inventory, evidence):
        x = self.layout(memory)
        self.require(x[48] == 4 and set(self.invalidations) == {(e, c) for e in range(1, 5) for c in range(self.o.cpus)},
                     'missing per-page per-CPU invalidation evidence')
        invalidations = self.o.array(memory, 'neu_authority_invalidations', 128)
        invalidation_rows = [invalidations[i*16:(i+1)*16] for i in range(4)]
        roots = self.o.prepared['roots']
        expected_pages = [(roots[0], 0x410000), (roots[0], 0x411000), (roots[1], 0x420000), (roots[1], 0x421000)]
        self.require([(r[2], r[3]) for r in invalidation_rows] == expected_pages and
                     all(r[0] == 4 and r[1] == i+1 and r[10] == (1 << self.o.cpus)-1 and
                         r[11] == 5-i and physical.word(r[7]) == 0 for i, r in enumerate(invalidation_rows)),
                     'revocation did not retire exactly its four mappings after acknowledgement')
        self.require(x[41] == (2 if self.o.case == 'smp-authority-wait' else 1)
                     and not x[12] and not x[45] and not x[47] and len(authority['mappings']) == 2,
                     'revocation transaction did not finish or removed independent mappings')
        for index in range(2):
            self.o.mapped(physical, roots[1], 0x430000+index*4096, P | W | U | NX, x[37+index])
            self.require(physical.word(x[37+index]+8) == 0x2202, 'surviving independent grant lost backing/data')
        data = [physical.words(self.o.prepared['data'][0 if slot != 1 else 1] + slot*128, 16) for slot in range(3)]
        affected = 0 if self.o.case == 'smp-authority-terminate' else (3 if self.o.case == 'smp-authority-queued' else 2)
        self.require(data[0][11] == affected
                     and data[0][13] == 4 and data[1][10] == 0x2202 and data[1][11] == (-17 & MASK64),
                     'actual userspace results lack attenuation, stale-handle or independent-grant evidence')
        if self.o.case == 'smp-authority-wait':
            self.require(self.wait_observed and data[2][11] == (-38 & MASK64) and data[0][14] == 1,
                         'derived pending operation did not resume with Revoked')
        if self.o.case == 'smp-authority-queued':
            self.require(data[1][14] == (-32 & MASK64), 'revoked queued capability was delivered')
        self.model = AuthorityModel(self.initial, self.require)
        records = self.o.array(memory, 'neu_authority_operations', 128*16)
        rows = [records[i*16:(i+1)*16] for i in range(x[42])]
        self.require(0 < len(rows) <= 128, 'missing operation ledger')
        hardware = {}
        for cpu, local in enumerate(cpu_words):
            traces = memory.words(self.o.symbol('neu_smp_sched_traces') + cpu*65536, local[25]*32)
            for offset in range(0, len(traces), 32):
                trace = traces[offset:offset+32]
                if trace[4] == 128:
                    hardware[(cpu, trace[0])] = trace
        for index, row in enumerate(rows):
            self.require(row[0] == index+1 and row[1] in (0, 1), 'operation publication order changed')
            trace = hardware.get((row[2], row[10]))
            self.require(trace and trace[1] == row[4] and trace[2] == row[3] and trace[3] == row[11]
                         and trace[24] == row[5] and [trace[18], trace[19], trace[21], trace[15], trace[17], trace[16]]
                         == [row[6], row[7], *row[12:16]], 'operation ledger differs from actual syscall frame')
            self.model.operation(row)
        self.require(self.model.pending is None and not self.model.waits, 'independent model has unfinished operations')
        self.require(authority['counters']['sent'] == self.model.sent and
                     authority['counters']['received'] == self.model.received and
                     authority['counters']['discarded'] == self.model.discarded, 'queue counters disagree with independent move/revoke model')
        self.o.output.append({'stage': 'authority-terminal', 'operations': rows, 'invalidations': invalidation_rows,
                              'mapping_transactions': expected_pages, 'authority': authority,
                              'data': data, 'layout': x, 'inventory': inventory})

    def native_result(self, before, after, slot, cpu, entry):
        expected = (0, 0) if before[14] == (-256 & MASK64) else self.model.results.get((cpu, entry))
        self.require(expected is not None and (after[14], after[11]) == expected,
                     'resumed syscall result differs from the independent operation model')
