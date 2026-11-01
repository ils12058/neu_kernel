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
WORK_CASES = {
    "smp-work-2": 2,
    "smp-work-4": 4,
    "smp-work-overlap": 2,
    "smp-work-repeat": 2,
    "smp-work-invalid": 2,
    "smp-ipi-2": 2,
    "smp-ipi-4": 4,
}
CASES.update(WORK_CASES)
REJECT = ("smp-bootstrap-reject",)
CPUS = {**CASES, "smp-bootstrap-reject": 4}

WORK_MAGIC = 0x4E45_555F_4D42_5831
WORK_ABI = 1
WORK_LAYOUT = [WORK_ABI, WORK_MAGIC, MAX_CPUS, 32, 256, 24, 192, 4, 10]
WORK_IPI_VECTOR = 0xF0
GATE_MAGIC = 0x4E45_555F_4741_5431
EVIDENCE_MAGIC = 0x4E45_555F_574B_4531

MAILBOX = {
    "MAGIC": 0, "LOGICAL": 1, "APIC_ID": 2, "PHASE": 3, "STATE": 4,
    "SEQUENCE": 5, "ACKED": 6, "KIND": 7, "INPUT": 8, "ROUNDS": 9,
    "PUBLISH_ORDER": 10, "ACCEPT_SEQUENCE": 11, "ACCEPT_ORDER": 12,
    "ACCEPT_LOGICAL": 13, "ACCEPT_APIC": 14, "START_MARKER": 15,
    "START_RSP": 16, "COMPLETE_SEQUENCE": 17, "COMPLETE_LOGICAL": 18,
    "COMPLETE_APIC": 19, "COMPLETE_MARKER": 20, "RESULT": 21,
    "RESULT_AUX": 22, "EXECUTIONS": 23, "EXEC_MASK": 24, "COMPLETE_ORDER": 25,
    "FAILURE": 26, "REJECTIONS": 27, "LAST_REJECT": 28, "IPI_RECEIVED": 29,
    "IPI_EOIS": 30, "SPURIOUS": 31,
}
GATE = {
    "MAGIC": 0, "EPOCH": 1, "PARTICIPANTS": 2, "PHASE": 3, "RELEASE": 4,
    "OVERLAP": 5, "STOP": 6, "FAILURE": 7,
}
GATE_ENTRY = {"ENTERED": 0, "READY": 1, "DONE": 2, "RESERVED": 3}
GATE_HEADER_FIELDS = 8
GATE_CPU_FIELDS = 4
GATE_WORDS = GATE_HEADER_FIELDS + GATE_CPU_FIELDS * MAX_CPUS
GATE_PHASE = {"Closed": 0, "Open": 1, "Checkpoint": 2, "Released": 3, "Complete": 4}
JOURNAL = {
    "SEQUENCE": 0, "KIND": 1, "INPUT": 2, "ROUNDS": 3, "RESULT": 4,
    "AUX": 5, "START": 6, "MARKER": 7, "ACCEPT_ORDER": 8, "COMPLETE_ORDER": 9,
}
JOURNAL_FIELDS = 10
JOURNAL_SLOTS = 4
PHASE_READY = 1
PHASE_STOPPED = 3
PHASE_FAILED = 4
PHASE_IPI_IDLE = 2
PHASE_IPI_BUSY = 5
STATE_IDLE = 0
STATE_PUBLISHED = 1
STATE_EXECUTING = 2
STATE_COMPLETED = 3
KIND_TRANSFORM = 1
KIND_CHECKSUM = 2
KIND_STOP = 3
MASK64 = (1 << 64) - 1

# Independent copies of the guest's documented command plans. Every AP runs
# either the whole sequential plan or the single command at its logical index.
WORK_PLANS = {
    "smp-work-2": [(KIND_TRANSFORM, 0x9E37_0001, 512), (KIND_CHECKSUM, 0x51ED_0002, 384)],
    "smp-work-4": [(KIND_TRANSFORM, 0x9101, 320), (KIND_CHECKSUM, 0x9202, 288),
                   (KIND_TRANSFORM, 0x9303, 256)],
    "smp-work-overlap": [(KIND_TRANSFORM, 0x0BAD_C0DE, 448)],
    "smp-work-repeat": [(KIND_TRANSFORM, 0x31, 96), (KIND_CHECKSUM, 0x32, 160),
                        (KIND_TRANSFORM, 0x33, 224)],
    "smp-work-invalid": [(KIND_CHECKSUM, 0x44, 192)],
    "smp-ipi-2": [(KIND_CHECKSUM, 0x5151, 512), (KIND_TRANSFORM, 0x5252, 384)],
    "smp-ipi-4": [(KIND_CHECKSUM, 0x6161, 320), (KIND_TRANSFORM, 0x6262, 288),
                  (KIND_CHECKSUM, 0x6363, 256)],
}
PARALLEL_WORK = ("smp-work-4", "smp-ipi-4")


