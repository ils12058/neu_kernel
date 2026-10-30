"""Independent stopped-CPU oracle for the bounded MemoryObject test profiles.

The guest descriptor carries addresses/layout offsets only. All ownership,
counts, permissions and allocator membership below come from raw production
storage or physical page-table reads. No guest 'verified' scalar is consumed.
"""
import hashlib
import re
import struct
import time

CASES = ('memory-object-lifetime', 'memory-object-lifetime-reverse',
         'memory-object-new-tables', 'memory-object-map-rollback',
         'memory-object-new-tables-reverse', 'memory-object-fail-stop')
ADDRESS = 0x000ffffffffff000
WINDOW = 0xffff900000000000
NX = 1 << 63


def observe(qmp, path, symbols, deadline, words, register, failure, output, expected_mode):
    def require(condition, message):
        if not condition:
            raise failure('MemoryObject QMP: ' + message)

    def read(address, length, physical=False):
        data = bytearray()
        for offset in range(0, (length + 7) // 8, 256):
            count = min(256, (length + 7) // 8 - offset)
            data.extend(struct.pack('<' + 'Q' * count, *words(qmp, address + offset * 8, count, physical)))
        return bytes(data[:length])

    def u64(data, offset=0):
        return struct.unpack_from('<Q', data, offset)[0]

    def identity(data, offset=0):
        return [u64(data, offset), u64(data, offset + 8)]

    previous = {}
    expected = None
    baseline = None
    original = None
    private = {}
    root_tables = {}
    premap_tables = {}
    previous_bitmap = None
    while time.monotonic() < deadline:
        cpu_hint = qmp.call('human-monitor-command', {'command-line': 'info registers'})
        rip = register(cpu_hint, 'RIP')
        if not symbols['neu_memory_checkpoint_wait'] <= rip < symbols['neu_memory_checkpoint_wait_end']:
            if 'MemoryObject lifecycle complete:' in path.read_text(errors='replace'):
                break
            time.sleep(0.001)
            continue
        hint = words(qmp, symbols['neu_memory_witness'], 3)
        if hint[1] in previous:
            time.sleep(0.001)
            continue
        qmp.call('stop')
        try:
            cpu = qmp.call('human-monitor-command', {'command-line': 'info registers'})
            if not symbols['neu_memory_checkpoint_wait'] <= register(cpu, 'RIP') < symbols['neu_memory_checkpoint_wait_end']:
                continue  # The running location hint expired before STOP.
            d = words(qmp, symbols['neu_memory_witness'], 128)
            magic, stage, mode, kernel, va, vb = d[:6]
            if stage in previous:
                continue
            require(magic == 0x4e45554d4f000001 and mode == expected_mode, 'invalid diagnostic schema or wrong profile')
            require((va, vb) == ((0x600000, 0x600000) if mode in (2, 4) else (0x410000, 0x420000)), 'wrong profile virtual-address contract')
            if expected is None:
                expected = [1, 2, 12] + ([13, 8] if mode == 3 else []) + [3] + ([8, 9] if mode in (2, 4) else []) + [4, 5, 6, 7, 10, 11]
            if mode == 5: expected = [1, 2, 12, 8]
            require(stage == expected[len(previous)], 'missing or reordered checkpoint')
            require('CPL=0' in cpu and register(cpu, 'CR3') == kernel and register(cpu, 'RFL') & 0x600 == 0, 'unsafe diagnostic CPU boundary')
            layout = d[16:54]
            require(layout[0] == 1 and layout[3] == 16 and layout[11] == 64 and layout[20] == 4 and layout[28] == 2, 'unexpected bounded registry layout')
            require(layout[34:38] == [16, 16, 24, 24], 'identity/provenance layout mismatch')
            frames = []
            for slot in range(16):
                row = read(layout[1] + slot * layout[2], layout[2])
                owner = layout[8]
                frames.append({'slot': slot, 'physical': u64(row, layout[4]), 'generation': u64(row, layout[5]),
                               'live': row[layout[6]], 'initialized': row[layout[7]],
                               'owner': [u64(row, owner), *identity(row, owner + 8)]})
            mappings = []
            for slot in range(64):
                row = read(layout[9] + slot * layout[10], layout[10])
                provenance = layout[16]
                mappings.append({'slot': slot, 'space': identity(row, layout[12]), 'address': u64(row, layout[13]),
                                 'frame': identity(row, layout[14]), 'write': row[layout[15]], 'execute': row[layout[15] + 1],
                                 'object': [u64(row, provenance), *identity(row, provenance + 8)], 'live': row[layout[17]]})
            objects = []
            for slot in range(4):
                row = read(layout[18] + slot * layout[19], layout[19])
                objects.append({'slot': slot, 'generation': u64(row, layout[21]), 'live': row[layout[22]],
                                'creator': row[layout[23]], 'frames': [identity(row, layout[24] + i * 16) for i in range(2)],
                                'pages': u64(row, layout[25])})
            spaces = []
            for slot in range(2):
                row = read(layout[26] + slot * layout[27], layout[27])
                spaces.append({'slot': slot, 'generation': u64(row, layout[29]), 'live': row[layout[30]],
                               'root': u64(row, layout[31]), 'pins': row[layout[32]], 'refs': row[layout[33]]})
            total = u64(read(d[64], 8))
            free = u64(read(d[65], 8))
            bitmap = read(d[66], ((total + 63) // 64) * 8)
            require(free <= total and sum(byte.bit_count() for byte in bitmap) == total - free, 'allocator bitmap/counters disagree')
            regions = read(d[68], d[69] * 24)
            kernel_start, kernel_end = u64(read(d[70], 8)), u64(read(d[71], 8))
            spans = []
            for offset in range(0, len(regions), 24):
                start, length, kind = struct.unpack_from('<QQQ', regions, offset)
                if kind != 0:
                    continue
                end = start + length
                start = max(start, 4096)
                spans += [(a, b) for a, b in ((start, min(end, kernel_start)), (max(start, kernel_end), end)) if a < b]
            require(sum((end - start) // 4096 for start, end in spans) == total, 'allocator total differs from raw memory-map spans')
            def allocated(physical):
                ordinal = 0
                for start, end in spans:
                    if start <= physical < end:
                        ordinal += (physical - start) // 4096
                        return bool(bitmap[ordinal // 8] & (1 << (ordinal % 8)))
                    ordinal += (end - start) // 4096
                raise failure('MemoryObject QMP: physical owner outside allocatable spans')
            tables = []
            for slot in range(d[82]):
                row = read(d[80] + slot * d[81], d[81])
                tables.append({'slot': slot, 'physical': u64(row, d[83]), 'level': row[d[84]], 'pinned': row[d[85]]})
            live_tables = {t['physical']: t for t in tables if t['physical']}
            live_frames = [f for f in frames if f['live']]
            require(len(live_tables) == sum(bool(t['physical']) for t in tables), 'duplicate table ownership')
            require(len({f['physical'] for f in live_frames}) == len(live_frames), 'duplicate data backing')
            require(not set(live_tables) & {f['physical'] for f in live_frames}, 'table/data ownership overlap')
            require(total - free == len(live_tables) + len(live_frames), 'orphan physical allocation')
            for physical in list(live_tables) + [f['physical'] for f in live_frames]:
                require(allocated(physical), 'live owner has a freed allocator bit')
            cache = {}
            def page(physical):
                if physical not in cache:
                    cache[physical] = words(qmp, physical, 512, True)
                return cache[physical]
            def walk(root, address):
                table, entries = root, []
                for depth, shift in enumerate((39, 30, 21, 12)):
                    require(table in live_tables and live_tables[table]['level'] == 4 - depth, 'walk entered a foreign/unowned table')
                    entry = page(table)[(address >> shift) & 511]
                    entries.append(entry)
                    if not entry & 1:
                        return {'present': False, 'entries': entries}
                    require(not depth < 3 or not entry & 128, 'unexpected large page')
                    table = entry & ADDRESS
                return {'present': True, 'entries': entries, 'physical': table,
                        'write': all(e & 2 for e in entries), 'user': all(e & 4 for e in entries),
                        'execute': not any(e & NX for e in entries)}
            root_users = {}
            def reachable(root):
                found = set()
                users = {}
                def visit(table, level, prefix=0, writable=True, user=True, executable=True):
                    require(table in live_tables and live_tables[table]['level'] == level, 'tree has stale table reference')
                    require(table not in found, 'duplicate or cyclic table parent')
                    found.add(table)
                    for index, entry in enumerate(page(table)):
                        if not entry & 1:
                            continue
                        address = prefix | (index << (12 + (level - 1) * 9))
                        w, u, x = writable and bool(entry & 2), user and bool(entry & 4), executable and not bool(entry & NX)
                        if level > 1:
                            require(not entry & 128, 'unexpected large table leaf')
                            visit(entry & ADDRESS, level - 1, address, w, u, x)
                        elif u:
                            require(0x400000 <= address < 0x800000, 'user mapping outside the bounded user range')
                            users[address] = {'physical': entry & ADDRESS, 'write': w, 'execute': x}
                visit(root, 4)
                root_users[root] = users
                return found
            common_tables = reachable(kernel)
            for slot in range(64):
                alias = walk(kernel, WINDOW + slot * 4096)
                require(alias['present'] == bool(tables[slot]['physical']), 'stale common workspace alias')
                if alias['present']:
                    require(alias['physical'] == tables[slot]['physical'] and alias['write'] and not alias['execute'] and not alias['user'], 'common workspace alias ownership/permissions')
            require(not walk(kernel, WINDOW + 511 * 4096)['present'], 'scratch alias survived a transaction')
            inventories = []
            walks = []
            all_tables = set(common_tables)
            live_spaces = [s for s in spaces if s['live']]
            require(len({s['root'] for s in live_spaces}) == len(live_spaces), 'shared CR3 root identity')
            for label in range(2):
                if not d[90 + label * 4]:
                    inventories.append(None)
                    continue
                root_ptr, id_ptr, inventory_ptr, state_ptr = d[90 + label * 4:94 + label * 4]
                root = u64(read(root_ptr, 8))
                sid = identity(read(id_ptr, 16))
                state = read(state_ptr, 1)[0]
                inv = list(read(inventory_ptr, 64))
                require(sid == [label, 1], 'wrong Node-local SpaceId generation')
                s = spaces[sid[0]]
                require(s['generation'] == sid[1], 'token generation mismatch')
                inventories.append({'root': root, 'id': sid, 'state': state, 'tables': inv})
                if not s['live']:
                    require(state == 2 and s['root'] == 0, 'destroyed space/token still usable')
                    continue
                require(state == 0 and root == s['root'] and s['pins'] == s['refs'] == 0, 'live token/root is not quiescent')
                owned = {tables[i]['physical'] for i, value in enumerate(inv) if value}
                require(0 not in owned and len(owned) == sum(inv), 'inventory contains retired tables')
                require(not owned & all_tables, 'foreign root table ownership')
                require(reachable(root) == owned, 'reachable tables differ from token inventory')
                ledger = [m for m in mappings if m['live'] and m['space'] == sid]
                require(len(ledger) == len(root_users[root]) and {m['address'] for m in ledger} == set(root_users[root]), 'user leaves differ from the mapping ledger')
                for m in ledger:
                    f = frames[m['frame'][0]]
                    require(root_users[root][m['address']] == {'physical': f['physical'], 'write': bool(m['write']), 'execute': bool(m['execute'])}, 'raw user leaf differs from mapping/frame record')
                all_tables |= owned
                aliases = []
                for slot in range(64):
                    alias = walk(root, WINDOW + slot * 4096)
                    require(alias['present'] == bool(inv[slot]), 'unauthorized or stale workspace alias')
                    if alias['present']:
                        require(alias['physical'] == tables[slot]['physical'] and alias['write'] and not alias['execute'] and not alias['user'], 'wrong workspace alias permissions/target')
                        aliases.append(slot)
                current_private = {}
                for address, writable, executable in ((0x400000, False, True), (0x401000, False, False), (0x402000, True, False), (0x404000, True, False)):
                    leaf = walk(root, address)
                    require(leaf['present'] and leaf['user'] and leaf['write'] == writable and leaf['execute'] == executable, 'private mapping permission changed')
                    current_private[address] = leaf['physical']
                if label in private:
                    require(private[label] == current_private, 'private backing changed')
                private[label] = current_private
                record = {'label': label, 'root': root, 'aliases': aliases, 'private': current_private,
                          'shared': [walk(root, [va, vb][label] + i * 4096) for i in range(2)]}
                if mode in (3, 5) and stage in (8, 13):
                    require(not walk(root, 0x5ff000)['present'] and not walk(root, 0x600000)['present'], 'failed map left a partial leaf')
                walks.append(record)
            require(all_tables == set(live_tables), 'table allocation absent from all root inventories')
            if len(private) == 2:
                require(not set(private[0].values()) & set(private[1].values()), 'private-space backing is shared')
            obj_live = [o for o in objects if o['live']]
            has_object = stage not in (1, 7, 11)
            require(len(obj_live) == int(has_object), 'object live count differs from model')
            expected_spaces = 2 if stage in (3, 4, 8, 9, 12, 13) else 1 if stage == 5 else 0
            require(len(live_spaces) == expected_spaces, 'space retirement count differs from model')
            shared = [m for m in mappings if m['live'] and m['object'][0] == 1]
            expected_shared = 4 if stage in (3, 4, 9) else 2 if stage == 5 or stage == 8 and mode in (2, 4) else 0
            require(len(shared) == expected_shared, 'shared mapping references differ from model')
            require(len([m for m in mappings if m['live']]) == expected_shared + expected_spaces * 4, 'unexpected mapping record')
            if has_object:
                obj = obj_live[0]
                oid = [obj['slot'], obj['generation']]
                require(obj['pages'] == 2 and obj['creator'] == int(stage in (2, 3, 8, 9, 10, 12, 13)), 'size/creator reference differs from model')
                require(obj['generation'] == (2 if stage == 10 else 1), 'unexpected object generation')
                backing = []
                for fid in obj['frames']:
                    f = frames[fid[0]]
                    require(f['live'] and f['initialized'] and f['generation'] == fid[1] and f['owner'] == [1, *oid], 'invalid object/frame generation or backing owner')
                    backing.append(f['physical'])
                    if stage in (2, 10):
                        require(read(f['physical'], 4096, True) == bytes(4096), 'backing not zeroed')
                require(backing[0] != backing[1], 'duplicate object page')
                if original is None:
                    original = {'id': oid, 'frames': obj['frames'], 'physical': backing}
                elif stage == 10:
                    require(oid[0] == original['id'][0] and backing == original['physical'] and all(n != o for n, o in zip(obj['frames'], original['frames'])), 'backing generation reuse/sanitation not demonstrated')
                else:
                    require(oid == original['id'] and obj['frames'] == original['frames'] and backing == original['physical'], 'object backing changed across teardown')
                for m in shared:
                    require(m['object'] == [1, *oid] and m['frame'] in obj['frames'] and not m['execute'], 'wrong mapping provenance')
                for record in walks:
                    label = record['label']
                    mapped = not (stage in (12, 13) or stage == 8 and (mode in (3, 5) or label == 1))
                    for i, leaf in enumerate(record['shared']):
                        require(leaf['present'] == mapped, 'mapping presence differs from lifecycle model')
                        if mapped:
                            require(leaf['physical'] == backing[i] and leaf['user'] and not leaf['execute'] and leaf['write'] == (label == 0), 'shared PTE backing/effective rights mismatch')
                            inv = inventories[label]
                            ledger = [m for m in shared if m['space'] == inv['id'] and m['address'] == [va, vb][label] + i * 4096]
                            require(len(ledger) == 1 and ledger[0]['frame'] == obj['frames'][i] and bool(ledger[0]['write']) == (label == 0), 'PTE does not match exact provenance record')
                require(len(live_frames) == 2 + expected_spaces * 4, 'backing frame conservation failure')
            else:
                require(not live_frames and not shared, 'retired backing still live')
                if original:
                    require(all(not allocated(pa) for pa in original['physical']), 'collected backing still allocated')
                    for f in original['frames']:
                        require(not frames[f[0]]['live'], 'collected frame identity still live')
            for m in mappings:
                if m['live'] and m['object'][0] == 0:
                    f = frames[m['frame'][0]]
                    require(f['live'] and f['generation'] == m['frame'][1] and f['owner'] == [0, *m['space']], 'private frame ownership changed')
            if baseline is None:
                baseline = {'total': total, 'free': free, 'tables': len(live_tables), 'bitmap': bitmap}
            require(total == baseline['total'], 'allocator total changed')
            if stage in (1, 7, 11):
                require(free == baseline['free'] and len(live_tables) == baseline['tables'] and bitmap == baseline['bitmap'], 'final accounting differs from exact baseline bitmap')
                if original:
                    for table in live_tables:
                        if live_tables[table]['level'] == 1:
                            require(not any(e & 1 and e & ADDRESS in original['physical'] for e in page(table)), 'stale PTE references collected backing')
            if stage in (3, 4, 8, 9, 12, 13):
                require([r['label'] for r in walks] == [0, 1], 'missing or foreign live root')
            if stage == 5:
                require([r['label'] for r in walks] == ([0] if mode in (1, 4) else [1]), 'wrong root destruction order')
            if stage == 12:
                previous_bitmap = bitmap
                premap_tables = {r['label']: sum(inventories[r['label']]['tables']) for r in walks}
            if stage == 3:
                require(all(sum(inventories[i]['tables']) == premap_tables[i] + int(mode in (2, 4)) for i in (0, 1)), 'dynamic table allocation count differs from model')
                root_tables = {r['label']: sum(inventories[r['label']]['tables']) for r in walks}
            if stage == 8 and mode in (2, 4):
                require(sum(inventories[1]['tables']) + 1 == root_tables[1], 'new empty leaf table not reclaimed')
            if stage == 9:
                require(sum(inventories[1]['tables']) == root_tables[1], 'remap did not restore table inventory')
            if mode == 3 and stage in (3, 8, 13):
                require(bitmap == previous_bitmap, 'rollback or existing-table remap leaked an allocation')
            snapshot = {'stage': stage, 'mode': mode, 'cpu': cpu, 'descriptor': d,
                        'frames': frames, 'objects': objects, 'spaces': spaces, 'mappings': mappings,
                        'tables': tables, 'inventories': inventories, 'walks': walks,
                        'allocator': {'total': total, 'free': free, 'allocated': total - free,
                                      'bitmap_sha256': hashlib.sha256(bitmap).hexdigest(), 'bitmap_words': list(struct.unpack('<' + 'Q' * (len(bitmap) // 8), bitmap))}}
            previous[stage] = snapshot
            output.append(snapshot)
        finally:
            qmp.call('cont')
        if len(previous) == len(expected):
            return {'stages': expected, 'mode': mode, 'baseline': {k: v for k, v in baseline.items() if k != 'bitmap'}, 'backing': original, 'checkpoints': output}
    raise failure('MemoryObject QMP: missing bounded checkpoint(s)')


LIVE_CASES = ('memory-object-user-share', 'memory-object-user-readonly-fault',
              'memory-object-user-a-exit', 'memory-object-user-xapic')
LIVE_MAGIC = 0x4e45554d4f000002
LIVE_VALUE = 0x4e45554d4f4c4956
LIVE_SECOND = 0x5a5a0123456789ab
LIVE_RESULT = 0x4c4956454f4b0001
LIVE_VA = (0x600000, 0x602000)
LIVE_DATA = 0x402000
LIVE_STACK = (0x404000, 0x408000, 0x40c000)
LIVE_BLOCK = (0x140, 0x1d0, 0x188)
LIVE_GATE = 0x20
LIVE_READBACK = 0x30
LIVE_FAULT_MARKER = 0x38
LIVE_POST = 0x40
LIVE_KSTACK = (0xffffc00000001000, 0xffffc00000005000)


def observe_live(qmp, path, symbols, deadline, words, register, failure, output,
                 expected_mode, image):
    """Prove live Ring 3 sharing from stopped-CPU production state.

    The descriptor publishes addresses and layout only. Object payload bytes,
    user PTE permissions, saved hardware frames, the y #PF record and allocator
    membership all come from raw memory or physical page-table reads.
    """
    def require(condition, message):
        if not condition:
            raise failure('MemoryObject live QMP: ' + message)

    def read(address, length, physical=False):
        data = bytearray()
        for offset in range(0, (length + 7) // 8, 256):
            count = min(256, (length + 7) // 8 - offset)
            data.extend(struct.pack('<' + 'Q' * count, *words(qmp, address + offset * 8, count, physical)))
        return bytes(data[:length])

    def u64(data, offset=0):
        return struct.unpack_from('<Q', data, offset)[0]

    def identity(data, offset=0):
        return [u64(data, offset), u64(data, offset + 8)]

    raw = image.read_bytes()
    header = struct.unpack_from('<16sHHIQQQIHHHHHH', raw)

    def image_bytes(address, length):
        for index in range(header[10]):
            segment = struct.unpack_from('<IIQQQQQQ', raw, header[5] + index * header[9])
            if segment[0] == 1 and segment[3] <= address and address + length <= segment[3] + segment[5]:
                start = segment[2] + address - segment[3]
                return raw[start:start + length]
        raise failure('MemoryObject live QMP: image range is not file-backed')

    payload = image_bytes(symbols['neu_multi_payload'],
                          symbols['neu_multi_payload_end'] - symbols['neu_multi_payload'])
    require(0 < len(payload) <= 4096, 'invalid embedded payload bounds')
    code_page = payload + bytes(4096 - len(payload))

    def user_va(name):
        return 0x400000 + symbols[name] - symbols['neu_multi_payload']

    def page(physical, cache):
        if physical not in cache:
            cache[physical] = words(qmp, physical, 512, True)
        return cache[physical]

    previous = {}
    expected_stages = list(range(8))
    baseline = None
    original = None
    root_values = {}
    while time.monotonic() < deadline:
        cpu_hint = qmp.call('human-monitor-command', {'command-line': 'info registers'})
        rip = register(cpu_hint, 'RIP')
        if not symbols['neu_memory_live_wait'] <= rip < symbols['neu_memory_live_wait_end']:
            if len(previous) == len(expected_stages) and 'MemoryObject live test passed:' in path.read_text(errors='replace'):
                break
            time.sleep(0.001)
            continue
        hint = words(qmp, symbols['neu_memory_live_witness'], 3)
        if hint[1] in previous:
            time.sleep(0.001)
            continue
        qmp.call('stop')
        try:
            cpu = qmp.call('human-monitor-command', {'command-line': 'info registers'})
            if not symbols['neu_memory_live_wait'] <= register(cpu, 'RIP') < symbols['neu_memory_live_wait_end']:
                continue
            d = words(qmp, symbols['neu_memory_live_witness'], 128)
            magic, stage, mode, kernel, va, vb, value, second, schema, node_a, node_b, space_a, space_b, fault_ptr = d[:14]
            result = d[14]
            if stage in previous:
                continue
            require(magic == LIVE_MAGIC and schema == 1 and mode == expected_mode, 'invalid live descriptor schema/profile')
            require((va, vb) == LIVE_VA, 'wrong live virtual-address contract')
            require(value == LIVE_VALUE and second == LIVE_SECOND and result == LIVE_RESULT, 'wrong live payload constants')
            require(fault_ptr == symbols['NEU_LIVE_FAULT'], 'fault record is not the production static')
            require(stage == len(previous), 'missing or reordered live checkpoint')
            require('CPL=0' in cpu and register(cpu, 'CR3') == kernel and register(cpu, 'RFL') & 0x600 == 0, 'unsafe live diagnostic CPU boundary')
            layout = d[16:54]
            require(layout[0] == 1 and layout[3] == 16 and layout[11] == 64 and layout[20] == 4 and layout[28] == 2, 'unexpected bounded registry layout')
            require(layout[34:38] == [16, 16, 24, 24], 'identity/provenance layout mismatch')
            require(d[104:108] == [0x140, 0x1d0, 0x188, LIVE_RESULT], 'wrong private-block schema')
            require(d[108:112] == [LIVE_GATE, LIVE_FAULT_MARKER, LIVE_READBACK, LIVE_POST], 'wrong private-block offsets')
            require(d[115:119] == [LIVE_DATA, *LIVE_STACK], 'wrong private DATA/stack addresses')
            frames = []
            for slot in range(16):
                row = read(layout[1] + slot * layout[2], layout[2])
                owner = layout[8]
                frames.append({'slot': slot, 'physical': u64(row, layout[4]), 'generation': u64(row, layout[5]),
                               'live': row[layout[6]], 'initialized': row[layout[7]],
                               'owner': [u64(row, owner), *identity(row, owner + 8)]})
            mappings = []
            for slot in range(64):
                row = read(layout[9] + slot * layout[10], layout[10])
                provenance = layout[16]
                mappings.append({'slot': slot, 'space': identity(row, layout[12]), 'address': u64(row, layout[13]),
                                 'frame': identity(row, layout[14]), 'write': row[layout[15]], 'execute': row[layout[15] + 1],
                                 'object': [u64(row, provenance), *identity(row, provenance + 8)], 'live': row[layout[17]]})
            objects = []
            for slot in range(4):
                row = read(layout[18] + slot * layout[19], layout[19])
                objects.append({'slot': slot, 'generation': u64(row, layout[21]), 'live': row[layout[22]],
                                'creator': row[layout[23]], 'frames': [identity(row, layout[24] + i * 16) for i in range(2)],
                                'pages': u64(row, layout[25])})
            spaces = []
            for slot in range(2):
                row = read(layout[26] + slot * layout[27], layout[27])
                spaces.append({'slot': slot, 'generation': u64(row, layout[29]), 'live': row[layout[30]],
                               'root': u64(row, layout[31]), 'pins': row[layout[32]], 'refs': row[layout[33]]})
            total = u64(read(d[64], 8))
            free = u64(read(d[65], 8))
            bitmap = read(d[66], ((total + 63) // 64) * 8)
            require(free <= total and sum(byte.bit_count() for byte in bitmap) == total - free, 'allocator bitmap/counters disagree')
            regions = read(d[68], d[69] * 24)
            kernel_start, kernel_end = u64(read(d[70], 8)), u64(read(d[71], 8))
            spans = []
            for offset in range(0, len(regions), 24):
                start, length, kind = struct.unpack_from('<QQQ', regions, offset)
                if kind != 0:
                    continue
                end = start + length
                start = max(start, 4096)
                spans += [(a, b) for a, b in ((start, min(end, kernel_start)), (max(start, kernel_end), end)) if a < b]
            require(sum((end - start) // 4096 for start, end in spans) == total, 'allocator total differs from raw memory-map spans')

            def allocated(physical):
                ordinal = 0
                for start, end in spans:
                    if start <= physical < end:
                        ordinal += (physical - start) // 4096
                        return bool(bitmap[ordinal // 8] & (1 << (ordinal % 8)))
                    ordinal += (end - start) // 4096
                raise failure('MemoryObject live QMP: physical owner outside allocatable spans')

            tables = []
            for slot in range(d[82]):
                row = read(d[80] + slot * d[81], d[81])
                tables.append({'slot': slot, 'physical': u64(row, d[83]), 'level': row[d[84]], 'pinned': row[d[85]]})
            live_tables = {t['physical']: t for t in tables if t['physical']}
            live_frames = [f for f in frames if f['live']]
            require(len(live_tables) == sum(bool(t['physical']) for t in tables), 'duplicate table ownership')
            require(len({f['physical'] for f in live_frames}) == len(live_frames), 'duplicate data backing')
            require(not set(live_tables) & {f['physical'] for f in live_frames}, 'table/data ownership overlap')
            require(total - free == len(live_tables) + len(live_frames), 'orphan physical allocation')
            for physical in list(live_tables) + [f['physical'] for f in live_frames]:
                require(allocated(physical), 'live owner has a freed allocator bit')
            cache = {}

            def walk(root, address):
                table, entries = root, []
                for depth, shift in enumerate((39, 30, 21, 12)):
                    require(table in live_tables and live_tables[table]['level'] == 4 - depth, 'walk entered a foreign/unowned table')
                    entry = page(table, cache)[(address >> shift) & 511]
                    entries.append(entry)
                    if not entry & 1:
                        return {'present': False, 'entries': entries}
                    require(not depth < 3 or not entry & 128, 'unexpected large page')
                    table = entry & ADDRESS
                return {'present': True, 'entries': entries, 'physical': table,
                        'write': all(e & 2 for e in entries), 'user': all(e & 4 for e in entries),
                        'execute': not any(e & NX for e in entries)}

            root_users = {}

            def reachable(root):
                found = set()
                users = {}

                def visit(table, level, prefix=0, writable=True, user=True, executable=True):
                    require(table in live_tables and live_tables[table]['level'] == level, 'tree has stale table reference')
                    require(table not in found, 'duplicate or cyclic table parent')
                    found.add(table)
                    for index, entry in enumerate(page(table, cache)):
                        if not entry & 1:
                            continue
                        address = prefix | (index << (12 + (level - 1) * 9))
                        w, u, x = writable and bool(entry & 2), user and bool(entry & 4), executable and not bool(entry & NX)
                        if level > 1:
                            require(not entry & 128, 'unexpected large table leaf')
                            visit(entry & ADDRESS, level - 1, address, w, u, x)
                        elif u:
                            require(0x400000 <= address < 0x800000, 'user mapping outside the bounded user range')
                            users[address] = {'physical': entry & ADDRESS, 'write': w, 'execute': x}

                visit(root, 4)
                root_users[root] = users
                return found

            common_tables = reachable(kernel)
            for slot in range(64):
                alias = walk(kernel, WINDOW + slot * 4096)
                require(alias['present'] == bool(tables[slot]['physical']), 'stale common workspace alias')
                if alias['present']:
                    require(alias['physical'] == tables[slot]['physical'] and alias['write'] and not alias['execute'] and not alias['user'], 'common workspace alias ownership/permissions')
            require(not walk(kernel, WINDOW + 511 * 4096)['present'], 'scratch alias survived a transaction')

            descriptors = []
            for index in range(2):
                fields = d[90 + index * 4:94 + index * 4]
                if fields[0] == 0:
                    require(all(v == 0 for v in fields), 'partial live space token')
                    descriptors.append(None)
                    continue
                root = u64(read(fields[0], 8))
                sid = identity(read(fields[1], 16))
                state = read(fields[3], 1)[0]
                inv = list(read(fields[2], 64))
                stack_inv = list(read(d[98 + index], 64)) if d[98 + index] else [0] * 64
                descriptors.append({'root': root, 'id': sid, 'state': state, 'tables': inv,
                                    'stack_tables': stack_inv})
                root_values[index] = root

            survivor = 1 if expected_mode == 2 else 0
            expected_slots = {0: (), 1: (), 2: (0, 1), 3: (0, 1), 4: (0, 1),
                              5: (survivor,), 6: (1,) if expected_mode == 2 else (), 7: ()}[stage]
            live_slots = [s['slot'] for s in spaces if s['live']]
            require(live_slots == list(expected_slots), 'live space set differs from teardown model')
            for index in range(2):
                require((descriptors[index] is not None) == (index in expected_slots), 'descriptor presence differs from teardown model')

            if stage < 2:
                require(node_a == node_b == space_a == space_b == 0 and d[112:115] == [0, 0, 0], 'pre-Node identity published too early')
            else:
                require(node_a == 0x1_00000001 and node_b == 0x1_00000002, 'wrong live Node identity')
                require(space_a == 0x101 and space_b == 0x102, 'wrong live Space identity')
                require(d[112:115] == [0x1_00000001, 0x1_00000002, 0], 'wrong live Thread identity')

            live_objects = [o for o in objects if o['live']]
            require(len(live_objects) == int(stage not in (0, 7)), 'object live count differs from lifecycle model')
            oid = None
            backing = []
            block_data = {}
            if live_objects:
                obj = live_objects[0]
                oid = [obj['slot'], obj['generation']]
                require(obj['pages'] == 2, 'wrong object page count')
                require(obj['creator'] == int(stage in (1, 2)), 'creator reference differs from lifecycle model')
                for fid in obj['frames']:
                    f = frames[fid[0]]
                    require(f['live'] and f['initialized'] and f['generation'] == fid[1] and f['owner'] == [1, *oid], 'invalid object backing owner/generation')
                    backing.append(f['physical'])
                require(backing[0] != backing[1], 'duplicate object page')
                if original is None:
                    original = {'id': oid, 'frames': obj['frames'], 'physical': backing}
                else:
                    require(oid == original['id'] and obj['frames'] == original['frames'] and backing == original['physical'], 'object backing changed across teardown')
                page0, page1 = read(backing[0], 4096, True), read(backing[1], 4096, True)
                if stage < 4:
                    require(page0 == bytes(4096) and page1 == bytes(4096), 'backing not zeroed before live execution')
                else:
                    require(u64(page0, 0) == LIVE_VALUE and u64(page0, 8) == LIVE_SECOND and u64(page1, 0) == 1, 'Ring 3 payload bytes absent from shared backing')
            else:
                require(stage in (0, 7), 'unexpected object retirement stage')

            mapping_total = 0
            for index in expected_slots:
                record = descriptors[index]
                require(record is not None, 'missing live space token')
                require(record['id'] == [index, 1] and record['state'] == 0 and spaces[index]['generation'] == 1, 'wrong Node-local SpaceId generation')
                require(spaces[index]['root'] == record['root'], 'descriptor root differs from production root')
                full = not (stage == 4 and expected_mode == 1 and index == 1)
                pinned = (stage in (2, 3) or stage == 5 or (stage == 6 and expected_mode == 2 and index == 1)
                          or (stage == 4 and not (expected_mode == 1 and index == 1)))
                require(spaces[index]['pins'] == int(pinned), 'space pins differ from queued Thread model')
                require(spaces[index]['refs'] == 0, 'unexpected retained space reference')
                owned = {tables[i]['physical'] for i, owned_bit in enumerate(record['tables']) if owned_bit}
                require(0 not in owned and len(owned) == sum(record['tables']), 'inventory contains retired tables')
                require(not owned & common_tables, 'foreign root table ownership')
                require(reachable(record['root']) == owned, 'reachable tables differ from token inventory')
                require(record['root'] in root_users, 'descriptor root is not the production walk root')
                users = root_users[record['root']]
                for slot in range(64):
                    alias = walk(record['root'], WINDOW + slot * 4096)
                    visible = bool(record['tables'][slot]) and not bool(record['stack_tables'][slot])
                    if alias['present'] != visible:
                        present = [i for i in range(64) if walk(record['root'], WINDOW + i * 4096)['present']]
                        raise failure(
                            f'MemoryObject live QMP: unauthorized or stale workspace alias at slot {slot} '
                            f'(present={alias["present"]}, owned={bool(record["tables"][slot])}, '
                            f'stack={bool(record["stack_tables"][slot])}, '
                            f'entries={alias["entries"]}, table={tables[slot]["physical"]:#x}, '
                            f'present_slots={present}, root={record["root"]:#x})')
                    if alias['present']:
                        require(alias['physical'] == tables[slot]['physical'] and alias['write'] and not alias['execute'] and not alias['user'], 'wrong workspace alias permissions/target')
                shared = {LIVE_VA[index] + i * 4096 for i in range(2)}
                private = {0x400000, 0x401000, LIVE_DATA}
                if full:
                    private.add(LIVE_STACK[index])
                require(set(users) == private | shared, 'root user leaves differ from the live layout')
                other = 1 - index
                for i in range(2):
                    require(not walk(record['root'], LIVE_VA[other] + i * 4096)['present'], 'Node root aliases its sibling VAs')
                ledger = [m for m in mappings
                          if m['live'] and m['space'] == [index, 1] and m['address'] < 0x1_0000_0000]
                require(len(ledger) == len(private) + len(shared), 'mapping ledger count differs from live layout')
                require({m['address'] for m in ledger} == private | shared, 'mapping ledger addresses differ from live layout')
                for m in ledger:
                    f = frames[m['frame'][0]]
                    require(f['live'] and f['generation'] == m['frame'][1], 'mapping names a stale frame')
                    require(users[m['address']] == {'physical': f['physical'], 'write': bool(m['write']), 'execute': bool(m['execute'])}, 'raw user leaf differs from mapping/frame record')
                    if m['address'] in shared:
                        require(oid is not None and m['object'] == [1, *oid], 'wrong shared mapping provenance')
                        require(m['frame'] == live_objects[0]['frames'][(m['address'] - LIVE_VA[index]) // 4096], 'shared leaf maps the wrong object page')
                        require(bool(m['write']) == (index == 0) and not m['execute'], 'shared leaf rights differ from the Node grant')
                    else:
                        require(m['object'][0] == 0 and f['owner'] == [0, index, 1], 'private frame ownership changed')
                code = walk(record['root'], 0x400000)
                require(code['present'] and code['user'] and code['execute'] and not code['write'], 'user code page permissions changed')
                require(read(code['physical'], 4096, True) == code_page, 'user code page differs from the ELF payload')
                require(walk(record['root'], 0x401000)['present'] and walk(record['root'], LIVE_DATA)['present'], 'private leaves disappeared')
                data_phys = walk(record['root'], LIVE_DATA)['physical']
                block_data[index] = read(data_phys, 4096, True)
                mapping_total += len(ledger)
            if live_objects:
                require(len(live_frames) == 2 + sum(4 if not (stage == 4 and expected_mode == 1 and index == 1) else 3 for index in expected_slots)
                        + 2 * sum(1 for index in expected_slots if spaces[index]['pins']), 'backing frame count differs from live layout')
            else:
                require(not live_frames and mapping_total == 0, 'retired backing still live')

            def block(index, offset):
                return u64(block_data[index], LIVE_BLOCK[index] + offset)

            if 0 in expected_slots:
                if stage < 4:
                    for offset in range(0, 0x48, 8):
                        require(block(0, offset) == 0 and u64(block_data[0], LIVE_BLOCK[2] + offset) == 0, 'private block written before Ring 3 execution')
                else:
                    require(block(0, 0) == LIVE_VALUE and block(0, 8) == LIVE_SECOND and block(0, 0x10) == 1, 'Node A private store observation missing')
                    require(block(0, 0x18) == LIVE_RESULT and block(0, LIVE_READBACK) == LIVE_VALUE, 'Node A did not read its own object bytes in Ring 3')
                    require(block(0, LIVE_GATE) == 1 and block(0, LIVE_POST) == 0 and block(0, LIVE_FAULT_MARKER) == 0, 'Node A gate/fault observation differs from the controller model')
                    for offset in range(0, 0x48, 8):
                        require(u64(block_data[0], LIVE_BLOCK[2] + offset) == 0, 'unused A2 observation block was written')
            if 1 in expected_slots:
                if stage < 4:
                    for offset in range(0, 0x48, 8):
                        require(block(1, offset) == 0, 'private block written before Ring 3 execution')
                elif stage == 4 and expected_mode == 2 or stage == 5 and expected_mode == 2:
                    for offset in range(0, 0x48, 8):
                        if offset == 0x28:
                            continue
                        require(block(1, offset) == 0, 'Node B read before its pre-gate was released')
                    require(block(1, 0x28) > 0, 'Node B wait gate did not spin')
                elif stage == 4 and expected_mode == 1:
                    require(block(1, 0) == LIVE_VALUE and block(1, 8) == LIVE_SECOND, 'Node B Ring 3 load observation missing')
                    require(block(1, 0x10) == 0 and block(1, 0x18) == LIVE_RESULT, 'Node B result/marker layout changed')
                    require(block(1, LIVE_FAULT_MARKER) == 1 and block(1, LIVE_GATE) == 0 and block(1, LIVE_POST) == 0, 'Node B fault marker or gates differ from the hardware fault model')
                elif stage == 4:
                    require(block(1, 0) == LIVE_VALUE and block(1, 8) == LIVE_SECOND and block(1, 0x18) == LIVE_RESULT, 'Node B Ring 3 load observation missing')
                    require(block(1, LIVE_GATE) == 0 and block(1, LIVE_POST) == 0 and block(1, LIVE_FAULT_MARKER) == 0, 'Node B gates/marker differ from the sharing model')
                elif stage == 6:
                    require(block(1, 0) == LIVE_VALUE and block(1, 8) == LIVE_SECOND and block(1, 0x18) == LIVE_RESULT, 'Node B did not read the shared bytes after Node A exited')
                    require(block(1, LIVE_GATE) == 1 and block(1, LIVE_POST) == 0 and block(1, LIVE_FAULT_MARKER) == 0, 'Node B post-A-exit gates differ from the model')
                else:
                    raise failure('MemoryObject live QMP: unexpected Node B block stage')

            fault = words(qmp, d[13], 16)
            if expected_mode == 1 and stage >= 4:
                b_root = root_values[1]
                require(fault[0] == 0x1_00000002 and fault[1] == 0x1_00000002 and fault[2] == 0x102, 'hardware fault recorded a foreign Node/Thread/Space')
                require(fault[3] == b_root, 'hardware fault CR3 is not Node B root')
                require(fault[4] == 14 and fault[5] == 0x7, 'hardware fault vector/error are not #PF write-protection')
                require(fault[6] == user_va('neu_multi_live_b_store_site'), 'hardware fault RIP is not the Ring 3 store site')
                require(fault[7] == LIVE_STACK[1] + 0xfb8 and fault[8] == LIVE_VA[1], 'hardware fault RSP/CR2 differ from the shared read-only page')
                require(fault[9] == 0x3b and fault[10] == 0x33 and fault[11] & 0x202 == 0x202 and fault[11] & 0x27100 == 0, 'hardware fault selectors/flags are not CPL3 user state')
                require(LIVE_KSTACK[1] <= fault[12] < LIVE_KSTACK[1] + 0x2000, 'hardware fault handler stack is outside Node B kernel stack')
                require(fault[13] == 1 and fault[14] == fault[15] == 0, 'hardware fault record is incomplete or synthetic')
            else:
                require(not any(fault), 'unexpected hardware fault record in a non-fault profile')

            def saved_frame(address):
                row = read(address, 160)
                registers = list(struct.unpack_from('<15Q', row, 0))
                rip, cs, flags, rsp, ss = struct.unpack_from('<5Q', row, 120)
                require(cs == 0x3b and ss == 0x33 and flags & 0x202 == 0x202 and flags & 0x27100 == 0, 'saved frame is not a Ring 3 frame')
                return {'rip': rip, 'cs': cs, 'ss': ss, 'flags': flags, 'rsp': rsp, 'rbx': registers[13]}

            def live_frame(address, index, start, end):
                frame = saved_frame(address)
                require(user_va(start) <= frame['rip'] < user_va(end), 'saved RIP is outside the expected Ring 3 gate')
                require(frame['rsp'] == LIVE_STACK[index] + 0xfb8, 'saved RSP is outside the private user stack page')
                require(frame['rbx'] == LIVE_DATA + LIVE_BLOCK[index], 'saved RBX does not name this Node private block')
                root = root_values[index]
                require(walk(root, frame['rip'] & ~4095)['present'] and walk(root, frame['rip'] & ~4095)['execute'], 'saved RIP has no executable user leaf')
                require(walk(root, frame['rsp'] & ~4095)['present'] and walk(root, frame['rsp'] & ~4095)['write'], 'saved RSP has no writable user leaf')
                return frame

            def initial_frame(address, index):
                frame = saved_frame(address)
                require(frame['rip'] == 0x400000 and frame['rsp'] == LIVE_STACK[index] + 0xff8 and frame['rbx'] == 0 and frame['flags'] == 0x202, 'initial saved frame differs from the user entry contract')
                return frame

            saved = {0: None, 1: None, 2: None}
            if stage in (2, 3):
                require(d[100] and d[101] and not d[102], 'missing fresh Node saved frames')
                saved[0] = initial_frame(d[100], 0)
                saved[1] = initial_frame(d[101], 1)
            elif stage == 4:
                require(d[100] and not d[102], 'missing Node A saved frame')
                saved[0] = live_frame(d[100], 0, 'neu_multi_live_a_post_gate', 'neu_multi_live_a_post_gate_end')
                if expected_mode == 1:
                    require(not d[101], 'faulting Node B retained a saved frame')
                elif expected_mode == 2:
                    saved[1] = live_frame(d[101], 1, 'neu_multi_live_b_wait_gate', 'neu_multi_live_b_wait_gate_end')
                else:
                    saved[1] = live_frame(d[101], 1, 'neu_multi_live_b_gate', 'neu_multi_live_b_gate_end')
            elif stage == 5:
                if expected_mode == 2:
                    require(not d[100] and d[101], 'Node A saved frame survived its teardown')
                    saved[1] = live_frame(d[101], 1, 'neu_multi_live_b_wait_gate', 'neu_multi_live_b_wait_gate_end')
                else:
                    require(d[100] and not d[101], 'surviving Node A lost its saved frame')
                    saved[0] = live_frame(d[100], 0, 'neu_multi_live_a_post_gate', 'neu_multi_live_a_post_gate_end')
            elif stage == 6 and expected_mode == 2:
                require(not d[100] and d[101], 'Node B saved frame absent after its post-exit read')
                saved[1] = live_frame(d[101], 1, 'neu_multi_live_b_post_gate', 'neu_multi_live_b_post_gate_end')
            else:
                require(d[100:103] == [0, 0, 0], 'terminal stage retained a saved frame')

            if stage == 0:
                baseline = {'total': total, 'free': free, 'tables': set(live_tables),
                            'frames': {f['physical'] for f in live_frames}, 'bitmap': bitmap,
                            'kernel_tables': set(common_tables)}
                require(not live_objects and not live_slots and mapping_total == 0, 'baseline already owns test resources')
            if stage == 7:
                require(baseline is not None, 'missing live baseline')
                require(original is not None, 'missing live object backing record')
                require(total == baseline['total'] and free == baseline['free'] and bitmap == baseline['bitmap'], 'final allocator differs from the exact baseline bitmap')
                require(set(live_tables) == baseline['tables'] and {f['physical'] for f in live_frames} == baseline['frames'], 'final table/frame inventory differs from baseline')
                require(set(common_tables) == baseline['kernel_tables'], 'kernel root tables changed during the live test')
                for f in original['frames']:
                    require(not frames[f[0]]['live'], 'collected object frame identity still live')
                for physical in original['physical']:
                    require(not allocated(physical), 'collected object backing still allocated')

            snapshot = {'stage': stage, 'mode': mode, 'cpu': cpu, 'descriptor': d,
                        'frames': frames, 'objects': objects, 'spaces': spaces, 'mappings': mappings,
                        'tables': tables, 'descriptors': descriptors, 'root_users': root_users,
                        'saved': saved, 'fault': fault, 'backing': backing,
                        'allocator': {'total': total, 'free': free, 'allocated': total - free,
                                      'bitmap_sha256': hashlib.sha256(bitmap).hexdigest(),
                                      'bitmap_words': list(struct.unpack('<' + 'Q' * (len(bitmap) // 8), bitmap))}}
            previous[stage] = snapshot
            output.append(snapshot)
        finally:
            qmp.call('cont')
        if len(previous) == len(expected_stages):
            return {'stages': expected_stages, 'mode': expected_mode,
                    'name': LIVE_CASES[expected_mode],
                    'baseline': {k: v for k, v in baseline.items() if k not in ('bitmap', 'tables', 'frames', 'kernel_tables')},
                    'backing': original, 'checkpoints': output}
    raise failure('MemoryObject live QMP: missing bounded checkpoint(s)')


def verify_fail_stop(qmp, before, words, failure):
    """At the panic halt, prove the poisoned transaction was not rolled back."""
    def walk(root, address):
        entries = []
        table = root
        for shift in (39, 30, 21, 12):
            entry = words(qmp, table + ((address >> shift) & 511) * 8, 1, True)[0]
            entries.append(entry)
            if not entry & 1:
                return {'present': False, 'entries': entries}
            table = entry & ADDRESS
        return {'present': True, 'entries': entries, 'physical': table}
    root = before['inventories'][0]['root']
    partial = walk(root, 0x5ff000)
    absent = walk(root, 0x600000)
    obj = next(o for o in before['objects'] if o['live'])
    frame = before['frames'][obj['frames'][0][0]]
    d = before['descriptor']
    layout = d[16:54]
    live_shared = 0
    for slot in range(64):
        address = layout[9] + slot * layout[10]
        live = words(qmp, address + layout[17], 1)[0] & 255
        tag = words(qmp, address + layout[16], 1)[0]
        live_shared += bool(live and tag == 1)
    bitmap = []
    for start in range(0, len(before['allocator']['bitmap_words']), 256):
        count = min(256, len(before['allocator']['bitmap_words']) - start)
        bitmap += words(qmp, d[66] + start * 8, count)
    if not partial['present'] or partial['physical'] != frame['physical'] or absent['present'] or live_shared or bitmap != before['allocator']['bitmap_words']:
        raise failure('MemoryObject fail-stop did not preserve the expected poisoned partial transaction')
    return {'expected_terminal_failure': True, 'recoverable_rollback': False,
            'partial_leaf': partial, 'missing_second_leaf': absent, 'published_shared_records': live_shared}
CAP_CASES = ('memory-object-cap',)
CAP_MAGIC = 0x4e45554d4f000003
CAP_WORDS = 194
CAP_VALUE = 0x4e45554d4f434150
CAP_SECOND = 0x5a5a0123456789ac
CAP_RESULT = 0x4c49564543415001
CAP_VA = 0x600000
CAP_DATA = 0x402000
CAP_STACK = (0x404000, 0x408000)
CAP_BLOCK = (0x240, 0x2c0)
CAP_FAILED = (1 << 64) - 18
CAP_HANDLE_A = (1 << 32) | 4
CAP_HANDLE_B = (2 << 32) | 7
CAP_WINDOW = 0xffff900000000000


def observe_cap(qmp, path, symbols, deadline, words, register, failure, output,
                expected_mode, image):
    """Prove K8B capability authority and transfer from stopped-CPU state.

    The guest publishes layout/address metadata only. Every live object,
    handle, queued token, mapping, page-table leaf, allocator bit and payload
    byte below is read from production memory. The serial 'verified' scalar is
    never consulted.
    """
    context = {'stage': None}

    def require(condition, message):
        if not condition:
            raise failure(f'MemoryObject capability QMP: stage={context["stage"]}: ' + message)

    def read(address, length, physical=False):
        data = bytearray()
        for offset in range(0, (length + 7) // 8, 256):
            count = min(256, (length + 7) // 8 - offset)
            data.extend(struct.pack('<' + 'Q' * count, *words(qmp, address + offset * 8, count, physical)))
        return bytes(data[:length])

    def u64(data, offset=0):
        return struct.unpack_from('<Q', data, offset)[0]

    def u32(data, offset=0):
        return struct.unpack_from('<I', data, offset)[0]

    def identity(data, offset=0):
        return [u64(data, offset), u64(data, offset + 8)]

    def u64s(data, count, offset=0):
        return list(struct.unpack_from('<' + 'Q' * count, data, offset))

    raw = image.read_bytes()
    header = struct.unpack_from('<16sHHIQQQIHHHHHH', raw)

    def image_bytes(address, length):
        for index in range(header[10]):
            segment = struct.unpack_from('<IIQQQQQQ', raw, header[5] + index * header[9])
            if segment[0] == 1 and segment[3] <= address and address + length <= segment[3] + segment[5]:
                start = segment[2] + address - segment[3]
                return raw[start:start + length]
        raise failure('MemoryObject capability QMP: image range is not file-backed')

    payload = image_bytes(symbols['neu_multi_payload'],
                          symbols['neu_multi_payload_end'] - symbols['neu_multi_payload'])
    require(0 < len(payload) <= 4096, 'invalid embedded payload bounds')
    code_page = payload + bytes(4096 - len(payload))

    def user_va(name):
        return 0x400000 + symbols[name] - symbols['neu_multi_payload']

    previous = {}
    expected_stages = list(range(10))
    baseline = None
    original = None
    space_tables = {}
    space_stacks = {}
    space_frames = {}
    bootstrap_a = None
    bootstrap_b = None
    while time.monotonic() < deadline:
        cpu_hint = qmp.call('human-monitor-command', {'command-line': 'info registers'})
        rip = register(cpu_hint, 'RIP')
        if not symbols['neu_memory_cap_wait'] <= rip < symbols['neu_memory_cap_wait_end']:
            if len(previous) == len(expected_stages) and 'MemoryObject capability test passed:' in path.read_text(errors='replace'):
                break
            time.sleep(0.001)
            continue
        hint = words(qmp, symbols['neu_memory_cap_witness'], 3)
        if hint[1] in previous:
            time.sleep(0.001)
            continue
        qmp.call('stop')
        try:
            cpu = qmp.call('human-monitor-command', {'command-line': 'info registers'})
            if not symbols['neu_memory_cap_wait'] <= register(cpu, 'RIP') < symbols['neu_memory_cap_wait_end']:
                continue
            d = words(qmp, symbols['neu_memory_cap_witness'], CAP_WORDS)
            magic, stage, schema, kernel, va, object_raw, value, second, result = d[:9]
            context['stage'] = stage
            node_a, node_b, space_a, space_b, thread_a, thread_b = d[9:15]
            if stage in previous:
                continue
            require(magic == CAP_MAGIC and schema == 1 and expected_mode == 0, 'invalid capability descriptor schema/profile')
            require(va == CAP_VA and value == CAP_VALUE and second == CAP_SECOND and result == CAP_RESULT, 'wrong capability payload contract')
            require(stage == len(previous), 'missing or reordered capability checkpoint')
            require('CPL=0' in cpu and register(cpu, 'CR3') == kernel and register(cpu, 'RFL') & 0x600 == 0, 'unsafe capability diagnostic CPU boundary')

            layout = d[16:55]
            require(layout[0] == 1 and layout[3] == 16 and layout[11] == 64 and layout[20] == 4 and layout[28] == 2, 'unexpected bounded registry layout')
            require(layout[34:38] == [16, 16, 24, 24], 'identity/provenance layout mismatch')
            frames = []
            for slot in range(16):
                row = read(layout[1] + slot * layout[2], layout[2])
                owner = layout[8]
                frames.append({'slot': slot, 'physical': u64(row, layout[4]), 'generation': u64(row, layout[5]),
                               'live': row[layout[6]], 'initialized': row[layout[7]],
                               'owner': [u64(row, owner), *identity(row, owner + 8)]})
            mappings = []
            for slot in range(64):
                row = read(layout[9] + slot * layout[10], layout[10])
                provenance = layout[16]
                mappings.append({'slot': slot, 'space': identity(row, layout[12]), 'address': u64(row, layout[13]),
                                 'frame': identity(row, layout[14]), 'write': row[layout[15]], 'execute': row[layout[15] + 1],
                                 'object': [u64(row, provenance), *identity(row, provenance + 8)], 'live': row[layout[17]]})
            memory_objects = []
            for slot in range(4):
                row = read(layout[18] + slot * layout[19], layout[19])
                memory_objects.append({'slot': slot, 'generation': u64(row, layout[21]), 'live': row[layout[22]],
                                       'creator': row[layout[23]], 'frames': [identity(row, layout[24] + i * 16) for i in range(2)],
                                       'pages': u64(row, layout[25]), 'cap_refs': row[layout[38]]})
            cap_layout = d[96:114]
            require(cap_layout[0] == 1 and cap_layout[3] == 6 and cap_layout[13] == 8
                    and cap_layout[14] == 8 and cap_layout[15] == 1 and cap_layout[16] == 1,
                    'invalid capability registry layout')
            cap_objects = []
            for slot in range(cap_layout[3]):
                row = read(cap_layout[4] + slot * cap_layout[5], cap_layout[5])
                cap_objects.append({'slot': slot, 'generation': u32(row, cap_layout[6]),
                                    'references': u32(row, cap_layout[7]), 'pins': u32(row, cap_layout[8]),
                                    'live': row[cap_layout[9]] != 0, 'kind': row[cap_layout[10]],
                                    'target': u64(row, cap_layout[11])})
            spaces = []
            for slot in range(2):
                row = read(layout[26] + slot * layout[27], layout[27])
                spaces.append({'slot': slot, 'generation': u64(row, layout[29]), 'live': row[layout[30]],
                               'root': u64(row, layout[31]), 'pins': row[layout[32]], 'refs': row[layout[33]]})

            allocator = d[146:154]
            total = u64(read(allocator[0], 8))
            free = u64(read(allocator[1], 8))
            bitmap = read(allocator[2], ((total + 63) // 64) * 8)
            require(allocator[3] * 8 >= len(bitmap), 'allocator bitmap capacity is too small')
            require(free <= total and sum(byte.bit_count() for byte in bitmap) == total - free, 'allocator bitmap/counters disagree')
            regions = read(allocator[4], allocator[5] * 24)
            kernel_start, kernel_end = u64(read(allocator[6], 8)), u64(read(allocator[7], 8))
            spans = []
            for offset in range(0, len(regions), 24):
                start, length, kind = struct.unpack_from('<QQQ', regions, offset)
                if kind != 0:
                    continue
                end = start + length
                start = max(start, 4096)
                spans += [(a, b) for a, b in ((start, min(end, kernel_start)), (max(start, kernel_end), end)) if a < b]
            require(sum((end - start) // 4096 for start, end in spans) == total, 'allocator total differs from raw memory-map spans')

            def allocated(physical):
                ordinal = 0
                for start, end in spans:
                    if start <= physical < end:
                        ordinal += (physical - start) // 4096
                        return bool(bitmap[ordinal // 8] & (1 << (ordinal % 8)))
                    ordinal += (end - start) // 4096
                raise failure('MemoryObject capability QMP: physical owner outside allocatable spans')

            tables_layout = d[154:160]
            tables = []
            for slot in range(tables_layout[2]):
                row = read(tables_layout[0] + slot * tables_layout[1], tables_layout[1])
                tables.append({'slot': slot, 'physical': u64(row, tables_layout[3]),
                               'level': row[tables_layout[4]], 'pinned': row[tables_layout[5]]})
            live_tables = {t['physical']: t for t in tables if t['physical']}
            require(len(live_tables) == sum(bool(t['physical']) for t in tables), 'duplicate table ownership')
            require(all(1 <= t['level'] <= 4 for t in live_tables.values()), 'invalid table level')

            def parse_table(layout_words, label, node_raw):
                if not any(layout_words):
                    return None
                require(layout_words[0] == 1 and layout_words[5] == 8 and layout_words[16] == 8
                        and layout_words[19] == 1 and layout_words[17] == 8, 'invalid table layout')
                require(layout_words[18] == node_raw, 'table identity mismatch')
                slots = []
                for index in range(8):
                    row = read(layout_words[3] + index * layout_words[4], layout_words[4])
                    slots.append({'slot': index, 'generation': u32(row, layout_words[6]),
                                  'live': row[layout_words[7]] != 0, 'kind': row[layout_words[8]],
                                  'target': u64(row, layout_words[9]), 'rights': row[layout_words[10]],
                                  'transferable': row[layout_words[11]] != 0})
                bootstrap = []
                for index in range(8):
                    row = read(layout_words[14] + index * 16, 16)
                    tag = u64(row, 0)
                    require(tag in (0, 1), 'invalid bootstrap Option tag')
                    bootstrap.append(u64(row, 8) if tag else None)
                require(all((handle is not None) == (index < layout_words[17])
                            for index, handle in enumerate(bootstrap)), 'bootstrap limit/handle mismatch')
                return {'label': label, 'base': layout_words[1], 'slots': slots,
                        'bootstrap': bootstrap, 'sealed': layout_words[13] != 0,
                        'live': layout_words[12] != 0, 'id': layout_words[18]}

            table_a = parse_table(d[56:76], 0, node_a) if stage >= 2 else None
            table_b = parse_table(d[76:96], 1, node_b) if stage >= 2 else None
            if stage in (0, 1):
                require(not any(d[56:96]), 'capability table published before Node construction')

            channel = d[114:146]
            require(channel[0] == 1 and channel[28] == 2 and channel[29] == 2, 'invalid channel layout')
            channel_base = channel[1]
            channel_live = read(channel_base + channel[4], 1)[0] != 0
            counters = {name: u64s(read(channel_base + offset, 16), 2)
                        for name, offset in (('sent', channel[9]), ('received', channel[10]), ('discarded', channel[11]))}
            queues = []
            for side in range(2):
                fifo = channel_base + channel[8] + side * channel[12]
                head = u64(read(fifo + channel[13], 8))
                tail = u64(read(fifo + channel[14], 8))
                length = u64(read(fifo + channel[15], 8))
                values = u64s(read(fifo + channel[16], 16), 2)
                transfers = []
                for slot in range(2):
                    transfer = fifo + channel[17] + slot * channel[18]
                    transfers.append({'live': read(transfer + channel[19], 1)[0] != 0,
                                      'object': u64(read(transfer + channel[20], 8)),
                                      'rights': read(transfer + channel[21], 1)[0]})
                queues.append({'head': head, 'tail': tail, 'len': length,
                               'values': values, 'transfers': transfers})
            for side, queue in enumerate(queues):
                require(queue['head'] < 2 and queue['tail'] < 2 and queue['len'] <= 2
                        and (queue['head'] + queue['len']) % 2 == queue['tail'], 'invalid FIFO indices')
                require(all(value == 0 for value in queue['values']), 'unexpected scalar message in capability queue')
                for slot, transfer in enumerate(queue['transfers']):
                    occupied = (slot + 2 - queue['head']) % 2 < queue['len']
                    if transfer['live']:
                        require(occupied and transfer['object'] != 0, 'live transfer outside FIFO occupancy')
                    else:
                        require(transfer['object'] == 0 and transfer['rights'] == 0, 'stale transfer identity in empty slot')
            queued = sum(transfer['live'] for queue in queues for transfer in queue['transfers'])

            live_caps = [o for o in cap_objects if o['live']]
            for o in live_caps:
                require(o['kind'] in (2, 3, 4, 5) and o['references'] > 0 and o['pins'] == 0,
                        'invalid live capability object')

            def cap_raw(o):
                return (o['kind'] << 40) | (o['generation'] << 8) | (o['slot'] + 1)

            by_raw = {cap_raw(o): o for o in live_caps}
            mem_caps = [o for o in live_caps if o['kind'] == 5]
            as_a = [o for o in live_caps if o['kind'] == 3 and o['target'] == space_a]
            as_b = [o for o in live_caps if o['kind'] == 3 and o['target'] == space_b]
            flags = [o for o in live_caps if o['kind'] == 2]
            require(len(mem_caps) == (1 if 2 <= stage <= 7 else 0), 'wrong MemoryObject capability count')
            require(len(as_a) == (1 if 2 <= stage <= 7 else 0), 'wrong Node A AddressSpace capability count')
            require(len(as_b) == (1 if 2 <= stage <= 6 else 0), 'wrong Node B AddressSpace capability count')
            require(len(flags) == (1 if 2 <= stage <= 6 else 0), 'wrong Node B Flag capability count')
            if mem_caps:
                require(mem_caps[0]['target'] == object_raw, 'capability names a foreign MemoryObject generation')
            if flags:
                require(flags[0]['target'] == 0, 'Flag capability has a mechanism target')
            if table_a is not None:
                require(all(slot['live'] and slot['kind'] == 4 for slot in table_a['slots'][4:8]),
                        'Node A endpoint handles are not four live endpoints')
                endpoint_a = table_a['slots'][4]['target']
                require(all(slot['target'] == endpoint_a for slot in table_a['slots'][4:8]),
                        'Node A endpoint handles name different endpoints')
                require(endpoint_a in by_raw and by_raw[endpoint_a]['kind'] == 4,
                        'Node A endpoint capability is absent')
            if table_b is not None:
                endpoint_b = table_b['slots'][4]['target']
                require(table_b['slots'][4]['live'] and table_b['slots'][5]['live']
                        and table_b['slots'][7]['live'] and table_b['slots'][4]['kind'] == 4,
                        'Node B endpoint handles differ from the close/receive model')
                require(table_b['slots'][5]['target'] == endpoint_b and table_b['slots'][7]['target'] == endpoint_b,
                        'Node B endpoint handles name different endpoints')
                require(endpoint_b in by_raw and by_raw[endpoint_b]['kind'] == 4,
                        'Node B endpoint capability is absent')

            roles = {}
            if as_a:
                roles['as_a'] = cap_raw(as_a[0])
            if as_b:
                roles['as_b'] = cap_raw(as_b[0])
            if mem_caps:
                roles['mem'] = cap_raw(mem_caps[0])
            if flags:
                roles['flag'] = cap_raw(flags[0])
            if table_a is not None:
                endpoint_a_raw = table_a['slots'][4]['target']
                if endpoint_a_raw in by_raw and by_raw[endpoint_a_raw]['kind'] == 4:
                    roles['ep_a'] = endpoint_a_raw
            if table_b is not None:
                endpoint_b_raw = table_b['slots'][4]['target']
                if endpoint_b_raw in by_raw and by_raw[endpoint_b_raw]['kind'] == 4:
                    roles['ep_b'] = endpoint_b_raw

            handle_counts = {}
            for table in (table_a, table_b):
                if table is None:
                    continue
                for slot in table['slots']:
                    if slot['live']:
                        handle_counts[slot['target']] = handle_counts.get(slot['target'], 0) + 1
            queue_counts = {}
            for queue in queues:
                for transfer in queue['transfers']:
                    if transfer['live']:
                        queue_counts[transfer['object']] = queue_counts.get(transfer['object'], 0) + 1
            require(set(handle_counts) <= set(by_raw) and set(queue_counts) <= set(by_raw),
                    'handle/queue names a dead capability object')
            for o in live_caps:
                expected = handle_counts.get(cap_raw(o), 0) + queue_counts.get(cap_raw(o), 0)
                require(o['references'] == expected, 'capability reference count differs from handles plus queue')

            expected_stats = {0: (0, 0), 1: (0, 0), 2: (6, 16), 3: (6, 15), 4: (6, 15),
                              5: (6, 15), 6: (6, 15), 7: (3, 8), 8: (0, 0), 9: (0, 0)}[stage]
            actual_stats = (len(live_caps), sum(o['references'] for o in live_caps))
            require(actual_stats == expected_stats,
                    f'capability registry totals differ from the model: {actual_stats} != {expected_stats} '
                    f'objects={[(o["slot"], o["kind"], o["references"]) for o in live_caps]}')
            require(sum(o['pins'] for o in live_caps) == 0, 'unexpected capability pin at checkpoint')
            require(d[171] == actual_stats[0] and d[172] == actual_stats[1] and d[173] == 0 and d[174] == queued,
                    'published capability accounting differs from raw registry')
            require(d[175] == 0, 'pending MemoryObject reference survived the architecture drain')

            if table_a is not None:
                require(table_a['sealed'] and table_a['live'] == (stage <= 4) and table_a['id'] == node_a,
                        'Node A table liveness/identity differs from the exit model')
                if bootstrap_a is None:
                    bootstrap_a = list(table_a['bootstrap'])
                else:
                    require(table_a['bootstrap'] == bootstrap_a, 'Node A bootstrap handles changed during transfer')
                expected = [('as_a', 16, False), ('as_a', 16, False), ('as_a', 0, False),
                            ('mem', 15, True) if stage <= 3 else None,
                            ('ep_a', 224, False), ('ep_a', 96, False), ('ep_a', 32, False), ('ep_a', 64, False)]
                require(len(table_a['slots']) == len(expected), 'wrong Node A slot count')
                for index, (slot, want) in enumerate(zip(table_a['slots'], expected)):
                    if want is None:
                        require(not slot['live'] and slot['kind'] == 0 and slot['target'] == 0
                                and slot['rights'] == 0 and not slot['transferable'], 'Node A free slot is not empty')
                        require(slot['generation'] == 1, 'Node A free slot generation changed')
                        continue
                    role, rights, transferable = want
                    require(role in roles, 'Node A expected capability role is absent')
                    require(slot['live'] and slot['target'] == roles[role] and slot['rights'] == rights
                            and slot['transferable'] == transferable, 'Node A authority/rights differ from the model')
                    require(slot['kind'] == by_raw[roles[role]]['kind'], 'Node A slot kind differs from its object')
                    require(slot['generation'] == 1, 'Node A slot generation changed')
                if stage == 2:
                    require(table_a['bootstrap'] == [((1 << 32) | (i + 1)) for i in range(8)],
                            'Node A bootstrap handles are not the initial grants')
            if table_b is not None:
                require(table_b['sealed'] and table_b['live'] == (stage <= 5) and table_b['id'] == node_b,
                        'Node B table liveness/identity differs from the exit model')
                if bootstrap_b is None:
                    bootstrap_b = list(table_b['bootstrap'])
                else:
                    require(table_b['bootstrap'] == bootstrap_b, 'Node B bootstrap handles changed during transfer')
                expected = [('as_b', 16, False), ('as_b', 16, False), ('as_b', 0, False),
                            ('flag', 1, True), ('ep_b', 224, False), ('ep_b', 96, False),
                            ('ep_b', 32, False) if stage == 2 else ('mem', 15, True) if stage == 5 else None,
                            ('ep_b', 64, False)]
                require(len(table_b['slots']) == len(expected), 'wrong Node B slot count')
                for index, (slot, want) in enumerate(zip(table_b['slots'], expected)):
                    if want is None:
                        require(not slot['live'] and slot['kind'] == 0 and slot['target'] == 0
                                and slot['rights'] == 0 and not slot['transferable'], 'Node B free slot is not empty')
                        require(slot['generation'] == (2 if stage >= 5 else 1),
                                f'Node B free slot generation changed: slot={index} gen={slot["generation"]} stage={stage}')
                        continue
                    role, rights, transferable = want
                    require(role in roles, 'Node B expected capability role is absent')
                    require(slot['live'] and slot['target'] == roles[role] and slot['rights'] == rights
                            and slot['transferable'] == transferable, 'Node B authority/rights differ from the model')
                    require(slot['kind'] == by_raw[roles[role]]['kind'], 'Node B slot kind differs from its object')
                    require(slot['generation'] == (2 if index == 6 and stage >= 5 else 1),
                            'Node B slot generation changed')
                if stage == 2:
                    require(table_b['bootstrap'] == [((1 << 32) | (i + 1)) for i in range(8)],
                            'Node B bootstrap handles are not the initial grants')
            if stage == 0 or stage == 1:
                require(table_a is None and table_b is None, 'table appeared before Node construction')

            expected_queues = {0: (0, 0), 1: (0, 0), 2: (0, 0), 3: (0, 0), 4: (0, 1),
                               5: (0, 0), 6: (1, 0), 7: (1, 0), 8: (0, 0), 9: (0, 0)}[stage]
            require(tuple(queue['len'] for queue in queues) == expected_queues, 'channel queue occupancy differs from the model')
            for side, length in enumerate(expected_queues):
                if not length:
                    continue
                transfer = queues[side]['transfers'][queues[side]['head']]
                require(transfer['live'] and 'mem' in roles and transfer['object'] == roles['mem']
                        and transfer['rights'] == 15, 'queued capability identity/rights were amplified or changed')
            expected_live = 2 <= stage <= 7
            require(channel_live == expected_live, 'channel liveness differs from the model')
            expected_counters = {0: ([0, 0], [0, 0], [0, 0]), 1: ([0, 0], [0, 0], [0, 0]),
                                 2: ([0, 0], [0, 0], [0, 0]), 3: ([0, 0], [0, 0], [0, 0]),
                                 4: ([1, 0], [0, 0], [0, 0]), 5: ([1, 0], [0, 1], [0, 0]),
                                 6: ([1, 1], [0, 1], [0, 0]), 7: ([1, 1], [0, 1], [0, 0]),
                                 8: ([1, 1], [0, 1], [1, 0]), 9: ([1, 1], [0, 1], [1, 0])}[stage]
            require((counters['sent'], counters['received'], counters['discarded']) == expected_counters,
                    'channel counters differ from the exactly-once transfer model')

            live_objects = [o for o in memory_objects if o['live']]
            object_mappings = [m for m in mappings if m['live'] and m['object'][0] == 1]
            if stage == 0:
                require(not live_objects, 'baseline already owns a MemoryObject')
            elif stage == 1:
                require(len(live_objects) == 1 and not object_mappings, 'object creation model mismatch')
                obj = live_objects[0]
                require(obj['creator'] == 1 and obj['cap_refs'] == 0 and obj['pages'] == 2, 'new object ownership/size mismatch')
                original = {'slot': obj['slot'], 'generation': obj['generation'],
                            'id': [obj['slot'], obj['generation']], 'frames': obj['frames']}
            elif stage <= 8:
                require(len(live_objects) == 1 and original is not None, 'object disappeared before collection')
                obj = live_objects[0]
                require([obj['slot'], obj['generation']] == original['id'] and obj['frames'] == original['frames'],
                        'object identity/backing changed across capability transfer')
                require(obj['creator'] == 0, 'creator reference survived bootstrap release')
                require(obj['pages'] == 2, 'object page count changed')
                require(obj['cap_refs'] == (1 if stage <= 7 else 0), 'capability paging reference differs from the model')
                require(len(object_mappings) == (2 if stage <= 7 else 0), 'object mapping count differs from the model')
                require(object_raw == (obj['generation'] << 8 | (obj['slot'] + 1)), 'witness object id is not the production object')
            else:
                require(not live_objects and not object_mappings, 'collected object still live or mapped')
                require(original is not None, 'missing object backing record')
                slot = original['slot']
                require(not memory_objects[slot]['live'] and memory_objects[slot]['cap_refs'] == 0,
                        'collected object slot/capability reference survived')

            live_frames = [f for f in frames if f['live']]
            live_spaces = [s for s in spaces if s['live']]
            expected_slots = {0: (), 1: (), 2: (0, 1), 3: (0, 1), 4: (0, 1),
                              5: (0, 1), 6: (0, 1), 7: (0,), 8: (), 9: ()}[stage]
            require([s['slot'] for s in live_spaces] == list(expected_slots), 'live space set differs from teardown model')
            require(len({s['root'] for s in live_spaces}) == len(live_spaces), 'shared CR3 root identity')

            if stage >= 2:
                require(node_a == 0x1_00000001 and node_b == 0x1_00000002, 'wrong Node identity')
                require(space_a == 0x101 and space_b == 0x102, 'wrong Space identity')
                require(thread_a == 0x1_00000001 and thread_b == 0x1_00000002, 'wrong Thread identity')
            else:
                require(node_a == node_b == space_a == space_b == thread_a == thread_b == 0, 'pre-Node identity published too early')

            descriptors = []
            for index, fields in enumerate((d[176:181], d[181:186])):
                if not fields[0]:
                    require(all(value == 0 for value in fields), 'partial live space token')
                    descriptors.append(None)
                    continue
                root = u64(read(fields[0], 8))
                sid = identity(read(fields[1], 16))
                state = read(fields[3], 1)[0]
                inventory = list(read(fields[2], 64))
                stacks = list(read(fields[4], 64))
                descriptors.append({'root': root, 'id': sid, 'state': state,
                                    'tables': inventory, 'stack_tables': stacks})
            for index in range(2):
                require((descriptors[index] is not None) == (index in expected_slots), 'descriptor presence differs from teardown model')

            cache = {}

            def page(physical):
                if physical not in cache:
                    cache[physical] = words(qmp, physical, 512, True)
                return cache[physical]

            def walk(root, address):
                table, entries = root, []
                for depth, shift in enumerate((39, 30, 21, 12)):
                    require(table in live_tables and live_tables[table]['level'] == 4 - depth, 'walk entered a foreign/unowned table')
                    entry = page(table)[(address >> shift) & 511]
                    entries.append(entry)
                    if not entry & 1:
                        return {'present': False, 'entries': entries}
                    require(not depth < 3 or not entry & 128, 'unexpected large page')
                    table = entry & 0x000ffffffffff000
                return {'present': True, 'entries': entries, 'physical': table,
                        'write': all(e & 2 for e in entries), 'user': all(e & 4 for e in entries),
                        'execute': not any(e & (1 << 63) for e in entries)}

            root_users = {}

            def reachable(root):
                found = set()
                users = {}

                def visit(table, level, prefix=0, writable=True, user=True, executable=True):
                    require(table in live_tables and live_tables[table]['level'] == level,
                            f'tree has stale table reference: tree={context.get("tree")} table={table:#x} '
                            f'level={level} owned={live_tables.get(table)}')
                    require(table not in found, 'duplicate or cyclic table parent')
                    found.add(table)
                    for index, entry in enumerate(page(table)):
                        if not entry & 1:
                            continue
                        address = prefix | (index << (12 + (level - 1) * 9))
                        w, u, x = writable and bool(entry & 2), user and bool(entry & 4), executable and not bool(entry & (1 << 63))
                        if level > 1:
                            require(not entry & 128, 'unexpected large table leaf')
                            visit(entry & 0x000ffffffffff000, level - 1, address, w, u, x)
                        elif u:
                            require(0x400000 <= address < 0x800000, 'user mapping outside the bounded user range')
                            users[address] = {'physical': entry & 0x000ffffffffff000, 'write': w, 'execute': x}

                visit(root, 4)
                root_users[root] = users
                return found

            if baseline is None:
                baseline = {'total': total, 'free': free, 'bitmap': bitmap, 'live_tables': set(live_tables),
                            'kernel_tables': set()}
            context['tree'] = 'kernel'
            common_tables = reachable(kernel)
            if stage == 0:
                baseline['kernel_tables'] = set(common_tables)
            require(set(common_tables) == baseline['kernel_tables'], 'kernel root tables changed during the capability test')
            for slot in range(64):
                alias = walk(kernel, CAP_WINDOW + slot * 4096)
                require(alias['present'] == bool(tables[slot]['physical']), 'stale common workspace alias')
                if alias['present']:
                    require(alias['physical'] == tables[slot]['physical'] and alias['write']
                            and not alias['execute'] and not alias['user'], 'common workspace alias ownership/permissions')
            require(not walk(kernel, CAP_WINDOW + 511 * 4096)['present'], 'scratch alias survived a transaction')
            require(not root_users[kernel], 'kernel root exposes a user leaf')

            live_mappings = [m for m in mappings if m['live']]
            expected_mapping_count = (3 * len(expected_slots) + (2 if 0 in expected_slots else 0)
                                      + 3 * sum(1 for index in expected_slots if spaces[index]['pins']))
            require(len(live_mappings) == expected_mapping_count, 'live mapping ledger count differs from the model')
            data_bytes = {}
            for index in expected_slots:
                record = descriptors[index]
                require(record is not None, 'missing live space token')
                require(record['id'] == [index, 1] and record['state'] == 0 and spaces[index]['generation'] == 1,
                        'wrong Node-local SpaceId generation')
                require(spaces[index]['root'] == record['root'], 'descriptor root differs from production root')
                pins = 0 if stage == 6 or (stage == 5 and index == 0) else 1 if stage in (2, 3, 4, 5) else 0
                if stage == 7:
                    pins = 0
                require(spaces[index]['pins'] == pins, 'space pins differ from queued Thread model')
                require(spaces[index]['refs'] == 1, 'AddressSpace capability paging reference differs from the model')
                owned = {tables[i]['physical'] for i, value in enumerate(record['tables']) if value}
                require(0 not in owned and len(owned) == sum(record['tables']), 'inventory contains retired tables')
                require(not owned & set(baseline['kernel_tables']), 'foreign root table ownership')
                context['tree'] = f'space{index}'
                require(reachable(record['root']) == owned, 'reachable tables differ from token inventory')
                require(record['root'] in root_users, 'descriptor root is not the production walk root')
                users = root_users[record['root']]
                for slot in range(64):
                    alias = walk(record['root'], CAP_WINDOW + slot * 4096)
                    visible = bool(record['tables'][slot]) and not bool(record['stack_tables'][slot])
                    require(alias['present'] == visible, 'unauthorized or stale workspace alias')
                    if alias['present']:
                        require(alias['physical'] == tables[slot]['physical'] and alias['write']
                                and not alias['execute'] and not alias['user'], 'wrong workspace alias permissions/target')
                private = {0x400000, 0x401000, CAP_DATA}
                if pins:
                    private.add(CAP_STACK[index])
                shared = {CAP_VA + i * 4096 for i in range(2)} if index == 0 else set()
                require(set(users) == private | shared, 'root user leaves differ from the capability layout')
                other = 1 - index
                for i in range(2):
                    require(not walk(record['root'], CAP_STACK[other] + i * 4096)['present'], 'Node root aliases its sibling stack')
                ledger = [m for m in live_mappings if m['space'] == [index, 1] and m['address'] < 0x1_0000_0000]
                require({m['address'] for m in ledger} == private | shared, 'mapping ledger addresses differ from the live layout')
                for m in ledger:
                    f = frames[m['frame'][0]]
                    require(f['live'] and f['generation'] == m['frame'][1], 'mapping names a stale frame')
                    require(users[m['address']] == {'physical': f['physical'], 'write': bool(m['write']), 'execute': bool(m['execute'])},
                            'raw user leaf differs from mapping/frame record')
                    if m['address'] in shared:
                        require(live_objects and m['object'] == [1, *original['id']], 'wrong shared mapping provenance')
                        require(m['frame'] == live_objects[0]['frames'][(m['address'] - CAP_VA) // 4096], 'shared leaf maps the wrong object page')
                        require(bool(m['write']) and not m['execute'], 'shared leaf rights differ from the capability grant')
                    else:
                        require(m['object'][0] == 0 and f['owner'] == [0, index, 1], 'private frame ownership changed')
                code = walk(record['root'], 0x400000)
                require(code['present'] and code['user'] and code['execute'] and not code['write'], 'user code page permissions changed')
                require(read(code['physical'], 4096, True) == code_page, 'user code page differs from the ELF payload')
                require(walk(record['root'], 0x401000)['present'], 'private rodata leaf disappeared')
                data = walk(record['root'], CAP_DATA)
                require(data['present'] and data['write'] and not data['execute'] and data['user'], 'private data leaf permissions changed')
                data_bytes[index] = read(data['physical'], 4096, True)
                if index == 1:
                    require(not walk(record['root'], CAP_VA)['present'], 'Node B maps the shared object without authority')

            if original is not None:
                obj = live_objects[0] if live_objects else None
                if obj is not None:
                    backing = []
                    for fid in obj['frames']:
                        f = frames[fid[0]]
                        require(f['live'] and f['initialized'] and f['generation'] == fid[1]
                                and f['owner'] == [1, obj['slot'], obj['generation']], 'invalid object/frame generation or backing owner')
                        backing.append(f['physical'])
                    require(backing[0] != backing[1], 'duplicate object page')
                    page0, page1 = read(backing[0], 4096, True), read(backing[1], 4096, True)
                    if stage <= 2:
                        require(page0 == bytes(4096) and page1 == bytes(4096), 'object backing not zeroed before Ring 3 execution')
                    else:
                        require(u64(page0, 0) == CAP_VALUE and u64(page0, 8) == CAP_SECOND, 'Ring 3 object payload absent from shared backing')
                        require(page0[16:] == bytes(4096 - 16) and page1 == bytes(4096), 'unexpected shared backing mutation')
                else:
                    for fid in original['frames']:
                        require(not frames[fid[0]]['live'], 'collected frame identity still live')
                        require(not allocated(frames[fid[0]]['physical']), 'collected object backing still allocated')

            expected_frames = ((2 if 1 <= stage <= 8 else 0) + 3 * len(expected_slots)
                               + 3 * sum(1 for index in expected_slots if spaces[index]['pins']))
            require(len(live_frames) == expected_frames, 'live data frame count differs from the model')
            require(len({f['physical'] for f in live_frames}) == len(live_frames), 'duplicate data backing')
            require(not set(live_tables) & {f['physical'] for f in live_frames}, 'table/data ownership overlap')
            require(total - free == len(live_tables) + len(live_frames), 'orphan physical allocation')
            for physical in list(live_tables) + [f['physical'] for f in live_frames]:
                require(allocated(physical), 'live owner has a freed allocator bit')

            for index in expected_slots:
                for address in ((0x400000, 0x401000, CAP_DATA, CAP_STACK[index]) if spaces[index]['pins']
                               else (0x400000, 0x401000, CAP_DATA)):
                    mapping = next((m for m in live_mappings if m['space'] == [index, 1] and m['address'] == address), None)
                    require(mapping is not None and frames[mapping['frame'][0]]['owner'] == [0, index, 1],
                            'private mapping/frame owner differs from the live space')

            def block_word(index, offset):
                return u64(data_bytes[index], CAP_BLOCK[index] + offset)

            if stage == 2:
                require(all(u64(data_bytes[i], CAP_BLOCK[i] + offset) == 0 for i in (0, 1) for offset in range(0, 0x48, 8)),
                        'private block written before Ring 3 execution')
            elif stage == 3:
                require(block_word(0, 0x00) == CAP_VALUE and block_word(0, 0x08) == CAP_SECOND, 'Node A value store missing')
                require(block_word(0, 0x10) == 1 and block_word(0, 0x18) == CAP_RESULT and block_word(0, 0x30) == CAP_VALUE,
                        'Node A readback/readiness missing')
                require(block_word(0, 0x20) == 0 and block_word(0, 0x28) > 0, 'Node A fail gate state differs from the controller model')
                require(block_word(0, 0x38) == CAP_FAILED and block_word(0, 0x40) == 0, 'Node A failed transfer status differs from the SEND-only model')
                require(block_word(0, 0x48) == CAP_HANDLE_A and block_word(0, 0x50) == 0 and block_word(0, 0x58) == 0,
                        'Node A handle/send gate changed before the successful transfer')
                for offset in range(0, 0x48, 8):
                    if offset == 0x28:
                        continue
                    require(u64(data_bytes[1], CAP_BLOCK[1] + offset) == 0,
                            'Node B wrote before its receive gate was released')
                require(block_word(1, 0x28) > 0, 'Node B receive gate did not spin')
            elif stage == 4:
                require(block_word(0, 0x10) == 2 and block_word(0, 0x20) == 1 and block_word(0, 0x58) == 0,
                        'Node A send gate/readiness differs from the pre-release model')
                require(block_word(0, 0x38) == CAP_FAILED and block_word(0, 0x40) == 0 and block_word(0, 0x48) == CAP_HANDLE_A,
                        'Node A successful transfer status/handle changed unexpectedly')
                require(block_word(0, 0x18) == CAP_RESULT and block_word(0, 0x30) == CAP_VALUE, 'Node A payload observation missing')
                for offset in range(0, 0x48, 8):
                    if offset == 0x28:
                        continue
                    require(u64(data_bytes[1], CAP_BLOCK[1] + offset) == 0, 'Node B wrote before its receive gate was released')
                require(block_word(1, 0x28) > 0, 'Node B receive gate did not spin')
            elif stage == 5:
                require(block_word(0, 0x10) == 2 and block_word(0, 0x20) == 1 and block_word(0, 0x58) == 1,
                        'Node A post-send gate state differs from the model')
                require(block_word(0, 0x40) == 0 and block_word(0, 0x48) == CAP_HANDLE_A, 'Node A send result/handle changed')
                require(block_word(1, 0x00) == 0 and block_word(1, 0x08) == 0, 'Node B loaded bytes without a mapping')
                require(block_word(1, 0x10) == 1 and block_word(1, 0x18) == CAP_RESULT, 'Node B receive readiness/result missing')
                require(block_word(1, 0x20) == 1 and block_word(1, 0x28) > 0 and block_word(1, 0x58) == 0,
                        'Node B receive gate state differs from the controller model')
                require(block_word(1, 0x40) == 0 and block_word(1, 0x50) == CAP_HANDLE_B, 'Node B received handle differs from the fresh-slot contract')
            elif stage == 6:
                require(block_word(0, 0x10) == 2 and block_word(0, 0x20) == 1 and block_word(0, 0x58) == 1,
                        'Node A terminal block state changed')
                require(block_word(1, 0x10) == 2 and block_word(1, 0x20) == 1 and block_word(1, 0x58) == 1,
                        'Node B terminal block state differs from the model')
                require(block_word(1, 0x18) == CAP_RESULT and block_word(1, 0x40) == 0 and block_word(1, 0x50) == CAP_HANDLE_B,
                        'Node B send result/received handle changed')
            elif stage == 7:
                require(block_word(0, 0x10) == 2 and block_word(0, 0x20) == 1 and block_word(0, 0x58) == 1,
                        'Node A block did not survive Node B teardown')
                require(block_word(0, 0x38) == CAP_FAILED and block_word(0, 0x40) == 0,
                        'Node A transfer evidence changed after Node B teardown')
            elif stage in (8, 9):
                require(not data_bytes, 'retired space still exposes a private block')

            def saved_frame(address):
                row = read(address, 160)
                registers = list(struct.unpack_from('<15Q', row, 0))
                rip, cs, flags, rsp, ss = struct.unpack_from('<5Q', row, 120)
                require(cs == 0x3b and ss == 0x33 and flags & 0x202 == 0x202 and flags & 0x27100 == 0,
                        'saved frame is not a Ring 3 frame')
                return {'rip': rip, 'cs': cs, 'ss': ss, 'flags': flags, 'rsp': rsp, 'rbx': registers[13]}

            def initial_frame(address, index):
                frame = saved_frame(address)
                require(frame['rip'] == 0x400000 and frame['rsp'] == CAP_STACK[index] + 0xff8
                        and frame['rbx'] == 0 and frame['flags'] == 0x202,
                        'initial saved frame differs from the user entry contract')
                return frame

            def gate_frame(address, index, start, end, block):
                frame = saved_frame(address)
                require(user_va(start) <= frame['rip'] < user_va(end), 'saved RIP is outside the expected Ring 3 gate')
                require(frame['rsp'] == CAP_STACK[index] + 0xfb8, 'saved RSP is outside the private user stack page')
                require(frame['rbx'] == CAP_DATA + block, 'saved RBX does not name this Node private block')
                root = spaces[index]['root']
                require(walk(root, frame['rip'] & ~4095)['present'] and walk(root, frame['rip'] & ~4095)['execute'],
                        'saved RIP has no executable user leaf')
                require(walk(root, frame['rsp'] & ~4095)['present'] and walk(root, frame['rsp'] & ~4095)['write'],
                        'saved RSP has no writable user leaf')
                return frame

            saved = {}
            if stage == 2:
                require(d[160] and d[161], 'missing fresh Node saved frames')
                saved[0] = initial_frame(d[160], 0)
                saved[1] = initial_frame(d[161], 1)
            elif stage == 3:
                require(d[160] and d[161], 'missing Node A/B saved frames')
                saved[0] = gate_frame(d[160], 0, 'neu_multi_cap_a_fail_gate', 'neu_multi_cap_a_fail_gate_end', CAP_BLOCK[0])
                saved[1] = gate_frame(d[161], 1, 'neu_multi_cap_b_receive_gate', 'neu_multi_cap_b_receive_gate_end', CAP_BLOCK[1])
            elif stage == 4:
                require(d[160] and d[161], 'missing Node A/B saved frames')
                saved[0] = gate_frame(d[160], 0, 'neu_multi_cap_a_send_gate', 'neu_multi_cap_a_send_gate_end', CAP_BLOCK[0])
                saved[1] = gate_frame(d[161], 1, 'neu_multi_cap_b_receive_gate', 'neu_multi_cap_b_receive_gate_end', CAP_BLOCK[1])
            elif stage == 5:
                require(d[160] == 0 and d[161], 'missing Node B send-gate saved frame')
                saved[1] = gate_frame(d[161], 1, 'neu_multi_cap_b_send_gate', 'neu_multi_cap_b_send_gate_end', CAP_BLOCK[1])
            else:
                require(d[160] == 0 and d[161] == 0, 'terminal stage retained a saved frame')

            if stage == 0:
                require(not live_frames and not set(live_tables) - set(baseline['kernel_tables']), 'baseline already owns test resources')
            if stage == 1:
                require(set(live_tables) == baseline['kernel_tables'] and len(live_frames) == 2
                        and free == baseline['free'] - 2, 'object creation did not allocate exactly two frames')
            if stage == 2:
                require(set(live_tables) == set(baseline['kernel_tables']) | {
                    t for index in (0, 1) for t in [tables[i]['physical'] for i, value in enumerate(descriptors[index]['tables']) if value]},
                    'space table ownership differs from all roots')
                space_tables[0] = {tables[i]['physical'] for i, value in enumerate(descriptors[0]['tables']) if value}
                space_tables[1] = {tables[i]['physical'] for i, value in enumerate(descriptors[1]['tables']) if value}
                space_stacks[0] = {tables[i]['physical'] for i, value in enumerate(descriptors[0]['stack_tables']) if value}
                space_stacks[1] = {tables[i]['physical'] for i, value in enumerate(descriptors[1]['stack_tables']) if value}
                space_frames[0] = {frames[m['frame'][0]]['physical'] for m in live_mappings
                                   if m['space'] == [0, 1] and m['object'][0] == 0}
                space_frames[1] = {frames[m['frame'][0]]['physical'] for m in live_mappings
                                   if m['space'] == [1, 1] and m['object'][0] == 0}
            if 3 <= stage <= 6:
                require(descriptors[0] is not None and descriptors[1] is not None, 'missing live roots during transfer')
                current = {}
                for index in (0, 1):
                    record = descriptors[index]
                    owned = {tables[i]['physical'] for i, value in enumerate(record['tables']) if value}
                    stacks = {tables[i]['physical'] for i, value in enumerate(record['stack_tables']) if value}
                    require(owned <= space_tables[index],
                            f'space {index} acquired a foreign table: {sorted(owned - space_tables[index])}')
                    retired = space_tables[index] - owned
                    require(retired <= space_stacks[index],
                            f'space {index} retired a non-stack table: {sorted(retired - space_stacks[index])}')
                    require(stacks == space_stacks[index] - retired,
                            f'space {index} stack inventory inconsistent: live={sorted(stacks)} '
                            f'expected={sorted(space_stacks[index] - retired)}')
                    if spaces[index]['pins']:
                        require(not retired, f'space {index} lost stack tables while its member is pinned')
                    current[index] = owned
                require(set(live_tables) == set(baseline['kernel_tables']) | current[0] | current[1],
                        f'table allocation absent from all root inventories: '
                        f'extra={sorted(set(live_tables) - set(baseline["kernel_tables"]) - current[0] - current[1])} '
                        f'missing={sorted((current[0] | current[1]) - set(live_tables))}')
            if stage == 7:
                require(descriptors[0] is not None and descriptors[1] is None, 'wrong root destruction order')
                owned = {tables[i]['physical'] for i, value in enumerate(descriptors[0]['tables']) if value}
                require(owned <= space_tables[0], 'survivor acquired a foreign table')
                require(space_tables[0] - owned <= space_stacks[0], 'survivor retired a non-stack table')
                require(set(live_tables) == set(baseline['kernel_tables']) | owned,
                        'Node B tables were not reclaimed exactly once')
                for physical in space_tables[1]:
                    require(not allocated(physical), 'Node B table survived teardown')
                for physical in space_frames[1]:
                    require(not allocated(physical), 'Node B private frame survived teardown')
                require(all(not frames[slot]['live'] for slot in range(16)
                            if frames[slot]['physical'] in space_frames[1]), 'Node B private frame identity survived')
            if stage == 8:
                require(descriptors == [None, None], 'Node A root survived teardown')
                require(set(live_tables) == baseline['kernel_tables'], 'Node A tables were not reclaimed exactly once')
                for physical in space_tables[0]:
                    require(not allocated(physical), 'Node A table survived teardown')
                for physical in space_frames[0]:
                    require(not allocated(physical), 'Node A private frame survived teardown')
                require(free == baseline['free'] - 2, 'collected backing was not the only remaining allocation')
            if stage == 9:
                require(descriptors == [None, None] and not live_frames, 'terminal stage retained a root or data frame')
                require(set(live_tables) == baseline['kernel_tables'], 'terminal table inventory differs from baseline')
                require(total == baseline['total'] and free == baseline['free'] and bitmap == baseline['bitmap'],
                        'final allocator differs from the exact baseline bitmap')
                if original is not None:
                    for fid in original['frames']:
                        require(not allocated(frames[fid[0]]['physical']), 'collected object backing still allocated')

            snapshot = {'stage': stage, 'mode': expected_mode, 'cpu': cpu, 'descriptor': d,
                        'frames': frames, 'objects': memory_objects, 'spaces': spaces, 'mappings': mappings,
                        'tables': tables, 'live_tables': live_tables, 'descriptors': descriptors,
                        'capability_objects': cap_objects, 'capability_queues': queues, 'counters': counters,
                        'saved': saved, 'backing': original,
                        'allocator': {'total': total, 'free': free, 'allocated': total - free,
                                      'bitmap_sha256': hashlib.sha256(bitmap).hexdigest(),
                                      'bitmap_words': list(struct.unpack('<' + 'Q' * (len(bitmap) // 8), bitmap))}}
            previous[stage] = snapshot
            output.append(snapshot)
        finally:
            qmp.call('cont')
        if len(previous) == len(expected_stages):
            return {'stages': expected_stages, 'mode': expected_mode,
                    'name': CAP_CASES[expected_mode],
                    'baseline': {k: v for k, v in baseline.items() if k not in ('bitmap', 'live_tables', 'kernel_tables')},
                    'backing': original, 'checkpoints': output}
    raise failure('MemoryObject capability QMP: missing bounded checkpoint(s)')


# K8C runtime mapping profiles. The tuple order is the kernel `mode()` value:
# 0 golden, 1 writable, 2 fault, 3 rights, 4 peer-exit, 5 cancel, 6 read-denied.
RUNTIME_CASES = ('memory-object-runtime', 'memory-object-runtime-writable',
                 'memory-object-runtime-fault', 'memory-object-runtime-rights',
                 'memory-object-runtime-peer-exit', 'memory-object-runtime-cancel',
                 'memory-object-runtime-read-denied')
RUNTIME_MAGIC = 0x4e45554d4f000004
RUNTIME_WORDS = 256
RUNTIME_VALUE = 0x4e45554d4f52554e
RUNTIME_SECOND = 0x5a5a52554e54494d
RUNTIME_WRITE = 0x4231575249544501
RUNTIME_RESULT = 0x4c49564552454144
RUNTIME_VA_A = 0x600000
RUNTIME_VA_B = 0x700000
RUNTIME_VA_B_WRITE = 0x701000
RUNTIME_VA_B2_FAULT = 0x407000
RUNTIME_DATA = 0x402000
RUNTIME_CODE = 0x400000
RUNTIME_RODATA = 0x401000
RUNTIME_STACKS = (0x404000, 0x408000, 0x40c000)
RUNTIME_OBJECT = 0x100000001
RUNTIME_MAP_WITNESS_MAGIC = 0x4e45554d4f4d4150
RUNTIME_MAP_STATES = {'pending': 0, 'committed': 1, 'rejected': 2, 'cancelled': 3}


def observe_runtime(qmp, path, symbols, deadline, words, register, failure, output,
                    expected_mode, image):
    """Prove K8C runtime mapping from a received capability on a stopped CPU.

    Every object, capability object, handle, queue token, mapping record, page
    table leaf, payload byte, allocator bit and fault record below is read from
    production storage through the addresses the guest descriptor publishes.
    The serial success scalar is never consulted.
    """
    context = {'stage': None}

    def require(condition, message):
        if not condition:
            raise failure(f'MemoryObject runtime QMP: stage={context["stage"]}: ' + message)

    def read(address, length, physical=False):
        data = bytearray()
        for offset in range(0, (length + 7) // 8, 256):
            count = min(256, (length + 7) // 8 - offset)
            data.extend(struct.pack('<' + 'Q' * count, *words(qmp, address + offset * 8, count, physical)))
        return bytes(data[:length])

    def u64(data, offset=0):
        return struct.unpack_from('<Q', data, offset)[0]

    def u32(data, offset=0):
        return struct.unpack_from('<I', data, offset)[0]

    def identity(data, offset=0):
        return [u64(data, offset), u64(data, offset + 8)]

    def u64s(data, count, offset=0):
        return list(struct.unpack_from('<' + 'Q' * count, data, offset))

    raw = image.read_bytes()
    header = struct.unpack_from('<16sHHIQQQIHHHHHH', raw)

    def image_bytes(address, length):
        for index in range(header[10]):
            segment = struct.unpack_from('<IIQQQQQQ', raw, header[5] + index * header[9])
            if segment[0] == 1 and segment[3] <= address and address + length <= segment[3] + segment[5]:
                start = segment[2] + address - segment[3]
                return raw[start:start + length]
        raise failure('MemoryObject runtime QMP: image range is not file-backed')

    payload = image_bytes(symbols['neu_multi_payload'],
                          symbols['neu_multi_payload_end'] - symbols['neu_multi_payload'])
    require(0 < len(payload) <= 4096, 'invalid embedded payload bounds')
    code_page = payload + bytes(4096 - len(payload))

    def user_va(name):
        return 0x400000 + symbols[name] - symbols['neu_multi_payload']

    golden, writable, faulting, rights, peer_exit, cancel, read_denied = range(7)
    modes = (golden, writable, faulting, rights, peer_exit, cancel, read_denied)
    require(modes[expected_mode] == expected_mode, 'runtime case order must match kernel modes')

    # Per-mode policy table: rights held by the received handle, teardown order
    # (1 = Node A first, 2 = Node B first), mapping presence after the safe-point
    # commit, and the capability-release stage.
    policy = {
        golden:     {'rights': 13, 'order': 1, 'b': (6, 7), 'bw': (),     'refs_end': 6},
        writable:   {'rights': 15, 'order': 2, 'b': (6,),   'bw': (6,),   'refs_end': 7},
        faulting:   {'rights': 13, 'order': 1, 'b': (6, 7), 'bw': (),     'refs_end': 8},
        rights:     {'rights': 11, 'order': 2, 'b': (),     'bw': (),     'refs_end': 7},
        peer_exit:  {'rights': 13, 'order': 1, 'b': (),     'bw': (),     'refs_end': 8},
        cancel:     {'rights': 13, 'order': 1, 'b': (),     'bw': (),     'refs_end': 8},
        read_denied: {'rights': 14, 'order': 2, 'b': (),    'bw': (),     'refs_end': 7},
    }[expected_mode]
    policy = dict(policy)
    # Node A is destroyed first in the A-first order; Node B otherwise.
    policy['a_alive'] = 7 if policy['order'] == 1 else 8
    policy['b_alive'] = 8 if policy['order'] == 1 else 7

    expected_mappings = {
        golden:      {2: 2, 3: 2, 4: 2, 5: 2, 6: 3, 7: 1, 8: 0, 9: 0, 10: 0},
        writable:    {2: 2, 3: 2, 4: 2, 5: 2, 6: 4, 7: 2, 8: 0, 9: 0, 10: 0},
        faulting:    {2: 2, 3: 2, 4: 2, 5: 2, 6: 3, 7: 1, 8: 0, 9: 0, 10: 0},
        rights:      {2: 2, 3: 2, 4: 2, 5: 2, 6: 2, 7: 2, 8: 0, 9: 0, 10: 0},
        peer_exit:   {2: 2, 3: 2, 4: 2, 5: 2, 6: 2, 7: 0, 8: 0, 9: 0, 10: 0},
        cancel:      {2: 2, 3: 2, 4: 2, 5: 2, 6: 2, 7: 0, 8: 0, 9: 0, 10: 0},
        read_denied: {2: 2, 3: 2, 4: 2, 5: 2, 6: 2, 7: 2, 8: 0, 9: 0, 10: 0},
    }[expected_mode]
    expected_cap_refs = {stage: (1 if 2 <= stage < policy['refs_end'] else 0) for stage in range(11)}
    expected_requests = {
        golden: {6: (3, False, None, 3, RUNTIME_MAP_STATES['rejected'], (1 << 64) - 17)},
        writable: {6: (2, False, None, 2, RUNTIME_MAP_STATES['committed'], 0)},
        faulting: {6: (1, False, None, 1, RUNTIME_MAP_STATES['committed'], 0)},
        rights: {6: (0, False, None, None, None, None)},
        peer_exit: {6: (1, True, 1, None, None, None), 7: (1, True, 1, None, None, None),
                    8: (1, False, None, 1, RUNTIME_MAP_STATES['committed'], 0)},
        cancel: {6: (1, True, 1, None, None, None), 7: (1, True, 1, None, None, None),
                 8: (1, False, None, 1, RUNTIME_MAP_STATES['cancelled'], 0)},
        read_denied: {6: (0, False, None, None, None, None)},
    }[expected_mode]

    def request_model(table, stage):
        if stage < 6:
            return (0, False, None, None, None, None)
        keys = [key for key in table if key <= stage]
        return table[max(keys)] if keys else (0, False, None, None, None, None)

    # Checkpoints 0..5 carry no resolved ledger entry; 6+ are mode-specific and
    # unchanged through teardown except where the table above overrides.
    previous = {}
    expected_stages = list(range(11))
    baseline = None
    original = None
    space_tables = {}
    space_frames = {}
    space_stacks = {}
    last_b_root = None

    while time.monotonic() < deadline:
        cpu_hint = qmp.call('human-monitor-command', {'command-line': 'info registers'})
        rip = register(cpu_hint, 'RIP')
        if not symbols['neu_memory_object_runtime_wait'] <= rip < symbols['neu_memory_object_runtime_wait_end']:
            if len(previous) == len(expected_stages) and 'MemoryObject runtime test passed' in path.read_text(errors='replace'):
                break
            time.sleep(0.001)
            continue
        hint = words(qmp, symbols['neu_memory_object_runtime_witness'], 2)
        if hint[1] in previous:
            time.sleep(0.001)
            continue
        qmp.call('stop')
        try:
            cpu = qmp.call('human-monitor-command', {'command-line': 'info registers'})
            if not symbols['neu_memory_object_runtime_wait'] <= register(cpu, 'RIP') < symbols['neu_memory_object_runtime_wait_end']:
                continue
            d = words(qmp, symbols['neu_memory_object_runtime_witness'], RUNTIME_WORDS)
            magic, stage, mode, kernel = d[:4]
            context['stage'] = stage
            if stage in previous:
                continue
            require(magic == RUNTIME_MAGIC and mode == expected_mode, 'invalid runtime descriptor schema/profile')
            require(stage == len(previous) and stage in expected_stages, 'missing or reordered runtime checkpoint')
            require('CPL=0' in cpu and register(cpu, 'CR3') == kernel, 'runtime checkpoint not at the kernel root')
            require(register(cpu, 'RFL') & 0x600 == 0, 'runtime checkpoint with interrupts enabled')
            require(d[3] == kernel, 'descriptor kernel root differs from CR3')
            require(d[203] == 0, 'kernel reports enabled interrupts at the runtime safe point')
            require(d[162] == 0x240 and d[163] == 0x2c0 and d[164] == 0x340, 'block layout changed')
            require(d[165] == 0 and d[166] == 8 and d[167] == 0x10 and d[168] == 0x18
                    and d[169] == 0x20 and d[170] == 0x28 and d[171] == 0x30 and d[172] == 0x38
                    and d[173] == 0x40 and d[174] == 0x48 and d[175] == 0x50 and d[176] == 0x58
                    and d[177] == 0x60 and d[178] == 0x68, 'block field layout changed')
            require(d[181] == RUNTIME_VA_B_WRITE and d[182] == RUNTIME_RODATA
                    and d[183] == RUNTIME_RESULT and d[224] == policy['order'], 'runtime contract constants changed')
            require(d[233] == 0x40 and d[234] == 0x500 and d[235] == 0xc00 and d[236] == 0x30,
                    'script layout changed')
            require(d[237] == RUNTIME_VA_B2_FAULT and d[240] == 0x3c0, 'probe layout changed')
            p_offsets = d[206:224]
            require(p_offsets == [0x3c0, 0, 8, 0x10, 0x18, 0x20, 0x28, 0x30, 0x38, 0x40, 0x48,
                                  0x50, 0x58, 0x60, 0x68, 0x70, 0x78, 0x80],
                    'probe field layout changed')

            layout = d[16:55]
            require(layout[0] == 1 and layout[3] == 16 and layout[11] == 64 and layout[20] == 4
                    and layout[28] == 2, 'unexpected bounded registry layout')
            require(layout[34:38] == [16, 16, 24, 24], 'identity/provenance layout mismatch')
            frames = []
            for slot in range(16):
                row = read(layout[1] + slot * layout[2], layout[2])
                owner = layout[8]
                frames.append({'slot': slot, 'physical': u64(row, layout[4]),
                               'generation': u64(row, layout[5]), 'live': row[layout[6]],
                               'initialized': row[layout[7]],
                               'owner': [u64(row, owner), *identity(row, owner + 8)]})
            mappings = []
            for slot in range(64):
                row = read(layout[9] + slot * layout[10], layout[10])
                provenance = layout[16]
                mappings.append({'slot': slot, 'space': identity(row, layout[12]),
                                 'address': u64(row, layout[13]), 'frame': identity(row, layout[14]),
                                 'write': row[layout[15]], 'execute': row[layout[15] + 1],
                                 'object': [u64(row, provenance), *identity(row, provenance + 8)],
                                 'live': row[layout[17]]})
            objects = []
            for slot in range(4):
                row = read(layout[18] + slot * layout[19], layout[19])
                objects.append({'slot': slot, 'generation': u64(row, layout[21]),
                                'live': row[layout[22]], 'creator': row[layout[23]],
                                'frames': [identity(row, layout[24] + i * 16) for i in range(2)],
                                'pages': u64(row, layout[25]), 'cap_refs': row[layout[38]]})
            spaces = []
            for slot in range(2):
                row = read(layout[26] + slot * layout[27], layout[27])
                spaces.append({'slot': slot, 'generation': u64(row, layout[29]),
                               'live': row[layout[30]], 'root': u64(row, layout[31]),
                               'pins': row[layout[32]], 'refs': row[layout[33]]})

            cap_layout = d[95:113]
            require(cap_layout[0] == 1 and cap_layout[3] == 6 and cap_layout[13] == 8
                    and cap_layout[14] == 8 and cap_layout[15] == 1 and cap_layout[16] == 1,
                    'invalid capability registry layout')
            cap_objects = []
            for slot in range(cap_layout[3]):
                row = read(cap_layout[4] + slot * cap_layout[5], cap_layout[5])
                cap_objects.append({'slot': slot, 'generation': u32(row, cap_layout[6]),
                                    'references': u32(row, cap_layout[7]),
                                    'pins': u32(row, cap_layout[8]),
                                    'live': row[cap_layout[9]] != 0, 'kind': row[cap_layout[10]],
                                    'target': u64(row, cap_layout[11])})

            allocator = d[145:153]
            total = u64(read(allocator[0], 8))
            free = u64(read(allocator[1], 8))
            bitmap = read(allocator[2], ((total + 63) // 64) * 8)
            require(allocator[3] * 8 >= len(bitmap), 'allocator bitmap capacity is too small')
            require(free <= total and sum(byte.bit_count() for byte in bitmap) == total - free,
                    'allocator bitmap/counters disagree')
            regions = read(allocator[4], allocator[5] * 24)
            kernel_start, kernel_end = u64(read(allocator[6], 8)), u64(read(allocator[7], 8))
            spans = []
            for offset in range(0, len(regions), 24):
                start, length, kind = struct.unpack_from('<QQQ', regions, offset)
                if kind != 0:
                    continue
                end = start + length
                start = max(start, 4096)
                spans += [(a, b) for a, b in ((start, min(end, kernel_start)),
                                               (max(start, kernel_end), end)) if a < b]
            require(sum((end - start) // 4096 for start, end in spans) == total,
                    'allocator total differs from raw memory-map spans')

            def allocated(physical):
                ordinal = 0
                for start, end in spans:
                    if start <= physical < end:
                        ordinal += (physical - start) // 4096
                        return bool(bitmap[ordinal // 8] & (1 << (ordinal % 8)))
                    ordinal += (end - start) // 4096
                raise failure('MemoryObject runtime QMP: physical owner outside allocatable spans')

            tables_layout = d[153:159]
            tables = []
            for slot in range(tables_layout[2]):
                row = read(tables_layout[0] + slot * tables_layout[1], tables_layout[1])
                tables.append({'slot': slot, 'physical': u64(row, tables_layout[3]),
                               'level': row[tables_layout[4]], 'pinned': row[tables_layout[5]]})
            live_tables = {t['physical']: t for t in tables if t['physical']}
            require(len(live_tables) == sum(bool(t['physical']) for t in tables), 'duplicate table ownership')
            require(all(1 <= t['level'] <= 4 for t in live_tables.values()), 'invalid table level')

            def parse_table(layout_words, label, node_raw):
                if not any(layout_words):
                    return None
                require(layout_words[0] == 1 and layout_words[5] == 8 and layout_words[16] == 8
                        and layout_words[19] == 1 and layout_words[17] == 8, 'invalid table layout')
                require(layout_words[18] == node_raw, 'table identity mismatch')
                slots = []
                for index in range(8):
                    row = read(layout_words[3] + index * layout_words[4], layout_words[4])
                    slots.append({'slot': index, 'generation': u32(row, layout_words[6]),
                                  'live': row[layout_words[7]] != 0, 'kind': row[layout_words[8]],
                                  'target': u64(row, layout_words[9]), 'rights': row[layout_words[10]],
                                  'transferable': row[layout_words[11]] != 0})
                bootstrap = []
                for index in range(8):
                    row = read(layout_words[14] + index * 16, 16)
                    tag = u64(row, 0)
                    require(tag in (0, 1), 'invalid bootstrap Option tag')
                    bootstrap.append(u64(row, 8) if tag else None)
                require(all((handle is not None) == (index < layout_words[17])
                            for index, handle in enumerate(bootstrap)), 'bootstrap limit/handle mismatch')
                return {'label': label, 'base': layout_words[1], 'slots': slots,
                        'bootstrap': bootstrap, 'sealed': layout_words[13] != 0,
                        'live': layout_words[12] != 0, 'id': layout_words[18]}

            table_a = parse_table(d[55:75], 0, d[9]) if any(d[55:75]) else None
            table_b = parse_table(d[75:95], 1, d[10]) if any(d[75:95]) else None
            require((table_a is not None) == (2 <= stage < policy['a_alive']),
                    'Node A capability table presence mismatch')
            require((table_b is not None) == (2 <= stage < policy['b_alive']),
                    'Node B capability table presence mismatch')

            channel = d[113:145]
            require(channel[0] == 1 and channel[28] == 2 and channel[29] == 2, 'invalid channel layout')
            channel_base = channel[1]
            channel_live = read(channel_base + channel[4], 1)[0] != 0
            counters = {name: u64s(read(channel_base + offset, 16), 2)
                        for name, offset in (('sent', channel[9]), ('received', channel[10]),
                                             ('discarded', channel[11]))}
            queues = []
            for side in range(2):
                fifo = channel_base + channel[8] + side * channel[12]
                head = u64(read(fifo + channel[13], 8))
                tail = u64(read(fifo + channel[14], 8))
                length = u64(read(fifo + channel[15], 8))
                values = u64s(read(fifo + channel[16], 16), 2)
                transfers = []
                for slot in range(2):
                    transfer = fifo + channel[17] + slot * channel[18]
                    transfers.append({'live': read(transfer + channel[19], 1)[0] != 0,
                                      'object': u64(read(transfer + channel[20], 8)),
                                      'rights': read(transfer + channel[21], 1)[0]})
                require(head < 2 and tail < 2 and length <= 2 and (head + length) % 2 == tail,
                        'invalid FIFO indices')
                require(all(value == 0 for value in values), 'unexpected scalar message in capability queue')
                for slot, transfer in enumerate(transfers):
                    occupied = (slot + 2 - head) % 2 < length
                    if transfer['live']:
                        require(occupied and transfer['object'] != 0,
                                'live transfer outside FIFO occupancy')
                    else:
                        require(transfer['object'] == 0 and transfer['rights'] == 0,
                                'stale transfer identity')
                queues.append({'head': head, 'tail': tail, 'len': length, 'transfers': transfers})

            live_caps = [o for o in cap_objects if o['live']]
            handle_counts = {}
            for table in (table_a, table_b):
                if table is None:
                    continue
                for slot in table['slots']:
                    if slot['live']:
                        handle_counts[slot['target']] = handle_counts.get(slot['target'], 0) + 1
            queue_counts = {}
            for queue in queues:
                for transfer in queue['transfers']:
                    if transfer['live']:
                        queue_counts[transfer['object']] = queue_counts.get(transfer['object'], 0) + 1
            raw_caps = {(o['kind'] << 40) | (o['generation'] << 8) | (o['slot'] + 1): o for o in live_caps}
            require(set(handle_counts) <= set(raw_caps) and set(queue_counts) <= set(raw_caps),
                    'handle/queue names a dead capability object')
            for o in live_caps:
                expected = handle_counts.get((o['kind'] << 40) | (o['generation'] << 8) | (o['slot'] + 1), 0) \
                    + queue_counts.get((o['kind'] << 40) | (o['generation'] << 8) | (o['slot'] + 1), 0)
                require(o['references'] == expected, 'capability reference count differs from handles plus queue')
            require(sum(o['pins'] for o in live_caps) == 0, 'unexpected capability pin at checkpoint')
            require(d[184] == len(live_caps) and d[185] == sum(o['references'] for o in live_caps)
                    and d[186] == 0 and d[187] == sum(queue_counts.values()) and d[188] == 0,
                    'published capability accounting differs from raw registry')
            mem_caps = [o for o in live_caps if o['kind'] == 5]
            require(all(o['target'] == d[244] for o in mem_caps),
                    'MemoryObject capability names a foreign generation')

            object_live = [o for o in objects if o['live']]
            object_mappings = [m for m in mappings if m['live'] and m['object'][0] == 1]
            live_mappings = [m for m in mappings if m['live']]
            live_frames = [f for f in frames if f['live']]
            if stage == 0:
                require(not object_live and not live_mappings and not live_frames, 'baseline already owns test resources')
                require(d[189] == 0 and d[190] == 0 and d[191] == 0 and d[192] == 0,
                        'baseline published ownership')
            elif stage == 1:
                require(len(object_live) == 1 and not live_mappings, 'object creation model mismatch')
                obj = object_live[0]
                require(obj['creator'] == 1 and obj['cap_refs'] == 0 and obj['pages'] == 2,
                        'new object ownership/size mismatch')
                require(len(live_frames) == 2, 'object creation did not allocate exactly two frames')
                original = {'slot': obj['slot'], 'generation': obj['generation'],
                            'id': [obj['slot'], obj['generation']], 'frames': obj['frames'],
                            'physical': [frames[fid[0]]['physical'] for fid in obj['frames']]}
                require(d[189] == 1 and d[190] == 0 and d[191] == 0 and d[192] == 2,
                        'object creation projection mismatch')
                require(d[242] == original['physical'][0] and d[243] == original['physical'][1],
                        'published object physicals differ from frames')
            else:
                require(original is not None, 'missing object creation baseline')
                obj = object_live[0] if object_live else None
                if stage <= 8:
                    require(obj is not None, 'object disappeared before collection')
                    require([obj['slot'], obj['generation']] == original['id']
                            and obj['frames'] == original['frames'], 'object identity/backing changed')
                    require(obj['creator'] == 0 and obj['pages'] == 2, 'object ownership/size changed')
                    require(obj['cap_refs'] == expected_cap_refs[stage],
                            f'capability reference differs from the model: {obj["cap_refs"]} != {expected_cap_refs[stage]}')
                    require(len(object_mappings) == expected_mappings[stage],
                            f'object mapping count differs from the model at stage {stage}: '
                            f'{len(object_mappings)} != {expected_mappings[stage]}')
                    require(d[244] == (obj['generation'] << 8 | (obj['slot'] + 1)),
                            'witness object id is not the production object')
                    require(d[242] == original['physical'][0] and d[243] == original['physical'][1],
                            'object physical pages changed')
                else:
                    require(obj is None and not object_mappings, 'collected object still live or mapped')
                    require(d[189] == 0 and d[190] == 0 and d[191] == 0 and d[192] == 0,
                            'collected object projection survived')
                    slot = original['slot']
                    require(not objects[slot]['live'] and objects[slot]['cap_refs'] == 0,
                            'collected object slot/capability reference survived')
                require(d[189] == 0 and d[190] == len(object_mappings) and
                        d[191] == (objects[original['slot']]['cap_refs'] if stage <= 8 else 0) and
                        d[192] == (2 if stage <= 8 else 0),
                        'object counts projection differs from raw registry')

            # Backing bytes are observed through the production data frames.
            if 1 <= stage <= 8:
                physical = original['physical']
                page0 = read(physical[0], 4096, True)
                page1 = read(physical[1], 4096, True)
                if stage <= 2:
                    require(page0 == bytes(4096) and page1 == bytes(4096),
                            'object backing not zeroed before Ring 3 execution')
                else:
                    require(u64(page0, 0) == RUNTIME_VALUE and u64(page0, 8) == RUNTIME_SECOND,
                            'Ring 3 payload absent from object backing')
                    require(u64(page1, 0) == (RUNTIME_WRITE if (expected_mode == writable and stage >= 6)
                                              else 1), 'object page 1 value differs from the model')
                    require(page0[16:] == bytes(4096 - 16), 'unexpected object page 0 mutation')
                    require(page1[8:] == bytes(4096 - 8), 'unexpected object page 1 mutation')

            # Saved scheduler contexts are Ring 3 frames or absent.
            def saved_frame(address):
                row = read(address, 160)
                registers = list(struct.unpack_from('<15Q', row, 0))
                rip, cs, flags, rsp, ss = struct.unpack_from('<5Q', row, 120)
                require(cs == 0x3b and ss == 0x33 and flags & 0x202 == 0x202,
                        'saved frame is not a Ring 3 frame')
                require(RUNTIME_CODE <= rip < RUNTIME_DATA, 'saved RIP outside the user image')
                require(any(base <= rsp < base + 4096 for base in RUNTIME_STACKS),
                        'saved RSP outside a private stack page')
                return {'rip': rip, 'rsp': rsp, 'flags': flags, 'rbx': registers[13]}

            saved = [saved_frame(d[159 + index]) if d[159 + index] else None for index in range(3)]
            if stage == 2:
                require(saved[0] and saved[1], 'missing installed Thread contexts')
                require(saved[2] is None,
                        'second Node B thread was installed before its deferred start')
            if stage == 7:
                require(bool(saved[2]) == (expected_mode == cancel),
                        'second Node B thread presence differs from the cancellation model')
            if stage >= 8:
                require(saved == [None, None, None], 'terminal stage retained a saved context')

            cache = {}

            def page(physical):
                if physical not in cache:
                    cache[physical] = words(qmp, physical, 512, True)
                return cache[physical]

            def walk(root, address):
                table, entries = root, []
                for depth, shift in enumerate((39, 30, 21, 12)):
                    require(table in live_tables and live_tables[table]['level'] == 4 - depth,
                            'walk entered a foreign/unowned table')
                    entry = page(table)[(address >> shift) & 511]
                    entries.append(entry)
                    if not entry & 1:
                        return {'present': False, 'entries': entries}
                    require(not depth < 3 or not entry & 128, 'unexpected large page')
                    table = entry & 0x000ffffffffff000
                return {'present': True, 'entries': entries, 'physical': table,
                        'write': all(e & 2 for e in entries), 'user': all(e & 4 for e in entries),
                        'execute': not any(e & (1 << 63) for e in entries)}

            def reachable(root):
                found = set()
                users = {}

                def visit(table, level, prefix=0, writable=True, user=True, executable=True):
                    require(table in live_tables and live_tables[table]['level'] == level,
                            f'tree has stale table reference: table={table:#x} level={level}')
                    require(table not in found, 'duplicate or cyclic table parent')
                    found.add(table)
                    for index, entry in enumerate(page(table)):
                        if not entry & 1:
                            continue
                        address = prefix | (index << (12 + (level - 1) * 9))
                        w, u, x = writable and bool(entry & 2), user and bool(entry & 4), \
                            executable and not bool(entry & (1 << 63))
                        if level > 1:
                            require(not entry & 128, 'unexpected large table leaf')
                            visit(entry & 0x000ffffffffff000, level - 1, address, w, u, x)
                        elif u:
                            require(0x400000 <= address < 0x800000,
                                    'user mapping outside the bounded user range')
                            users[address] = {'physical': entry & 0x000ffffffffff000,
                                              'write': w, 'execute': x}

                visit(root, 4)
                return found, users

            context['tree'] = 'kernel'
            common_tables, kernel_users = reachable(kernel)
            if stage == 0:
                baseline = {'total': total, 'free': free, 'bitmap': bitmap,
                            'live_tables': set(live_tables), 'kernel_tables': set(common_tables)}
            require(set(common_tables) == baseline['kernel_tables'], 'kernel root tables changed')
            for slot in range(64):
                alias = walk(kernel, (0xffff900000000000 + slot * 4096))
                require(alias['present'] == bool(tables[slot]['physical']), 'stale common workspace alias')
                if alias['present']:
                    require(alias['physical'] == tables[slot]['physical'] and alias['write']
                            and not alias['execute'] and not alias['user'],
                            'common workspace alias ownership/permissions')
            require(not kernel_users, 'kernel root exposes a user leaf')

            spaces_alive = {0: 2 <= stage < policy['a_alive'], 1: 2 <= stage < policy['b_alive']}
            if stage <= 1:
                require(not spaces_alive[0] and not spaces_alive[1], 'space appeared before construction')
            elif stage <= 6:
                require(spaces_alive[0] and spaces_alive[1], 'space lifetime differs from the model')
            elif stage == 7:
                require(spaces_alive == ({0: False, 1: True} if policy['order'] == 1 else
                                         {0: True, 1: False}), 'first teardown order differs from the model')
            else:
                require(not spaces_alive[0] and not spaces_alive[1],
                        'space survived the second teardown')
            if stage >= 2:
                require(d[9] == 0x1_00000001 and d[10] == 0x1_00000002,
                        'wrong Node identity')
                require(d[11] == 0x101 and d[12] == 0x102, 'wrong Space identity')
                require(d[13] == 0x1_00000001 and d[14] == 0x1_00000002 and d[15] == 0x1_00000003,
                        'wrong Thread identity')

            descriptors = []
            for index in range(2):
                fields = d[193 + index * 5:198 + index * 5]
                if not any(fields):
                    require(not spaces_alive[index], 'live space token missing')
                    descriptors.append(None)
                    continue
                require(spaces_alive[index], 'token for a retired space')
                fields = d[193 + index * 5:198 + index * 5]
                root = u64(read(fields[0], 8))
                sid = identity(read(fields[1], 16))
                state = read(fields[3], 1)[0]
                inventory = list(read(fields[2], 64))
                stacks = list(read(fields[4], 64))
                require(sid == [index, 1] and state == 0 and spaces[index]['generation'] == 1,
                        'wrong Node-local SpaceId generation')
                require(spaces[index]['root'] == root, 'descriptor root differs from production root')
                descriptors.append({'root': root, 'id': sid, 'tables': inventory,
                                    'stack_tables': stacks})
            if descriptors[1] is not None:
                last_b_root = descriptors[1]['root']

            for index in range(2):
                if not spaces_alive[index]:
                    continue
                record = descriptors[index]
                owned = {tables[i]['physical'] for i, value in enumerate(record['tables']) if value}
                require(0 not in owned and len(owned) == sum(record['tables']),
                        'inventory contains retired tables')
                require(not owned & set(baseline['kernel_tables']), 'foreign root table ownership')
                context['tree'] = f'space{index}'
                found, users = reachable(record['root'])
                require(found == owned, 'reachable tables differ from token inventory')
                ledger = [m for m in live_mappings
                          if m['space'] == [index, 1] and 0x400000 <= m['address'] < 0x800000]
                require({m['address'] for m in ledger} == set(users),
                        'mapping ledger addresses differ from raw user leaves: '
                        f'ledger-only={sorted({m["address"] for m in ledger} - set(users))} '
                        f'leaf-only={sorted(set(users) - {m["address"] for m in ledger})}')
                for m in ledger:
                    f = frames[m['frame'][0]]
                    require(f['live'] and f['generation'] == m['frame'][1], 'mapping names a stale frame')
                    require(users[m['address']] == {'physical': f['physical'], 'write': bool(m['write']),
                                                    'execute': bool(m['execute'])},
                            'raw user leaf differs from mapping/frame record')
                    if m['object'][0] == 1:
                        require(obj is not None and m['object'] == [1, *original['id']],
                                'wrong shared mapping provenance')
                        page_index = (m['address'] - RUNTIME_VA_A) // 4096 if index == 0 else \
                            (m['address'] - RUNTIME_VA_B) // 4096
                        require(0 <= page_index < 2 and
                                m['frame'] == obj['frames'][page_index],
                                'shared leaf maps the wrong object page')
                        require(f['physical'] == original['physical'][page_index],
                                'shared leaf physical differs from object backing')
                    else:
                        require(f['owner'] == [0, index, 1], 'private frame ownership changed')
                stacks_present = {address for address in users if address in RUNTIME_STACKS}
                require(len(stacks_present) == spaces[index]['pins'],
                        'space pins differ from the number of live stack leaves')
                require(spaces[index]['refs'] == 1, 'AddressSpace paging reference differs from the model')
                require(walk(record['root'], RUNTIME_CODE)['present'] and
                        walk(record['root'], RUNTIME_CODE)['execute'] and
                        not walk(record['root'], RUNTIME_CODE)['write'],
                        'user code leaf permissions changed')
                code = walk(record['root'], RUNTIME_CODE)
                require(read(code['physical'], 4096, True) == code_page,
                        'user code page differs from the ELF payload')
                require(walk(record['root'], RUNTIME_RODATA)['present'],
                        'private rodata leaf disappeared')
                data = walk(record['root'], RUNTIME_DATA)
                require(data['present'] and data['write'] and not data['execute'] and data['user'],
                        'private data leaf permissions changed')

            # Explicit shared-mapping expectations for the post-commit stages.
            def shared(index, address, write, page_index):
                record = descriptors[index]
                require(record is not None, 'shared mapping expected in a retired space')
                translation = walk(record['root'], address)
                require(translation['present'], f'expected shared leaf {address:#x} is absent')
                require(translation['physical'] == original['physical'][page_index]
                        and translation['user'] and not translation['execute']
                        and translation['write'] == write,
                        f'shared leaf {address:#x} has wrong target/permissions')

            if spaces_alive[0]:
                shared(0, RUNTIME_VA_A, True, 0)
                shared(0, RUNTIME_VA_A + 4096, True, 1)

            def shared_map(index, address):
                record = descriptors[index]
                return record is not None and walk(record['root'], address)['present']

            if stage >= 2:
                for index in (0, 1):
                    if not spaces_alive[index]:
                        continue
                    present_b = stage in policy['b'] and index == 1
                    present_bw = stage in policy['bw'] and index == 1
                    require(shared_map(index, RUNTIME_VA_B) == present_b,
                            f'VA_B presence differs from the commit model at stage {stage}')
                    require(shared_map(index, RUNTIME_VA_B_WRITE) == present_bw,
                            f'VA_B_WRITE presence differs from the commit model at stage {stage}')
                    if present_b:
                        shared(index, RUNTIME_VA_B, False, 0)
                    if present_bw:
                        shared(index, RUNTIME_VA_B_WRITE, True, 1)

            # Ledger and map-witness projections.
            diagnostics = d[245:254]
            requests, cancelled, pending_present, pending_seq, pending_state = diagnostics[:5]
            last_present, last_seq, last_state, last_status = diagnostics[5:]
            request_count, want_pending, want_pending_seq, want_last_seq, want_last_state, want_last_status = \
                request_model(expected_requests, stage)
            if stage < 6:
                require(requests == 0 and not pending_present and not last_present,
                        'ledger entry before the mapping request')
            else:
                require(requests == request_count, f'request counter differs from the model: {requests} != {request_count}')
                require(bool(pending_present) == bool(want_pending), 'pending request presence mismatch')
                if want_pending:
                    require(pending_seq == want_pending_seq and pending_state == 0, 'pending request identity mismatch')
                if want_last_seq is not None:
                    require(last_present and last_seq == want_last_seq and last_state == want_last_state
                            and last_status == want_last_status, 'resolved request verdict mismatch')
                else:
                    require(not last_present or stage >= 8, 'unexpected resolved request')
            cancelled_from = 8 if expected_mode == cancel else 7
            if stage >= cancelled_from:
                require(cancelled == (1 if expected_mode == cancel else 0),
                        'cancellation counter differs from the model')

            witness = words(qmp, d[179], 24)
            if stage >= 6 and request_count:
                require(witness[0] == RUNTIME_MAP_WITNESS_MAGIC, 'map witness magic missing')
                require(witness[21] == requests, 'map witness request counter mismatch')
                require(witness[22] == cancelled, 'map witness cancellation counter mismatch')
                entry = witness[1:21]
                if want_pending:
                    require(entry[0] == pending_seq and entry[15] == 0, 'witness does not name the pending request')
                elif want_last_seq is not None:
                    require(entry[0] == last_seq and entry[15] == want_last_state
                            and entry[16] == (want_last_status if want_last_status is not None else 0),
                            'witness does not name the resolved request')
            elif stage < 6:
                require(witness[0] in (0, RUNTIME_MAP_WITNESS_MAGIC), 'map witness written before any request')

            # Ring 3 probe/verdict block, read through Node B's own root.
            p_base = d[240]
            sentinel = d[241]
            p_values = None
            if spaces_alive[1]:
                data_leaf = walk(descriptors[1]['root'], RUNTIME_DATA)
                b_data = read(data_leaf['physical'], 4096, True)
                p_values = [u64(b_data, p_base + offset) for offset in range(0, 0x88, 8)]
                if stage == 4:
                    require(all(value == sentinel for value in p_values[:17]),
                            'probe block changed before the receive stage')
                if stage == 5:
                    require(p_values[0] == (1 << 64) - 17 and p_values[1] == (1 << 64) - 16
                            and p_values[2] == (1 << 64) - 19 and p_values[3] == (1 << 64) - 2
                            and p_values[4] == (1 << 64) - 2 and p_values[5] == (1 << 64) - 2
                            and p_values[6] == (1 << 64) - 17 and p_values[7] == (1 << 64) - 2,
                            'negative probe verdicts differ from the fixed rejection model')
                    require(all(value == sentinel for value in p_values[8:17]),
                            'commit verdicts appeared before the safe point')
                if stage == 6:
                    denied = (1 << 64) - 18
                    expected = {
                        golden: [-(1 << 64) + 0, 0, 0, 0, 0, 0],
                        writable: [0, 0, 0, 0, 0, 0],
                        faulting: [0, 0, 0, 0, 0, 0],
                        rights: [0, denied, 0, 0, 0, denied],
                        peer_exit: [0, 0, 0, 0, 0, 0],
                        cancel: [0, 0, 0, 0, 0, 0],
                        read_denied: [0, denied, 0, 0, 0, denied],
                    }[expected_mode]
                    if expected_mode == golden:
                        expected[0] = (1 << 64) - 24
                        expected[1] = denied
                        expected[2] = expected[3] = expected[4] = (1 << 64) - 17
                    if expected_mode in (writable, faulting):
                        expected[4] = (1 << 64) - 21
                    require(p_values[8:14] == expected, 'commit verdict words differ from the model')
                    if expected_mode == rights or expected_mode == read_denied:
                        require(p_values[14] == denied and p_values[15] == denied,
                                'denied read/write verdicts differ from the model')
                    else:
                        require(p_values[14] == sentinel and p_values[15] == sentinel,
                                'unexpected denied read/write verdicts')
                    if expected_mode in (peer_exit, cancel):
                        require(p_values[16] == sentinel,
                                'unexpected completion mark before the deferred wake')
                    else:
                        require(p_values[16] == 1, 'probe block did not mark completion')

            # Hardware fault evidence for the genuine Ring 3 fault profiles.
            fault_from = 8 if expected_mode == cancel else 6
            if stage >= fault_from and expected_mode in (faulting, cancel):
                record = words(qmp, d[180], 16)
                node, thread, space, cr3, vector, error, rip, rsp, cr2, cs, ss, flags, _, done, _, _ = record
                if expected_mode == faulting:
                    require(node == d[10] and thread == d[14] and space == d[12],
                            'fault record names a foreign Node/Thread/Space')
                    require(cr3 == last_b_root, 'fault CR3 is not the Node B root')
                    require(vector == 14 and error == 7 and cr2 == RUNTIME_VA_B
                            and rip == d[238], 'hardware fault evidence differs from the read-only write model')
                    require(0x408000 <= rsp < 0x409000, 'fault RSP outside the Node B private stack')
                else:
                    require(node == d[10] and thread == d[15] and space == d[12],
                            'fault record names a foreign Node/Thread/Space')
                    require(cr3 == last_b_root, 'fault CR3 is not the Node B root')
                    require(vector == 14 and error == 6 and cr2 == RUNTIME_VA_B2_FAULT
                            and rip == d[239], 'unmapped-gap fault evidence differs from the model')
                    require(0x40c000 <= rsp < 0x40d000, 'fault RSP outside the second Node B stack')
                require(cs == 0x3b and ss == 0x33 and flags & 0x202 == 0x202 and done == 1,
                        'fault record is not a complete CPL3 hardware frame')

            # Capability table authority model.
            def table_expectations(table, side):
                if table is None:
                    return
                expected = []
                if side == 0:
                    expected = [('as', 16, False), ('as', 16, False), ('as', 0, False),
                                ('mem', policy['rights'], True), ('ep', 224, False),
                                ('ep', 96, False), ('ep', 32, False), ('ep', 64, False)]
                    if stage >= 4:
                        expected[3] = None
                else:
                    expected = [('as', 16, False), ('as', 16, False), ('as', 0, False),
                                ('flag', 1, True), ('ep', 224, False), ('ep', 96, False),
                                None, ('ep', 64, False)]
                    if stage >= 5:
                        expected[6] = ('mem', policy['rights'], True)
                    if stage >= 6 and expected_mode == golden:
                        expected[6] = None
                for index, want in enumerate(expected):
                    slot = table['slots'][index]
                    if want is None:
                        require(not slot['live'] and slot['kind'] == 0 and slot['target'] == 0
                                and slot['rights'] == 0 and not slot['transferable'],
                                f'Node {side} free slot {index} is not empty')
                        continue
                    role, slot_rights, transferable = want
                    require(slot['live'] and slot['rights'] == slot_rights
                            and slot['transferable'] == transferable,
                            f'Node {side} slot {index} authority/rights differ from the model')
                    if role == 'mem':
                        require(slot['kind'] == 5 and slot['target'] in raw_caps
                                and raw_caps[slot['target']]['target'] == d[244],
                                'received MemoryObject capability identity differs from the object')
                if side == 0 and stage == 2:
                    require(table['bootstrap'] == [((1 << 32) | (i + 1)) for i in range(8)],
                            'Node A bootstrap handles are not the initial grants')
                if side == 1 and stage == 2:
                    require(table['bootstrap'] == [((1 << 32) | (i + 1)) for i in range(8)],
                            'Node B bootstrap handles are not the initial grants')

            table_expectations(table_a, 0)
            table_expectations(table_b, 1)

            # Channel state matches the exactly-once transfer.
            expected_queue = 1 if stage == 4 else 0
            require(sum(queue['len'] for queue in queues) == expected_queue,
                    f'channel queue occupancy differs from the transfer model at stage {stage}')
            if expected_queue:
                queue = next(queue for queue in queues if queue['len'])
                transfer = queue['transfers'][queue['head']]
                require(transfer['live'] and transfer['object'] in raw_caps
                        and transfer['rights'] == policy['rights'],
                        'queued capability identity/rights were amplified or changed')
            require(channel_live == (spaces_alive[0] or spaces_alive[1]),
                    'channel liveness differs from endpoint ownership')
            expected_counters = {0: 0, 1: 0, 2: 0, 3: 0, 4: 1, 5: 1, 6: 1, 7: 1, 8: 1, 9: 1, 10: 1}
            require(counters['sent'] == [expected_counters[stage], 0], 'send counter differs from the model')
            require(counters['received'] == [0, 1 if stage >= 5 else 0], 'receive counter differs from the model')
            require(counters['discarded'] == [0, 0], 'capability transfer discarded a token')

            # Table/frame allocation accounting.
            owned_all = set()
            for index in range(2):
                if spaces_alive[index]:
                    owned_all |= {tables[i]['physical'] for i, value in enumerate(descriptors[index]['tables']) if value}
            require(set(live_tables) == baseline['kernel_tables'] | owned_all,
                    'table allocation absent from or foreign to all root inventories')
            data_physicals = {f['physical'] for f in live_frames}
            require(not set(live_tables) & data_physicals, 'table/data ownership overlap')
            require(total - free == len(live_tables) + len(live_frames), 'orphan physical allocation')
            for physical in list(live_tables) + [f['physical'] for f in live_frames]:
                require(allocated(physical), 'live owner has a freed allocator bit')
            if stage == 1:
                require(free == baseline['free'] - 2, 'object creation did not allocate exactly two frames')
            if stage == 8:
                require(free == baseline['free'] - 2, 'only the object backing should remain allocated')
            if stage >= 9:
                require(total == baseline['total'] and free == baseline['free']
                        and bitmap == baseline['bitmap'], 'final allocator differs from the exact baseline bitmap')

            # Space inventory chronology.
            if stage == 2:
                for index in range(2):
                    space_tables[index] = {tables[i]['physical'] for i, value
                                           in enumerate(descriptors[index]['tables']) if value}
                    space_stacks[index] = {tables[i]['physical'] for i, value
                                           in enumerate(descriptors[index]['stack_tables']) if value}
                    space_frames[index] = {frames[m['frame'][0]]['physical'] for m in live_mappings
                                           if m['space'] == [index, 1] and m['object'][0] == 0}
            if 3 <= stage <= 7:
                current = {}
                for index in range(2):
                    if not spaces_alive[index]:
                        continue
                    record = descriptors[index]
                    owned = {tables[i]['physical'] for i, value in enumerate(record['tables']) if value}
                    stacks = {tables[i]['physical'] for i, value in enumerate(record['stack_tables']) if value}
                    extra = owned - space_tables[index]
                    if index == 0:
                        require(not extra, 'Node A acquired a table after construction')
                    elif extra:
                        require(stage >= 6, f'Node B acquired a table before the runtime mapping: {sorted(extra)}')
                        require(not extra & (set(baseline['kernel_tables']) | space_tables[0]
                                             | space_tables[1]),
                                f'Node B runtime mapping reused a foreign table: {sorted(extra)}')
                        new_stacks = stacks - space_stacks[index]
                        require(new_stacks <= extra,
                                f'Node B acquired an untracked stack table: {sorted(new_stacks - extra)}')
                        space_tables[1] |= extra
                        space_stacks[1] |= new_stacks
                    retired = space_tables[index] - owned
                    require(retired <= space_stacks[index], f'space {index} retired a non-stack table')
                    require(stacks == space_stacks[index] - retired,
                            f'space {index} stack inventory inconsistent')
                    current[index] = owned
                require(set(live_tables) == baseline['kernel_tables'] | set().union(*current.values()),
                        'live tables differ from the surviving root inventories')
            if stage == 7 or stage == 8:
                gone = [index for index in range(2) if not spaces_alive[index]]
                live_private = {frames[m['frame'][0]]['physical'] for m in live_mappings
                                if spaces_alive[m['space'][0]]}
                for index in gone:
                    require(descriptors[index] is None, f'retired space {index} kept a descriptor')
                    require(not any(m['space'] == [index, 1] for m in live_mappings),
                            f'retired space {index} kept a live mapping')
                    for physical in space_tables.get(index, set()):
                        require(not allocated(physical), f'space {index} table survived teardown')
                    for physical in space_frames.get(index, set()):
                        if allocated(physical):
                            owner = physical in live_private or any(
                                physical in space_tables.get(side, set())
                                for side in range(2) if spaces_alive[side])
                            require(owner,
                                    f'space {index} private frame survived teardown: {physical:#x}')
                        require(not any(slot['live'] and slot['owner'] == [0, index, 1]
                                        for slot in frames),
                                f'space {index} private frame identity survived: {physical:#x}')
            if stage >= 9:
                require(descriptors == [None, None] and not live_frames, 'terminal stage retained a root or frame')
                require(set(live_tables) == baseline['kernel_tables'], 'terminal table inventory differs from baseline')

            # Root identities must never repeat while both spaces are alive.
            live_spaces = [spaces[index] for index in range(2) if spaces_alive[index]]
            require(len({s['root'] for s in live_spaces}) == len(live_spaces), 'shared CR3 root identity')

            snapshot = {'stage': stage, 'mode': expected_mode, 'cpu': cpu, 'descriptor': d,
                        'frames': frames, 'objects': objects, 'spaces': spaces, 'mappings': mappings,
                        'tables': tables, 'descriptors': descriptors, 'capability_objects': cap_objects,
                        'queues': queues, 'counters': counters, 'saved': saved,
                        'probes': p_values, 'backing': original,
                        'allocator': {'total': total, 'free': free, 'allocated': total - free,
                                      'bitmap_sha256': hashlib.sha256(bitmap).hexdigest()}}
            previous[stage] = snapshot
            output.append(snapshot)
        finally:
            qmp.call('cont')
        if len(previous) == len(expected_stages):
            return {'stages': expected_stages, 'mode': expected_mode,
                    'name': RUNTIME_CASES[expected_mode],
                    'baseline': {k: v for k, v in baseline.items() if k not in ('bitmap', 'live_tables', 'kernel_tables')},
                    'backing': original, 'checkpoints': output}
    raise failure('MemoryObject runtime QMP: missing bounded checkpoint(s)')
