"""Independent stopped-CPU evidence for the Phase 9A SMP bootstrap.

The guest only reports its own conclusions on the serial line. This module
stops the whole VM and reads the production per-CPU publication, the loader
handshake, live vCPU registers, descriptor tables and translations directly.
A guest `true` is never accepted as evidence.
"""

import re

BLOCK = {
    "MAGIC": 0, "LOGICAL": 1, "IS_BSP": 2, "APIC_ID": 3, "PROCESSOR_ID": 4,
    "CPUID_APIC": 5, "STATE": 6, "HISTORY": 7, "ENTRY_COUNT": 8,
    "STACK_BOTTOM": 9, "STACK_TOP": 10, "ENTRY_RSP": 11, "IST_BOTTOM": 12,
    "IST_TOP": 13, "GDT_BASE": 14, "GDT_LIMIT": 15, "TSS_BASE": 16,
    "TSS_LIMIT": 17, "IDT_BASE": 18, "IDT_LIMIT": 19, "CR0": 20, "CR3": 21,
    "CR4": 22, "EFER": 23, "RFLAGS": 24, "APIC_BASE": 25, "GS_BASE": 26,
    "READY_RSP": 27, "PARK_RSP": 28, "PARK_COUNT": 29, "CANARY": 30,
    "OBSERVED_CANARY": 31, "SELF_TEST": 32, "FAULT_VECTOR": 33,
    "FAULT_ERROR": 34, "FAULT_RIP": 35, "LOADER_CR3": 36, "LOADER_RSP": 37,
    "ISOLATION": 38, "SCHEDULER_LOCAL": 39, "MP_INFO": 40, "RELEASE_RIP": 41,
    "CANARY_IST": 42, "OBSERVED_IST_CANARY": 43, "WAIT_SPINS": 44,
    "FAILURE": 45, "EXPECTED_APIC_BASE": 46, "RESERVED": 47,
}

MAGIC = 0x4E45_555F_4350_5531
CANARY = 0x4E45_555F_4150_5354
SELF_TEST_ALL = (1 << 9) - 1
ISOLATION_AP = (1 << 6) - 1
ISOLATION_BSP = ISOLATION_AP & ~(1 << 3)
MAX_CPUS = 4
BLOCK_FIELDS = 48
BLOCK_STRIDE = 384
PARKED = 4
PARKED_HISTORY = 0b11111
AP_WAIT_LIMIT = 1_000_000_000

APIC_BSP_BIT = 1 << 8
APIC_X2APIC = 1 << 10
APIC_GLOBAL_ENABLE = 1 << 11

HALT = 16 * 1024
IST = 8 * 1024
BSP_HALT = 64 * 1024
BSP_IST = 32 * 1024
GDT_LIMIT = 0x27
TSS_LIMIT = 0x67
IDT_LIMIT = 0xFFF

CASES = {
    "smp-bootstrap-1": 1,
    "smp-bootstrap-2": 2,
    "smp-bootstrap-4": 4,
    "smp-cpu-identity": 4,
    "smp-ap-isolation": 4,
}
REJECT = ("smp-bootstrap-reject",)
CPUS = {**CASES, "smp-bootstrap-reject": 4}


def _machine(qmp, cpu, command):
    return qmp.call("human-monitor-command", {"command-line": command, "cpu-index": cpu})


def _registers(qmp, cpu):
    return _machine(qmp, cpu, "info registers")


def _number(text, pattern, what, failure):
    match = re.search(pattern, text)
    if not match:
        raise failure(f"SMP QMP observation is missing {what}:\n{text}")
    return int(match[1], 16)


def _register(qmp, cpu, name, failure):
    return _number(_registers(qmp, cpu), r"\b" + name + r"=([0-9a-fA-F]+)", name, failure)


def _words(qmp, cpu, address, count, failure):
    raw = _machine(qmp, cpu, f"x /{count}gx {address:#x}")
    values = [int(value, 16) for value in re.findall(r"0x[0-9a-fA-F]+", raw)]
    if len(values) != count:
        raise failure("Incomplete SMP debugger memory observation: " + raw)
    return values


def _gpa(qmp, cpu, address, failure):
    raw = _machine(qmp, cpu, f"gva2gpa {address:#x}")
    if "Unmapped" in raw:
        return None
    match = re.search(r"gpa:\s*(0x[0-9a-fA-F]+)", raw)
    if not match:
        raise failure("Unexpected SMP gva2gpa output: " + raw)
    return int(match[1], 16)


