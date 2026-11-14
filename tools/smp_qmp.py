"""Independent stopped-CPU evidence for the Phase 9A SMP bootstrap.

The guest only reports its own conclusions on the serial line. This module
stops the whole VM and reads the production per-CPU publication, the loader
handshake, live vCPU registers, descriptor tables and translations directly.
A guest `true` is never accepted as evidence.
"""

import re
import struct
import time

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
# Phase 9C-1: statically placed Ring 3 Threads on the BSP and one AP. Every
# profile publishes two private roots and one shared barrier page.
USER_CASES = {
    "smp-user-2": 2,
    "smp-user-overlap": 2,
    "smp-user-isolation": 2,
    "smp-user-ap-fault": 2,
    "smp-user-completion": 2,
    "smp-user-invalid": 2,
}
CASES.update(USER_CASES)
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

# Phase 9C-1 per-CPU user-execution publication, independent of the kernel.
USER_BLOCK = {
    "MAGIC": 0, "LOGICAL": 1, "APIC_ID": 2, "PHASE": 3, "ENTRY_COUNT": 4,
    "EXIT_COUNT": 5, "KERNEL_CR3": 6, "USER_CR3": 7, "SPACE_ROOT": 8,
    "ENTRY_RIP": 9, "ENTRY_RSP": 10, "ENTRY_FLAGS": 11, "SAVED_RIP": 12,
    "SAVED_RSP": 13, "SAVED_FLAGS": 14, "SAVED_CS": 15, "SAVED_SS": 16,
    "STACK_BOTTOM": 17, "STACK_TOP": 18, "ARENA_BOTTOM": 19, "ARENA_TOP": 20,
    "TSS_BASE": 21, "TSS_RSP0": 22, "GDT_BASE": 23, "IDT_BASE": 24,
    "NODE_ID": 25, "THREAD_ID": 26, "THREAD_GEN": 27, "SPACE_ID": 28,
    "BARRIER_VA": 29, "BARRIER_PA": 30, "BARRIER_A": 31, "BARRIER_B": 32,
    "BARRIER_RELEASE": 33, "BARRIER_SURVIVOR": 34, "CODE_VA": 35,
    "CODE_PA": 36, "CONSTANTS_VA": 37, "CONSTANTS_PA": 38, "DATA_VA": 39,
    "DATA_PA": 40, "STACK_VA": 41, "STACK_PA": 42, "EXPECT_VECTOR": 43,
    "EXPECT_ERROR": 44, "EXPECT_RIP": 45, "EXPECT_RSP": 46, "EXPECT_CR2": 47,
    "EXPECT_AX": 48, "EXPECT_BX": 49, "EXPECT_R8": 50, "EXPECT_R9": 51,
    "EXIT_KIND": 52, "EXIT_CODE": 53, "EXIT_RIP": 54, "EXIT_RSP": 55,
    "EXIT_FLAGS": 56, "EXIT_CS": 57, "EXIT_SS": 58, "FAULT_VECTOR": 59,
    "FAULT_ERROR": 60, "FAULT_RIP": 61, "FAULT_CR2": 62, "FAULT_CS": 63,
    "FAULT_SS": 64, "FAULT_RSP": 65, "HANDLER_RSP": 66, "EXIT_CR3": 67,
    "RESTORED_CR3": 68, "RESTORED_GS": 69, "RESTORED_CR0": 70,
    "RESTORED_EFER": 71, "COMPLETE_COUNT": 72, "ACK_COUNT": 73,
    "COMPLETION": 74, "FAILURE": 75, "BASELINE_ALLOCATED": 76,
    "BASELINE_FREE": 77, "BASELINE_TABLES": 78, "FINAL_ALLOCATED": 79,
    "FINAL_FREE": 80, "FINAL_TABLES": 81, "DATA0": 82, "DATA1": 83,
    "DATA2": 84, "DATA3": 85, "DATA4": 86, "DATA5": 87, "DATA6": 88,
    "DATA7": 89, "DATA8": 90, "CHECKS": 91, "SESSION_BARRIER": 92,
    "ROLE": 93, "RESERVED_A": 94, "RESERVED_B": 95,
}
USER_MAGIC = 0x4E45_555F_5553_5231
USER_FIELDS = 96
USER_STRIDE = 768
USER_BARRIER_SURVIVOR = 0x4E45_555F_5553_5231
USER_STACK_ROUND_TRIP = 0x5A5A_A5A5
USER_PHASE = {"Prepared": 1, "Released": 2, "Entered": 3, "Exited": 4,
              "Completed": 5, "Acknowledged": 6, "Reaped": 7,
              "Cancelled": 8, "Failed": 9}
USER_EXIT_RETURN = 1
USER_EXIT_FAULT = 2
USER_CHECK = {
    "placement": 1 << 0, "identities": 1 << 1, "roots": 1 << 2, "barrier": 1 << 3,
    "exit": 1 << 4, "handler": 1 << 5, "result": 1 << 6, "overlap": 1 << 7,
    "complete": 1 << 8, "ack": 1 << 9, "reap": 1 << 10, "teardown": 1 << 11,
    "baseline": 1 << 12, "fault": 1 << 13, "peer": 1 << 14,
    "fatal_clear": 1 << 15, "disarm": 1 << 16, "isolation": 1 << 17,
    "invalid": 1 << 18, "profile": 1 << 19,
}
USER_CHECKS = {
    "smp-user-2": 0x9DF7F,
    "smp-user-overlap": 0x9DFFF,
    "smp-user-isolation": 0xBDF7F,
    "smp-user-ap-fault": 0x9FF7F,
    "smp-user-completion": 0x9DF7F,
    "smp-user-invalid": 0x40000,
}
USER_ISOLATION_BSP = 0x76
USER_ISOLATION_AP = 0x7E
USER_CS = 0x3B
USER_SS = 0x33
USER_VA = {"CODE": 0x400000, "CONSTANTS": 0x401000, "DATA": 0x402000,
           "GUARD": 0x403000, "STACK": 0x404000, "HOLE": 0x405000,
           "UNMAPPED": 0x406000, "BARRIER": 0x407000}
USER_RSP = 0x404FF8
USER_SEED = (0x4E45_5553_0001, 0x4E45_5553_0002)
USER_KIND = (0x51, 0x52)
KSTACK_BASE = 0xFFFF_C000_0000_0000
KSTACK_STRIDE = 4 * 4096
USER_FLAGS_FORBIDDEN = ((1 << 8) | (1 << 9) | (1 << 10) | (3 << 12)
                        | (1 << 14) | (1 << 17) | (1 << 18))
PTE_ADDRESS = 0x000F_FFFF_FFFF_F000
PTE_PRESENT = 1 << 0
PTE_RW = 1 << 1
PTE_USER = 1 << 2
PTE_HUGE = 1 << 7
PTE_NX = 1 << 63

# Phase 9C-1 independent root, descriptor and result expectations.
USER_LAYOUT = [USER_MAGIC, MAX_CPUS, USER_FIELDS, USER_STRIDE]
USER_GDT_LIMIT = 0x3F
USER_GDT_ENTRIES = 8
USER_TSS_LIMIT = 0x67
USER_TSS_FIELDS = 13
USER_IDT_LIMIT = 0xFFF
KERNEL_TABLES = 7
USER_PRIVATE_TABLES = 10
USER_KERNEL_PML4 = {288, 511}
USER_ROOT_PML4 = {0, 288, 511}
USER_CONTROL = {7, 8, 9, 10, 11, 12, 13, 14, 15, 16}
USER_CONTROL_B = {17, 18, 19, 20, 21, 22, 23, 24, 25, 26}
USER_CONTROL_BASE = (7, 17)
USER_BRIDGE = {"KERNEL_RSP": 0, "KERNEL_CR3": 1, "CR0": 2, "EFER": 3, "GS": 4,
               "RIP": 5, "RSP": 6, "FLAGS": 7, "USER_CR3": 8}
USER_BRIDGE_WORDS = 16
USER_BRIDGE_STRIDE = 128
USER_DATA_C3_INDEX = 0x100 // 8
USER_STACK_ROUND_TRIP_INDEX = 0xFF0 // 8
USER_STAGE_CHECKS = 0x3
USER_CR0_ENTRY_BITS = 0x0A
USER_PREPARED, USER_CONCURRENT, USER_RESULTS = "prepared", "concurrent", "results"
USER_LEAVES = {0: "CODE", 1: "CONSTANTS", 2: "DATA", 4: "STACK", 7: "BARRIER"}
USER_ABSENT = ("GUARD", "HOLE", "UNMAPPED")
USER_KSTACK_BOTTOMS = (0xFFFF_C000_0000_1000, 0xFFFF_C000_0000_5000)
USER_KSTACK_BYTES = 0x2000
USER_NODES = (0x1_0000_0001, 0x1_0000_0002)
USER_THREADS = (0x1_0000_0001, 0x1_0000_0002)
USER_SPACES = (0x101, 0x102)
USER_RESULT_CLEAR = (USER_CHECK["complete"] | USER_CHECK["ack"]
                     | USER_CHECK["reap"] | USER_CHECK["teardown"]
                     | USER_CHECK["baseline"] | USER_CHECK["fatal_clear"]
                     | USER_CHECK["disarm"] | USER_CHECK["profile"])
USER_CHECKPOINT_ATTEMPTS = 4000
USER_FAULT_CASES = ("smp-user-ap-fault",)


def user_fault(case, cpu):
    return case in USER_FAULT_CASES and cpu == 1


def user_site(symbols, name):
    """Translates a fixture label into its Ring 3 virtual address."""
    return USER_VA["CODE"] + symbols[name] - symbols["neu_smp_user_payload"]


def user_payload_range(symbols):
    """The Ring 3 virtual span of the copied fixture image."""
    start = USER_VA["CODE"]
    return start, start + symbols["neu_smp_user_payload_end"] - symbols["neu_smp_user_payload"]


def user_expected_exit(case, cpu, symbols):
    """Independent copy of every raw exit the fixture must produce."""
    if user_fault(case, cpu):
        return {"kind": USER_EXIT_FAULT, "code": 14, "rip": user_site(symbols, "neu_smp_user_fault_site"),
                "rsp": USER_RSP, "cr2": symbols["__text_start"], "ax": 1, "bx": USER_KIND[1],
                "r8": 1, "r9": 1, "vector": 14, "error": 7,
                "fault_rip": user_site(symbols, "neu_smp_user_fault_site"),
                "fault_cr2": symbols["__text_start"], "cs": USER_CS, "ss": USER_SS}
    return {"kind": USER_EXIT_RETURN, "code": 128, "rip": user_site(symbols, "neu_smp_user_done_resume"),
            "rsp": USER_RSP, "cr2": 0, "ax": USER_SEED[cpu] + USER_KIND[cpu], "bx": 3,
            "r8": USER_CS, "r9": USER_SS, "vector": 128, "error": 0, "fault_rip": 0,
            "fault_cr2": 0, "cs": USER_CS, "ss": USER_SS}



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


