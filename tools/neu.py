#!/usr/bin/env python3
"""Small stdlib-only build/run/test runner for Neu bootstrap and owned paging."""

import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import re
import shlex
import shutil
import socket
import struct
import subprocess
import sys
import tarfile
import tempfile
import time

ROOT = Path(__file__).resolve().parent.parent
BUILD = ROOT / "build"
TARGET = "x86_64-unknown-none"
FEATURES = {"boot": None, "panic": "test-panic", "missing-map": "test-missing-memory-map",
            "memory": "test-memory", "invalid-opcode": "test-invalid-opcode",
            "general-protection": "test-general-protection", "page-fault": "test-page-fault",
            "double-fault": "test-double-fault", "breakpoint": "test-breakpoint",
            "paging": "test-paging", "unmapped": "test-unmapped", "readonly": "test-readonly", "nx": "test-nx",
            "paging-bootstrap-failure": "test-paging-bootstrap-failure",
            "guard-low": "test-guard-low", "guard-high": "test-guard-high",
            "df-guard-low": "test-df-guard-low", "df-guard-high": "test-df-guard-high",
            "user": "test-user", "user-kernel": "test-user-kernel", "user-readonly": "test-user-readonly",
            "user-nx": "test-user-nx", "user-unmapped": "test-user-unmapped",
            "user-privileged": "test-user-privileged", "user-gate": "test-user-gate", "user-guard": "test-user-guard",
            "native": "test-native", "native-unknown": "test-native-unknown", "native-exit-invalid": "test-native-exit-invalid",
            "native-return-rip": "test-native-return-rip", "native-return-rsp": "test-native-return-rsp",
            "native-return-flags": "test-native-return-flags", "native-return-selectors": "test-native-return-selectors",
            "native-disabled": "test-native-disabled",
            "nodes": "test-nodes", "nodes-a-fault": "test-nodes-a-fault", "nodes-b-fault": "test-nodes-b-fault",
            "nodes-cross": "test-nodes-cross", "nodes-reclaim": "test-nodes-reclaim", "nodes-rollback": "test-nodes-rollback",
            "timer": "test-timer", "timer-xapic": "test-timer-xapic", "timer-repeat": "test-timer-repeat",
            "timer-spurious": "test-timer-spurious", "timer-unsupported": "test-timer-unsupported",
            "timer-unexpected": "test-timer-unexpected", "timer-software": "test-timer-software",
            "switch": "test-switch", "switch-a-fault": "test-switch-a-fault", "switch-b-fault": "test-switch-b-fault",
            "switch-cross": "test-switch-cross", "switch-stack-user": "test-switch-stack-user",
            "switch-stack-guard": "test-switch-stack-guard", "switch-invalid-context": "test-switch-invalid-context",
            "switch-rollback": "test-switch-rollback", "switch-stale": "test-switch-stale"}
USER_CASES = {name: index for index, name in enumerate(("user", "user-kernel", "user-readonly", "user-nx", "user-unmapped", "user-privileged", "user-gate", "user-guard"))}
NATIVE_CASES = {name: index for index, name in enumerate(("native", "native-unknown", "native-exit-invalid", "native-return-rip", "native-return-rsp", "native-return-flags", "native-return-selectors", "native-disabled"))}
NODE_CASES = {name:index for index,name in enumerate(("nodes","nodes-a-fault","nodes-b-fault","nodes-cross","nodes-reclaim","nodes-rollback"))}
TIMER_CASES = ("timer", "timer-xapic", "timer-repeat", "timer-spurious", "timer-unsupported", "timer-unexpected", "timer-software")
TIMER_POSITIVE = TIMER_CASES[:4]
SWITCH_CASES = {name:index for index,name in enumerate(("switch","switch-a-fault","switch-b-fault","switch-cross","switch-stack-user","switch-stack-guard","switch-invalid-context","switch-rollback","switch-stale"))}
USER_CODE, USER_RSP = 0x400000, 0x404ff8
TEST_ADDRESS = 0xffffa00000000000


def host_target():
    details = subprocess.check_output([str(require("rustc")), "-vV"], text=True, cwd=ROOT)
    return next(line.removeprefix("host: ") for line in details.splitlines() if line.startswith("host: "))


class Failure(Exception):
    pass


def require(program):
    path = shutil.which(program)
    if not path:
        raise Failure(f"Missing {program}; see docs/IMPLEMENTATION_STATUS.md for prerequisites.")
    return Path(path)


def command(arguments):
    arguments = [str(arg) for arg in arguments]
    print("+ " + shlex.join(arguments), flush=True)
    subprocess.run(arguments, cwd=ROOT, check=True)


def version(program):
    return subprocess.check_output([str(require(program)), "--version"], text=True, cwd=ROOT).splitlines()[0]


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def limine():
    lock = json.loads((ROOT / "boot/limine.lock.json").read_text())
    cache = ROOT / ".cache/limine"
    cache.mkdir(parents=True, exist_ok=True)
    archive = cache / f"limine-binary-{lock['version']}.tar.gz"
    if not archive.exists():
        pending = archive.with_suffix(".download")
        command([require("curl"), "--fail", "--silent", "--show-error", "--location",
                 "--retry", "2", "--connect-timeout", "10", "--max-time", "120",
                 "--output", pending, lock["url"]])
        if sha256(pending) != lock["sha256"]:
            raise Failure("Limine download checksum mismatch; refusing to use it.")
        pending.replace(archive)
    if sha256(archive) != lock["sha256"]:
        raise Failure(f"Limine archive checksum mismatch: {archive}")

    files = cache / "limine-binary"
    efi = files / "BOOTX64.EFI"
    if not efi.exists() or sha256(efi) != lock["efi_sha256"] or not all(
        (files / name).exists() for name in ("LICENSE", "3RDPARTY.md")
    ):
        files.mkdir(exist_ok=True)
        # Extract only named regular files, never archive paths or links.
        with tarfile.open(archive, "r:gz") as package:
            for name in ("BOOTX64.EFI", "LICENSE", "3RDPARTY.md"):
                member = package.getmember(f"limine-binary/{name}")
                if not member.isfile() or member.size > 8 * 1024 * 1024:
                    raise Failure(f"Unexpected Limine archive member: {name}")
                with package.extractfile(member) as source:
                    (files / name).write_bytes(source.read())
    if sha256(efi) != lock["efi_sha256"]:
        raise Failure("Limine EFI checksum mismatch after extraction.")
    return files, lock


def elf_info(path):
    """Check the emitted executable and read the halt symbols for QMP tests."""
    data = path.read_bytes()
    header = struct.unpack_from("<16sHHIQQQIHHHHHH", data)
    if data[:7] != b"\x7fELF\x02\x01\x01" or header[1:3] != (2, 62):
        raise Failure("Kernel must be a little-endian x86-64 ET_EXEC ELF binary.")
    if header[9] != 56 or header[11] != 64:
        raise Failure("Unexpected ELF header sizes.")
    loads = []
    for index in range(header[10]):
        item = struct.unpack_from("<IIQQQQQQ", data, header[5] + index * header[9])
        kind, flags, offset, address, _, size, memory_size, _ = item
        if kind == 1:
            if address < 0xffffffff80000000 or size > memory_size or offset + size > len(data) or flags & 3 == 3:
                raise Failure("Invalid or writable/executable kernel LOAD segment.")
            loads.append(item)
    if not loads or not any(item[3] <= header[4] < item[3] + item[6] and item[1] & 1 for item in loads):
        raise Failure("ELF entry does not belong to an executable LOAD segment.")

    sections = [struct.unpack_from("<IIQQQQIIQQ", data, header[6] + i * header[11])
                for i in range(header[12])]
    symbols = {}
    for section in sections:
        if section[1] != 2:  # SHT_SYMTAB
            continue
        names_section = sections[section[6]]
        names = data[names_section[4]:names_section[4] + names_section[5]]
        for offset in range(section[4], section[4] + section[5], section[9]):
            name, _, _, _, value, _ = struct.unpack_from("<IBBHQQ", data, offset)
            label = names[name:names.find(b"\0", name)].decode("ascii")
            if label.startswith(("neu_test_", "neu_user_", "__user_kernel_", "neu_native_", "__native_", "neu_abi_", "neu_node_", "neu_irq_", "neu_timer_", "neu_switch_", "neu_context_")) or label in ("neu_halt_begin", "neu_halt_end", "__stack_bottom", "__stack_top",
                    "__df_stack_bottom", "__df_stack_top", "__kernel_start", "__kernel_end", "NEU_PF_PROBE",
                    "__stack_guard_low", "__stack_guard_high", "__df_guard_low", "__df_guard_high"):
                symbols[label] = value
    if symbols.get("__stack_top", 0) - symbols.get("__stack_bottom", 0) != 65536:
        raise Failure("Kernel ELF is missing its 64 KiB stack.")
    if not symbols.get("neu_halt_begin", 0) < symbols.get("neu_halt_end", 0):
        raise Failure("Kernel ELF is missing halt-loop symbols.")
    if symbols.get("__df_stack_top", 0) - symbols.get("__df_stack_bottom", 0) != 32768:
        raise Failure("Kernel ELF is missing its 32 KiB double-fault stack.")
    for name in ("__stack_guard_low", "__stack_guard_high", "__df_guard_low", "__df_guard_high"):
        if not symbols.get(name) or symbols[name] & 4095:
            raise Failure("Kernel ELF is missing a page-aligned stack guard.")
    if "__user_kernel_top" in symbols:
        if symbols["__user_kernel_top"] - symbols["__user_kernel_bottom"] != 32768 or any(symbols[n] & 4095 for n in ("__user_kernel_bottom", "__user_kernel_top", "__user_kernel_guard_low", "__user_kernel_guard_high")):
            raise Failure("Invalid user-transition kernel stack/guards.")
        prefix = "neu_native" if "neu_native_payload" in symbols else "neu_user"
        if not 0 < symbols[prefix + "_payload_end"] - symbols[prefix + "_payload"] <= 4096:
            raise Failure("Invalid embedded user fixture bounds.")
        if prefix == "neu_native":
            if symbols["__native_emergency_top"] - symbols["__native_emergency_bottom"] != 16384:
                raise Failure("Invalid native emergency IST stack.")
            if any(symbols[n] & 4095 for n in ("__native_guard_low", "__native_guard_high")):
                raise Failure("Invalid native emergency guards.")
            for label in ("neu_native_syscall_site", "neu_native_bad_rsp_site"):
                address = symbols[label]
                segment = next((item for item in loads if item[3] <= address < item[3] + item[5]), None)
                if segment is None or data[segment[2] + address - segment[3]:segment[2] + address - segment[3] + 2] != b"\x0f\x05":
                    raise Failure("Native fixture site is not an actual SYSCALL instruction.")
    if "neu_switch_payload" in symbols:
        if not 0<symbols["neu_switch_payload_end"]-symbols["neu_switch_payload"]<=4096:
            raise Failure("Cooperative payload exceeds its owned RX page.")
        for name,opcode in (("neu_switch_syscall_site",b"\x0f\x05"),("neu_context_iret",b"\x48\xcf")):
            address=symbols[name]
            segment=next((item for item in loads if item[3]<=address<item[3]+item[5]),None)
            if segment is None or data[segment[2]+address-segment[3]:segment[2]+address-segment[3]+len(opcode)]!=opcode:
                raise Failure("Cooperative transition is missing its actual architectural instruction.")
    return {"entry": header[4], "symbols": symbols, "load_segments": len(loads)}