def _apic_identities(qmp, fast, failure):
    identities = {}
    for entry in fast:
        cpu = entry.get("cpu-index")
        path = entry.get("qom-path")
        if not isinstance(cpu, int) or not path:
            raise failure("SMP QMP CPU description is incomplete: " + repr(entry))
        apic = qmp.call("qom-get", {"path": path, "property": "apic-id"})
        if not isinstance(apic, int) or not 0 <= apic < 256:
            raise failure("SMP QMP APIC identity is outside the xAPIC range.")
        if apic in identities:
            raise failure("SMP QMP reports a duplicate APIC identity.")
        identities[apic] = cpu
    return identities


def _lapic_section(text, cpu):
    marker = f"dumping local APIC state for CPU {cpu}"
    start = text.find(marker)
    if start < 0:
        return text
    end = text.find("dumping local APIC state for CPU ", start + len(marker))
    return text[start:end if end >= 0 else len(text)]


def _range(item):
    return item["bottom"], item["bottom"] + item["size"]


def _overlaps(first, second):
    return first[0] < second[1] and second[0] < first[1]


def observe(qmp, case, symbols, expected_root, failure):
    """Stops the VM and returns the raw per-CPU/bootstrap evidence."""
    if qmp.call("query-status")["status"] != "running":
        raise failure("SMP VM is not running before the stopped-CPU observation.")
    qmp.call("stop")
    if qmp.call("query-status")["status"] != "paused":
        raise failure("SMP QMP stop did not pause the whole VM.")
    count = CPUS[case]
    fast = qmp.call("query-cpus-fast")
    if len(fast) != count:
        raise failure(f"SMP QMP reports {len(fast)} CPUs instead of {count}.")
    cpu_of_apic = _apic_identities(qmp, fast, failure)
    if len(cpu_of_apic) != count:
        raise failure("SMP QMP APIC identities are not distinct.")
    first_cpu = fast[0]["cpu-index"]

    layout = _words(qmp, first_cpu, symbols["neu_smp_layout"], 4, failure)
    if layout != [MAGIC, MAX_CPUS, BLOCK_FIELDS, BLOCK_STRIDE]:
        raise failure("SMP per-CPU publication layout is not the documented one.")
    base = symbols["neu_smp_cpus"]
    blocks = [_words(qmp, first_cpu, base + logical * BLOCK_STRIDE, BLOCK_FIELDS, failure)
              for logical in range(MAX_CPUS)]
    released = _words(qmp, first_cpu, symbols["neu_smp_released"], 1, failure)[0]
    observation = {
        "case": case, "cpus": count, "layout": layout,
        "released": released, "blocks": blocks, "cpus_qmp": {},
    }

    def require(condition, message):
        if not condition:
            raise failure(message)

    if case in REJECT:
        _reject(qmp, symbols, failure, require, count, fast, cpu_of_apic,
                blocks, released, observation)
    else:
        _positive(qmp, case, symbols, failure, require, expected_root, count,
                  cpu_of_apic, first_cpu, blocks, released, observation)
    return observation


