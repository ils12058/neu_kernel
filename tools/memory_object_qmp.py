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