def build(case="boot"):
    loader, lock = limine()
    arguments = [require("cargo"), "build", "--locked", "--release", "--target", TARGET,
                 "--target-dir", ROOT / "target", "--package", "neu-kernel", "--no-default-features"]
    if FEATURES[case]:
        arguments += ["--features", FEATURES[case]]
    command(arguments)
    binary = ROOT / "target" / TARGET / "release/neu-kernel"
    info = elf_info(binary)
    destination = BUILD / case
    esp = destination / "esp"
    (esp / "EFI/BOOT").mkdir(parents=True, exist_ok=True)
    (esp / "boot").mkdir(exist_ok=True)
    shutil.copyfile(loader / "BOOTX64.EFI", esp / "EFI/BOOT/BOOTX64.EFI")
    shutil.copyfile(ROOT / "boot/limine.conf", esp / "EFI/BOOT/limine.conf")
    shutil.copyfile(binary, esp / "boot/neu-kernel")
    for name in ("LICENSE", "3RDPARTY.md"):
        shutil.copyfile(loader / name, destination / f"LIMINE-{name}")
    manifest = {"case": case, "target": TARGET, "rustc": version("rustc"), "cargo": version("cargo"),
                "limine": lock, "kernel_sha256": sha256(binary), **info}
    (destination / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print(f"Built {esp.relative_to(ROOT)} (ELF64/x86-64, {info['load_segments']} LOAD segments)", flush=True)
    return esp


def firmware():
    code, variables = os.environ.get("OVMF_CODE"), os.environ.get("OVMF_VARS")
    if code or variables:
        if not code or not variables:
            raise Failure("Set OVMF_CODE and OVMF_VARS together to a compatible x86-64 pair.")
        pair = Path(code), Path(variables)
        if not all(path.is_file() for path in pair):
            raise Failure("OVMF_CODE or OVMF_VARS does not name a file.")
        return pair
    qemu = require("qemu-system-x86_64").resolve()
    directories = [qemu.parent.parent / "share/qemu", Path("/opt/homebrew/share/qemu"),
                   Path("/usr/local/share/qemu"), Path("/usr/share/OVMF"),
                   Path("/usr/share/edk2/ovmf"), Path("/usr/share/edk2/x64")]
    for directory in directories:
        for code_name, vars_name in (("edk2-x86_64-code.fd", "edk2-i386-vars.fd"),
                                     ("OVMF_CODE_4M.fd", "OVMF_VARS_4M.fd"),
                                     ("OVMF_CODE.fd", "OVMF_VARS.fd")):
            pair = directory / code_name, directory / vars_name
            if all(path.is_file() for path in pair):
                return pair
    raise Failure("No x86-64 OVMF pair found. Set OVMF_CODE and OVMF_VARS; see implementation documentation.")


def qemu_command(esp, temporary, serial, qmp=None):
    code, template = firmware()
    variables = temporary / "vars.fd"
    shutil.copyfile(template, variables)  # Each boot gets disposable NVRAM.
    runtime_esp = temporary / "esp"
    shutil.copytree(esp, runtime_esp)  # Freeze files while vvfat is serving them.
    arguments = [str(require("qemu-system-x86_64")), "-machine", "q35", "-accel", "tcg",
                 "-cpu", "max", "-smp", "1", "-m", "512M", "-display", "none",
                 "-monitor", "none", "-serial", serial, "-nic", "none", "-boot", "strict=on",
                 "-no-reboot", "-no-shutdown",
                 "-drive", f"if=pflash,unit=0,format=raw,readonly=on,file={code}",
                 "-drive", f"if=pflash,unit=1,format=raw,file={variables}",
                 "-blockdev", json.dumps({"driver": "vvfat", "node-name": "neu-esp",
                                           "dir": str(runtime_esp), "read-only": True}),
                 "-device", "virtio-blk-pci,drive=neu-esp,bootindex=1"]
    if qmp:
        arguments += ["-qmp", f"unix:{qmp},server=on,wait=off"]
    return arguments


class QMP:
    def __init__(self, path, deadline):
        self.deadline = deadline
        self.socket = socket.socket(socket.AF_UNIX)
        self.socket.settimeout(1)
        self.socket.connect(str(path))
        self.stream = self.socket.makefile("rwb")
        if "QMP" not in self.receive():
            raise Failure("Missing QMP greeting.")
        self.sequence = 0
        self.call("qmp_capabilities")

    def receive(self):
        remaining = self.deadline - time.monotonic()
        if remaining <= 0:
            raise Failure("QMP guest deadline exceeded.")
        self.socket.settimeout(min(1, remaining))
        line = self.stream.readline(1024 * 1024)
        if not line or not line.endswith(b"\n"):
            raise Failure("QMP closed or returned an oversized response.")
        return json.loads(line)

    def call(self, execute, arguments=None):
        self.sequence += 1
        payload = {"execute": execute, "id": self.sequence}
        if arguments:
            payload["arguments"] = arguments
        self.stream.write(json.dumps(payload).encode() + b"\n")
        self.stream.flush()
        while True:
            response = self.receive()
            if response.get("id") != self.sequence:
                continue  # Asynchronous QMP events are not command replies.
            if "error" in response:
                raise Failure(f"QMP {execute}: {response['error']}")
            return response["return"]

    def close(self):
        self.stream.close()
        self.socket.close()


COMMON = ["[NEU] Neu Kernel", "[NEU] Architecture: x86_64", "[NEU] Early serial initialized",
          "[NEU] CPU and kernel stack validated", "[NEU] Kernel GDT/TSS/IDT initialized; double-fault IST ready"]
PROTOCOL = ["[NEU] Boot protocol initialized: Limine base 6, UEFI64", "[NEU] Memory map validated:"]
OWNED = ["[NEU] Boot memory map copied:", "[NEU] Physical frame allocator initialized:"]
PAGING = ["[NEU] Kernel paging activated:", "[NEU] Kernel mappings verified: RX text, R rodata, RW/NX data, 4 guards"]
EXPECT = {
    "boot": (COMMON + PROTOCOL + OWNED + PAGING + ["[NEU] Kernel bootstrap successful", "[NEU] Idle: interrupts disabled; halting"],
             ["[NEU] PANIC:", "[NEU] BOOT FAILURE:", "[NEU] EXCEPTION:"]),
    "panic": (COMMON + PROTOCOL + OWNED + PAGING + ["[NEU] PANIC:", "controlled Phase 0 test panic"],
              ["[NEU] Kernel bootstrap successful", "[NEU] BOOT FAILURE:", "[NEU] EXCEPTION:"]),
    "missing-map": (COMMON + ["[NEU] BOOT FAILURE: required Limine memory-map response is missing"],
                    ["[NEU] Kernel bootstrap successful", "[NEU] Boot protocol initialized", "[NEU] PANIC:"]),
}
EXPECT["memory"] = (COMMON + PROTOCOL + OWNED + ["[NEU] Memory self-test passed:",
    *PAGING, "[NEU] Kernel bootstrap successful", "[NEU] Idle: interrupts disabled; halting"], EXPECT["boot"][1])
EXPECT["paging"] = (COMMON + PROTOCOL + OWNED + PAGING + ["[NEU] Paging self-test passed:",
    "[NEU] Kernel bootstrap successful", "[NEU] Idle: interrupts disabled; halting"], EXPECT["boot"][1])
EXPECT["paging-bootstrap-failure"] = (COMMON + PROTOCOL + OWNED + ["[NEU] Paging bootstrap rollback verified:"],
    ["[NEU] Kernel paging activated:", "[NEU] Kernel bootstrap successful", "[NEU] BOOT FAILURE:", "[NEU] PANIC:", "[NEU] EXCEPTION:"])
FAULTS = {"invalid-opcode": (6, 0, "neu_test_ud_site"), "general-protection": (13, 0x28, "neu_test_gp_site"),
          "page-fault": (14, 3, "neu_test_pf_site"), "double-fault": (8, 0, None), "breakpoint": (3, 0, "neu_test_bp_resume"),
          "unmapped": (14, 0, "neu_test_unmapped_site"), "readonly": (14, 3, "neu_test_readonly_site"), "nx": (14, 17, TEST_ADDRESS),
          "guard-low": (14, 0, "neu_test_guard_site"), "guard-high": (14, 0, "neu_test_guard_site"),
          "df-guard-low": (14, 0, "neu_test_guard_site"), "df-guard-high": (14, 0, "neu_test_guard_site")}
PAGING_FAULTS = {"unmapped": (0, TEST_ADDRESS), "readonly": (1, TEST_ADDRESS), "nx": (2, TEST_ADDRESS),
    "guard-low": (3, "__stack_guard_low"), "guard-high": (4, "__stack_guard_high"),
    "df-guard-low": (5, "__df_guard_low"), "df-guard-high": (6, "__df_guard_high")}
for name, (vector, _, _) in FAULTS.items():
    trigger = ["[NEU] Test: read-only mapping verified for #PF"] if name == "page-fault" else (
        ["[NEU] Test: forcing #DF via #GP delivery failure"] if name == "double-fault" else [])
    if name in PAGING_FAULTS:
        trigger = [f"[NEU] Paging fault prepared: kind={PAGING_FAULTS[name][0]} "]
    EXPECT[name] = (COMMON + PROTOCOL + OWNED + PAGING + trigger + [f"[NEU] EXCEPTION: vector={vector} ",
        "[NEU] EXCEPTION: fatal privileged fault; halting"], ["[NEU] Kernel bootstrap successful",
        "[NEU] PANIC:", "[NEU] BOOT FAILURE:", "nested fatal fault"])



for name, kind in USER_CASES.items():
    trigger = ["[NEU] User construction rollback verified:"] if kind == 0 else []
    EXPECT[name] = (COMMON + PROTOCOL + OWNED + PAGING + trigger + [f"[NEU] User space ready: kind={kind} ",
        "[NEU] User boundary:", "[NEU] User boundary verified: true", "[NEU] User teardown verified:",
        f"[NEU] User execution test passed: kind={kind} result=42", "[NEU] Kernel bootstrap successful",
        "[NEU] Idle: interrupts disabled; halting"], EXPECT["boot"][1] + ["User boundary verified: false"])


for name, kind in NATIVE_CASES.items():
    EXPECT[name] = (COMMON + PROTOCOL + OWNED + PAGING + ["[NEU] Native MSRs verified:",
        f"[NEU] Native user space ready: kind={kind} ", "[NEU] Native terminal:",
        "[NEU] Native teardown verified:", f"[NEU] Native ABI test passed: kind={kind}",
        "[NEU] Kernel bootstrap successful", "[NEU] Idle: interrupts disabled; halting"],
        EXPECT["boot"][1] + ["verified=false", "User execution test passed:"])


def user_ready(text, case):
    match = re.search(r"User space ready: kind=(\d+) root=(0x[0-9a-f]+) kernel=(0x[0-9a-f]+) entry=(0x[0-9a-f]+) rsp=(0x[0-9a-f]+)", text)
    if not match:
        raise Failure("Missing user root/entry contract.")
    kind, root, kernel, entry, rsp = [int(v, 0) for v in match.groups()]
    if kind != USER_CASES[case] or root == kernel or entry != USER_CODE or rsp != USER_RSP:
        raise Failure("Invalid user address-space identity or entry contract.")
    return {"kind": kind, "root": root, "kernel_root": kernel, "entry": entry, "rsp": rsp}


def observe_user(qmp, text_path, case, symbols, deadline):
    ready = user_ready(transcript(text_path), case)
    while time.monotonic() < deadline:
        registers = qmp.call("human-monitor-command", {"command-line": "info registers"})
        if "CPL=3" in registers:
            def reg(name):
                match = re.search(r"\b" + name + r"\s*=([0-9a-fA-F]+)", registers)
                if not match:
                    raise Failure("Missing user CPU register: " + name)
                return int(match[1], 16)
            length = symbols["neu_user_payload_end"] - symbols["neu_user_payload"]
            if (reg("CR3") != ready["root"] or reg("CS") != 0x3b or reg("SS") != 0x33
                    or reg("RSP") != USER_RSP or not USER_CODE <= reg("RIP") < USER_CODE + length
                    or reg("RFL") & ((1 << 8) | (1 << 9) | (1 << 10) | (3 << 12) | (1 << 14) | (1 << 18))
                    or reg("CR0") & 10 != 10 or reg("EFER") & 1 or "HLT=0" not in registers
                    or not re.search(r"FS =0000 0000000000000000", registers)
                    or not re.search(r"GS =0000 0000000000000000", registers)):
                raise Failure("Actual QMP user CPU state violates the entry contract.")
            ready["qmp_user_observation"] = registers
            return ready
        if "User boundary:" in transcript(text_path):
            raise Failure("Guest completed without a direct QMP CPL=3 observation.")
        time.sleep(0.01)
    raise Failure("Timed out observing actual CPL=3 execution.")


def check_user(case, text, symbols, observed):
    if not observed:
        raise Failure("Missing hardware-observed Ring 3 evidence.")
    boundary = re.search(r"User boundary: kind=(\d+) vector=(\d+) error=(0x[0-9a-f]+) RIP=(0x[0-9a-f]+) CR2=(0x[0-9a-f]+)", text)
    context = re.search(r"User context: CS=(0x[0-9a-f]+) SS=(0x[0-9a-f]+) RSP=(0x[0-9a-f]+) RFLAGS=(0x[0-9a-f]+) RAX=(\d+) kernel_CS=(0x[0-9a-f]+) handler_rsp=(0x[0-9a-f]+) CR3=(0x[0-9a-f]+)", text)
    if not boundary or not context:
        raise Failure("Missing complete user boundary CPU frame.")
    kind, vector, error, rip, cr2 = [int(v, 0) for v in boundary.groups()]
    cs, ss, rsp, flags, result, kernel_cs, handler_rsp, root = [int(v, 0) for v in context.groups()]
    table = [(128, 0, "neu_user_done_resume", None), (14, 5, "neu_user_kernel_site", 0xffffffff80000000),
             (14, 7, "neu_user_readonly_site", 0x401000), (14, 21, 0x402100, 0x402100),
             (14, 4, "neu_user_unmapped_site", 0x406000), (13, 0, "neu_user_privileged_site", None),
             (13, 0x40a, "neu_user_gate_site", None), (14, 4, "neu_user_guard_site", 0x403000)]
    expected_vector, expected_error, target, fault_address = table[USER_CASES[case]]
    target = USER_CODE + symbols[target] - symbols["neu_user_payload"] if isinstance(target, str) else target
    if (kind != USER_CASES[case] or vector != expected_vector or error != expected_error or rip != target
            or (fault_address is not None and cr2 != fault_address) or cs != 0x3b or ss != 0x33
            or rsp != USER_RSP - (8 if case == "user-nx" else 0) or result != 42 or kernel_cs != 8
            or flags & ((1 << 8) | (1 << 9) | (1 << 10) | (3 << 12) | (1 << 14) | (1 << 17) | (1 << 18))
            or not flags & 2 or root != observed["root"]
            or not symbols["__user_kernel_bottom"] <= handler_rsp < symbols["__user_kernel_top"]):
        raise Failure("User boundary vector/error/RIP/CR2/privilege/stack/result does not match.")
    observed["boundary"] = {"vector": vector, "error": error, "rip": rip, "cr2": cr2, "cs": cs, "ss": ss,
        "rsp": rsp, "rflags": flags, "result": result, "kernel_cs": kernel_cs, "handler_rsp": handler_rsp}
    return observed



for name, number in NODE_CASES.items():
    EXPECT[name] = (COMMON + PROTOCOL + OWNED + PAGING + ["[NEU] Node ready: label=1 ", "[NEU] Node ready: label=2 ",
        "[NEU] Two Nodes live:", "[NEU] Node start:", "[NEU] Node outcome:", "[NEU] Node accounting restored:",
        f"[NEU] Node isolation test passed: case={number}", "[NEU] Kernel bootstrap successful", "[NEU] Idle: interrupts disabled; halting"],
        EXPECT["boot"][1] + ["verified=false"])


for name in TIMER_CASES:
    end = ["[NEU] Timer test passed: sessions=" + ("2" if name == "timer-repeat" else "1"),
           "[NEU] Kernel bootstrap successful", "[NEU] Idle: interrupts disabled; halting"]
    if name == "timer-unsupported":
        end = ["[NEU] Timer unsupported configuration rejected: injected_APIC=false hardware_unchanged=true IF=0",
               "[NEU] Kernel bootstrap successful", "[NEU] Idle: interrupts disabled; halting"]
    elif name in ("timer-unexpected", "timer-software"):
        end = ["[NEU] Timer controller verified:", "[NEU] Interrupt failure: vector=" + ("65" if name == "timer-unexpected" else "48")]
    EXPECT[name] = (COMMON + PROTOCOL + OWNED + PAGING + ["[NEU] Timer capabilities:"] + end,
                    EXPECT["boot"][1] + ([] if name in ("timer-unexpected", "timer-software") else ["Interrupt failure:"]))


def observe_timer(qmp, serial, symbols, sequence, root, deadline):
    while time.monotonic() < deadline:
        qmp.call("stop")
        try:
            registers = qmp.call("human-monitor-command", {"command-line": "info registers"})
            reg = lambda name: native_register(registers, name)
            if ("CPL=0" in registers and reg("RFL") & 0x600 == 0x200
                    and symbols["neu_timer_wait_begin"] <= reg("RIP") < symbols["neu_timer_wait_end"]):
                if (reg("CR3") != root or reg("CS") != 8 or reg("SS") != 0x10
                        or reg("DS") != 0x10 or reg("ES") != 0x10 or reg("EFER")&1
                        or not re.search(r"FS =0000 0000000000000000",registers)
                        or not re.search(r"GS =0000 0000000000000000",registers)
                        or not symbols["__stack_bottom"] <= reg("RSP") < symbols["__stack_top"]
                        or any(reg(name.upper()) != symbols["neu_timer_canary_"+name]
                               for name in ("rbx","rcx","rbp","rsi","rdi","r8","r9","r10","r11","r12","r13","r14","r15"))):
                    raise Failure("Timer enabled window used the wrong root/stack/registers.")
                controller = qmp.call("human-monitor-command", {"command-line": "info lapic"})
                device = []
                selected = re.search(r"Timer controller verified: mode=(\d+) base=(0x[0-9a-f]+)",transcript(serial))
                if selected and int(selected[1])==1:
                    table = root
                    for shift in (39,30,21,12):
                        address = table + ((0xffffb00000000000 >> shift)&511)*8
                        raw = qmp.call("human-monitor-command", {"command-line":f"xp /1gx {address:#x}"})
                        values = re.findall(r"0x[0-9a-fA-F]+",raw)
                        if len(values)!=1:
                            raise Failure("Cannot independently inspect the LAPIC device PTE: " + repr(raw))
                        entry = int(values[0],16)
                        if entry&7!=3 or (shift!=12 and entry&128):
                            raise Failure("LAPIC hierarchy is not private supervisor 4K paging.")
                        device.append({"physical_address":address,"entry":entry,"raw":raw})
                        table = entry&0x000ffffffffff000
                    if (device[-1]["entry"]&((1<<63)|0x9f)!=(1<<63)|0x1b
                            or table!=int(selected[2],16)&0x000ffffffffff000):
                        raise Failure("Actual LAPIC PTE is not the selected supervisor UC mapping.")
                return {"sequence":sequence,"registers":registers,"lapic":controller,"device_page_walk":device}
        finally:
            qmp.call("cont")
        if "Timer session stopped: sequence="+str(sequence) in transcript(serial):
            raise Failure("Timer session ended without independent enabled CPU/controller observation.")
        time.sleep(0.001)
    raise Failure("Timed out observing timer-enabled kernel execution.")


def check_timer(case, text, symbols, observations, root, qmp):
    caps = re.search(r"Timer capabilities: APIC=true x2APIC=(true|false) PAT=(true|false) base=(0x[0-9a-f]+) bits=(\d+) cpus=1",text)
    if not caps or not 32 <= int(caps[4]) <= 52:
        raise Failure("Missing validated BSP APIC capabilities.")
    if case == "timer-unsupported":
        if observations or any(marker in text for marker in ("Timer controller verified:","Timer event:","Timer session armed:")):
            raise Failure("Unsupported configuration changed or enabled the controller.")
        return {"rejected":True,"injection":"APIC capability removed before hardware writes"}
    controller = re.search(r"Timer controller verified: mode=(\d+) base=(0x[0-9a-f]+) id=(\d+) version=(0x[0-9a-f]+) vector=48 spurious=255 PIC=masked leaf=(0x[0-9a-f]+) pat=(0x[0-9a-f]+) root=(0x[0-9a-f]+)",text)
    if not controller:
        raise Failure("Missing controller mode, device mapping or vector evidence.")
    mode,base,apic_id,version,leaf,pat,actual_root = [int(v,0) for v in controller.groups()]
    if (mode != (2 if caps[1]=="true" else 1) or actual_root!=root or apic_id!=0
            or base!=int(caps[3],16)|(0x400 if mode==2 else 0)
            or version&255 < 0x10 or not 5 <= (version>>16)&255 <= 6):
        raise Failure("Controller does not match discovered capabilities/root.")
    if mode==1 and (caps[2]!="true" or pat>>24&255 or leaf&((1<<63)|0x9f)!=(1<<63)|0x1b
                    or leaf&0x000ffffffffff000 != base&0x000ffffffffff000):
        raise Failure("xAPIC lacks a supervisor RW/NX UC device mapping.")
    if mode==2 and (leaf or pat):
        raise Failure("x2APIC must use its MSR interface.")
    final_lapic = qmp.call("human-monitor-command", {"command-line":"info lapic"})
    if case in ("timer-unexpected","timer-software"):
        vector = 65 if case=="timer-unexpected" else 48
        expected = f"Interrupt failure: vector={vector} reason=unexpected or non-hardware event ticks=0 eois=0 IF=0"
        if (expected not in text or observations or "Timer event:" in text or "Timer test passed:" in text
                or not re.search(r"LVTT\s+0x00010030.*masked",final_lapic)
                or "TPR 0xff" not in final_lapic
                or not re.search(r"ISR\s+\(none\)",final_lapic)
                or not re.search(r"IRR\s+\(none\)",final_lapic)):
            raise Failure("Unexpected/software vector was counted or resumed.")
        return {"rejected":True,"vector":vector,"mode":mode,"final_lapic":final_lapic}
    calibration = re.search(r"Timer calibrated: PIT_Hz=(\d+) reference=(\d+) samples=(\d+),(\d+),(\d+) divided_Hz=(\d+) initial=(\d+) divide=16 period_us=1000",text)
    if not calibration:
        raise Failure("Missing actual reference/countdown calibration samples.")
    hz,reference,*values = [int(v) for v in calibration.groups()]
    samples,divided,initial = values[:3],values[3],values[4]
    ordered = sorted(samples)
    if (hz!=1193182 or reference!=11932 or min(samples)<100 or max(samples)>100000000
            or (ordered[2]-ordered[0])*4>ordered[1]
            or divided!=ordered[1]*hz//reference or initial!=(divided+500)//1000):
        raise Failure("APIC frequency was assumed or invalidly calibrated.")
    armed=list(re.finditer(r"Timer session armed: sequence=(\d+) ticks_before=(\d+) root=(0x[0-9a-f]+) reference=(\d+)",text))
    stopped=list(re.finditer(r"Timer session stopped: sequence=(\d+) ticks_after=(\d+) eois=(\d+) ISR=0 IRR=0 IF=0 GPRs=15 RSP=preserved",text))
    runs=2 if case=="timer-repeat" else 1
    if len(armed)!=runs or len(stopped)!=runs or set(observations)!=set(range(1,runs+1)):
        raise Failure("Missing independent timer session/CPU/controller evidence.")
    ticks=0
    for sequence,(start,end) in enumerate(zip(armed,stopped),1):
        if (tuple(int(v,0) for v in start.groups())!=(sequence,ticks,root,59659)
                or int(end[1])!=sequence or int(end[2])!=int(end[3])
                or not 8 <= int(end[2])-ticks <= 128 or start.end()>=end.start()
                or (sequence<runs and end.end()>=armed[sequence].start())):
            raise Failure("Timer rearm/monotonic accounting/order mismatch.")
        ticks=int(end[2])
        lapic=observations[sequence]["lapic"]
        lvtt=re.search(r"LVTT\s+(0x[0-9a-f]+)",lapic)
        timer=re.search(r"DCR=0x3 \(divide by 16\) initial_count = (\d+) current_count = (\d+)",lapic)
        if (not lvtt or int(lvtt[1],16)!=0x20030 or not timer or int(timer[1])!=initial
                or not 0<=int(timer[2])<=initial or "APIC enabled" not in lapic
                or "spurious vec 255" not in lapic or "TPR 0x00" not in lapic
                or ("X2APIC ID" in lapic)!=(mode==2)):
            raise Failure("Independent LAPIC snapshot does not show the calibrated unmasked periodic source.")
        for source in ("LVT0","LVT1","LVTPC","LVTERR","LVTTHMR"):
            if not re.search(source+r"\s+0x00010000.*masked",lapic):
                raise Failure("Unexpected local interrupt source is unmasked.")
    events=re.findall(r"Timer event: tick=(\d+) phase=(\d+) RIP=(0x[0-9a-f]+) RSP=(0x[0-9a-f]+) CS=(0x[0-9a-f]+) SS=(0x[0-9a-f]+) saved_flags=(0x[0-9a-f]+) handler_flags=(0x[0-9a-f]+) handler_rsp=(0x[0-9a-f]+) root=(0x[0-9a-f]+) ISR_before=1 ISR_after=0",text)
    if len(events)!=ticks or ticks>256:
        raise Failure("Missing bounded hardware ISR/EOI/interrupt-frame records.")
    records=[]
    for tick,event in enumerate(events,1):
        number,phase,rip,rsp,cs,ss,saved,handler,stack,cr3=[int(v,0) for v in event]
        if (number!=tick or phase not in (2,3) or cr3!=root or cs!=8 or ss!=0x10
                or saved&0x3600!=0x200 or handler&0x600
                or not symbols["neu_timer_wait_begin"]<=rip<symbols["neu_timer_wait_end"]
                or not symbols["__stack_bottom"]<=stack<rsp<symbols["__stack_top"]):
            raise Failure("Timer trace has wrong privilege, safe point, stack, root or monotonic tick.")
        records.append([int(v,0) for v in event])
    shutdown=re.search(r"Timer shutdown verified: ticks=(\d+) eois=(\d+) spurious=(\d+) LVT=masked initial=0 current=0 SVR=disabled TPR=255 PIC=masked IF=0 root=(0x[0-9a-f]+) tables=(\d+) accounting=restored",text)
    expected_spurious=1 if case=="timer-spurious" else 0
    baseline=re.search(r"Timer accounting baseline: allocated=(\d+) free=(\d+) tables=(\d+)",text)
    restored=re.search(r"Timer accounting restored: allocated=(\d+) free=(\d+) tables=(\d+)",text)
    if (not shutdown or tuple(int(v,0) for v in shutdown.groups())!=(ticks,ticks,expected_spurious,root,7)
            or not baseline or not restored or baseline.groups()!=restored.groups()
            or not re.search(r"LVTT\s+0x00030030.*masked.*periodic",final_lapic)
            or not re.search(r"initial_count = 0 current_count = 0",final_lapic)
            or "APIC disabled" not in final_lapic or "TPR 0xff" not in final_lapic
            or not re.search(r"ISR\s+\(none\)",final_lapic) or not re.search(r"IRR\s+\(none\)",final_lapic)):
        raise Failure("Controller, IRQs or resources were not quiescent before final halt.")
    return {"mode":mode,"base":base,"version":version,"device_leaf":leaf,"pat":pat,
            "calibration":{"samples":samples,"divided_hz":divided,"initial":initial},
            "sessions":runs,"ticks":ticks,"eois":ticks,"events":records,"observations":observations,
            "final_lapic":final_lapic,"accounting":[int(v) for v in restored.groups()]}


for name,number in SWITCH_CASES.items():
    EXPECT[name] = (COMMON + PROTOCOL + OWNED + PAGING + ["[NEU] Switch Thread ready: label=1", "[NEU] Switch Thread ready: label=2",
        "[NEU] Switch two runnable:", "[NEU] Switch selected:", "[NEU] Switch accounting restored:",
        f"[NEU] Cooperative switch test passed: case={number}", "[NEU] Kernel bootstrap successful", "[NEU] Idle: interrupts disabled; halting"],
        EXPECT["boot"][1]+["verified=false"])


def switch_site(symbols,name):
    return USER_CODE+symbols[name]-symbols["neu_switch_payload"]


def debug_words(qmp,address,count,physical=False):
    raw=qmp.call("human-monitor-command",{"command-line":f"{'xp' if physical else 'x'} /{count}gx {address:#x}"})
    values=[int(value,16) for value in re.findall(r"0x[0-9a-fA-F]+",raw)]
    if len(values)!=count:
        raise Failure("Incomplete debugger memory observation: "+raw)
    return values


def switch_walk(qmp,root,address):
    table=root;entries=[]
    for shift in (39,30,21,12):
        value=debug_words(qmp,table+((address>>shift)&511)*8,1,True)[0]
        entries.append(value)
        if value&1==0: return {"entries":entries,"present":False}
        if shift!=12 and value&128: raise Failure("Unexpected large switch-stack page.")
        table=value&0x000ffffffffff000
    return {"entries":entries,"present":True,"physical":table}


def observe_switch(qmp,serial,symbols,start,sequence,label,phase,root,bottom,top,deadline,faulted):
    snapshots={};seed=35 if label==1 else 77
    for stage in (["user"] if faulted else ["user","kernel"]):
        low,high=(switch_site(symbols,f"neu_switch_phase{phase}_wait"),switch_site(symbols,f"neu_switch_phase{phase}_wait_end")) if stage=="user" else (symbols["neu_native_kernel_wait"],symbols["neu_native_kernel_wait_end"])
        while time.monotonic()<deadline:
            qmp.call("stop")
            try:
                cpu=qmp.call("human-monitor-command",{"command-line":"info registers"})
                reg=lambda name:native_register(cpu,name)
                if ("CPL=3" if stage=="user" else "CPL=0") in cpu and low<=reg("RIP")<high:
                    selector=0x33 if stage=="user" else 0x10
                    rsp=USER_RSP-24 if stage=="user" else top-160
                    expected=[symbols["neu_switch_canary_"+key]^seed for key in ("bx","bp","12","13","14","15")]
                    if (reg("CR3")!=root or reg("CS")!=(0x3b if stage=="user" else 8) or reg("SS")!=selector
                            or reg("DS")!=selector or reg("ES")!=selector or reg("RSP")!=rsp or reg("RFL")&0x3600
                            or reg("CR0")&10!=10 or reg("EFER")&1!=1
                            or [reg(name) for name in ("RBX","RBP","R12","R13","R14","R15")]!=expected
                            or not re.search(r"FS =0000 0000000000000000",cpu) or not re.search(r"GS =0000 0000000000000000",cpu)):
                        raise Failure("Wrong cooperative CPU identity/root/stack/register state:\n"+cpu)
                    tr=re.search(r"TR =0018 ([0-9a-fA-F]+)",cpu)
                    if not tr or debug_words(qmp,int(tr[1],16)+4,1)[0]!=top:
                        raise Failure("Actual TSS.RSP0 does not select the running Thread's stack.")
                    if (debug_words(qmp,symbols["neu_native_selected_top"],1)[0]!=top
                            or debug_words(qmp,symbols["neu_native_busy"],1)[0]!=(0 if stage=="user" else 1)):
                        raise Failure("Native selected-stack/entry guard does not match the active Thread.")
                    common_rsp=debug_words(qmp,symbols["neu_user_saved_rsp"],1)[0]
                    if not symbols["__stack_bottom"]<=common_rsp<symbols["__stack_top"]:
                        raise Failure("Cooperative continuation is not on the permanent common stack.")
                    record={"cpu":cpu,"tss_rsp0":top,"common_rsp":common_rsp}
                    if stage=="user":
                        words=debug_words(qmp,0x402000,3);markers=debug_words(qmp,USER_RSP-24,3)
                        expected_markers=[symbols[f"neu_switch_marker_{i}"]^seed for i in range(3)]
                        if words!=[seed+7*(phase+1),phase+1,expected_markers[0]] or markers!=expected_markers or reg("RAX")!=words[0] or reg("RDX")!=(33 if phase==0 else 0):
                            raise Failure("Resumed user computation or persistent stack witnesses were lost/restarted.")
                        walks={str(address):switch_walk(qmp,root,address) for address in (bottom-4096,bottom,bottom+4096,top)}
                        for address in (bottom-4096,top):
                            if walks[str(address)]["present"]: raise Failure("Thread kernel-stack guard is mapped.")
                        for address in (bottom,bottom+4096):
                            walk=walks[str(address)]
                            if not walk["present"] or any(value&4 for value in walk["entries"]) or walk["entries"][-1]&((1<<63)|0x82)!=(1<<63)|2:
                                raise Failure("Thread kernel stack is not supervisor RW/NX 4K backing.")
                        record.update(words=words,markers=markers,walks=walks)
                    else:
                        frame=debug_words(qmp,rsp,20)
                        if (frame[15]!=switch_site(symbols,"neu_switch_resume") or frame[16]!=0x3b
                                or frame[18]!=USER_RSP-32 or frame[19]!=0x33 or frame[17]&~0x8d7
                                or frame[12]!=frame[15] or frame[4]!=frame[17]
                                or frame[14]!=(1 if phase in (0,3) else symbols["neu_switch_yield_op"])):
                            raise Failure("Native entry did not preserve the correct complete suspended frame.")
                        user_return=debug_words(qmp,USER_RSP-32,1)[0]
                        if phase in (1,2) and user_return!=switch_site(symbols,f"neu_switch_after_yield_{phase}"):
                            raise Failure("Yield lost the user stack's later-instruction return address.")
                        record.update(frame=frame,user_return=user_return)
                    snapshots[stage]=record
                    break
            finally:
                qmp.call("cont")
            if "Switch common restored:" in transcript(serial)[start:]:
                raise Failure("Cooperative slice ended before its required CPU observation.")
            time.sleep(0.001)
        else: raise Failure("Timed out observing cooperative "+stage+" context.")
    return {"sequence":sequence,"label":label,"phase":phase,"root":root,"bottom":bottom,"top":top,"snapshots":snapshots}


def check_switch(case,text,symbols,observations,kernel_root,qmp):
    number=SWITCH_CASES[case]
    ready=list(re.finditer(r"Switch Thread ready: label=(\d+) node=(\d+) thread=(\d+) space=(\d+) root=(0x[0-9a-f]+) bottom=(0x[0-9a-f]+) top=(0x[0-9a-f]+) pa0=(0x[0-9a-f]+) pa1=(0x[0-9a-f]+) code_pa=(0x[0-9a-f]+) data_pa=(0x[0-9a-f]+) user_stack_pa=(0x[0-9a-f]+) seed=(\d+)",text))
    if len(ready)!=2:raise Failure("Two owned runnable Threads were not constructed.")
    records={}
    for match in ready:
        values=[int(v,0) for v in match.groups()];label,node,thread,space,root,bottom,top,*rest=values
        if label in records or label not in (1,2) or values[-1]!=(35 if label==1 else 77) or bottom!=0xffffc00000000000+((thread&0xffffffff)-1)*16384+4096 or top-bottom!=8192:
            raise Failure("Invalid per-Thread identity/guarded stack binding.")
        records[label]=dict(zip(("label","node","thread","space","root","bottom","top","pa0","pa1","code","data","user_stack","seed"),values))
    if set(records)!={1,2} or any(records[1][k]==records[2][k] for k in ("node","thread","space","root","bottom","top")):
        raise Failure("Cooperative Threads accidentally share an identity/root/stack.")
    if {records[1][k] for k in ("pa0","pa1","code","data","user_stack")} & {records[2][k] for k in ("pa0","pa1","code","data","user_stack")}:
        raise Failure("Cooperative backing frames are shared across domains.")
    selected=list(re.finditer(r"Switch selected: sequence=(\d+) label=(\d+) node=(\d+) thread=(\d+) space=(\d+) root=(0x[0-9a-f]+) phase=(\d+) fresh=(true|false)\n",text))
    both=re.search(r"Switch two runnable: A=(\d+) B=(\d+) queue=2 pins=verified",text)
    if not both or tuple(int(v) for v in both.groups())!=(records[1]["thread"],records[2]["thread"]) or not max(r.end() for r in ready)<both.start()<selected[0].start():
        raise Failure("Both Threads must be queued with retained resources before dispatch.")
    fifo=[1,2];phases={1:0,2:0};expected=[];terminal={}
    while fifo:
        label=fifo.pop(0);phase=phases[label];phases[label]+=1
        fault=phase==1 and ((label==1 and number in (1,3,4,5,6)) or (label==2 and number==2))
        expected.append((label,phase,fault))
        if fault or phase==3:terminal[label]=(phase,fault)
        else:fifo.append(label)
    if len(selected)!=len(expected):raise Failure("FIFO did not make the required genuine suspend/resume alternations.")
    expected_cpu={i for i,(label,phase,_) in enumerate(expected,1) if not(number==6 and label==1 and phase==1)}
    if set(observations)!=expected_cpu:raise Failure("Missing or spurious actual cooperative CPU episodes.")
    frames={};outcomes={}
    for index,(selected_run,wanted) in enumerate(zip(selected,expected)):
        sequence,label,node,thread,space,root,phase=[int(v,0) for v in selected_run.groups()[:7]]
        record=records[label];faulted=wanted[2];rejected=number==6 and label==1 and phase==1
        if (sequence!=index+1 or (label,phase)!=wanted[:2] or selected_run[8]!=("true" if phase==0 else "false")
                or (node,thread,space,root)!=tuple(record[k] for k in ("node","thread","space","root"))):
            raise Failure("Wrong FIFO Thread/Node association or restart instead of resume.")
        segment=text[selected_run.start():selected[index+1].start() if index+1<len(selected) else len(text)]
        frame_pattern=r"Switch frame: event=(taken|saved) thread=(\d+) RIP=(0x[0-9a-f]+) RSP=(0x[0-9a-f]+) flags=(0x[0-9a-f]+) regs=([^\n]+)\n"
        parsed=[]
        for entry in re.finditer(frame_pattern,segment):
            values=[int(v,0) for v in entry.groups()[1:5]]+[int(v,0) for v in entry[6].split(',')]
            if len(values)!=19:raise Failure("Incomplete saved CPU register record.")
            parsed.append((entry[1],values))
        if not parsed or parsed[0][0]!="taken" or parsed[0][1][0]!=thread:
            raise Failure("No owned Thread context was consumed for dispatch.")
        taken=parsed[0][1]
        if phase==0:
            if taken[1:]!=[USER_CODE,USER_RSP,2]+[0]*15:raise Failure("Invalid initial context.")
        elif frames.get(thread)!=taken:
            raise Failure("Previously saved full GPR/RIP/RSP/RFLAGS context was lost or mixed with another Thread.")
        if rejected:
            if (len(parsed)!=1 or "Switch native ready:" in segment or "Native MSRs verified:" in segment
                    or not re.search(r"Switch rejected before resume: thread="+str(thread)+r" RIP=0x800000000000 reason=3 root="+hex(kernel_root),segment)):
                raise Failure("Invalid saved return state reached activation/native entry.")
        else:
            actual=observations[sequence]
            if (actual["label"],actual["phase"],actual["root"],actual["bottom"],actual["top"])!=(label,phase,root,record["bottom"],record["top"]):
                raise Failure("CPU snapshots belong to the wrong saved context.")
            expected_stages={"user"} if faulted else {"user","kernel"}
            if set(actual["snapshots"])!=expected_stages:raise Failure("Missing direct CPL/stack/root observations.")
            for offset,key in ((0,"pa0"),(4096,"pa1")):
                if actual["snapshots"]["user"]["walks"][str(record["bottom"]+offset)]["physical"]!=record[key]:
                    raise Failure("Thread kernel-stack backing changed across suspension.")
            native_ready=re.search(r"Switch native ready: node=(\d+) thread=(\d+) space=(\d+) root=(0x[0-9a-f]+) stack_bottom=(0x[0-9a-f]+) stack_top=(0x[0-9a-f]+) TSS_RSP0=(0x[0-9a-f]+) RIP=(0x[0-9a-f]+) RSP=(0x[0-9a-f]+) fresh=(true|false)",segment)
            if not native_ready or tuple(int(v,0) for v in native_ready.groups()[:9])!=(node,thread,space,root,record["bottom"],record["top"],record["top"],taken[1],taken[2]):
                raise Failure("Native session/TSS/return contract was not freshly bound.")
            msrs=re.search(r"Native MSRs verified: EFER=(0x[0-9a-f]+) STAR=(0x[0-9a-f]+) LSTAR=(0x[0-9a-f]+) FMASK=(0x[0-9a-f]+) SYSENTER=0",segment)
            if not msrs or tuple(int(v,16) for v in msrs.groups())!=(0xd01,symbols["neu_abi_star"],symbols["neu_native_entry"],symbols["neu_abi_fmask"]):raise Failure("Stale cooperative MSR state.")
            if not re.search(r"Switch common restored: thread="+str(thread)+r" root="+hex(kernel_root)+r" MSRs=restored guard=clear entry=clear stack_margin=512",segment):
                raise Failure("Slice did not finish its kernel context before selection.")
            native_terminal=re.search(r"Native terminal: kind=(\d+) reason=(\d+) status=(\d+) entries=(\d+) returns=(\d+) queries=(\d+) errors=(\d+) verified=true",segment)
            expected_terminal=(32+number,8,0,0,0,0,0) if faulted else ((32+number,0,0,2,1,1,0) if phase==3 else ((32+number,100,0,2,1,1,0) if phase==0 else (32+number,100,0,1,0,0,0)))
            if not native_terminal or tuple(int(v) for v in native_terminal.groups())!=expected_terminal:
                raise Failure("Wrong yield/exit/syscall counts for suspended Thread.")
        if not faulted and phase<3:
            if len(parsed)!=2 or parsed[1][0]!="saved":raise Failure("Yield terminated or failed to save its Thread.")
            saved=parsed[1][1];regs=saved[4:]
            if (saved[0]!=thread or saved[1]!=switch_site(symbols,"neu_switch_resume") or saved[2]!=USER_RSP-32
                    or saved[3]&~0x8d7 or regs[14]!=0 or regs[11]!=0 or regs[12]!=saved[1] or regs[4]!=saved[3]):
                raise Failure("Yield saved an invalid complete success context.")
            frames[thread]=saved
            yielded=re.search(r"Switch yielded: label=(\d+) thread=(\d+) phase=(\d+) result=(\d+) steps=(\d+) queue=(\d+)",segment)
            if not yielded or tuple(int(v) for v in yielded.groups())!=(label,thread,phase,record["seed"]+7*(phase+1),phase+1,2 if len(outcomes)==0 else 1):
                raise Failure("Yield queue membership/result is inconsistent.")
        else:
            if len(parsed)!=1:raise Failure("Terminal Thread retained a resumable frame.")
            steps=(1 if rejected else 2) if faulted else 4
            outcome=re.search(r"Switch outcome: label=(\d+) thread=(\d+) reason=(normal|fault) result=(\d+) steps=(\d+) exited_dispatch=rejected",segment)
            if not outcome or (int(outcome[1]),int(outcome[2]),outcome[3],int(outcome[4]),int(outcome[5]))!=(label,thread,"fault" if faulted else "normal",record["seed"]+7*steps,steps):
                raise Failure("Wrong Node outcome or a terminal Thread remained dispatchable.")
            if faulted and not rejected:
                fault=re.search(r"Node user fault: node=(\d+) thread=(\d+) space=(\d+) root=(0x[0-9a-f]+) vector=(\d+) error=(0x[0-9a-f]+) RIP=(0x[0-9a-f]+) CR2=(0x[0-9a-f]+) CS=(0x[0-9a-f]+) SS=(0x[0-9a-f]+) RSP=(0x[0-9a-f]+) RFLAGS=(0x[0-9a-f]+) handler_rsp=(0x[0-9a-f]+)",segment)
                if not fault:raise Failure("No actual scheduled user exception frame.")
                values=[int(v,0) for v in fault.groups()]
                vector,error,target,site_name={1:(6,0,0,"neu_switch_ud_site"),2:(6,0,0,"neu_switch_ud_site"),3:(14,4,0x406000,"neu_switch_cross_site"),4:(14,5,0xffffc00000001000,"neu_switch_stack_user_site"),5:(14,4,0xffffc00000000000,"neu_switch_stack_guard_site")}[number]
                if (tuple(values[:7])!=(node,thread,space,root,vector,error,switch_site(symbols,site_name))
                        or (vector==14 and values[7]!=target) or values[8:11]!=[0x3b,0x33,USER_RSP-24]
                        or values[11]&0x3600 or not record["bottom"]<=values[12]<record["top"]):
                    raise Failure("Fault did not use its correct Node/root/private kernel stack.")
            reclaimed=re.search(r"Switch reclaimed: label="+str(label)+r" thread="+str(thread)+r" space="+str(space)+r" saved=false queued=false outcome=retained",segment)
            if not reclaimed or reclaimed.start()<outcome.end():raise Failure("Premature or wrong-domain reclamation.")
            other=2 if label==1 else 1
            if other not in outcomes:
                survivor=re.search(r"Switch survivor retained: label="+str(other)+r" thread="+str(records[other]["thread"])+r" root="+hex(records[other]["root"])+r" saved=true pinned=true",segment[reclaimed.end():])
                if not survivor:raise Failure("Healthy suspended sibling did not survive early reclamation.")
            outcomes[label]={"reason":outcome[3],"result":int(outcome[4]),"steps":steps}
    if number==7 and "Switch construction rollback: stack_failpoints=5 context_rejected=true sibling_saved=unchanged" not in text:
        raise Failure("Missing stack/context-construction rollback with a pinned sibling.")
    if number==8:
        stale=re.search(r"Switch stale identity rejected: old=(\d+) replacement=(\d+)",text)
        if not stale or int(stale[1])!=records[1]["thread"] or int(stale[1])==int(stale[2]):raise Failure("Stale Thread generation selected a context.")
    baseline=re.search(r"Switch accounting baseline: allocated=(\d+) free=(\d+) tables=(\d+)",text)
    restored=re.search(r"Switch accounting restored: allocated=(\d+) free=(\d+) tables=(\d+)",text)
    final=re.search(r"Cooperative switch test passed: case=(\d+) dispatches=(\d+) queue=empty IF=0",text)
    if not baseline or not restored or baseline.groups()!=restored.groups() or not final or tuple(int(v) for v in final.groups())!=(number,len(expected)):
        raise Failure("Cooperative scheduler did not finish empty with exact resource accounting.")
    for symbol in ("neu_native_busy","neu_native_selected_top","neu_native_saved_user_rsp","neu_user_saved_rsp"):
        if debug_words(qmp,symbols[symbol],1)[0]!=0:raise Failure("Stale entry/continuation storage survived final cleanup.")
    return {"case":number,"threads":records,"order":[[label,phase] for label,phase,_ in expected],"contexts":frames,"outcomes":outcomes,"observations":observations,"accounting":[int(v) for v in restored.groups()]}


def native_register(registers, name):
    match = re.search(r"\b" + name + r"\s*=([0-9a-fA-F]+)", registers)
    if not match:
        raise Failure("Missing native CPU register: " + name)
    return int(match[1], 16)


def native_site(symbols, name):
    return USER_CODE + symbols[name] - symbols["neu_native_payload"]


def observe_native(qmp, serial, case, symbols, deadline, start=0, expected_kind=None, node=False):
    view = lambda: transcript(serial)[start:]
    match = None
    # UART files can end in a partially emitted line. Do not bind a context
    # until its complete contract is present; a prefix is not readiness evidence.
    while time.monotonic() < deadline:
        match = re.search(r"Native user space ready: kind=(\d+) root=(0x[0-9a-f]+) kernel=(0x[0-9a-f]+) entry=(0x[0-9a-f]+) rsp=(0x[0-9a-f]+)\n", view())
        if match:
            break
        time.sleep(0.001)
    if not match:
        raise Failure("Missing complete native execution contract.")
    kind, root, kernel, entry, rsp = [int(v, 0) for v in match.groups()]
    if kind != (NATIVE_CASES[case] if expected_kind is None else expected_kind) or root == kernel or entry != USER_CODE or rsp != USER_RSP:
        raise Failure("Invalid native root or entry contract.")
    observed = {"kind": kind, "root": root, "kernel_root": kernel, "observations": {}}
    phases = ["before"] + (["kernel"] if kind != 7 else []) + (["after"] if kind <= 2 or node else [])
    canaries = [symbols["neu_native_canary_" + suffix] for suffix in ("bx", "bp", "12", "13", "14", "15")]
    for phase in phases:
        if phase == "kernel":
            low, high = symbols["neu_native_kernel_wait"], symbols["neu_native_kernel_wait_end"]
        else:
            low, high = (native_site(symbols, "neu_native_" + phase + "_wait"), native_site(symbols, "neu_native_" + phase + "_wait_end"))
        while time.monotonic() < deadline:
            qmp.call("stop")
            try:
                registers = qmp.call("human-monitor-command", {"command-line": "info registers"})
            finally:
                qmp.call("cont")
            reg = lambda name: native_register(registers, name)
            cpl = "CPL=0" if phase == "kernel" else "CPL=3"
            if cpl in registers and low <= reg("RIP") < high:
                expected_rsp = symbols["__user_kernel_top"] - 160 if phase == "kernel" else USER_RSP
                expected_cs, expected_ss = (8, 0x10) if phase == "kernel" else (0x3b, 0x33)
                if (reg("CR3") != root or reg("RSP") != expected_rsp or reg("CS") != expected_cs
                        or reg("SS") != expected_ss or reg("DS") != expected_ss or reg("ES") != expected_ss
                        or reg("RFL") & symbols["neu_abi_fmask"]
                        or bool(reg("EFER") & 1) != (kind != 7) or "HLT=0" not in registers
                        or reg("CR0") & 10 != 10
                        or not re.search(r"FS =0000 0000000000000000", registers)
                        or not re.search(r"GS =0000 0000000000000000", registers)
                        or [reg(name) for name in ("RBX", "RBP", "R12", "R13", "R14", "R15")] != canaries):
                    raise Failure("Native QMP privilege/root/stack/flags/register state mismatch in " + phase + " expected root=" + hex(root) + "\n" + registers)
                if phase == "after" and (reg("RAX") != 0 or reg("RDX") != symbols["neu_abi_info"]):
                    raise Failure("Ring 3 did not observe the expected native scalar result.")
                observed["observations"][phase] = registers
                break
            if "Native terminal:" in view():
                raise Failure("Native execution finished without the required QMP " + phase + " observation.")
            time.sleep(0.01)
        else:
            raise Failure("Timed out observing native phase " + phase)
    return observed


def check_native(case, text, symbols, observed):
    if not observed:
        raise Failure("Missing direct native privilege-transition observations.")
    kind = NATIVE_CASES[case]
    msrs = re.search(r"Native MSRs verified: EFER=(0x[0-9a-f]+) STAR=(0x[0-9a-f]+) LSTAR=(0x[0-9a-f]+) FMASK=(0x[0-9a-f]+) SYSENTER=0", text)
    if not msrs:
        raise Failure("Missing verified syscall MSRs.")
    efer, star, lstar, fmask = [int(v, 16) for v in msrs.groups()]
    if efer & 0xd01 != 0xd01 or star != symbols["neu_abi_star"] or lstar != symbols["neu_native_entry"] or fmask != symbols["neu_abi_fmask"]:
        raise Failure("Native MSRs do not select the expected entry/selectors/mask.")
    entries = re.findall(r"Native entry: seq=(\d+) op=(0x[0-9a-f]+) arg1=(0x[0-9a-f]+) RIP=(0x[0-9a-f]+) RSP=(0x[0-9a-f]+) R11=(0x[0-9a-f]+)", text)
    contexts = re.findall(r"Native kernel: CS=(0x[0-9a-f]+) SS=(0x[0-9a-f]+) RSP=(0x[0-9a-f]+) frame=(0x[0-9a-f]+) RFLAGS=(0x[0-9a-f]+) CR3=(0x[0-9a-f]+)", text)
    canaries = re.findall(r"Native canaries: RBX=(0x[0-9a-f]+) RBP=(0x[0-9a-f]+) R12=(0x[0-9a-f]+) R13=(0x[0-9a-f]+) R14=(0x[0-9a-f]+) R15=(0x[0-9a-f]+)", text)
    query, exit_op = symbols["neu_abi_query"], symbols["neu_abi_exit"]
    operations = [query, query, exit_op] if kind == 0 else ([query, (1 << 64) - 1, query, exit_op] if kind == 1 else ([query, exit_op, query, exit_op] if kind == 2 else ([] if kind == 7 else [query])))
    arguments = [11, 11, 0] if kind == 0 else ([11, 11, 11, 0] if kind == 1 else ([11, 256, 11, 0] if kind == 2 else ([] if kind == 7 else [11])))
    if len(entries) != len(operations) or len(contexts) != len(entries) or len(canaries) != len(entries):
        raise Failure("Incorrect native entry/context count.")
    parsed = []
    expected_canaries = [symbols["neu_native_canary_" + suffix] for suffix in ("bx", "bp", "12", "13", "14", "15")]
    for index, (entry, context, preserved) in enumerate(zip(entries, contexts, canaries)):
        seq, op, arg1, rip, rsp, r11 = [int(v, 0) for v in entry]
        cs, ss, kernel_rsp, frame, flags, root = [int(v, 0) for v in context]
        expected_rip = native_site(symbols, "neu_native_bad_rsp_resume" if kind == 4 else "neu_native_resume")
        expected_rsp = 0x800000000000 if kind == 4 else USER_RSP - 8
        if (seq != index + 1 or op != operations[index] or arg1 != arguments[index] or rip != expected_rip
                or rsp != expected_rsp or cs != 8 or ss != 0x10 or root != observed["root"]
                or frame != symbols["__user_kernel_top"] - 160 or flags & fmask
                or not symbols["__user_kernel_bottom"] <= kernel_rsp < frame
                or [int(v, 16) for v in preserved] != expected_canaries
                or (kind == 5 and not r11 & (1 << 10))):
            raise Failure("Native hardware entry/register/frame invariants failed.")
        parsed.append({"seq": seq, "operation": op, "arg1": arg1, "return_rip": rip, "user_rsp": rsp, "user_flags": r11, "kernel_rsp": kernel_rsp, "frame": frame})
    returns = re.findall(r"Native return prepared: seq=(\d+) RAX=(0x[0-9a-f]+) RDX=(0x[0-9a-f]+) RIP=(0x[0-9a-f]+) RSP=(0x[0-9a-f]+) preserved=true", text)
    outputs = [(0, symbols["neu_abi_info"])]
    if kind == 1:
        outputs.append((symbols["neu_abi_unknown"], 0))
    if kind == 2:
        outputs.append((symbols["neu_abi_invalid"], 0))
    outputs.append((0, symbols["neu_abi_info"]))
    if kind >= 3:
        outputs = []
    if len(returns) != len(outputs):
        raise Failure("Incorrect native return count.")
    for index, (returned, expected) in enumerate(zip(returns, outputs)):
        seq, rax, rdx, rip, rsp = [int(v, 0) for v in returned]
        if seq != index + 1 or (rax, rdx) != expected or rip != native_site(symbols, "neu_native_resume") or rsp != USER_RSP - 8:
            raise Failure("Native returned scalar/status/context mismatch.")
    terminal = re.search(r"Native terminal: kind=(\d+) reason=(\d+) status=(\d+) entries=(\d+) returns=(\d+) queries=(\d+) errors=(\d+) verified=true", text)
    expected_reason = [0, 0, 0, 3, 4, 5, 2, 6][kind]
    expected_counts = (3, 2, 2, 0) if kind == 0 else ((4, 3, 2, 1) if kind in (1, 2) else ((0, 0, 0, 0) if kind == 7 else (1, 0, 0, 0)))
    if not terminal or tuple(int(v) for v in terminal.groups()) != (kind, expected_reason, 0, *expected_counts):
        raise Failure("Unexpected native terminal classification/accounting.")
    if kind <= 2:
        if "Native thread_exit: status=0" not in text or "Native return rejected:" in text or "Native user fault:" in text:
            raise Failure("Native thread_exit did not terminate the successful sequence.")
    elif kind != 7:
        rejection = re.search(r"Native return rejected: reason=(\d+) RIP=(0x[0-9a-f]+) RSP=(0x[0-9a-f]+) CS=(0x[0-9a-f]+) SS=(0x[0-9a-f]+) RFLAGS=(0x[0-9a-f]+)", text)
        if not rejection or int(rejection[1]) != expected_reason:
            raise Failure("Missing unsafe return-state rejection.")
        values = [int(v, 0) for v in rejection.groups()[1:]]
        if ((kind == 3 and values[0] != 0x800000000000) or (kind == 4 and values[1] != 0x800000000000)
                or (kind == 5 and not values[4] & (1 << 10)) or (kind == 6 and tuple(values[2:4]) != (8, 0x10))):
            raise Failure("Wrong return-state corruption was tested.")
    else:
        fault = re.search(r"Native user fault: vector=(\d+) error=(0x[0-9a-f]+) RIP=(0x[0-9a-f]+) CR2=(0x[0-9a-f]+) CS=(0x[0-9a-f]+) SS=(0x[0-9a-f]+) RSP=(0x[0-9a-f]+) handler_rsp=(0x[0-9a-f]+) verified=true", text)
        if not fault:
            raise Failure("Missing disabled-SCE hardware fault.")
        vector, error, rip, _, cs, ss, rsp, handler = [int(v, 0) for v in fault.groups()]
        if (vector != 6 or error != 0 or rip != native_site(symbols, "neu_native_syscall_site") or cs != 0x3b or ss != 0x33
                or rsp != USER_RSP - 8 or not symbols["__user_kernel_bottom"] <= handler < symbols["__user_kernel_top"]):
            raise Failure("Disabled-SCE case did not fault at the real user SYSCALL.")
    observed.update(msrs={"efer": efer, "star": star, "lstar": lstar, "fmask": fmask}, entries=parsed,
                    returns=[[int(v, 0) for v in returned] for returned in returns], terminal=[int(v) for v in terminal.groups()])
    return observed



def check_nodes(case, text, symbols, observations):
    number = NODE_CASES[case]
    ready_pattern = r"Node ready: label=(\d+) node=(\d+) thread=(\d+) space=(\d+) root=(0x[0-9a-f]+) code_pa=(0x[0-9a-f]+) data_pa=(0x[0-9a-f]+) stack_pa=(0x[0-9a-f]+) seed=(\d+) private=(true|false)"
    ready = list(re.finditer(ready_pattern,text))
    starts = list(re.finditer(r"Node start: label=(\d+) node=(\d+) thread=(\d+) space=(\d+) root=(0x[0-9a-f]+) kind=(\d+)",text))
    if len(ready)!=2 or len(starts)!=2 or set(observations)!={1,2}:
        raise Failure("Two independent live Node records and executions are required.")
    records = {}
    for match in ready:
        label,node,thread,space,root,code,data,stack,seed = [int(v,0) for v in match.groups()[:9]]
        if label in records or seed!={1:35,2:77}.get(label) or (match[10]=="true")!=(label==2):
            raise Failure("Invalid Node identity or independent seed/private mapping.")
        records[label] = {"node":node,"thread":thread,"space":space,"root":root,"code_pa":code,"data_pa":data,"stack_pa":stack}
    if set(records)!={1,2} or any(records[1][key]==records[2][key] for key in ("node","thread","space","root")):
        raise Failure("Node/Thread/space/root identities are not independent.")
    if {records[1][k] for k in ("code_pa","data_pa","stack_pa")} & {records[2][k] for k in ("code_pa","data_pa","stack_pa")}:
        raise Failure("User backing frames are accidentally shared between Nodes.")
    pair = re.search(r"Two Nodes live: A=(\d+) B=(\d+) A_root=(0x[0-9a-f]+) B_root=(0x[0-9a-f]+) capacity=verified",text)
    if not pair or tuple(int(v,0) for v in pair.groups()) != (records[1]["node"],records[2]["node"],records[1]["root"],records[2]["root"]) or not max(m.end() for m in ready)<pair.start()<starts[0].start():
        raise Failure("Both ownership domains were not retained before either execution.")
    expected_order = [2,1] if number==2 else [1,2]
    if [int(m[1]) for m in starts]!=expected_order:
        raise Failure("Incorrect sequential execution order.")
    outcomes = {}
    query, exit_op = symbols["neu_abi_query"], symbols["neu_abi_exit"]
    for index, start in enumerate(starts):
        label,node,thread,space,root,kind = [int(v,0) for v in start.groups()]
        record = records[label]
        if (node,thread,space,root) != tuple(record[k] for k in ("node","thread","space","root")):
            raise Failure("Active Thread does not belong to its declared Node/root.")
        faulted = (number==1 and label==1) or (number==2 and label==2) or (number==3 and label==1)
        expected_kind = 10 if number==3 and label==1 else (11 if faulted else (8 if label==1 else 9))
        observed = observations[label]
        if kind!=expected_kind or observed["root"]!=root or observed["kind"]!=kind or set(observed["observations"])!={"before","kernel","after"}:
            raise Failure("Missing per-Node actual CPL=3/0/3 and CR3 evidence.")
        end = starts[index+1].start() if index+1<len(starts) else len(text)
        segment = text[start.start():end]
        binding = re.search(r"Node execution bound: node=(\d+) thread=(\d+) space=(\d+) root=(0x[0-9a-f]+)",segment)
        msrs = re.search(r"Native MSRs verified: EFER=(0x[0-9a-f]+) STAR=(0x[0-9a-f]+) LSTAR=(0x[0-9a-f]+) FMASK=(0x[0-9a-f]+) SYSENTER=0",segment)
        if not binding or tuple(int(v,0) for v in binding.groups())!=(node,thread,space,root) or not msrs:
            raise Failure("Missing trusted native context binding/configuration.")
        efer,star,lstar,fmask=[int(v,16) for v in msrs.groups()]
        if efer & 0xd01!=0xd01 or star!=symbols["neu_abi_star"] or lstar!=symbols["neu_native_entry"] or fmask!=symbols["neu_abi_fmask"]:
            raise Failure("Node native MSRs do not match the preserved ABI.")
        entries = re.findall(r"Native entry: seq=(\d+) op=(0x[0-9a-f]+) arg1=(0x[0-9a-f]+) RIP=(0x[0-9a-f]+) RSP=(0x[0-9a-f]+) R11=(0x[0-9a-f]+)",segment)
        contexts = re.findall(r"Native kernel: CS=(0x[0-9a-f]+) SS=(0x[0-9a-f]+) RSP=(0x[0-9a-f]+) frame=(0x[0-9a-f]+) RFLAGS=(0x[0-9a-f]+) CR3=(0x[0-9a-f]+)",segment)
        bound = re.findall(r"Node syscall bound: node=(\d+) thread=(\d+) space=(\d+) root=(0x[0-9a-f]+) seq=(\d+)",segment)
        expected_ops = [query] if faulted else [query,query,exit_op]
        if len(entries)!=len(expected_ops) or len(contexts)!=len(entries) or len(bound)!=len(entries):
            raise Failure("Incorrect per-Node syscall sequence/count.")
        for seq,(entry,context,association) in enumerate(zip(entries,contexts,bound),1):
            actual_seq,op,arg,rip,rsp,_=[int(v,0) for v in entry]
            cs,ss,kernel_rsp,frame,flags,current_root=[int(v,0) for v in context]
            if (actual_seq!=seq or op!=expected_ops[seq-1] or arg!=(0 if op==exit_op else 11)
                    or rip!=native_site(symbols,"neu_native_resume") or rsp!=USER_RSP-8
                    or cs!=8 or ss!=0x10 or current_root!=root or flags&fmask
                    or frame!=symbols["__user_kernel_top"]-160 or not symbols["__user_kernel_bottom"]<=kernel_rsp<frame
                    or tuple(int(v,0) for v in association)!=(node,thread,space,root,seq)):
                raise Failure("Syscall used a stale/wrong Node, Thread, root or return contract.")
        returned = re.findall(r"Native return prepared: seq=(\d+) RAX=(0x[0-9a-f]+) RDX=(0x[0-9a-f]+) RIP=(0x[0-9a-f]+) RSP=(0x[0-9a-f]+) preserved=true",segment)
        expected_returns = 1 if faulted else 2
        if len(returned)!=expected_returns:
            raise Failure("Node did not complete its expected native query returns.")
        for seq,row in enumerate(returned,1):
            if tuple(int(v,0) for v in row)!=(seq,0,symbols["neu_abi_info"],native_site(symbols,"neu_native_resume"),USER_RSP-8):
                raise Failure("Node query status/scalar/return state mismatch.")
        terminal = re.search(r"Native terminal: kind=(\d+) reason=(\d+) status=(\d+) entries=(\d+) returns=(\d+) queries=(\d+) errors=(\d+) verified=true",segment)
        wanted = (kind,8,0,1,1,1,0) if faulted else (kind,0,0,3,2,2,0)
        if not terminal or tuple(int(v) for v in terminal.groups())!=wanted:
            raise Failure("Node terminal state was not independently recorded.")
        outcome = re.search(r"Node outcome: label=(\d+) node=(\d+) thread=(\d+) reason=(normal|fault) status=(\d+) result=(\d+)",segment)
        if not outcome or (int(outcome[1]),int(outcome[2]),int(outcome[3]),outcome[4],int(outcome[5]),int(outcome[6]))!=(label,node,thread,"fault" if faulted else "normal",0,42 if label==1 else 84):
            raise Failure("Node outcome/result is mixed with another domain.")
        restored = re.search(r"Node execution restored: node=(\d+) thread=(\d+) root=(0x[0-9a-f]+) MSRs=restored guard=clear",segment)
        if not restored or tuple(int(v,0) for v in restored.groups())!=(node,thread,observed["kernel_root"]):
            raise Failure("Kernel continuation or native session was not restored between Nodes.")
        parsed_fault = None
        if faulted:
            fault = re.search(r"Node user fault: node=(\d+) thread=(\d+) space=(\d+) root=(0x[0-9a-f]+) vector=(\d+) error=(0x[0-9a-f]+) RIP=(0x[0-9a-f]+) CR2=(0x[0-9a-f]+) CS=(0x[0-9a-f]+) SS=(0x[0-9a-f]+) RSP=(0x[0-9a-f]+) RFLAGS=(0x[0-9a-f]+) handler_rsp=(0x[0-9a-f]+)",segment)
            if not fault:
                raise Failure("Missing actual Node-local exception frame.")
            values=[int(v,0) for v in fault.groups()]
            fn,ft,fs,fr,vector,error,rip,cr2,cs,ss,rsp,flags,handler=values
            target="neu_node_cross_site" if kind==10 else "neu_node_ud_site"
            if ((fn,ft,fs,fr)!=(node,thread,space,root) or (vector,error)!=((14,4) if kind==10 else (6,0))
                    or rip!=native_site(symbols,target) or (kind==10 and cr2!=0x406000)
                    or cs!=0x3b or ss!=0x33 or rsp!=USER_RSP or flags&((1<<9)|(3<<12))
                    or not symbols["__user_kernel_bottom"]<=handler<symbols["__user_kernel_top"]):
                raise Failure("Fault was not contained to the expected CPL=3 Node/context.")
            parsed_fault=values
        elif "Node user fault:" in segment or "Native thread_exit: status=0" not in segment:
            raise Failure("Healthy Node faulted or failed to exit.")
        outcomes[label]={"identity":record,"cpu":observed,"fault":parsed_fault,"terminal":wanted,"result":int(outcome[6])}
    reclaimed=list(re.finditer(r"Node reclaimed: label=(\d+) node=(\d+) space=(\d+) outcome=retained",text))
    if len(reclaimed)!=2 or {int(m[1]) for m in reclaimed}!={1,2}:
        raise Failure("Nodes were not reclaimed independently.")
    for match in reclaimed:
        record=records[int(match[1])]
        if (int(match[2]),int(match[3]))!=(record["node"],record["space"]):
            raise Failure("Wrong Node address space was reclaimed.")
    if number in (1,2,3,4):
        first,second=expected_order
        released=next(m for m in reclaimed if int(m[1])==first)
        survivor=re.search(r"Node survivor verified: label="+str(second)+r" node=(\d+) space=(\d+) root=(0x[0-9a-f]+) data=0",text[released.end():starts[1].start()])
        if not released.end()<starts[1].start() or not survivor or tuple(int(v,0) for v in survivor.groups())!=tuple(records[second][k] for k in ("node","space","root")):
            raise Failure("Survivor did not retain valid mappings/resources after sibling reclamation.")
    if number==5:
        rollback=re.search(r"Node B rollback verified: failpoints=(\d+) A_retained=true",text)
        if not rollback or int(rollback[1])<10 or rollback.end()>pair.start():
            raise Failure("Missing second-Node allocation-failure rollback proof.")
    baseline=re.search(r"Node accounting baseline: allocated=(\d+) free=(\d+) tables=(\d+)",text)
    restored=re.search(r"Node accounting restored: allocated=(\d+) free=(\d+) tables=(\d+)",text)
    if not baseline or not restored or baseline.groups()!=restored.groups():
        raise Failure("Node resources did not return to the original accounting baseline.")
    return {"case":number,"order":expected_order,"nodes":outcomes,"accounting":[int(v) for v in restored.groups()]}


def check_fault(case, text, symbols):
    if case not in FAULTS:
        return None
    match = re.search(r"EXCEPTION: vector=(\d+) error=(0x[0-9a-f]+) RIP=(0x[0-9a-f]+) CR2=(0x[0-9a-f]+)", text)
    context = re.search(r"EXCEPTION: CS=(0x[0-9a-f]+) RFLAGS=(0x[0-9a-f]+) RSP=(0x[0-9a-f]+) handler_rsp=(0x[0-9a-f]+) IST=(\d+)", text)
    if not match or not context:
        raise Failure("Missing complete exception frame diagnostics.")
    vector, error, rip, cr2 = [int(value, 0) for value in match.groups()]
    cs, flags, original_rsp, handler_rsp, ist = [int(value, 0) for value in context.groups()]
    expected_vector, expected_error, site = FAULTS[case]
    if vector != expected_vector or error != expected_error or cs != 8 or flags & 512:
        raise Failure("Exception vector, hardware error code, CS, or saved IF does not match the test.")
    if site and rip != (site if isinstance(site, int) else symbols[site]):
        raise Failure("Saved exception RIP does not match the intended fault/trap instruction.")
    if case == "page-fault" and cr2 != symbols["NEU_PF_PROBE"]:
        raise Failure("Page-fault CR2 does not match the verified read-only target.")
    if case in PAGING_FAULTS:
        kind, target = PAGING_FAULTS[case]
        target = symbols[target] if isinstance(target, str) else target
        if cr2 != target or f"Paging fault prepared: kind={kind} address={target:#x}" not in text:
            raise Failure("Paging fault CR2 or verified setup does not match the intended target.")
    expected_ist = int(case == "double-fault")
    bottom, top = ("__df_stack_bottom", "__df_stack_top") if expected_ist else ("__stack_bottom", "__stack_top")
    if ist != expected_ist or not symbols[bottom] <= handler_rsp < symbols[top]:
        raise Failure("Exception handler is not executing on its expected normal/IST stack.")
    return {"vector": vector, "error": error, "rip": rip, "cr2": cr2, "original_rsp": original_rsp,
            "handler_rsp": handler_rsp, "ist": ist}


def transcript(path):
    return path.read_text(errors="replace") if path.exists() else ""


def check_halt(qmp, symbols, expected_root=None):
    if qmp.call("query-status")["status"] != "running":
        raise Failure("VM reset, shut down, or stopped before the controlled halt.")
    registers = qmp.call("human-monitor-command", {"command-line": "info registers"})
    rip = re.search(r"\bRIP=([0-9a-fA-F]+)", registers)
    flags = re.search(r"\b(?:RFL|EFL)=([0-9a-fA-F]+)", registers)
    if not rip or not flags or "HLT=1" not in registers or "CPL=0" not in registers:
        raise Failure("CPU is not halted at Ring 0 (QMP register inspection):\n" + registers)
    if not symbols["neu_halt_begin"] <= int(rip[1], 16) < symbols["neu_halt_end"] or int(flags[1], 16) & (1 << 9):
        raise Failure("CPU is outside Neu's halt loop or interrupts are enabled.")
    if expected_root is not None:
        cr3 = re.search(r"\bCR3=([0-9a-fA-F]+)", registers)
        if not cr3 or int(cr3[1], 16) != expected_root:
            raise Failure("QMP CR3 does not match the activated owned root.")
    return registers


def run_case(case, esp, timeout):
    logs = BUILD / "test-logs"
    logs.mkdir(parents=True, exist_ok=True)
    serial, errors = logs / f"{case}.serial.log", logs / f"{case}.qemu.log"
    serial.write_text("")
    started = time.monotonic()
    result_path = logs / f"{case}.json"
    result = {"case": case, "passed": False, "kernel_sha256": sha256(esp / "boot/neu-kernel")}
    result_path.write_text(json.dumps(result, indent=2) + "\n")
    with tempfile.TemporaryDirectory(prefix="neu-", dir="/tmp") as directory:
        temporary = Path(directory)  # Keep QMP Unix socket paths below macOS's limit.
        endpoint = temporary / "qmp.sock"
        arguments = qemu_command(esp, temporary, f"file:{serial}", endpoint)
        if case == "timer-xapic":
            arguments[arguments.index("-cpu") + 1] = "max,x2apic=off"
        result["command"] = arguments
        print("+ " + shlex.join(arguments), flush=True)
        qmp = None
        with errors.open("w") as error_log:
            process = subprocess.Popen(arguments, cwd=ROOT, stdout=error_log, stderr=error_log)
            try:
                required, forbidden = EXPECT[case]
                symbols = elf_info(temporary / "esp/boot/neu-kernel")["symbols"]
                user_observation = None
                native_observation = None
                node_observations = {}
                timer_observations = {}
                switch_observations = {}
                while required[-1] not in transcript(serial):
                    if process.poll() is not None:
                        raise Failure(f"QEMU exited early with status {process.returncode}.")
                    if time.monotonic() - started >= timeout:
                        raise Failure(f"{case}: timed out after {timeout:g}s waiting for the expected serial result.")
                    if case in USER_CASES and user_observation is None and "User space ready:" in transcript(serial):
                        qmp = QMP(endpoint, started + timeout)
                        user_observation = observe_user(qmp, serial, case, symbols, started + timeout)
                    if case in NATIVE_CASES and native_observation is None and "Native user space ready:" in transcript(serial):
                        qmp = QMP(endpoint, started + timeout)
                        native_observation = observe_native(qmp, serial, case, symbols, started + timeout)
                    if case in NODE_CASES:
                        current = transcript(serial)
                        for start in re.finditer(r"Node start: label=(\d+) node=(\d+) thread=(\d+) space=(\d+) root=(0x[0-9a-f]+) kind=(\d+)\n",current):
                            label = int(start[1])
                            if label not in node_observations and "Native user space ready:" in current[start.start():]:
                                if qmp is None: qmp = QMP(endpoint, started + timeout)
                                node_observations[label] = observe_native(qmp,serial,case,symbols,started+timeout,start.start(),int(start[6]),True)
                    if case in TIMER_POSITIVE:
                        for armed in re.finditer(r"Timer session armed: sequence=(\d+) ticks_before=(\d+) root=(0x[0-9a-f]+) reference=\d+\n",transcript(serial)):
                            sequence = int(armed[1])
                            if sequence not in timer_observations:
                                if qmp is None: qmp = QMP(endpoint, started + timeout)
                                timer_observations[sequence] = observe_timer(qmp,serial,symbols,sequence,int(armed[3],16),started+timeout)
                    if case in SWITCH_CASES:
                        current=transcript(serial)
                        selected_runs=list(re.finditer(r"Switch selected: sequence=(\d+) label=(\d+) node=(\d+) thread=(\d+) space=(\d+) root=(0x[0-9a-f]+) phase=(\d+) fresh=(true|false)\n",current))
                        for run_index,selected in enumerate(selected_runs):
                            sequence,label,_,_,_,root,phase=[int(v,0) for v in selected.groups()[:7]]
                            episode=current[selected.start():selected_runs[run_index+1].start() if run_index+1<len(selected_runs) else len(current)]
                            ready=re.search(r"Switch native ready: node=\d+ thread=\d+ space=\d+ root=(0x[0-9a-f]+) stack_bottom=(0x[0-9a-f]+) stack_top=(0x[0-9a-f]+) TSS_RSP0=(0x[0-9a-f]+) RIP=(0x[0-9a-f]+) RSP=(0x[0-9a-f]+) fresh=(true|false)\n",episode)
                            if sequence not in switch_observations and ready:
                                if qmp is None:qmp=QMP(endpoint,started+timeout)
                                number=SWITCH_CASES[case]
                                faulted=phase==1 and ((label==1 and number in (1,3,4,5)) or (label==2 and number==2))
                                switch_observations[sequence]=observe_switch(qmp,serial,symbols,selected.start(),sequence,label,phase,root,int(ready[2],16),int(ready[3],16),started+timeout,faulted)
                    time.sleep(0.01 if case in USER_CASES or case in NATIVE_CASES or case in NODE_CASES or case in TIMER_CASES or case in SWITCH_CASES else 0.05)
                if qmp is None:
                    qmp = QMP(endpoint, started + timeout)
                paging = None
                if case == "paging-bootstrap-failure":
                    match = re.search(r"Paging bootstrap rollback verified: CR3=(0x[0-9a-f]+) accounting restored", transcript(serial))
                    if not match:
                        raise Failure("Missing loader-root rollback proof.")
                    paging = {"root": int(match[1], 16), "mode": "loader root retained after rollback"}
                elif case != "missing-map":
                    match = re.search(r"Kernel paging activated: CR3=(0x[0-9a-f]+) old=(0x[0-9a-f]+) tables=(\d+)", transcript(serial))
                    if not match or int(match[1], 16) == int(match[2], 16) or int(match[3]) < 4:
                        raise Failure("Missing a distinct, owned CR3 transition.")
                    paging = {"root": int(match[1], 16), "loader_root": int(match[2], 16), "tables": int(match[3])}
                # UART output precedes HLT. Allow only the remaining test budget
                # for the CPU to arrive at the halt loop, then verify it twice.
                while True:
                    try:
                        first = check_halt(qmp, symbols, paging["root"] if paging else None)
                        break
                    except Failure:
                        if time.monotonic() - started >= timeout:
                            raise
                        time.sleep(0.05)
                time.sleep(0.1)
                second = check_halt(qmp, symbols, paging["root"] if paging else None)
                text = transcript(serial)
                cursor = 0
                for marker in required:
                    position = text.find(marker, cursor)
                    if position < 0:
                        raise Failure(f"Missing or out-of-order serial marker: {marker}")
                    cursor = position + len(marker)
                if any(marker in text for marker in forbidden):
                    raise Failure(f"Unexpected panic/failure/success marker in {case} transcript.")
                fault = check_fault(case, text, symbols)
                user = check_user(case, text, symbols, user_observation) if case in USER_CASES else None
                native = check_native(case, text, symbols, native_observation) if case in NATIVE_CASES else None
                nodes = check_nodes(case,text,symbols,node_observations) if case in NODE_CASES else None
                if nodes and (any(item["kernel_root"] != paging["root"] for item in node_observations.values()) or int(re.search(r"EFER=([0-9a-fA-F]+)",second)[1],16)&1):
                    raise Failure("Node execution did not restore kernel CR3/MSRs.")
                if native and native["kernel_root"] != paging["root"]:
                    raise Failure("Native teardown did not restore the kernel root.")
                if native and int(re.search(r"EFER=([0-9a-fA-F]+)", second)[1], 16) & 1:
                    raise Failure("Native teardown left SCE enabled.")
                if user and user["kernel_root"] != paging["root"]:
                    raise Failure("User teardown did not restore the original kernel root.")
                timer = check_timer(case,text,symbols,timer_observations,paging["root"],qmp) if case in TIMER_CASES else None
                switching = check_switch(case,text,symbols,switch_observations,paging["root"],qmp) if case in SWITCH_CASES else None
                qmp.call("quit")  # A host-controlled exit after the observed halt.
                if process.wait(timeout=5) != 0:
                    raise Failure(f"QEMU did not exit cleanly after QMP quit: {process.returncode}")
                result.update(timer=timer,switching=switching)
                result.update(passed=True, halt_observations=[first, second], exception=fault, paging=paging, user=user, native=native, nodes=nodes)
                print(f"PASS {case}: expected serial result; HLT=1, CPL=0, IF=0 in Neu halt loop; clean QMP exit", flush=True)
                return result
            except Exception as error:
                result["error"] = str(error)
                print(transcript(serial)[-4000:], file=sys.stderr)
                print(transcript(errors)[-2000:], file=sys.stderr)
                raise
            finally:
                if qmp:
                    qmp.close()
                if process.poll() is None:
                    process.terminate()
                    try:
                        process.wait(timeout=2)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait(timeout=2)
                result.update(elapsed_seconds=round(time.monotonic() - started, 3), qemu_exit=process.returncode)
                result_path.write_text(json.dumps(result, indent=2) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("build", "run", "test", "doctor", "check"))
    parser.add_argument("--timeout", type=float, default=30)
    parser.add_argument("--case", choices=FEATURES, help="Run only one smoke scenario (default: all).")
    args = parser.parse_args()
    if not math.isfinite(args.timeout) or args.timeout <= 0:
        raise Failure("Test timeout must be finite and positive.")
    if args.case and args.action != "test":
        raise Failure("--case applies only to the test command.")
    if args.action == "doctor":
        for tool in ("rustc", "cargo", "python3", "qemu-system-x86_64"):
            print(version(tool))
        code, variables = firmware()
        print(f"OVMF_CODE={code}\nOVMF_VARS={variables}\nCODE SHA256={sha256(code)}\nVARS SHA256={sha256(variables)}")
        command([require("rustup"), "target", "list", "--installed"])
    elif args.action == "build":
        build()
    elif args.action == "check":
        command([require("cargo"), "fmt", "--all", "--", "--check"])
        for feature in FEATURES.values():
            arguments = [require("cargo"), "clippy", "--locked", "--target", TARGET]
            if feature:
                arguments += ["--features", feature]
            command(arguments + ["--", "-D", "warnings"])
        command([require("cargo"), "clippy", "--locked", "--lib", "--target", host_target(), "--", "-D", "warnings"])
    elif args.action == "run":
        esp = build()
        with tempfile.TemporaryDirectory(prefix="neu-", dir="/tmp") as directory:
            command(qemu_command(esp, Path(directory), "stdio"))
    else:
        BUILD.mkdir(exist_ok=True)
        report_path = BUILD / "test-results.json"
        report = {"passed": False, "timeout_seconds": args.timeout, "results": []}
        report_path.write_text(json.dumps(report, indent=2) + "\n")
        try:
            code, variables = firmware()
            report.update(qemu=version("qemu-system-x86_64"), ovmf_code=str(code),
                          ovmf_code_sha256=sha256(code), ovmf_vars_template_sha256=sha256(variables))
            if not args.case or args.case in ("memory", "paging", "user", "native", "nodes", "timer", "switch"):
                target = host_target()
                command([require("cargo"), "test", "--locked", "--lib", "--target", target])
                report["host_tests"] = {"passed": True, "target": target}
            for case in ([args.case] if args.case else FEATURES):
                report["results"].append(run_case(case, build(case), args.timeout))
            report["passed"] = True
        except Exception as error:
            report["error"] = str(error)
            raise
        finally:
            report_path.write_text(json.dumps(report, indent=2) + "\n")
        print(f"{len(report['results'])} smoke scenarios passed; evidence: build/test-results.json", flush=True)


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        sys.exit(130)
    except (Failure, subprocess.SubprocessError, OSError, ValueError, KeyError, struct.error) as error:
        print(f"ERROR: {error}", file=sys.stderr)
        sys.exit(1)