def work_plan(case, logical):
    if case in PARALLEL_WORK:
        return [WORK_PLANS[case][logical - 1]]
    return list(WORK_PLANS[case])


def mix(value):
    value = (value + 0x9E37_79B9_7F4A_7C15) & MASK64
    value = ((value ^ (value >> 30)) * 0xBF58_476D_1CE4_E5B9) & MASK64
    value = ((value ^ (value >> 27)) * 0x94D0_49BB_1331_11EB) & MASK64
    return value ^ (value >> 31)


def start_marker(sequence, logical, apic):
    return mix(sequence ^ (logical << 32) ^ apic)


def complete_marker(start, result, aux):
    rotated = ((aux << 17) | (aux >> 47)) & MASK64
    return mix(start ^ result ^ rotated)


def execute(kind, command_input, rounds):
    if kind == KIND_TRANSFORM:
        state = command_input ^ 0x243F_6A88_85A3_08D3
        aux = 0
        for index in range(rounds):
            state = (state * 6_364_136_223_846_793_005 + 1_442_695_040_888_963_407) & MASK64
            state ^= state >> 29
            aux = (aux + (state ^ index)) & MASK64
        return state, aux
    if kind == KIND_CHECKSUM:
        state = 0xCBF2_9CE4_8422_2325
        for index in range(rounds):
            state ^= (command_input + index) & MASK64
            state = (state * 0x0000_0100_0000_01B3) & MASK64
        return state, rounds
    if kind == KIND_STOP:
        return 0, 0
    raise ValueError("undocumented work command kind")


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
    elif case in WORK_CASES:
        _positive(qmp, case, symbols, failure, require, expected_root, count,
                  cpu_of_apic, first_cpu, blocks, released, observation)
        _work_state(qmp, case, symbols, failure, require, count, first_cpu,
                    blocks, observation)
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