def _positive(qmp, case, symbols, failure, require, root, count,
              cpu_of_apic, first_cpu, blocks, released, observation):
    require(root is not None, "SMP positive case has no activated owned root.")
    require(released == count - 1,
            "SMP release counter does not match the running processor count.")
    start, end = symbols["__kernel_start"], symbols["__kernel_end"]
    bsp = blocks[0]
    require(bsp[BLOCK["MAGIC"]] == MAGIC, "SMP BSP block magic is missing.")
    require(bsp[BLOCK["LOGICAL"]] == 0 and bsp[BLOCK["IS_BSP"]] == 1,
            "SMP BSP block does not identify logical 0.")
    require(bsp[BLOCK["ENTRY_COUNT"]] == 1, "SMP BSP entered its kernel more than once.")
    require(bsp[BLOCK["STACK_BOTTOM"]] == symbols["__stack_bottom"]
            and bsp[BLOCK["STACK_TOP"]] == symbols["__stack_top"]
            and bsp[BLOCK["STACK_TOP"]] - bsp[BLOCK["STACK_BOTTOM"]] == BSP_HALT,
            "SMP BSP stack does not match the ELF layout.")
    require(bsp[BLOCK["IST_BOTTOM"]] == symbols["__df_stack_bottom"]
            and bsp[BLOCK["IST_TOP"]] == symbols["__df_stack_top"]
            and bsp[BLOCK["IST_TOP"]] - bsp[BLOCK["IST_BOTTOM"]] == BSP_IST,
            "SMP BSP IST stack does not match the ELF layout.")
    require(bsp[BLOCK["GDT_LIMIT"]] == GDT_LIMIT
            and bsp[BLOCK["TSS_LIMIT"]] == TSS_LIMIT
            and bsp[BLOCK["IDT_LIMIT"]] == IDT_LIMIT,
            "SMP BSP descriptor limits are not the documented ones.")
    bsp_apic = bsp[BLOCK["APIC_ID"]]
    require(bsp_apic < 256 and bsp_apic in cpu_of_apic,
            "SMP BSP APIC identity is absent from the QMP CPU set.")
    require(bsp[BLOCK["CPUID_APIC"]] == bsp_apic,
            "SMP BSP CPUID and published APIC identity disagree.")
    require(bsp[BLOCK["APIC_BASE"]] & APIC_BSP_BIT
            and bsp[BLOCK["APIC_BASE"]] & APIC_X2APIC == 0
            and bsp[BLOCK["APIC_BASE"]] & APIC_GLOBAL_ENABLE
            and bsp[BLOCK["EXPECTED_APIC_BASE"]] == bsp[BLOCK["APIC_BASE"]],
            "SMP BSP APIC base does not have the xAPIC bootstrap identity.")

    for logical in range(1, count):
        block = blocks[logical]
        own = symbols
        require(block[BLOCK["MAGIC"]] == MAGIC,
                f"SMP AP{logical} block magic is missing.")
        require(block[BLOCK["LOGICAL"]] == logical and block[BLOCK["IS_BSP"]] == 0,
                f"SMP AP{logical} block does not identify its logical index.")
        require(block[BLOCK["ENTRY_COUNT"]] == 1,
                f"SMP AP{logical} did not enter exactly once.")
        require(block[BLOCK["STACK_BOTTOM"]] == own[f"neu_smp_ap{logical}_stack_bottom"]
                and block[BLOCK["STACK_TOP"]] == own[f"neu_smp_ap{logical}_stack_top"]
                and block[BLOCK["STACK_TOP"]] - block[BLOCK["STACK_BOTTOM"]] == HALT,
                f"SMP AP{logical} stack does not match the ELF layout.")
        require(block[BLOCK["IST_BOTTOM"]] == own[f"neu_smp_ap{logical}_ist_bottom"]
                and block[BLOCK["IST_TOP"]] == own[f"neu_smp_ap{logical}_ist_top"]
                and block[BLOCK["IST_TOP"]] - block[BLOCK["IST_BOTTOM"]] == IST,
                f"SMP AP{logical} IST stack does not match the ELF layout.")
        require(block[BLOCK["GDT_BASE"]] == own[f"neu_smp_ap{logical}_gdt"]
                and block[BLOCK["TSS_BASE"]] == own[f"neu_smp_ap{logical}_tss"]
                and block[BLOCK["IDT_BASE"]] == own[f"neu_smp_ap{logical}_idt"],
                f"SMP AP{logical} descriptor tables do not match the ELF layout.")
        require(block[BLOCK["GDT_LIMIT"]] == GDT_LIMIT
                and block[BLOCK["TSS_LIMIT"]] == TSS_LIMIT
                and block[BLOCK["IDT_LIMIT"]] == IDT_LIMIT,
                f"SMP AP{logical} descriptor limits are not the documented ones.")
        apic = block[BLOCK["APIC_ID"]]
        require(apic < 256 and apic != bsp_apic and apic in cpu_of_apic,
                f"SMP AP{logical} APIC identity is invalid or shared with the BSP.")
        require(block[BLOCK["CPUID_APIC"]] == apic,
                f"SMP AP{logical} CPUID and published APIC identity disagree.")
        require(block[BLOCK["APIC_BASE"]] == (bsp[BLOCK["APIC_BASE"]] & ~APIC_BSP_BIT)
                and block[BLOCK["APIC_BASE"]] & APIC_BSP_BIT == 0
                and block[BLOCK["APIC_BASE"]] & APIC_X2APIC == 0
                and block[BLOCK["APIC_BASE"]] & APIC_GLOBAL_ENABLE
                and block[BLOCK["EXPECTED_APIC_BASE"]] == block[BLOCK["APIC_BASE"]],
                f"SMP AP{logical} APIC base is not the expected non-bootstrap xAPIC state.")
        if logical > 1:
            require(blocks[logical - 1][BLOCK["APIC_ID"]] < apic,
                    "SMP AP logical indices are not ordered by ascending APIC identity.")

    for logical in range(count, MAX_CPUS):
        require(all(word == 0 for word in blocks[logical]),
                f"SMP per-CPU slot {logical} is published beyond the running profile.")

    identities = set()
    for logical in range(count):
        block = blocks[logical]
        identities.add(block[BLOCK["APIC_ID"]])
        require(block[BLOCK["STATE"]] == PARKED,
                f"SMP CPU{logical} did not publish the Parked state.")
        require(block[BLOCK["HISTORY"]] == PARKED_HISTORY,
                f"SMP CPU{logical} did not publish every bootstrap transition.")
        require(block[BLOCK["FAILURE"]] == 0 and block[BLOCK["FAULT_VECTOR"]] == 0
                and block[BLOCK["FAULT_ERROR"]] == 0 and block[BLOCK["FAULT_RIP"]] == 0,
                f"SMP CPU{logical} recorded a bootstrap failure or fault.")
        require(block[BLOCK["PARK_COUNT"]] == (0 if logical == 0 else 1),
                f"SMP CPU{logical} did not record its documented park count.")
        if logical == 0:
            require(block[BLOCK["CANARY"]] == CANARY
                    and block[BLOCK["CANARY_IST"]] == CANARY
                    and block[BLOCK["OBSERVED_CANARY"]] == 0
                    and block[BLOCK["OBSERVED_IST_CANARY"]] == 0,
                    "SMP BSP stack/IST canaries are not preserved.")
        else:
            require(block[BLOCK["CANARY"]] == CANARY
                    and block[BLOCK["OBSERVED_CANARY"]] == CANARY
                    and block[BLOCK["CANARY_IST"]] == CANARY
                    and block[BLOCK["OBSERVED_IST_CANARY"]] == CANARY,
                    f"SMP CPU{logical} stack/IST canaries are not preserved.")
        require(block[BLOCK["SELF_TEST"]] == SELF_TEST_ALL,
                f"SMP CPU{logical} did not pass every local self-test.")
        require(block[BLOCK["SCHEDULER_LOCAL"]] == 0 and block[BLOCK["RESERVED"]] == 0,
                f"SMP CPU{logical} published an unowned scheduler or reserved word.")
        require(block[BLOCK["GS_BASE"]] == symbols["neu_smp_cpus"] + logical * BLOCK_STRIDE,
                f"SMP CPU{logical} GS base does not name its own block.")
        require(block[BLOCK["CR3"]] == root,
                f"SMP CPU{logical} did not publish the active owned root.")
        stack = block[BLOCK["STACK_BOTTOM"]], block[BLOCK["STACK_TOP"]]
        for field in ("ENTRY_RSP", "READY_RSP") + (("PARK_RSP",) if logical else ()):
            require(stack[0] <= block[BLOCK[field]] < stack[1],
                    f"SMP CPU{logical} {field} is outside its own stack.")
        require(block[BLOCK["READY_RSP"]] >= block[BLOCK["PARK_RSP"]],
                f"SMP CPU{logical} published park state before readiness.")
        cr0, cr4, efer, rflags = (block[BLOCK["CR0"]], block[BLOCK["CR4"]],
                                  block[BLOCK["EFER"]], block[BLOCK["RFLAGS"]])
        require(cr0 & 0x8001_0001 == 0x8001_0001
                and cr4 & (1 << 5) and cr4 & ((1 << 7) | (1 << 12) | (1 << 17)) == 0
                and efer & 0xD00 == 0xD00 and efer & 1 == 0
                and rflags & ((1 << 8) | (1 << 9)) == 0,
                f"SMP CPU{logical} privileged state or interrupt flag is invalid.")
        if logical == 0:
            require(block[BLOCK["LOADER_CR3"]] == 0 and block[BLOCK["LOADER_RSP"]] == 0
                    and block[BLOCK["MP_INFO"]] == 0 and block[BLOCK["RELEASE_RIP"]] == 0
                    and block[BLOCK["WAIT_SPINS"]] == 0,
                    "SMP BSP published application-processor handshake fields.")
            require(block[BLOCK["ISOLATION"]] == ISOLATION_BSP,
                    "SMP BSP isolation claims are incomplete.")
            require(block[BLOCK["PARK_RSP"]] == 0,
                    "SMP BSP published an application-processor park stack.")
        else:
            require(block[BLOCK["LOADER_CR3"]] not in (0, root),
                    f"SMP AP{logical} did not record a distinct loader root.")
            loader_rsp = block[BLOCK["LOADER_RSP"]]
            require(loader_rsp != 0 and not start <= loader_rsp < end,
                    f"SMP AP{logical} did not abandon the loader-owned stack.")
            require(block[BLOCK["ISOLATION"]] == ISOLATION_AP,
                    f"SMP AP{logical} isolation claims are incomplete.")
            mp_info = block[BLOCK["MP_INFO"]]
            require(mp_info >= 0xFFFF_8000_0000_0000 and mp_info & 7 == 0,
                    f"SMP AP{logical} MP descriptor is not an aligned higher-half pointer.")
            require(block[BLOCK["RELEASE_RIP"]] == symbols[f"neu_smp_ap_entry{logical}"],
                    f"SMP AP{logical} release target is not its kernel entry.")
            require(0 < block[BLOCK["WAIT_SPINS"]] < AP_WAIT_LIMIT,
                    f"SMP AP{logical} root wait was not a bounded completed wait.")
        # SAFETY of the QMP read: the block above is production memory and the
        # stack words belong to this CPU's private arena.
        require(_words(qmp, first_cpu, block[BLOCK["STACK_BOTTOM"]], 1, failure)[0] == CANARY,
                f"SMP CPU{logical} stack canary changed in memory.")
        require(_words(qmp, first_cpu, block[BLOCK["IST_BOTTOM"]], 1, failure)[0] == CANARY,
                f"SMP CPU{logical} IST canary changed in memory.")
    require(len(identities) == count,
            "SMP published per-CPU APIC identities are not distinct.")

    # Whole-VM register and descriptor inspection, one stopped vCPU at a time.
    for logical in range(count):
        block = blocks[logical]
        cpu = cpu_of_apic[block[BLOCK["APIC_ID"]]]
        text = _registers(qmp, cpu)
        rip = _number(text, r"\bRIP=([0-9a-fA-F]+)", "RIP", failure)
        require("HLT=1" in text and "CPL=0" in text,
                f"SMP CPU{logical} is not halted at Ring 0.")
        require(_number(text, r"\bRFL=([0-9a-fA-F]+)", "RFL", failure) & (1 << 9) == 0,
                f"SMP CPU{logical} has interrupts enabled.")
        if logical == 0:
            require(symbols["neu_halt_begin"] <= rip < symbols["neu_halt_end"],
                    "SMP BSP is not in Neu's kernel halt loop.")
        else:
            require(symbols["neu_smp_park_begin"] <= rip < symbols["neu_smp_park_end"],
                    f"SMP AP{logical} is not in its CPU-local park loop.")
        require(_number(text, r"\bCR3=([0-9a-fA-F]+)", "CR3", failure) == root,
                f"SMP CPU{logical} is not running on the observed owned root.")
        for name in ("CR0", "CR4", "EFER"):
            require(_number(text, r"\b" + name + r"=([0-9a-fA-F]+)", name, failure)
                    == block[BLOCK[name]],
                    f"SMP CPU{logical} live {name} disagrees with its publication.")
        gs = re.search(r"\bGS\s*=\s*[0-9a-fA-F]+\s+([0-9a-fA-F]+)", text)
        require(gs and int(gs[1], 16) == block[BLOCK["GS_BASE"]],
                f"SMP CPU{logical} live GS base is not its published block.")
        gdt = re.search(r"\bGDT=\s*([0-9a-fA-F]+)\s+([0-9a-fA-F]+)", text)
        require(gdt and int(gdt[1], 16) == block[BLOCK["GDT_BASE"]]
                and int(gdt[2], 16) == block[BLOCK["GDT_LIMIT"]],
                f"SMP CPU{logical} live GDT is not its published table.")
        idt = re.search(r"\bIDT=\s*([0-9a-fA-F]+)\s+([0-9a-fA-F]+)", text)
        require(idt and int(idt[1], 16) == block[BLOCK["IDT_BASE"]]
                and int(idt[2], 16) == block[BLOCK["IDT_LIMIT"]],
                f"SMP CPU{logical} live IDT is not its published table.")
        tr = re.search(r"\bTR\s*=\s*([0-9a-fA-F]+)\s+([0-9a-fA-F]+)\s+"
                       r"([0-9a-fA-F]+)\s+([0-9a-fA-F]+)", text)
        require(tr and int(tr[1], 16) == 0x18
                and int(tr[2], 16) == block[BLOCK["TSS_BASE"]]
                and int(tr[3], 16) == block[BLOCK["TSS_LIMIT"]]
                and int(tr[4], 16) & 0xFF00 in (0x8900, 0x8B00)
                and ("TSS64-avl" in text or "TSS64-busy" in text),
                f"SMP CPU{logical} live TR is not its published TSS.")
        rsp = _number(text, r"\bRSP=([0-9a-fA-F]+)", "RSP", failure)
        require(block[BLOCK["STACK_BOTTOM"]] <= rsp < block[BLOCK["STACK_TOP"]],
                f"SMP CPU{logical} is not running on its own stack.")
        cs = re.search(r"\bCS\s*=\s*([0-9a-fA-F]+)", text)
        require(cs and int(cs[1], 16) == 8 and "CS64" in text and "DPL=0" in text,
                f"SMP CPU{logical} is not executing the Ring 0 64-bit code segment.")
        lapic = _lapic_section(_machine(qmp, cpu, "info lapic"), cpu)
        lvtt = re.search(r"LVTT\s+0x([0-9a-fA-F]+)", lapic)
        require(lvtt is not None, f"SMP CPU{logical} has no readable local APIC timer state.")
        require(int(lvtt[1], 16) & 0x10000,
                f"SMP CPU{logical} has an unmasked local APIC timer.")
        spiv = re.search(r"SPIV\s+0x([0-9a-fA-F]+)", lapic)
        require(spiv is not None and int(spiv[1], 16) & 0x100,
                f"SMP CPU{logical} local APIC is not software enabled.")
        timer = re.search(r"Timer\s+DCR=0x([0-9a-fA-F]+).*?initial_count = (\d+).*?current_count = (\d+)", lapic)
        require(timer is not None, f"SMP CPU{logical} has no readable local APIC timer counters.")
        if logical:
            require(int(timer[2]) == 0 and int(timer[3]) == 0,
                    f"SMP AP{logical} has an armed local APIC timer.")
        observation.setdefault("lapic", {})[str(logical)] = {
            "lvtt": int(lvtt[1], 16), "spiv": int(spiv[1], 16),
            "timer_initial": int(timer[2]), "timer_current": int(timer[3])}
        observation["cpus_qmp"][str(logical)] = {
            "cpu_index": cpu, "apic_id": block[BLOCK["APIC_ID"]],
            "rip": rip, "rsp": rsp, "cr3": root, "gs_base": block[BLOCK["GS_BASE"]],
            "gdt": block[BLOCK["GDT_BASE"]], "tss": block[BLOCK["TSS_BASE"]],
            "idt": block[BLOCK["IDT_BASE"]], "hlt": 1, "cpl": 0, "if": 0,
            "apic_base": block[BLOCK["APIC_BASE"]],
        }

    # Guard pages must be absent and every stack must have its own frame.
    guards = [symbols[f"neu_smp_ap{index}_{name}"]
              for index in (1, 2, 3)
              for name in ("guard_low", "guard_mid", "guard_ist")]
    guards += [symbols["__stack_guard_low"], symbols["__stack_guard_high"],
               symbols["__df_guard_low"], symbols["__df_guard_high"]]
    bsp_cpu = cpu_of_apic[blocks[0][BLOCK["APIC_ID"]]]
    for guard in guards:
        require(_gpa(qmp, bsp_cpu, guard, failure) is None,
                f"SMP guard page {guard:#x} is mapped in the owned root.")
    frames = {}
    for logical in range(count):
        block = blocks[logical]
        for name in ("STACK_BOTTOM", "IST_BOTTOM"):
            gpa = _gpa(qmp, bsp_cpu, block[BLOCK[name]], failure)
            require(gpa is not None,
                    f"SMP CPU{logical} {name} is unmapped in the owned root.")
            require(gpa not in frames,
                    "SMP per-CPU stacks share one physical frame.")
            frames[gpa] = f"{logical}:{name}"
    observation["stack_frames"] = {str(address): owner for address, owner in frames.items()}

    # Per-CPU arenas, descriptor tables and the BSP stacks must be disjoint.
    items = [
        {"name": "bsp-stack", "bottom": symbols["__stack_bottom"], "size": BSP_HALT},
        {"name": "bsp-ist", "bottom": symbols["__df_stack_bottom"], "size": BSP_IST},
    ]
    for logical in range(1, count):
        block = blocks[logical]
        items += [
            {"name": f"ap{logical}-stack", "bottom": block[BLOCK["STACK_BOTTOM"]], "size": HALT},
            {"name": f"ap{logical}-ist", "bottom": block[BLOCK["IST_BOTTOM"]], "size": IST},
            {"name": f"ap{logical}-gdt", "bottom": block[BLOCK["GDT_BASE"]], "size": 64},
            {"name": f"ap{logical}-tss", "bottom": block[BLOCK["TSS_BASE"]], "size": 104},
            {"name": f"ap{logical}-idt", "bottom": block[BLOCK["IDT_BASE"]], "size": 4096},
        ]
    for index, item in enumerate(items):
        for other in items[:index]:
            require(not _overlaps(_range(item), _range(other)),
                    f"SMP resources overlap: {item['name']} and {other['name']}.")

    observation["guards"] = len(guards)
    observation["stack_frames_count"] = len(frames)
    return observation