def observe(qmp, case, symbols, expected_root, failure, capture=None):
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
        "root": expected_root,
    }

    def require(condition, message):
        if not condition:
            raise failure(message)

    if case in REJECT:
        _reject(qmp, symbols, failure, require, count, fast, cpu_of_apic,
                blocks, released, observation)
        return observation
    if case in USER_CASES:
        return _user_final(qmp, case, symbols, failure, require, expected_root,
                           count, cpu_of_apic, first_cpu, blocks, released,
                           observation, capture)
    _positive(qmp, case, symbols, failure, require, expected_root, count,
              cpu_of_apic, first_cpu, blocks, released, observation)
    if case in WORK_CASES:
        _work_state(qmp, case, symbols, failure, require, count, first_cpu,
                    blocks, observation)
    return observation


def _positive(qmp, case, symbols, failure, require, root, count,
              cpu_of_apic, first_cpu, blocks, released, observation,
              isolation=(ISOLATION_BSP, ISOLATION_AP), extra_guards=(),
              gdt_limit=GDT_LIMIT):
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
    require(bsp[BLOCK["GDT_LIMIT"]] == gdt_limit
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
        require(block[BLOCK["GDT_LIMIT"]] == gdt_limit
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
            require(block[BLOCK["ISOLATION"]] == isolation[0],
                    "SMP BSP isolation claims are incomplete.")
            require(block[BLOCK["PARK_RSP"]] == 0,
                    "SMP BSP published an application-processor park stack.")
        else:
            require(block[BLOCK["LOADER_CR3"]] not in (0, root),
                    f"SMP AP{logical} did not record a distinct loader root.")
            loader_rsp = block[BLOCK["LOADER_RSP"]]
            require(loader_rsp != 0 and not start <= loader_rsp < end,
                    f"SMP AP{logical} did not abandon the loader-owned stack.")
            require(block[BLOCK["ISOLATION"]] == isolation[1],
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
    guards += list(extra_guards)
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


# ---------------------------------------------------------------------------
# Phase 9C-1: independent stopped-VM inspection of the two-CPU Ring 3 profile.
#
# The guest publishes its own conclusions on the serial line; none of them is
# accepted here. Every checkpoint stops all vCPUs and reads the production
# per-CPU blocks, the user-execution blocks, the bridge records, live registers
# and raw physical memory (including both private page tables).
# ---------------------------------------------------------------------------

USER_READY = 3
USER_READY_HISTORY = 0xF
USER_TSS_IOMAP = 104
USER_ARENA_BYTES = 0x4000
USER_KERNEL_PML4_ENTRIES = 2
USER_GATE_BYTES = 16
USER_GATE_VECTOR = 128
USER_AREA_MASK = ~0x60
USER_FRAME_BYTES = 4096
USER_RESULTS_CHECKS = {
    "smp-user-2": 0x407F,
    "smp-user-overlap": 0x40FF,
    "smp-user-isolation": 0x2407F,
    "smp-user-ap-fault": 0x607F,
    "smp-user-completion": 0x407F,
}
USER_PROFILE_NAMES = {
    "smp-user-2": "Base",
    "smp-user-overlap": "Overlap",
    "smp-user-isolation": "Isolation",
    "smp-user-ap-fault": "ApFault",
    "smp-user-completion": "Completion",
    "smp-user-invalid": "Invalid",
}
USER_GUARD_SYMBOLS = (
    "__stack_guard_low", "__stack_guard_high",
    "__df_guard_low", "__df_guard_high",
    "neu_smp_ap1_guard_low", "neu_smp_ap1_guard_mid", "neu_smp_ap1_guard_ist",
    "neu_smp_ap2_guard_low", "neu_smp_ap2_guard_mid", "neu_smp_ap2_guard_ist",
    "neu_smp_ap3_guard_low", "neu_smp_ap3_guard_mid", "neu_smp_ap3_guard_ist",
    "neu_smp_user_guard0_low", "neu_smp_user_guard0_high",
    "neu_smp_user_guard1_low", "neu_smp_user_guard1_high",
)
USER_FAULT_RECORD_BYTES = 192
USER_SPIN_LIMIT = 0x2000_0000


def _user_registers(qmp, cpu, failure):
    """Reads one stopped vCPU without assuming its privilege level."""
    text = _registers(qmp, cpu)
    cpl = re.search(r"CPL=(\d+)", text)
    cs = re.search(r"\bCS\s*=\s*([0-9a-fA-F]+)", text)
    ss = re.search(r"\bSS\s*=\s*([0-9a-fA-F]+)", text)
    gs = re.search(r"\bGS\s*=\s*[0-9a-fA-F]+\s+([0-9a-fA-F]+)", text)
    if not cpl or not cs or not ss or not gs:
        raise failure("SMP user live register observation is incomplete:\n" + text)
    return {
        "text": text, "cpl": int(cpl[1]), "cs": int(cs[1], 16),
        "ss": int(ss[1], 16), "gs": int(gs[1], 16),
        "rip": _number(text, r"\bRIP=([0-9a-fA-F]+)", "RIP", failure),
        "rsp": _number(text, r"\bRSP=([0-9a-fA-F]+)", "RSP", failure),
        "rfl": _number(text, r"\bRFL=([0-9a-fA-F]+)", "RFL", failure),
        "cr0": _number(text, r"\bCR0=([0-9a-fA-F]+)", "CR0", failure),
        "cr3": _number(text, r"\bCR3=([0-9a-fA-F]+)", "CR3", failure),
        "efer": _number(text, r"\bEFER=([0-9a-fA-F]+)", "EFER", failure),
        "hlt": "HLT=1" in text,
    }


class PhysicalMemory:
    """Reads raw physical pages through the HMP `xp` command."""

    def __init__(self, qmp, cpu, failure):
        self.qmp, self.cpu, self.failure = qmp, cpu, failure
        self.pages = {}

    def page(self, address):
        base = address & ~0xFFF
        if base not in self.pages:
            raw = _machine(self.qmp, self.cpu, f"xp /512gx {base:#x}")
            values = [int(value, 16)
                      for value in re.findall(r"0x[0-9a-fA-F]+", raw)]
            if len(values) != 512:
                raise self.failure("Incomplete SMP physical page observation: " + raw)
            self.pages[base] = values
        return self.pages[base]

    def word(self, address):
        return self.page(address)[(address & 0xFFF) // 8]

    def words(self, address, count):
        base = address & ~0x7
        offset = (base & 0xFFF) // 8
        values = []
        while count:
            page = self.page(base)
            chunk = min(count, 512 - offset)
            values.extend(page[offset:offset + chunk])
            count -= chunk
            base = (base & ~0xFFF) + 0x1000
            offset = 0
        return values

    def bytes(self, address, count):
        start = address & ~0x7
        values = self.words(start, (address - start + count + 7) // 8)
        raw = b"".join(value.to_bytes(8, "little") for value in values)
        offset = address - start
        return raw[offset:offset + count]

    def page_bytes(self, address):
        return self.bytes(address & ~0xFFF, 4096)


def _walk(memory, root, address, failure):
    table = root & PTE_ADDRESS
    chain = []
    for level in range(4):
        index = (address >> (39 - level * 9)) & 0x1FF
        raw = memory.word(table + index * 8)
        chain.append((table, index, raw))
        if raw & PTE_PRESENT == 0:
            return {"present": False, "chain": chain, "leaf": 0, "table": 0}
        if raw & PTE_HUGE:
            raise failure(f"SMP user oracle found an unexpected huge mapping at {address:#x}.")
        table = raw & PTE_ADDRESS
    return {"present": True, "chain": chain, "leaf": chain[3][2], "table": table}


def _leaf(memory, root, address, failure):
    walked = _walk(memory, root, address, failure)
    return walked["leaf"] if walked["present"] else None


def _user_layout_words(symbols):
    return [USER_MAGIC, MAX_CPUS, USER_FIELDS, USER_STRIDE]


def _image_regions(symbols):
    return [
        (symbols["__requests_start"], symbols["__requests_end"], True, False),
        (symbols["__text_start"], symbols["__text_end"], False, True),
        (symbols["__rodata_start"], symbols["__rodata_end"], False, False),
        (symbols["__data_start"], symbols["__kernel_end"], True, False),
    ]


def _image_expected(symbols, base_pa, require):
    base = symbols["__requests_start"]
    guards = {symbols[name] for name in USER_GUARD_SYMBOLS}
    regions = _image_regions(symbols)
    expected = {}
    for index in range(512):
        address = base + index * 0x1000
        leaf = None
        for start, end, write, execute in regions:
            if start <= address < end and address not in guards:
                leaf = ((base_pa + address - base) | PTE_PRESENT
                        | (PTE_RW if write else 0) | (0 if execute else PTE_NX))
                break
        expected[index] = leaf
    return expected


def _kernel_root(memory, symbols, root, require, failure, live=False):
    """Rebuilds the owned kernel root from raw physical tables."""
    require(root not in (None, 0) and root & 0xFFF == 0,
            "SMP user final observation has no page-aligned kernel root.")
    pml4 = memory.words(root, 512)
    require({index for index, value in enumerate(pml4) if value} == USER_KERNEL_PML4,
            "SMP user kernel root PML4 entries are not the documented pair.")
    image_pdpt = pml4[511] & PTE_ADDRESS
    win_pdpt = pml4[288] & PTE_ADDRESS
    require(pml4[511] & USER_AREA_MASK == image_pdpt | 3
            and pml4[288] & USER_AREA_MASK == win_pdpt | 3,
            "SMP user kernel root image/window entries are not supervisor tables.")
    image_pd = memory.word(image_pdpt + 510 * 8)
    require(image_pd != 0 and image_pd & PTE_ADDRESS != 0
            and image_pd & USER_AREA_MASK == (image_pd & PTE_ADDRESS) | 3,
            "SMP user kernel image PDPT entry is not the documented table.")
    image_pd = image_pd & PTE_ADDRESS
    image_pt = memory.word(image_pd)
    require(image_pt != 0 and image_pt & USER_AREA_MASK == (image_pt & PTE_ADDRESS) | 3,
            "SMP user kernel image PD entry is not the documented table.")
    image_pt = image_pt & PTE_ADDRESS
    win_pd = memory.word(win_pdpt)
    require(win_pd != 0 and win_pd & USER_AREA_MASK == (win_pd & PTE_ADDRESS) | 3,
            "SMP user kernel window PDPT entry is not the documented table.")
    win_pd = win_pd & PTE_ADDRESS
    control = memory.word(win_pd)
    require(control != 0 and control & USER_AREA_MASK == (control & PTE_ADDRESS) | 3,
            "SMP user kernel window PD entry is not the documented table.")
    control = control & PTE_ADDRESS
    for index in range(512):
        if index != 510:
            require(memory.word(image_pdpt + index * 8) == 0,
                    "SMP user kernel image PDPT has an unexpected entry.")
        if index != 0:
            require(memory.word(image_pd + index * 8) == 0
                    and memory.word(win_pdpt + index * 8) == 0
                    and memory.word(win_pd + index * 8) == 0,
                    "SMP user kernel image/window table has an unexpected entry.")
    tables = [root, image_pdpt, image_pd, image_pt, win_pdpt, win_pd, control]
    require(len(set(tables)) == KERNEL_TABLES,
            "SMP user kernel root does not own seven distinct tables.")
    control_words = memory.words(control, 512)
    for slot in range(KERNEL_TABLES):
        require(control_words[slot] & USER_AREA_MASK == tables[slot] | PTE_NX | 3,
                "SMP user kernel control alias is not the documented table.")
    aliases = {}
    for slot in range(KERNEL_TABLES, 512):
        entry = control_words[slot]
        if live and entry & PTE_PRESENT:
            address = entry & PTE_ADDRESS
            require(address != 0 and entry & USER_AREA_MASK == address | PTE_NX | 3
                    and address not in tables and address not in aliases.values(),
                    "SMP user kernel control alias is not a live supervisor table.")
            aliases[slot] = address
        else:
            require(entry == 0,
                    "SMP user kernel control table has an unexpected alias.")

    base = symbols["__requests_start"]
    require(base & 0xFFF == 0 and symbols["__kernel_end"] - base <= 0x200000,
            "SMP user oracle does not support this kernel image layout.")
    base_entry = memory.word(image_pt)
    require(base_entry & PTE_PRESENT and (base_entry & PTE_ADDRESS) != 0,
            "SMP user kernel image does not map its own base page.")
    base_pa = base_entry & PTE_ADDRESS
    expected = _image_expected(symbols, base_pa, require)
    for index in range(512):
        actual = memory.word(image_pt + index * 8) & USER_AREA_MASK
        want = expected[index] or 0
        require(actual == want,
                f"SMP user kernel image leaf {index} does not match the ELF-backed map.")
    require(_leaf(memory, root, base, failure) & USER_AREA_MASK
            == expected[0],
            "SMP user kernel image base page permission is wrong.")
    return {"root": root, "base": base_pa, "tables": tables,
            "image": expected, "control": control, "aliases": aliases}


def _fixture_bytes(memory, symbols, kernel, require, failure):
    payload = symbols["neu_smp_user_payload"]
    end = symbols["neu_smp_user_payload_end"]
    length = end - payload
    require(0 < length <= USER_FRAME_BYTES,
            "SMP user fixture bounds are not the documented ones.")
    address = kernel["base"] + payload - symbols["__requests_start"]
    leaf = _leaf(memory, kernel["root"], payload, failure)
    require(leaf is not None and leaf & PTE_ADDRESS == address & ~0xFFF,
            "SMP user fixture virtual address does not reach its image bytes.")
    return memory.bytes(address, length)


def _user_space(memory, block, cpu, kernel, symbols, barrier_pa, require, failure):
    """Rebuilds one private root and its effective user mappings."""
    root = block[USER_BLOCK["USER_CR3"]]
    require(block[USER_BLOCK["SPACE_ROOT"]] == root,
            f"SMP user CPU{cpu} space root and CR3 disagree.")
    require(root != 0 and root & 0xFFF == 0 and root != kernel["root"],
            f"SMP user CPU{cpu} private root is not a distinct owned table.")
    pml4 = memory.words(root, 512)
    require({index for index, value in enumerate(pml4) if value} == USER_ROOT_PML4,
            f"SMP user CPU{cpu} private root PML4 entries are not the documented triad.")

    def present_table(raw, what):
        require(raw & PTE_PRESENT and raw & PTE_HUGE == 0 and raw & PTE_ADDRESS,
                f"SMP user CPU{cpu} {what} is not a present table.")
        return raw & PTE_ADDRESS

    user_pdpt = present_table(pml4[0], "user PDPT")
    require(pml4[0] & USER_AREA_MASK == user_pdpt | 7,
            f"SMP user CPU{cpu} private root user entry is not user RW.")
    image_pdpt = present_table(pml4[511], "image PDPT")
    require(pml4[511] & USER_AREA_MASK == image_pdpt | 3,
            f"SMP user CPU{cpu} private root image entry is not a supervisor table.")
    win_pdpt = present_table(pml4[288], "window PDPT")
    require(pml4[288] & USER_AREA_MASK == win_pdpt | 3,
            f"SMP user CPU{cpu} private root window entry is not a supervisor table.")

    user_pdpt_words = memory.words(user_pdpt, 512)
    require(user_pdpt_words[0] & USER_AREA_MASK == (user_pdpt_words[0] & PTE_ADDRESS) | 7
            and user_pdpt_words[0] & PTE_PRESENT,
            f"SMP user CPU{cpu} user PDPT entry is not user RW.")
    user_pd = user_pdpt_words[0] & PTE_ADDRESS
    require(all(word == 0 for word in user_pdpt_words[1:]),
            f"SMP user CPU{cpu} user PDPT has an unexpected entry.")

    user_pd_words = memory.words(user_pd, 512)
    require(user_pd_words[2] & USER_AREA_MASK == (user_pd_words[2] & PTE_ADDRESS) | 7
            and user_pd_words[2] & PTE_PRESENT,
            f"SMP user CPU{cpu} user PD entry is not user RW.")
    user_pt = user_pd_words[2] & PTE_ADDRESS
    require(all(word == 0 for index, word in enumerate(user_pd_words) if index != 2),
            f"SMP user CPU{cpu} user PD has an unexpected entry.")

    frames = {
        "CODE": block[USER_BLOCK["CODE_PA"]],
        "CONSTANTS": block[USER_BLOCK["CONSTANTS_PA"]],
        "DATA": block[USER_BLOCK["DATA_PA"]],
        "STACK": block[USER_BLOCK["STACK_PA"]],
    }
    for name, address in frames.items():
        require(address & 0xFFF == 0 and address != 0,
                f"SMP user CPU{cpu} {name} frame is not a page-aligned physical frame.")
    require(len(set(frames.values())) == 4,
            f"SMP user CPU{cpu} user frames are not distinct.")
    leaf_expected = {
        0: frames["CODE"] | PTE_USER | PTE_PRESENT,
        1: frames["CONSTANTS"] | PTE_NX | PTE_USER | PTE_PRESENT,
        2: frames["DATA"] | PTE_NX | PTE_RW | PTE_USER | PTE_PRESENT,
        4: frames["STACK"] | PTE_NX | PTE_RW | PTE_USER | PTE_PRESENT,
        7: barrier_pa | PTE_NX | PTE_RW | PTE_USER | PTE_PRESENT,
    }
    user_pt_words = memory.words(user_pt, 512)
    for index in range(512):
        actual = user_pt_words[index] & USER_AREA_MASK
        want = leaf_expected.get(index, 0)
        require(actual == want,
                f"SMP user CPU{cpu} user leaf {index} is not the documented mapping.")
    require(block[USER_BLOCK["CODE_VA"]] == USER_VA["CODE"]
            and block[USER_BLOCK["CONSTANTS_VA"]] == USER_VA["CONSTANTS"]
            and block[USER_BLOCK["DATA_VA"]] == USER_VA["DATA"]
            and block[USER_BLOCK["STACK_VA"]] == USER_VA["STACK"],
            f"SMP user CPU{cpu} user virtual layout is not the documented one.")
    for name, address in (("CODE", USER_VA["CODE"]), ("CONSTANTS", USER_VA["CONSTANTS"]),
                          ("DATA", USER_VA["DATA"]), ("STACK", USER_VA["STACK"]),
                          ("BARRIER", USER_VA["BARRIER"])):
        walked = _walk(memory, root, address, failure)
        require(walked["present"], f"SMP user CPU{cpu} {name} is not translated.")
    require(_leaf(memory, root, USER_VA["GUARD"], failure) is None
            and _leaf(memory, root, USER_VA["HOLE"], failure) is None
            and _leaf(memory, root, USER_VA["UNMAPPED"], failure) is None,
            f"SMP user CPU{cpu} private root maps a guard or hole page.")

    image_pdpt_words = memory.words(image_pdpt, 512)
    require(image_pdpt_words[510] & USER_AREA_MASK == (image_pdpt_words[510] & PTE_ADDRESS) | 3
            and image_pdpt_words[510] & PTE_PRESENT,
            f"SMP user CPU{cpu} image PDPT entry is not the documented table.")
    image_pd = image_pdpt_words[510] & PTE_ADDRESS
    require(all(word == 0 for index, word in enumerate(image_pdpt_words) if index != 510),
            f"SMP user CPU{cpu} image PDPT has an unexpected entry.")
    image_pd_words = memory.words(image_pd, 512)
    require(image_pd_words[0] & USER_AREA_MASK == (image_pd_words[0] & PTE_ADDRESS) | 3
            and image_pd_words[0] & PTE_PRESENT,
            f"SMP user CPU{cpu} image PD entry is not the documented table.")
    image_pt = image_pd_words[0] & PTE_ADDRESS
    require(all(word == 0 for index, word in enumerate(image_pd_words) if index != 0),
            f"SMP user CPU{cpu} image PD has an unexpected entry.")
    image_words = memory.words(image_pt, 512)
    for index in range(512):
        actual = image_words[index] & USER_AREA_MASK
        want = kernel["image"][index] or 0
        require(actual == want,
                f"SMP user CPU{cpu} image leaf {index} does not match the kernel image map.")

    win_pdpt_words = memory.words(win_pdpt, 512)
    require(win_pdpt_words[0] & USER_AREA_MASK == (win_pdpt_words[0] & PTE_ADDRESS) | 3
            and win_pdpt_words[0] & PTE_PRESENT,
            f"SMP user CPU{cpu} window PDPT entry is not the documented table.")
    win_pd = win_pdpt_words[0] & PTE_ADDRESS
    require(all(word == 0 for index, word in enumerate(win_pdpt_words) if index != 0),
            f"SMP user CPU{cpu} window PDPT has an unexpected entry.")
    win_pd_words = memory.words(win_pd, 512)
    require(win_pd_words[0] & USER_AREA_MASK == (win_pd_words[0] & PTE_ADDRESS) | 3
            and win_pd_words[0] & PTE_PRESENT,
            f"SMP user CPU{cpu} window PD entry is not the documented table.")
    control = win_pd_words[0] & PTE_ADDRESS
    require(all(word == 0 for index, word in enumerate(win_pd_words) if index != 0),
            f"SMP user CPU{cpu} window PD has an unexpected entry.")
    tables = [root, image_pdpt, image_pd, image_pt, win_pdpt, win_pd, control,
              user_pdpt, user_pd, user_pt]
    require(len(set(tables)) == USER_PRIVATE_TABLES,
            f"SMP user CPU{cpu} private root does not own ten distinct tables.")
    control_words = memory.words(control, 512)
    base = USER_CONTROL_BASE[cpu]
    for offset, table in enumerate(tables):
        require(control_words[base + offset] & USER_AREA_MASK == table | PTE_NX | 3,
                f"SMP user CPU{cpu} private control alias {offset} is wrong.")
    for index in range(512):
        if not base <= index < base + USER_PRIVATE_TABLES:
            require(control_words[index] == 0,
                    f"SMP user CPU{cpu} private control table has an unexpected alias.")
    require(control_words[0] == 0,
            f"SMP user CPU{cpu} private control table keeps the temporary root alias.")
    require(block[USER_BLOCK["BARRIER_PA"]] == barrier_pa,
            f"SMP user CPU{cpu} published barrier physical page is wrong.")
    require(_leaf(memory, kernel["root"], symbols["neu_smp_user_barrier"], failure)
            & USER_AREA_MASK == barrier_pa | PTE_NX | PTE_RW | PTE_PRESENT,
            f"SMP user CPU{cpu} kernel barrier mapping is not the shared page.")
    return {"root": root, "tables": tables, "frames": frames,
            "frames_order": list(frames.values()), "control": control}


def _user_descriptor(block, cpu, case, symbols, kernel_root, cpu_blocks,
                     require, failure):
    expected = user_expected_exit(case, cpu, symbols)
    info = USER_BLOCK
    require(block[info["MAGIC"]] == USER_MAGIC,
            f"SMP user CPU{cpu} descriptor magic is missing.")
    require(block[info["LOGICAL"]] == cpu and block[info["ROLE"]] == cpu,
            f"SMP user CPU{cpu} descriptor identity or role is wrong.")
    require(block[info["APIC_ID"]] == cpu_blocks[cpu][BLOCK["APIC_ID"]],
            f"SMP user CPU{cpu} descriptor APIC identity disagrees with its CPU block.")
    require(block[info["NODE_ID"]] == USER_NODES[cpu]
            and block[info["THREAD_ID"]] == USER_THREADS[cpu]
            and block[info["THREAD_GEN"]] == 1
            and block[info["SPACE_ID"]] == USER_SPACES[cpu],
            f"SMP user CPU{cpu} Node/Thread/Space identities are not the documented ones.")
    require(block[info["KERNEL_CR3"]] == kernel_root,
            f"SMP user CPU{cpu} descriptor kernel root is not the owned root.")
    require(block[info["ENTRY_RIP"]] == USER_VA["CODE"]
            and block[info["ENTRY_FLAGS"]] == 2,
            f"SMP user CPU{cpu} entry descriptor is not the documented one.")
    require(block[info["SAVED_RIP"]] == USER_VA["CODE"]
            and block[info["SAVED_RSP"]] == USER_RSP
            and block[info["SAVED_FLAGS"]] == 2
            and block[info["SAVED_CS"]] == USER_CS
            and block[info["SAVED_SS"]] == USER_SS,
            f"SMP user CPU{cpu} saved context is not the documented Ring 3 frame.")
    require(block[info["STACK_BOTTOM"]] == USER_KSTACK_BOTTOMS[cpu]
            and block[info["STACK_TOP"]] == USER_KSTACK_BOTTOMS[cpu] + USER_KSTACK_BYTES,
            f"SMP user CPU{cpu} saved stack binding is not the model binding.")
    arena = symbols[f"neu_smp_user_arena{cpu}"]
    require(block[info["ARENA_BOTTOM"]] == arena
            and block[info["ARENA_TOP"]] == arena + USER_ARENA_BYTES,
            f"SMP user CPU{cpu} entry arena is not the image-reserved one.")
    require(block[info["TSS_RSP0"]] == arena + USER_ARENA_BYTES,
            f"SMP user CPU{cpu} published RSP0 is not its arena top.")
    require(block[info["TSS_BASE"]] == cpu_blocks[cpu][BLOCK["TSS_BASE"]]
            and block[info["GDT_BASE"]] == cpu_blocks[cpu][BLOCK["GDT_BASE"]]
            and block[info["IDT_BASE"]] == cpu_blocks[cpu][BLOCK["IDT_BASE"]],
            f"SMP user CPU{cpu} descriptor tables are not its CPU-local tables.")
    require(block[info["BARRIER_VA"]] == USER_VA["BARRIER"],
            f"SMP user CPU{cpu} barrier virtual address is not the documented one.")
    for name, field, want in (
            ("VECTOR", "EXPECT_VECTOR", expected["vector"]),
            ("ERROR", "EXPECT_ERROR", expected["error"]),
            ("RIP", "EXPECT_RIP", expected["rip"]),
            ("RSP", "EXPECT_RSP", expected["rsp"]),
            ("CR2", "EXPECT_CR2", expected["cr2"]),
            ("AX", "EXPECT_AX", expected["ax"]),
            ("BX", "EXPECT_BX", expected["bx"]),
            ("R8", "EXPECT_R8", expected["r8"]),
            ("R9", "EXPECT_R9", expected["r9"])):
        require(block[info[field]] == want,
                f"SMP user CPU{cpu} EXPECT_{name} is not the documented value.")
    require(block[info["FAILURE"]] == 0,
            f"SMP user CPU{cpu} recorded a profile failure.")
    require(block[info["RESERVED_A"]] == 0 and block[info["RESERVED_B"]] == 0,
            f"SMP user CPU{cpu} published a reserved descriptor word.")


def _user_tables(memory, kernel_root, cpu_block, cpu, user_block, symbols,
                 armed, rsp0, require, failure, descriptors=True):
    gdt = cpu_block[BLOCK["GDT_BASE"]]
    tss = cpu_block[BLOCK["TSS_BASE"]]
    idt = cpu_block[BLOCK["IDT_BASE"]]
    require(gdt != 0 and gdt & 7 == 0 and tss != 0 and tss & 7 == 0
            and idt != 0 and idt & 15 == 0,
            f"SMP user CPU{cpu} descriptor tables are misaligned.")
    def physical(table, alignment):
        leaf = _leaf(memory, kernel_root, table, failure)
        if leaf is None or leaf & PTE_PRESENT == 0:
            raise failure(f"SMP user CPU{cpu} descriptor table is not mapped in the kernel root.")
        address = (leaf & PTE_ADDRESS) | (table & 0xFFF)
        if address & alignment:
            raise failure(f"SMP user CPU{cpu} descriptor table translation is misaligned.")
        return address
    gdt_pa = physical(gdt, 7)
    tss_pa = physical(tss, 7)
    idt_pa = physical(idt, 15)
    require(cpu_block[BLOCK["GDT_LIMIT"]] == USER_GDT_LIMIT
            and cpu_block[BLOCK["TSS_LIMIT"]] == USER_TSS_LIMIT
            and cpu_block[BLOCK["IDT_LIMIT"]] == USER_IDT_LIMIT,
            f"SMP user CPU{cpu} descriptor limits are not the Phase 9C-1 profile.")
    entries = memory.words(gdt_pa, USER_GDT_ENTRIES)
    # The Accessed bit (access-byte bit 0) is hardware/software managed
    # bookkeeping; the documented descriptors ignore it on both sides.
    expected = [
        0,
        0x00AF_9A00_0000_FFFF,
        0x00CF_9200_0000_FFFF,
        103 | ((tss & 0xFF_FFFF) << 16) | (0x8B << 40) | (((tss >> 24) & 0xFF) << 56),
        tss >> 32,
        0,
        0x00CF_F200_0000_FFFF,
        0x00AF_FA00_0000_FFFF,
    ]
    accessed = 1 << 40
    observed = [entry & ~accessed if index in (1, 2, 6, 7) else entry
                for index, entry in enumerate(entries)]
    require(observed == expected,
            f"SMP user CPU{cpu} GDT does not publish the documented eight entries: "
            f"observed={[hex(entry) for entry in observed]} "
            f"expected={[hex(entry) for entry in expected]}.")
    tss_bytes = bytearray(USER_TSS_LIMIT + 1)
    tss_bytes[4:12] = rsp0.to_bytes(8, "little")
    tss_bytes[36:44] = symbols["__df_stack_top"].to_bytes(8, "little") \
        if cpu == 0 else cpu_block[BLOCK["IST_TOP"]].to_bytes(8, "little")
    tss_bytes[102:104] = USER_TSS_IOMAP.to_bytes(2, "little")
    require(memory.bytes(tss_pa, USER_TSS_LIMIT + 1) == bytes(tss_bytes),
            f"SMP user CPU{cpu} TSS is not the documented armed/disarmed image.")
    handler = symbols["neu_user_trap"] if armed else symbols["neu_isr_255"]
    attributes = 0xEE if armed else 0x8E
    gate = memory.bytes(idt_pa + USER_GATE_VECTOR * USER_GATE_BYTES, USER_GATE_BYTES)
    low = (handler & 0xFFFF) | (8 << 16) | (attributes << 40) | (((handler >> 16) & 0xFFFF) << 48)
    high = (handler >> 32) & 0xFFFF_FFFF
    require(int.from_bytes(gate[0:8], "little") == low
            and int.from_bytes(gate[8:16], "little") == high,
            f"SMP user CPU{cpu} vector 128 gate is not the documented one.")
    if descriptors:
        require(user_block[USER_BLOCK["TSS_BASE"]] == tss
                and user_block[USER_BLOCK["GDT_BASE"]] == gdt
                and user_block[USER_BLOCK["IDT_BASE"]] == idt,
                f"SMP user CPU{cpu} user block descriptor tables disagree with its CPU block.")


def _user_cpu_block(cpu_block, cpu, case, stage, kernel_root, symbols, require):
    require(cpu_block[BLOCK["MAGIC"]] == MAGIC
            and cpu_block[BLOCK["LOGICAL"]] == cpu
            and cpu_block[BLOCK["IS_BSP"]] == (1 if cpu == 0 else 0),
            f"SMP user CPU{cpu} kernel block identity is wrong.")
    require(cpu_block[BLOCK["STATE"]] == USER_READY
            and cpu_block[BLOCK["HISTORY"]] == USER_READY_HISTORY,
            f"SMP user CPU{cpu} is not in the ready bootstrap state.")
    require(cpu_block[BLOCK["GS_BASE"]] == symbols["neu_smp_cpus"] + cpu * BLOCK_STRIDE,
            f"SMP user CPU{cpu} kernel GS base is not its own block.")
    require(cpu_block[BLOCK["CR3"]] == kernel_root,
            f"SMP user CPU{cpu} published kernel root is wrong.")
    require(cpu_block[BLOCK["FAILURE"]] == 0 and cpu_block[BLOCK["FAULT_VECTOR"]] == 0
            and cpu_block[BLOCK["FAULT_ERROR"]] == 0 and cpu_block[BLOCK["FAULT_RIP"]] == 0,
            f"SMP user CPU{cpu} kernel block recorded a failure or fault.")
    require(cpu_block[BLOCK["PARK_COUNT"]] == 0,
            f"SMP user CPU{cpu} parked before the terminal state.")
    if stage == USER_PREPARED:
        isolation = (ISOLATION_BSP, ISOLATION_AP)
    else:
        isolation = (USER_ISOLATION_BSP, USER_ISOLATION_AP)
    require(cpu_block[BLOCK["ISOLATION"]] == isolation[cpu],
            f"SMP user CPU{cpu} isolation claims are wrong for the {stage} stage.")


def _verify_live_descriptor(live, cpu_block, expect_cpl, require, failure):
    text = live["text"]
    require(live["cpl"] == expect_cpl,
            f"SMP user live CPU is not at CPL{expect_cpl}.")
    if expect_cpl == 3:
        require(live["cs"] == USER_CS and live["ss"] == USER_SS,
                "SMP user live CPU is not on the Ring 3 selectors.")
        require(live["rfl"] & 2 and live["rfl"] & USER_FLAGS_FORBIDDEN == 0,
                "SMP user live CPU Ring 3 flags are invalid.")
        require(live["gs"] == 0, "SMP user live CPU retains a user GS base.")
    else:
        require(live["cs"] == 8 and "CS64" in text and "DPL=0" in text,
                "SMP user live CPU is not on the Ring 0 code segment.")
    gdt = re.search(r"\bGDT=\s*([0-9a-fA-F]+)\s+([0-9a-fA-F]+)", text)
    require(gdt and int(gdt[1], 16) == cpu_block[BLOCK["GDT_BASE"]]
            and int(gdt[2], 16) == cpu_block[BLOCK["GDT_LIMIT"]],
            "SMP user live GDTR disagrees with the CPU block.")
    idt = re.search(r"\bIDT=\s*([0-9a-fA-F]+)\s+([0-9a-fA-F]+)", text)
    require(idt and int(idt[1], 16) == cpu_block[BLOCK["IDT_BASE"]]
            and int(idt[2], 16) == cpu_block[BLOCK["IDT_LIMIT"]],
            "SMP user live IDTR disagrees with the CPU block.")
    tr = re.search(r"\bTR\s*=\s*([0-9a-fA-F]+)\s+([0-9a-fA-F]+)\s+"
                   r"([0-9a-fA-F]+)\s+([0-9a-fA-F]+)", text)
    require(tr and int(tr[1], 16) == 0x18
            and int(tr[2], 16) == cpu_block[BLOCK["TSS_BASE"]]
            and int(tr[3], 16) == cpu_block[BLOCK["TSS_LIMIT"]]
            and int(tr[4], 16) & 0xFF00 in (0x8900, 0x8B00)
            and ("TSS64-avl" in text or "TSS64-busy" in text),
            "SMP user live TR disagrees with the CPU block.")


def _virtual_words(memory, root, address, count, failure):
    """Reads kernel virtual memory through raw page tables, one leaf at a time."""
    values = []
    while count:
        leaf = _leaf(memory, root, address, failure)
        if leaf is None or leaf & PTE_PRESENT == 0:
            raise failure(f"SMP user kernel address {address:#x} is not mapped.")
        offset = address & 0xFFF
        chunk = min(count, (USER_FRAME_BYTES - offset) // 8)
        values.extend(memory.words((leaf & PTE_ADDRESS) + offset, chunk))
        address += chunk * 8
        count -= chunk
    return values


def _virtual_bytes(memory, root, address, count, failure):
    first = address & ~0xFFF
    words = _virtual_words(memory, root, first, (address - first + count + 7) // 8,
                           failure)
    raw = b"".join(word.to_bytes(8, "little") for word in words)
    offset = address - first
    return raw[offset:offset + count]


def _bridge(memory, cpu, symbols, kernel_root, failure):
    address = symbols["neu_smp_user_bridges"] + cpu * USER_BRIDGE_STRIDE
    return _virtual_words(memory, kernel_root, address, USER_BRIDGE_WORDS, failure)


def _check_bridge(bridge, cpu, stage, case, symbols, live, kernel_root,
                  user_root, cpu_block, require, failure):
    if stage == USER_PREPARED:
        require(all(word == 0 for word in bridge),
                f"SMP user CPU{cpu} bridge was used before release.")
        return
    expect = {
        USER_BRIDGE["KERNEL_CR3"]: kernel_root,
        USER_BRIDGE["CR0"]: cpu_block[BLOCK["CR0"]],
        USER_BRIDGE["EFER"]: cpu_block[BLOCK["EFER"]],
        USER_BRIDGE["GS"]: symbols["neu_smp_cpus"] + cpu * BLOCK_STRIDE,
        USER_BRIDGE["RIP"]: USER_VA["CODE"],
        USER_BRIDGE["RSP"]: USER_RSP,
        USER_BRIDGE["FLAGS"]: 2,
        USER_BRIDGE["USER_CR3"]: user_root,
    }
    for slot, want in expect.items():
        require(bridge[slot] == want,
                f"SMP user CPU{cpu} bridge slot {slot} is not the documented value.")
    require(all(word == 0 for word in bridge[9:]),
            f"SMP user CPU{cpu} bridge published reserved words.")
    if stage == USER_CONCURRENT:
        continue_rsp = bridge[USER_BRIDGE["KERNEL_RSP"]]
        bottom = cpu_block[BLOCK["STACK_BOTTOM"]]
        top = cpu_block[BLOCK["STACK_TOP"]]
        require(bottom <= continue_rsp < top,
                f"SMP user CPU{cpu} bridge continuation is not on its own kernel stack.")
    else:
        require(bridge[USER_BRIDGE["KERNEL_RSP"]] == 0,
                f"SMP user CPU{cpu} bridge was not cleared by its resume.")
    if stage == USER_CONCURRENT:
        require(live["cr0"] == bridge[USER_BRIDGE["CR0"]] | USER_CR0_ENTRY_BITS,
                f"SMP user CPU{cpu} live CR0 is not the entry CR0.")
        require(live["efer"] == bridge[USER_BRIDGE["EFER"]] & ~1,
                f"SMP user CPU{cpu} live EFER is not the kernel EFER without SCE.")
    else:
        require(live["cr0"] == bridge[USER_BRIDGE["CR0"]],
                f"SMP user CPU{cpu} live CR0 was not restored after Ring 3.")
        require(live["efer"] == bridge[USER_BRIDGE["EFER"]],
                f"SMP user CPU{cpu} live EFER was not restored after Ring 3.")


def _check_pages(memory, space, cpu, case, stage, symbols, kernel, barrier_pa,
                 fixture, data_words, require, failure):
    code = space["frames"]["CODE"]
    constants = space["frames"]["CONSTANTS"]
    data = space["frames"]["DATA"]
    stack = space["frames"]["STACK"]
    code_bytes = memory.page_bytes(code)
    require(code_bytes[:len(fixture)] == fixture
            and all(byte == 0 for byte in code_bytes[len(fixture):]),
            f"SMP user CPU{cpu} CODE frame is not the copied fixture image.")
    seed, kind = USER_SEED[cpu], USER_KIND[cpu]
    fault = user_fault(case, cpu)
    constants_words = memory.words(constants, 512)
    expected_constants = [seed, kind, cpu, 1 if fault else 0, symbols["__text_start"], 0]
    expected_constants += [0] * (512 - len(expected_constants))
    require(constants_words == expected_constants,
            f"SMP user CPU{cpu} CONSTANTS frame is not the published fixture inputs.")
    data_page = memory.words(data, 512)
    stack_page = memory.words(stack, 512)
    if stage == USER_PREPARED:
        expected_data = [0] * 512
        expected_data[USER_DATA_C3_INDEX] = 0xC3
        require(data_page == expected_data,
                f"SMP user CPU{cpu} DATA frame was published before release.")
        require(all(word == 0 for word in stack_page),
                f"SMP user CPU{cpu} STACK frame was published before release.")
        return
    if stage == USER_CONCURRENT:
        require(data_page[0] == seed + kind,
                f"SMP user CPU{cpu} DATA result is not the executed seed/kind sum.")
        require(data_page[1] == 3,
                f"SMP user CPU{cpu} did not publish the two-CPU stage-A mask.")
        require(data_page[2] == 0 and data_page[3] == 0 and data_page[4] == 0
                and data_page[5] == 0 and data_page[8] == 0,
                f"SMP user CPU{cpu} published stage-B evidence during stage A.")
        require(0 <= data_page[6] < USER_SPIN_LIMIT,
                f"SMP user CPU{cpu} stage-A spin count is out of bounds.")
        require(0 < data_page[7] < (1 << 63),
                f"SMP user CPU{cpu} entry TSC is not a user-observed timestamp.")
        require(data_page[USER_DATA_C3_INDEX] == 0xC3,
                f"SMP user CPU{cpu} DATA marker byte was lost.")
        for index in range(512):
            if index not in list(range(0, 9)) + [USER_DATA_C3_INDEX]:
                require(data_page[index] == 0,
                        f"SMP user CPU{cpu} DATA frame has an unexpected word.")
        require(all(word == 0 for word in stack_page),
                f"SMP user CPU{cpu} STACK frame was written during stage A.")
        return
    expected_data = [0] * 512
    expected_data[0] = seed + kind
    expected_data[1] = 3
    expected_data[2] = 3
    expected_data[3] = USER_BARRIER_SURVIVOR if cpu == 0 else 0
    expected_data[4] = 0 if cpu == 0 else 1
    expected_data[5] = 0 if fault else USER_STACK_ROUND_TRIP
    spins, entry, exit_tsc = data_page[6], data_page[7], data_page[8]
    require(0 <= spins < USER_SPIN_LIMIT,
            f"SMP user CPU{cpu} stage-A spin count is out of bounds.")
    require(entry > 0 and entry < (1 << 63),
            f"SMP user CPU{cpu} entry TSC is not a user-observed timestamp.")
    if fault:
        require(exit_tsc == 0,
                f"SMP user CPU{cpu} faulting AP published an exit timestamp.")
    else:
        require(exit_tsc > entry,
                f"SMP user CPU{cpu} exit TSC does not follow its entry TSC.")
    expected_data[6], expected_data[7], expected_data[8] = spins, entry, exit_tsc
    expected_data[USER_DATA_C3_INDEX] = 0xC3
    require(data_page == expected_data,
            f"SMP user CPU{cpu} DATA frame is not the final payload result.")
    expected_stack = [0] * 512
    expected_stack[USER_STACK_ROUND_TRIP_INDEX] = \
        0 if fault else USER_STACK_ROUND_TRIP
    require(stack_page == expected_stack,
            f"SMP user CPU{cpu} STACK frame is not the final round-trip image.")
    if data_words is not None:
        require(data_words == data_page[:len(data_words)],
                f"SMP user CPU{cpu} published DATA words disagree with raw memory.")


def _check_barrier(memory, barrier_pa, cpu, case, stage, require):
    words = memory.words(barrier_pa, 512)
    if stage == USER_PREPARED:
        require(all(word == 0 for word in words),
                "SMP user shared barrier page was written before release.")
        return
    if stage == USER_CONCURRENT:
        require(words[0] == 3,
                "SMP user concurrent checkpoint does not prove two stage-A arrivals.")
        require(words[1] in (0, 1, 3) and words[2] in (0, 1)
                and words[3] in (0, USER_BARRIER_SURVIVOR),
                "SMP user concurrent barrier content is not a user payload state.")
        require(all(word == 0 for word in words[4:]),
                "SMP user concurrent barrier page has an unexpected word.")
        return
    require(words[0] == 3 and words[1] == 3 and words[2] == 1
            and words[3] == USER_BARRIER_SURVIVOR,
            "SMP user final barrier words are not the two-CPU publication.")
    require(all(word == 0 for word in words[4:]),
            "SMP user final barrier page has an unexpected word.")


def _check_exit(block, cpu, case, symbols, space, barrier_pa, cpu_block, require):
    expected = user_expected_exit(case, cpu, symbols)
    info = USER_BLOCK
    require(block[info["ENTRY_COUNT"]] == 1 and block[info["EXIT_COUNT"]] == 1,
            f"SMP user CPU{cpu} did not enter and exit exactly once.")
    require(block[info["EXIT_KIND"]] == expected["kind"]
            and block[info["EXIT_CODE"]] == expected["code"],
            f"SMP user CPU{cpu} exit kind or code is wrong.")
    require(block[info["EXIT_RIP"]] == expected["rip"]
            and block[info["EXIT_RSP"]] == expected["rsp"],
            f"SMP user CPU{cpu} exit RIP or RSP is wrong.")
    require(block[info["EXIT_CS"]] == expected["cs"]
            and block[info["EXIT_SS"]] == expected["ss"],
            f"SMP user CPU{cpu} exit selectors are not Ring 3.")
    flags = block[info["EXIT_FLAGS"]]
    require(flags & 2 and flags & USER_FLAGS_FORBIDDEN == 0,
            f"SMP user CPU{cpu} exit flags are not the documented user flags.")
    require(block[info["EXIT_CR3"]] == space["root"],
            f"SMP user CPU{cpu} exit did not observe its private root.")
    arena = (block[info["ARENA_BOTTOM"]], block[info["ARENA_TOP"]])
    require(arena[0] <= block[info["HANDLER_RSP"]] < arena[1],
            f"SMP user CPU{cpu} exit handler did not use its own arena.")
    require(block[info["RESTORED_CR3"]] == block[info["KERNEL_CR3"]]
            and block[info["RESTORED_GS"]] == cpu_block[BLOCK["GS_BASE"]]
            and block[info["RESTORED_CR0"]] == cpu_block[BLOCK["CR0"]]
            and block[info["RESTORED_EFER"]] == cpu_block[BLOCK["EFER"]],
            f"SMP user CPU{cpu} resume record is inconsistent.")
    if expected["kind"] == USER_EXIT_FAULT:
        require(block[info["FAULT_VECTOR"]] == expected["vector"]
                and block[info["FAULT_ERROR"]] == expected["error"]
                and block[info["FAULT_RIP"]] == expected["fault_rip"]
                and block[info["FAULT_CR2"]] == expected["fault_cr2"]
                and block[info["FAULT_CS"]] == USER_CS
                and block[info["FAULT_SS"]] == USER_SS
                and block[info["FAULT_RSP"]] == USER_RSP,
                f"SMP user CPU{cpu} contained fault record is wrong.")
    else:
        for name in ("FAULT_VECTOR", "FAULT_ERROR", "FAULT_RIP", "FAULT_CR2",
                     "FAULT_CS", "FAULT_SS", "FAULT_RSP"):
            require(block[info[name]] == 0,
                    f"SMP user CPU{cpu} published a fault record without a fault.")
    require(block[info["BARRIER_A"]] == 3 and block[info["BARRIER_B"]] == 3
            and block[info["BARRIER_RELEASE"]] == 1
            and block[info["BARRIER_SURVIVOR"]] == USER_BARRIER_SURVIVOR
            and block[info["SESSION_BARRIER"]] == 3,
            f"SMP user CPU{cpu} copied barrier evidence is wrong.")


def _snapshot_user(qmp, case, symbols, kernel_root, failure):
    fast = qmp.call("query-cpus-fast")
    count = CPUS[case]
    if len(fast) != count:
        raise failure(f"SMP user checkpoint reports {len(fast)} CPUs instead of {count}.")
    cpu_of_apic = _apic_identities(qmp, fast, failure)
    first_cpu = fast[0]["cpu-index"]
    layout = _words(qmp, first_cpu, symbols["neu_smp_user_layout"], 4, failure)
    if layout != [USER_MAGIC, MAX_CPUS, USER_FIELDS, USER_STRIDE]:
        raise failure("SMP user publication layout is not the documented one.")
    base = symbols["neu_smp_user_blocks"]
    blocks = [_words(qmp, first_cpu, base + logical * USER_STRIDE, USER_FIELDS, failure)
              for logical in range(MAX_CPUS)]
    memory = PhysicalMemory(qmp, first_cpu, failure)
    bridges = [_bridge(memory, logical, symbols, kernel_root, failure)
               for logical in range(MAX_CPUS)]
    live = []
    for logical in range(2):
        cpu = cpu_of_apic.get(blocks[logical][USER_BLOCK["APIC_ID"]])
        if cpu is None:
            raise failure(f"SMP user CPU{logical} APIC identity is absent from the QMP CPU set.")
        live.append(_user_registers(qmp, cpu, failure))
    return {"qmp": qmp, "layout": layout, "blocks": blocks, "bridges": bridges,
            "live": live, "memory": memory, "first_cpu": first_cpu,
            "kernel_root": kernel_root, "cpu_of_apic": cpu_of_apic}


def _lost_user_stage(snapshot, stage):
    blocks = snapshot["blocks"]
    phases = [blocks[cpu][USER_BLOCK["PHASE"]] for cpu in (0, 1)]
    failures = [blocks[cpu][USER_BLOCK["FAILURE"]] for cpu in (0, 1)]
    if any(failures):
        return "a CPU recorded a profile failure"
    if stage == USER_PREPARED:
        if any(phase != USER_PHASE["Prepared"] for phase in phases):
            return "the prepared phase was left"
    elif stage == USER_CONCURRENT:
        if any(phase >= USER_PHASE["Exited"] for phase in phases):
            return f"a CPU reached its terminal exit (phases={phases})"
    else:
        if phases[0] >= USER_PHASE["Completed"]:
            return "the BSP left the results window"
    return None


def observe_user_checkpoint(qmp, case, symbols, stage, kernel_root, capture,
                            failure, deadline):
    """Stops every vCPU and captures one documented Ring 3 stage."""
    if case not in USER_CASES or case == "smp-user-invalid":
        raise failure("SMP user checkpoint requested for an unknown profile.")
    if stage not in (USER_PREPARED, USER_CONCURRENT, USER_RESULTS):
        raise failure("SMP user checkpoint stage is not documented.")
    for name in ("neu_smp_user_layout", "neu_smp_user_blocks", "neu_smp_user_bridges",
                 "neu_smp_user_payload", "neu_smp_user_payload_end",
                 "neu_smp_user_done_resume", "neu_smp_user_fault_site",
                 "neu_smp_user_arena0", "neu_smp_user_arena1",
                 "neu_smp_user_barrier", "neu_user_trap", "neu_isr_255",
                 "neu_smp_user_prepared_begin", "neu_smp_user_prepared_end",
                 "neu_smp_user_results_begin", "neu_smp_user_results_end",
                 "neu_smp_user_wait_begin", "neu_smp_user_wait_end",
                 "neu_smp_user_dwell0_begin", "neu_smp_user_dwell0_end",
                 "neu_smp_user_dwell1_begin", "neu_smp_user_dwell1_end"):
        if name not in symbols:
            raise failure(f"SMP user image is missing its published symbol {name}.")
    if qmp.call("query-status")["status"] != "running":
        raise failure("SMP user VM is not running before its checkpoint.")
    if stage == USER_CONCURRENT:
        if capture is None:
            raise failure("SMP user concurrency checkpoint has no prepared capture.")
        kernel_root = capture["kernel_root"]
    attempts = 0
    while True:
        qmp.call("stop")
        try:
            if qmp.call("query-status")["status"] != "paused":
                raise failure("SMP user checkpoint did not pause the whole VM.")
            snapshot = _snapshot_user(qmp, case, symbols, kernel_root, failure)
            lost = _lost_user_stage(snapshot, stage)
            if lost is not None:
                raise failure(f"SMP user {stage} checkpoint window was lost: {lost}.")
            if _user_stage_ready(snapshot, stage, symbols, failure):
                return _verify_user_stage(snapshot, case, symbols, stage,
                                          kernel_root, capture, failure)
        finally:
            qmp.call("cont")
        attempts += 1
        if attempts >= USER_CHECKPOINT_ATTEMPTS or time.monotonic() >= deadline:
            raise failure(f"SMP user {stage} checkpoint was not observed within its bound.")
        time.sleep(0.001)


def _user_stage_ready(snapshot, stage, symbols, failure):
    blocks = snapshot["blocks"]
    live = snapshot["live"]
    if stage == USER_PREPARED:
        if any(blocks[cpu][USER_BLOCK["PHASE"]] != USER_PHASE["Prepared"]
               for cpu in (0, 1)):
            return False
        if any(live[cpu]["cpl"] != 0 or live[cpu]["cs"] != 8 for cpu in (0, 1)):
            return False
        if not (symbols["neu_smp_user_prepared_begin"] <= live[0]["rip"]
                < symbols["neu_smp_user_prepared_end"]):
            return False
        return (symbols["neu_smp_user_wait_begin"] <= live[1]["rip"]
                < symbols["neu_smp_user_wait_end"])
    payload, end = user_payload_range(symbols)
    if stage == USER_CONCURRENT:
        if any(blocks[cpu][USER_BLOCK["PHASE"]] != USER_PHASE["Entered"]
               for cpu in (0, 1)):
            return False
        if any(live[cpu]["cpl"] != 3 or not payload <= live[cpu]["rip"] < end
               for cpu in (0, 1)):
            return False
        memory = snapshot["memory"]
        barrier = _leaf(memory, blocks[0][USER_BLOCK["KERNEL_CR3"]],
                        symbols["neu_smp_user_barrier"], failure)
        if barrier is None or barrier & PTE_ADDRESS == 0:
            return False
        return memory.word(barrier & PTE_ADDRESS) == 3
    if any(blocks[cpu][USER_BLOCK["PHASE"]] != USER_PHASE["Exited"]
           for cpu in (0, 1)):
        return False
    if any(live[cpu]["cpl"] != 0 or live[cpu]["cs"] != 8 for cpu in (0, 1)):
        return False
    if not (symbols["neu_smp_user_results_begin"] <= live[0]["rip"]
            < symbols["neu_smp_user_results_end"]):
        return False
    return (symbols["neu_smp_user_wait_begin"] <= live[1]["rip"]
            < symbols["neu_smp_user_wait_end"])


def _verify_user_stage(snapshot, case, symbols, stage, kernel_root, capture,
                       failure):
    """Rechecks one stopped stage against raw tables, memory and registers."""
    def require(condition, message):
        if not condition:
            raise failure(message)

    memory = snapshot["memory"]
    blocks = snapshot["blocks"]
    bridges = snapshot["bridges"]
    live = snapshot["live"]
    cpu_base = symbols["neu_smp_cpus"]
    cpu_blocks = [_words(snapshot["qmp"], snapshot["first_cpu"],
                         cpu_base + logical * BLOCK_STRIDE, BLOCK_FIELDS, failure)
                  for logical in range(2)]
    kernel = _kernel_root(memory, symbols, kernel_root, require, failure, live=True)
    barrier_leaf = _leaf(memory, kernel_root, symbols["neu_smp_user_barrier"], failure)
    require(barrier_leaf is not None, "SMP user barrier is not mapped in the kernel root.")
    barrier_pa = barrier_leaf & PTE_ADDRESS
    require(barrier_pa != 0 and barrier_pa & 0xFFF == 0,
            "SMP user barrier physical page is invalid.")
    fixture = _fixture_bytes(memory, symbols, kernel, require, failure)
    if capture is not None:
        require(capture["barrier"] == barrier_pa,
                "SMP user shared barrier changed between checkpoints.")
        require(capture["kernel_tables"] == kernel["tables"],
                "SMP user kernel root tables changed between checkpoints.")

    spaces = []
    for cpu in (0, 1):
        block = blocks[cpu]
        _user_descriptor(block, cpu, case, symbols, kernel_root, cpu_blocks,
                         require, failure)
        _user_cpu_block(cpu_blocks[cpu], cpu, case, stage, kernel_root,
                        symbols, require)
        space = _user_space(memory, block, cpu, kernel, symbols, barrier_pa,
                            require, failure)
        _user_tables(memory, kernel_root, cpu_blocks[cpu], cpu, block, symbols, True,
                     block[USER_BLOCK["ARENA_TOP"]], require, failure)
        _verify_live_descriptor(live[cpu], cpu_blocks[cpu],
                                3 if stage == USER_CONCURRENT else 0,
                                require, failure)
        _check_bridge(bridges[cpu], cpu, stage, case, symbols, live[cpu],
                      kernel_root, space["root"], cpu_blocks[cpu], require, failure)
        if stage in (USER_PREPARED, USER_RESULTS):
            stack = (cpu_blocks[cpu][BLOCK["STACK_BOTTOM"]],
                     cpu_blocks[cpu][BLOCK["STACK_TOP"]])
            require(stack[0] <= live[cpu]["rsp"] < stack[1],
                    f"SMP user CPU{cpu} kernel RSP is outside its own stack.")
        if stage == USER_CONCURRENT:
            low, high = user_payload_range(symbols)
            require(live[cpu]["cr3"] == space["root"] and live[cpu]["rsp"] == USER_RSP,
                    f"SMP user CPU{cpu} is not executing on its private user stack.")
            require(low <= live[cpu]["rip"] < high,
                    f"SMP user CPU{cpu} is not executing the copied fixture.")
        if stage == USER_RESULTS:
            require(block[USER_BLOCK["PHASE"]] == USER_PHASE["Exited"],
                    f"SMP user CPU{cpu} did not publish the Exited phase.")
            _check_exit(block, cpu, case, symbols, space, barrier_pa,
                        cpu_blocks[cpu], require)
            require(block[USER_BLOCK["COMPLETE_COUNT"]] == 0
                    and block[USER_BLOCK["ACK_COUNT"]] == 0,
                    f"SMP user CPU{cpu} completed inside the results window.")
            require(block[USER_BLOCK["CHECKS"]] == USER_RESULTS_CHECKS[case],
                    f"SMP user CPU{cpu} results checks mask is wrong.")
        if stage != USER_RESULTS:
            require(block[USER_BLOCK["EXIT_COUNT"]] == 0
                    and block[USER_BLOCK["COMPLETE_COUNT"]] == 0
                    and block[USER_BLOCK["ACK_COUNT"]] == 0
                    and block[USER_BLOCK["SESSION_BARRIER"]] == 0,
                    f"SMP user CPU{cpu} published terminal evidence too early.")
        if stage == USER_CONCURRENT:
            require(block[USER_BLOCK["ENTRY_COUNT"]] == 1,
                    f"SMP user CPU{cpu} did not publish its Ring 3 entry.")
        if stage == USER_PREPARED:
            require(block[USER_BLOCK["ENTRY_COUNT"]] == 0,
                    f"SMP user CPU{cpu} entered Ring 3 before release.")
        if stage in (USER_PREPARED, USER_CONCURRENT):
            require(block[USER_BLOCK["CHECKS"]] == USER_STAGE_CHECKS,
                    f"SMP user CPU{cpu} stage checks mask is not the prepared/concurrent mask.")
        if stage == USER_PREPARED:
            require(all(block[USER_BLOCK[f"DATA{index}"]] == 0 for index in range(9)),
                    f"SMP user CPU{cpu} published DATA before release.")
            require(block[USER_BLOCK["FINAL_ALLOCATED"]] == 0
                    and block[USER_BLOCK["FINAL_FREE"]] == 0
                    and block[USER_BLOCK["FINAL_TABLES"]] == 0,
                    f"SMP user CPU{cpu} published final statistics before teardown.")
        spaces.append(space)

    expected_aliases = {}
    for cpu in (0, 1):
        for offset, table in enumerate(spaces[cpu]["tables"]):
            expected_aliases[USER_CONTROL_BASE[cpu] + offset] = table
    require(kernel["aliases"] == expected_aliases,
            "SMP user kernel workspace aliases do not name the two private roots.")
    require(spaces[0]["root"] != spaces[1]["root"],
            "SMP user private roots are not distinct.")
    require(set(spaces[0]["tables"]).isdisjoint(spaces[1]["tables"]),
            "SMP user private roots share a page table.")
    for first in spaces[0]["frames_order"]:
        for second in spaces[1]["frames_order"]:
            require(first != second,
                    "SMP user private roots share a user frame.")
    for cpu in (0, 1):
        require(barrier_pa not in spaces[cpu]["tables"]
                and barrier_pa not in spaces[cpu]["frames_order"],
                f"SMP user CPU{cpu} reuses the shared barrier as private storage.")
    _check_barrier(memory, barrier_pa, 0, case, stage, require)
    for cpu in (0, 1):
        block = blocks[cpu]
        space = spaces[cpu]
        _check_pages(memory, space, cpu, case, stage, symbols, kernel,
                     barrier_pa, fixture, block[USER_BLOCK["DATA0"]:USER_BLOCK["DATA0"] + 9],
                     require, failure)
    return {
        "stage": stage, "kernel_root": kernel_root,
        "kernel_tables": kernel["tables"], "barrier": barrier_pa,
        "roots": [spaces[0]["root"], spaces[1]["root"]],
        "tables": [spaces[0]["tables"], spaces[1]["tables"]],
        "frames": [spaces[0]["frames_order"], spaces[1]["frames_order"]],
        "blocks": blocks, "bridges": bridges,
        "live": [{key: live[cpu][key] for key in
                  ("rip", "rsp", "cr3", "rfl", "cpl", "cs", "ss", "gs")}
                 for cpu in (0, 1)],
    }


def _user_positive_final(qmp, case, symbols, failure, require, first_cpu,
                         blocks, bridges, observation, capture):
    memory = PhysicalMemory(qmp, first_cpu, failure)
    kernel = _kernel_root(memory, symbols, capture["kernel_root"], require, failure)
    require(kernel["root"] == observation["root"],
            "SMP user final kernel root is not the activated owned root.")
    require(kernel["tables"] == capture["kernel_tables"],
            "SMP user final kernel tables are not the captured baseline.")
    barrier_pa = capture["barrier"]
    require(_leaf(memory, kernel["root"], symbols["neu_smp_user_barrier"], failure)
            & USER_AREA_MASK == barrier_pa | PTE_NX | PTE_RW | PTE_PRESENT,
            "SMP user final barrier mapping is not the shared page.")
    fixture = _fixture_bytes(memory, symbols, kernel, require, failure)
    cpu_base = symbols["neu_smp_cpus"]
    cpu_blocks = [_words(qmp, first_cpu, cpu_base + logical * BLOCK_STRIDE,
                         BLOCK_FIELDS, failure) for logical in range(2)]
    for cpu in (0, 1):
        block = blocks[cpu]
        info = USER_BLOCK
        require(block[info["PHASE"]] == USER_PHASE["Reaped"],
                f"SMP user CPU{cpu} did not publish the Reaped phase.")
        require(block[info["CHECKS"]] == USER_CHECKS[case],
                f"SMP user CPU{cpu} final checks mask is not the exact profile mask.")
        require(block[info["ENTRY_COUNT"]] == 1 and block[info["EXIT_COUNT"]] == 1
                and block[info["COMPLETE_COUNT"]] == 1 and block[info["ACK_COUNT"]] == 1
                and block[info["COMPLETION"]] == 1,
                f"SMP user CPU{cpu} did not record completion exactly once.")
        require(block[info["FAILURE"]] == 0,
                f"SMP user CPU{cpu} recorded a profile failure.")
        require(block[info["BASELINE_TABLES"]] == KERNEL_TABLES
                and block[info["FINAL_TABLES"]] == KERNEL_TABLES
                and block[info["FINAL_ALLOCATED"]] == block[info["BASELINE_ALLOCATED"]]
                and block[info["FINAL_FREE"]] == block[info["BASELINE_FREE"]],
                f"SMP user CPU{cpu} final allocator/table inventory is not the baseline.")
        require(block[info["SESSION_BARRIER"]] == 3,
                f"SMP user CPU{cpu} session barrier evidence is wrong.")
        space = {"root": capture["roots"][cpu],
                 "tables": capture["tables"][cpu],
                 "frames": {"CODE": capture["frames"][cpu][0],
                            "CONSTANTS": capture["frames"][cpu][1],
                            "DATA": capture["frames"][cpu][2],
                            "STACK": capture["frames"][cpu][3]},
                 "frames_order": capture["frames"][cpu]}
        _check_exit(block, cpu, case, symbols, space, barrier_pa,
                    cpu_blocks[cpu], require)
        _check_pages(memory, space, cpu, case, USER_RESULTS, symbols, kernel,
                     barrier_pa, fixture, block[info["DATA0"]:info["DATA0"] + 9],
                     require, failure)
        _user_tables(memory, kernel["root"], cpu_blocks[cpu], cpu, block, symbols, False,
                     symbols["__stack_top"] if cpu == 0
                     else cpu_blocks[cpu][BLOCK["STACK_TOP"]],
                     require, failure)
        bridge = bridges[cpu]
        require(bridge[USER_BRIDGE["KERNEL_RSP"]] == 0
                and bridge[USER_BRIDGE["KERNEL_CR3"]] == capture["kernel_root"]
                and bridge[USER_BRIDGE["CR0"]] == cpu_blocks[cpu][BLOCK["CR0"]]
                and bridge[USER_BRIDGE["EFER"]] == cpu_blocks[cpu][BLOCK["EFER"]]
                and bridge[USER_BRIDGE["GS"]] == cpu_base + cpu * BLOCK_STRIDE
                and bridge[USER_BRIDGE["RIP"]] == USER_VA["CODE"]
                and bridge[USER_BRIDGE["RSP"]] == USER_RSP
                and bridge[USER_BRIDGE["FLAGS"]] == 2
                and bridge[USER_BRIDGE["USER_CR3"]] == capture["roots"][cpu]
                and all(word == 0 for word in bridge[9:]),
                f"SMP user CPU{cpu} final bridge record is not the documented one.")
        require(cpu_blocks[cpu][BLOCK["ISOLATION"]] ==
                (USER_ISOLATION_BSP if cpu == 0 else USER_ISOLATION_AP),
                f"SMP user CPU{cpu} final isolation claims are not the user profile.")
        require(block[info["USER_CR3"]] == capture["roots"][cpu]
                and block[info["SPACE_ROOT"]] == capture["roots"][cpu],
                f"SMP user CPU{cpu} final descriptor lost its private root identity.")
    require(capture["roots"][0] != capture["roots"][1],
            "SMP user captured private roots are not distinct.")
    for table in capture["tables"][0] + capture["tables"][1]:
        require(all(word == 0 for word in memory.words(table, 512)),
                "SMP user destroyed private table page was not zeroed before release.")
    require(_virtual_bytes(memory, kernel["root"], symbols["NEU_LAST_FAULT"],
                           USER_FAULT_RECORD_BYTES, failure)
            == bytes(USER_FAULT_RECORD_BYTES),
            "SMP user containment reached the fatal fault record.")
    observation["user"] = {
        "case": case, "stage": "final", "kernel_root": kernel["root"],
        "roots": capture["roots"], "barrier": barrier_pa,
        "checks": USER_CHECKS[case], "baseline_tables": KERNEL_TABLES,
        "baseline_free": blocks[0][USER_BLOCK["BASELINE_FREE"]],
        "tables_zeroed": len(capture["tables"][0]) + len(capture["tables"][1]),
        "profile": USER_PROFILE_NAMES[case],
    }


def _user_invalid_final(qmp, case, symbols, failure, require, first_cpu,
                        blocks, bridges, observation):
    memory = PhysicalMemory(qmp, first_cpu, failure)
    root = observation["root"]
    kernel = _kernel_root(memory, symbols, root, require, failure)
    cpu_base = symbols["neu_smp_cpus"]
    cpu_blocks = [_words(qmp, first_cpu, cpu_base + logical * BLOCK_STRIDE,
                         BLOCK_FIELDS, failure) for logical in range(2)]
    published = {USER_BLOCK["MAGIC"], USER_BLOCK["LOGICAL"], USER_BLOCK["APIC_ID"],
                 USER_BLOCK["ROLE"], USER_BLOCK["BASELINE_ALLOCATED"],
                 USER_BLOCK["BASELINE_FREE"], USER_BLOCK["BASELINE_TABLES"],
                 USER_BLOCK["PHASE"], USER_BLOCK["CHECKS"],
                 USER_BLOCK["FINAL_ALLOCATED"], USER_BLOCK["FINAL_FREE"],
                 USER_BLOCK["FINAL_TABLES"]}
    for cpu in (0, 1):
        block = blocks[cpu]
        info = USER_BLOCK
        require(block[info["MAGIC"]] == USER_MAGIC
                and block[info["LOGICAL"]] == cpu
                and block[info["ROLE"]] == cpu
                and block[info["APIC_ID"]] == cpu_blocks[cpu][BLOCK["APIC_ID"]],
                f"SMP user CPU{cpu} invalid descriptor identity is wrong.")
        require(block[info["PHASE"]] == USER_PHASE["Cancelled"],
                f"SMP user CPU{cpu} invalid profile did not publish Cancelled.")
        require(block[info["CHECKS"]] == USER_CHECK["invalid"],
                f"SMP user CPU{cpu} invalid checks mask is wrong.")
        require(block[info["ENTRY_COUNT"]] == 0 and block[info["EXIT_COUNT"]] == 0
                and block[info["COMPLETE_COUNT"]] == 0 and block[info["ACK_COUNT"]] == 0,
                f"SMP user CPU{cpu} invalid profile entered Ring 3 or completed.")
        require(block[info["BASELINE_TABLES"]] == KERNEL_TABLES
                and block[info["FINAL_TABLES"]] == KERNEL_TABLES
                and block[info["FINAL_ALLOCATED"]] == block[info["BASELINE_ALLOCATED"]]
                and block[info["FINAL_FREE"]] == block[info["BASELINE_FREE"]],
                f"SMP user CPU{cpu} invalid teardown did not restore the baseline.")
        for index in range(USER_FIELDS):
            if index not in published:
                require(block[index] == 0,
                        f"SMP user CPU{cpu} invalid profile published field {index}.")
        require(all(word == 0 for word in bridges[cpu]),
                f"SMP user CPU{cpu} invalid profile used its Ring 3 bridge.")
        _user_tables(memory, kernel["root"], cpu_blocks[cpu], cpu, block, symbols, False,
                     symbols["__stack_top"] if cpu == 0
                     else cpu_blocks[cpu][BLOCK["STACK_TOP"]],
                     require, failure, descriptors=False)
    require(_virtual_bytes(memory, kernel["root"], symbols["NEU_LAST_FAULT"],
                           USER_FAULT_RECORD_BYTES, failure)
            == bytes(USER_FAULT_RECORD_BYTES),
            "SMP user invalid profile reached the fatal fault record.")
    observation["user"] = {
        "case": case, "stage": "invalid", "kernel_root": kernel["root"],
        "roots": [], "barrier": 0, "checks": USER_CHECK["invalid"],
        "baseline_tables": KERNEL_TABLES,
        "baseline_free": blocks[0][USER_BLOCK["BASELINE_FREE"]],
        "tables_zeroed": 0, "profile": USER_PROFILE_NAMES[case],
    }


def _user_final(qmp, case, symbols, failure, require, expected_root, count,
                cpu_of_apic, first_cpu, blocks, released, observation, capture):
    if case == "smp-user-invalid":
        _positive(qmp, case, symbols, failure, require, expected_root, count,
                  cpu_of_apic, first_cpu, blocks, released, observation,
                  gdt_limit=USER_GDT_LIMIT,
                  extra_guards=[symbols[name] for name in USER_GUARD_SYMBOLS])
        user_blocks = [_words(qmp, first_cpu,
                              symbols["neu_smp_user_blocks"] + logical * USER_STRIDE,
                              USER_FIELDS, failure) for logical in range(2)]
        memory = PhysicalMemory(qmp, first_cpu, failure)
        bridges = [_bridge(memory, logical, symbols, observation["root"], failure)
                   for logical in range(2)]
        _user_invalid_final(qmp, case, symbols, failure, require, first_cpu,
                            user_blocks, bridges, observation)
        return observation
    require(capture is not None,
            "SMP user positive final has no prepared stopped-VM capture.")
    _positive(qmp, case, symbols, failure, require, expected_root, count,
              cpu_of_apic, first_cpu, blocks, released, observation,
              isolation=(USER_ISOLATION_BSP, USER_ISOLATION_AP),
              gdt_limit=USER_GDT_LIMIT,
              extra_guards=[symbols[name] for name in USER_GUARD_SYMBOLS])
    memory = PhysicalMemory(qmp, first_cpu, failure)
    user_blocks = [_words(qmp, first_cpu,
                          symbols["neu_smp_user_blocks"] + logical * USER_STRIDE,
                          USER_FIELDS, failure) for logical in range(2)]
    bridges = [_bridge(memory, logical, symbols, capture["kernel_root"], failure)
               for logical in range(2)]
    require(capture["kernel_root"] == expected_root,
            "SMP user prepared capture does not belong to the activated root.")
    _user_positive_final(qmp, case, symbols, failure, require, first_cpu,
                         user_blocks, bridges, observation, capture)
    return observation