def _work_state(qmp, case, symbols, failure, require, count, first_cpu,
                blocks, observation):
    """Rechecks every production mailbox, journal slot and evidence word."""
    for name in ("neu_smp_work_layout", "neu_smp_work_mailboxes", "neu_smp_work_gate",
                 "neu_smp_work_journal", "neu_smp_work_evidence"):
        if name not in symbols:
            raise failure(f"SMP work image is missing its published symbol {name}.")
    layout = _words(qmp, first_cpu, symbols["neu_smp_work_layout"], 9, failure)
    require(layout == WORK_LAYOUT, "SMP work layout is not the documented one.")
    ipi = case.startswith("smp-ipi")
    base = symbols["neu_smp_work_mailboxes"]
    mailboxes = [_words(qmp, first_cpu, base + logical * 256, 32, failure)
                 for logical in range(MAX_CPUS)]
    total_work = sum(len(work_plan(case, logical)) for logical in range(1, count))

    def slot(logical):
        return mailboxes[logical]

    for logical in range(MAX_CPUS):
        table = slot(logical)
        if logical == 0 or logical >= count:
            require(all(word == 0 for word in table),
                    f"SMP work mailbox slot {logical} is published beyond the profile.")
            continue
        block = blocks[logical]
        apic = block[BLOCK["APIC_ID"]]
        plan = work_plan(case, logical)
        expected = len(plan) + 1
        require(table[MAILBOX["MAGIC"]] == WORK_MAGIC,
                f"SMP work mailbox {logical} magic is missing.")
        require(table[MAILBOX["LOGICAL"]] == logical and table[MAILBOX["APIC_ID"]] == apic,
                f"SMP work mailbox {logical} identity is wrong.")
        require(table[MAILBOX["PHASE"]] == PHASE_STOPPED,
                f"SMP work mailbox {logical} did not stop.")
        require(table[MAILBOX["STATE"]] == STATE_IDLE,
                f"SMP work mailbox {logical} is not idle after the final ack.")
        for field in ("SEQUENCE", "ACKED", "EXECUTIONS"):
            require(table[MAILBOX[field]] == expected,
                    f"SMP work mailbox {logical} {field} is not the exact-once count.")
        require(table[MAILBOX["PUBLISH_ORDER"]] == expected
                and table[MAILBOX["ACCEPT_SEQUENCE"]] == expected
                and table[MAILBOX["ACCEPT_ORDER"]] == expected
                and table[MAILBOX["COMPLETE_SEQUENCE"]] == expected
                and table[MAILBOX["COMPLETE_ORDER"]] == expected,
                f"SMP work mailbox {logical} ordering fields disagree with its sequence.")
        require(table[MAILBOX["EXEC_MASK"]] == ((1 << expected) - 1) & MASK64,
                f"SMP work mailbox {logical} execution mask shows a hole or replay.")
        require(table[MAILBOX["ACCEPT_LOGICAL"]] == logical
                and table[MAILBOX["COMPLETE_LOGICAL"]] == logical
                and table[MAILBOX["ACCEPT_APIC"]] == apic
                and table[MAILBOX["COMPLETE_APIC"]] == apic,
                f"SMP work mailbox {logical} accepted/completed for another CPU.")
        start = start_marker(expected, logical, apic)
        require(table[MAILBOX["START_MARKER"]] == start
                and table[MAILBOX["COMPLETE_MARKER"]] == complete_marker(start, 0, 0),
                f"SMP work mailbox {logical} final markers are inconsistent.")
        require(table[MAILBOX["KIND"]] == KIND_STOP and table[MAILBOX["INPUT"]] == 0
                and table[MAILBOX["ROUNDS"]] == 0 and table[MAILBOX["RESULT"]] == 0
                and table[MAILBOX["RESULT_AUX"]] == 0,
                f"SMP work mailbox {logical} final command is not the published stop.")
        stack = (block[BLOCK["STACK_BOTTOM"]], block[BLOCK["STACK_TOP"]])
        require(stack[0] <= table[MAILBOX["START_RSP"]] < stack[1],
                f"SMP work mailbox {logical} start RSP is outside its private stack.")
        require(table[MAILBOX["FAILURE"]] == 0 and table[MAILBOX["REJECTIONS"]] == 0
                and table[MAILBOX["LAST_REJECT"]] == 0,
                f"SMP work mailbox {logical} recorded a rejected or failed command.")
        if ipi:
            require(table[MAILBOX["IPI_RECEIVED"]] == expected
                    and table[MAILBOX["IPI_EOIS"]] == expected
                    and table[MAILBOX["SPURIOUS"]] == 0,
                    f"SMP work mailbox {logical} IPI accounting is inconsistent.")
        else:
            require(table[MAILBOX["IPI_RECEIVED"]] == 0
                    and table[MAILBOX["IPI_EOIS"]] == 0
                    and table[MAILBOX["SPURIOUS"]] == 0,
                    f"SMP work polling mailbox {logical} recorded an interrupt.")

    journal_base = symbols["neu_smp_work_journal"]
    for logical in range(MAX_CPUS):
        apic = blocks[logical][BLOCK["APIC_ID"]] if logical < count else 0
        plan = work_plan(case, logical) if 0 < logical < count else []
        entries = plan + [(KIND_STOP, 0, 0)] if plan else []
        for index in range(JOURNAL_SLOTS):
            address = journal_base + ((logical * JOURNAL_SLOTS + index)
                                      * JOURNAL_FIELDS * 8)
            row = _words(qmp, first_cpu, address, JOURNAL_FIELDS, failure)
            if index >= len(entries):
                require(all(word == 0 for word in row),
                        f"SMP work journal slot {logical}:{index} was written beyond the plan.")
                continue
            sequence = index + 1
            kind, command_input, rounds = entries[index]
            require(row[JOURNAL["SEQUENCE"]] == sequence,
                    f"SMP work journal slot {logical}:{index} has the wrong sequence.")
            require(row[JOURNAL["KIND"]] == kind and row[JOURNAL["INPUT"]] == command_input
                    and row[JOURNAL["ROUNDS"]] == rounds,
                    f"SMP work journal slot {logical}:{index} does not match the plan.")
            result, aux = execute(kind, command_input, rounds)
            require(row[JOURNAL["RESULT"]] == result and row[JOURNAL["AUX"]] == aux,
                    f"SMP work journal slot {logical}:{index} result is not reproducible.")
            start = start_marker(sequence, logical, apic)
            require(row[JOURNAL["START"]] == start
                    and row[JOURNAL["MARKER"]] == complete_marker(start, result, aux),
                    f"SMP work journal slot {logical}:{index} markers are inconsistent.")
            require(row[JOURNAL["ACCEPT_ORDER"]] == sequence
                    and row[JOURNAL["COMPLETE_ORDER"]] == sequence,
                    f"SMP work journal slot {logical}:{index} ordering is not independent.")

    evidence = _words(qmp, first_cpu, symbols["neu_smp_work_evidence"], 19, failure)
    require(evidence[0] == EVIDENCE_MAGIC and evidence[1] == WORK_ABI,
            "SMP work evidence header is invalid.")
    require(evidence[2] == (2 if ipi else 1), "SMP work mode evidence is wrong.")
    require(evidence[3] == count, "SMP work evidence CPU count is wrong.")
    require(evidence[4] == total_work
            and evidence[5] == (1 if case in PARALLEL_WORK else len(WORK_PLANS[case])),
            "SMP work evidence command counts are wrong.")
    require(evidence[6] == count - 1, "SMP work evidence stopped AP count is wrong.")
    require(evidence[18] == 0, "SMP work driver recorded a failure.")
    negative = 6 if case == "smp-work-invalid" else 0
    require(evidence[11] == negative and evidence[12] == negative,
            "SMP work negative-check evidence is wrong.")
    sent = sum(len(work_plan(case, logical)) + 1 for logical in range(1, count)) if ipi else 0
    require(evidence[7] == sent and evidence[8] == sent and evidence[9] == sent
            and evidence[10] == 0,
            "SMP work IPI evidence is inconsistent.")
    gate = _words(qmp, first_cpu, symbols["neu_smp_work_gate"], GATE_WORDS, failure)
    if case == "smp-work-overlap":
        result, aux = execute(*WORK_PLANS[case][0])
        require(evidence[13] == result and evidence[14] == aux,
                "SMP work BSP result is not reproducible.")
        require(evidence[15] == 1 and evidence[16] == 0b11 and evidence[17] == 1,
                "SMP work overlap evidence is incomplete.")
        require(gate[GATE["MAGIC"]] == GATE_MAGIC and gate[GATE["EPOCH"]] == 1
                and gate[GATE["PARTICIPANTS"]] == 2
                and gate[GATE["PHASE"]] == GATE_PHASE["Complete"]
                and gate[GATE["RELEASE"]] == 1 and gate[GATE["OVERLAP"]] == 0b11
                and gate[GATE["STOP"]] == 0 and gate[GATE["FAILURE"]] == 0,
                "SMP work gate final state is inconsistent.")
        for logical in (0, 1):
            offset = GATE_HEADER_FIELDS + logical * GATE_CPU_FIELDS
            require(gate[offset + GATE_ENTRY["ENTERED"]] == 1
                    and gate[offset + GATE_ENTRY["READY"]] == 1
                    and gate[offset + GATE_ENTRY["DONE"]] == 1
                    and gate[offset + GATE_ENTRY["RESERVED"]] == 0,
                    f"SMP work gate participant {logical} is not fully published.")
    else:
        require(evidence[13] == 0 and evidence[14] == 0 and evidence[15] == 0
                and evidence[16] == 0 and evidence[17] == 0 and all(word == 0 for word in gate),
                "SMP work profile published an unexpected BSP or gate result.")
    observation["work"] = {
        "layout": layout, "mailboxes": mailboxes, "gate": gate, "evidence": evidence,
        "commands": total_work, "mode": "ipi" if ipi else "poll",
    }
    return observation