def _reject(qmp, symbols, failure, require, count, fast, cpu_of_apic,
            blocks, released, observation):
    for logical in range(MAX_CPUS):
        require(all(word == 0 for word in blocks[logical]),
                f"Rejected SMP profile published per-CPU slot {logical}.")
    require(released == 0, "Rejected SMP profile recorded a processor release.")
    start, end = symbols["__kernel_start"], symbols["__kernel_end"]
    halt_low, halt_high = symbols["neu_halt_begin"], symbols["neu_halt_end"]
    park_low, park_high = symbols["neu_smp_park_begin"], symbols["neu_smp_park_end"]
    base = symbols["neu_smp_cpus"]
    bootstrap = None
    loader_root = None
    observed = {}
    for entry in fast:
        cpu = entry["cpu-index"]
        text = _registers(qmp, cpu)
        rip = _number(text, r"\bRIP=([0-9a-fA-F]+)", "RIP", failure)
        cr3 = _number(text, r"\bCR3=([0-9a-fA-F]+)", "CR3", failure)
        require("CPL=0" in text, "Rejected SMP CPU left Ring 0.")
        if halt_low <= rip < halt_high:
            require(bootstrap is None, "Rejected SMP profile halted more than one CPU.")
            require("HLT=1" in text and _number(text, r"\bRFL=([0-9a-fA-F]+)", "RFL", failure) & (1 << 9) == 0,
                    "Rejected SMP BSP is not halted with interrupts disabled.")
            bootstrap, loader_root = cpu, cr3
        else:
            require(not start <= rip < end and not park_low <= rip < park_high,
                    "Rejected SMP application processor entered kernel code.")
        gs = re.search(r"\bGS\s*=\s*[0-9a-fA-F]+\s+([0-9a-fA-F]+)", text)
        require(gs and not base <= int(gs[1], 16) < base + MAX_CPUS * BLOCK_STRIDE,
                "Rejected SMP CPU owns a per-CPU identity block.")
        observed[str(cpu)] = {"rip": rip, "cr3": cr3}
    require(bootstrap is not None, "Rejected SMP profile did not halt its BSP in Neu.")
    require(loader_root != 0, "Rejected SMP BSP has no active loader root.")
    for entry in fast:
        if entry["cpu-index"] == bootstrap:
            continue
        require(observed[str(entry["cpu-index"])]["cr3"] == loader_root,
                "Rejected SMP processors do not share the loader root.")
    require(len(cpu_of_apic) == count, "Rejected SMP QMP CPU set changed.")
    observation["bootstrap_cpu"] = bootstrap
    observation["loader_root"] = loader_root
    observation["cpus_qmp"] = observed
    return observation