def _live_registers(qmp, cpu, failure):
    text = _registers(qmp, cpu)
    if "CPL=0" not in text:
        raise failure("SMP work checkpoint CPU left Ring 0.")
    cs = re.search(r"\bCS\s*=\s*([0-9a-fA-F]+)", text)
    gs = re.search(r"\bGS\s*=\s*[0-9a-fA-F]+\s+([0-9a-fA-F]+)", text)
    if not cs or not gs or int(cs[1], 16) != 8:
        raise failure("SMP work checkpoint CPU is not on the Ring 0 64-bit code segment.")
    return {
        "rip": _number(text, r"\bRIP=([0-9a-fA-F]+)", "RIP", failure),
        "rsp": _number(text, r"\bRSP=([0-9a-fA-F]+)", "RSP", failure),
        "cr3": _number(text, r"\bCR3=([0-9a-fA-F]+)", "CR3", failure),
        "flags": _number(text, r"\bRFL=([0-9a-fA-F]+)", "RFL", failure),
        "gs": int(gs[1], 16),
        "cs": int(cs[1], 16),
    }


def observe_work_checkpoint(qmp, case, symbols, root, failure):
    """Stops the whole VM at the published work checkpoint and rechecks both
    the BSP and every AP inside the intended, exported work phase."""
    if case not in WORK_CASES:
        raise failure("SMP work checkpoint requested for an unknown profile.")
    if root is None:
        raise failure("SMP work checkpoint has no activated owned root.")
    for name in ("neu_smp_work_layout", "neu_smp_work_mailboxes", "neu_smp_work_gate",
                 "neu_smp_work_bsp_dwell_begin", "neu_smp_work_bsp_dwell_end",
                 "neu_smp_work_ap_dwell_begin", "neu_smp_work_ap_dwell_end",
                 "neu_smp_work_gate_wait_begin", "neu_smp_work_gate_wait_end"):
        if name not in symbols:
            raise failure(f"SMP work checkpoint symbol {name} is missing.")
    if qmp.call("query-status")["status"] != "running":
        raise failure("SMP work VM is not running before its checkpoint.")
    qmp.call("stop")
    try:
        if qmp.call("query-status")["status"] != "paused":
            raise failure("SMP work checkpoint did not pause the whole VM.")
        count = CPUS[case]
        fast = qmp.call("query-cpus-fast")
        if len(fast) != count:
            raise failure(f"SMP work checkpoint reports {len(fast)} CPUs instead of {count}.")
        cpu_of_apic = _apic_identities(qmp, fast, failure)
        first_cpu = fast[0]["cpu-index"]
        layout = _words(qmp, first_cpu, symbols["neu_smp_work_layout"], 9, failure)
        if layout != WORK_LAYOUT:
            raise failure("SMP work checkpoint layout is not the documented one.")
        ipi = case.startswith("smp-ipi")
        base = symbols["neu_smp_cpus"]
        mailbox_base = symbols["neu_smp_work_mailboxes"]
        bsp_dwell = (symbols["neu_smp_work_bsp_dwell_begin"],
                     symbols["neu_smp_work_bsp_dwell_end"])
        ap_dwell = (symbols["neu_smp_work_ap_dwell_begin"],
                    symbols["neu_smp_work_ap_dwell_end"])
        gate_wait = (symbols["neu_smp_work_gate_wait_begin"],
                     symbols["neu_smp_work_gate_wait_end"])
        bsp = None
        ap_cpus = {}
        checkpoint = {"case": case, "cpus": count, "layout": layout,
                      "phase": "gate" if case == "smp-work-overlap" else "parallel",
                      "cpus_qmp": {}}
        for apic, cpu in cpu_of_apic.items():
            live = _live_registers(qmp, cpu, failure)
            if live["cr3"] != root or live["flags"] & (1 << 9):
                raise failure("SMP work checkpoint CPU is not on the owned root with IF=0.")
            if bsp_dwell[0] <= live["rip"] < bsp_dwell[1]:
                if bsp is not None:
                    raise failure("SMP work checkpoint observed two BSP dwells.")
                if live["gs"] != base:
                    raise failure("SMP work checkpoint BSP GS base is not its block.")
                if not symbols["__stack_bottom"] <= live["rsp"] < symbols["__stack_top"]:
                    raise failure("SMP work checkpoint BSP RSP is outside its stack.")
                bsp = (apic, cpu, live)
                continue
            if ap_dwell[0] <= live["rip"] < ap_dwell[1]:
                phase = "ap-dwell"
            elif gate_wait[0] <= live["rip"] < gate_wait[1]:
                phase = "gate-wait"
            else:
                raise failure("SMP work checkpoint CPU is outside every exported work phase.")
            if case != "smp-work-overlap" and phase != "ap-dwell":
                raise failure("SMP work parallel checkpoint AP is not inside its dwell.")
            if case == "smp-work-overlap" and phase != "gate-wait":
                raise failure("SMP work gate checkpoint AP is not inside the barrier.")
            offset = live["gs"] - base
            if offset % 384 or not 1 <= offset // 384 < count:
                raise failure("SMP work checkpoint AP GS base is not a live CPU block.")
            logical = offset // 384
            if logical in ap_cpus:
                raise failure("SMP work checkpoint reported one AP logical index twice.")
            ap_cpus[logical] = (apic, cpu, live, phase)
        if bsp is None or len(ap_cpus) != count - 1 or set(ap_cpus) != set(range(1, count)):
            raise failure("SMP work checkpoint did not observe every CPU in a work phase.")
        for logical, (apic, cpu, live, phase) in ap_cpus.items():
            table = _words(qmp, first_cpu, mailbox_base + logical * 256, 32, failure)
            expected_phase = PHASE_IPI_BUSY if ipi else PHASE_READY
            if (table[MAILBOX["MAGIC"]] != WORK_MAGIC
                    or table[MAILBOX["LOGICAL"]] != logical
                    or table[MAILBOX["APIC_ID"]] != apic):
                raise failure(f"SMP work checkpoint mailbox {logical} identity is wrong.")
            if table[MAILBOX["PHASE"]] != expected_phase:
                raise failure(f"SMP work checkpoint mailbox {logical} phase is wrong.")
            sequence = table[MAILBOX["SEQUENCE"]]
            if (table[MAILBOX["STATE"]] != STATE_EXECUTING or sequence != 1
                    or table[MAILBOX["PUBLISH_ORDER"]] != 1
                    or table[MAILBOX["ACCEPT_SEQUENCE"]] != 1
                    or table[MAILBOX["ACCEPT_ORDER"]] != 1
                    or table[MAILBOX["ACCEPT_LOGICAL"]] != logical
                    or table[MAILBOX["ACCEPT_APIC"]] != apic
                    or table[MAILBOX["START_MARKER"]] != start_marker(1, logical, apic)):
                raise failure(f"SMP work checkpoint mailbox {logical} did not publish one accepted command.")
            if (table[MAILBOX["COMPLETE_SEQUENCE"]] != 0
                    or table[MAILBOX["EXECUTIONS"]] != 0
                    or table[MAILBOX["EXEC_MASK"]] != 0
                    or table[MAILBOX["COMPLETE_MARKER"]] != 0
                    or table[MAILBOX["RESULT"]] != 0 or table[MAILBOX["RESULT_AUX"]] != 0):
                raise failure(f"SMP work checkpoint mailbox {logical} already published completion.")
            stack = (symbols[f"neu_smp_ap{logical}_stack_bottom"],
                     symbols[f"neu_smp_ap{logical}_stack_top"])
            if (not stack[0] <= table[MAILBOX["START_RSP"]] < stack[1]
                    or not stack[0] <= live["rsp"] < stack[1]):
                raise failure(f"SMP work checkpoint AP{logical} start RSP is outside its private stack.")
            checkpoint["cpus_qmp"][str(logical)] = {
                "apic_id": apic, "cpu_index": cpu, "rip": live["rip"],
                "phase": phase, "mailbox_state": table[MAILBOX["STATE"]],
                "sequence": sequence,
            }
        checkpoint["cpu_bsp"] = {"apic_id": bsp[0], "cpu_index": bsp[1],
                                 "rip": bsp[2]["rip"], "phase": "bsp-dwell"}
        gate = _words(qmp, first_cpu, symbols["neu_smp_work_gate"], GATE_WORDS, failure)
        if case == "smp-work-overlap":
            if (gate[GATE["MAGIC"]] != GATE_MAGIC or gate[GATE["EPOCH"]] != 1
                    or gate[GATE["PARTICIPANTS"]] != 2
                    or gate[GATE["PHASE"]] != GATE_PHASE["Checkpoint"]
                    or gate[GATE["RELEASE"]] != 0 or gate[GATE["OVERLAP"]] != 0b11
                    or gate[GATE["STOP"]] != 0 or gate[GATE["FAILURE"]] != 0):
                raise failure("SMP work gate checkpoint publication is inconsistent.")
            for logical in (0, 1):
                offset = GATE_HEADER_FIELDS + logical * GATE_CPU_FIELDS
                if (gate[offset + GATE_ENTRY["ENTERED"]] != 1
                        or gate[offset + GATE_ENTRY["READY"]] != 1
                        or gate[offset + GATE_ENTRY["DONE"]] != 0
                        or gate[offset + GATE_ENTRY["RESERVED"]] != 0):
                    raise failure(f"SMP work gate participant {logical} is not simultaneously ready.")
        else:
            if any(gate):
                raise failure("SMP work parallel checkpoint published an unexpected gate epoch.")
        checkpoint["gate"] = gate
        return checkpoint
    finally:
        qmp.call("cont")
