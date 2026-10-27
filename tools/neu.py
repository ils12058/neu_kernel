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
PREEMPT_CASES = ('preempt', 'preempt-xapic', 'preempt-a-fault', 'preempt-b-fault', 'preempt-cross', 'preempt-native', 'preempt-invalid-context', 'preempt-stack-mismatch', 'preempt-handoff', 'preempt-stale', 'preempt-unsafe', 'preempt-unexpected', 'preempt-software', 'preempt-origin', 'preempt-fpu')
PREEMPT_NEGATIVE = ("preempt-unexpected","preempt-software","preempt-origin")
FEATURES.update({name:"test-"+name for name in PREEMPT_CASES})
MULTI_CASES = ('threads', 'threads-xapic', 'threads-a1-fault', 'threads-a2-fault', 'threads-b-fault', 'threads-cross', 'threads-a-cross', 'threads-rollback', 'threads-stale', 'threads-stack', 'threads-user-guard', 'threads-return-stack')
FEATURES.update({name:"test-"+name for name in MULTI_CASES})
CAP_CASES = ('capabilities','capabilities-xapic','capabilities-a-fault','capabilities-a2-fault','capabilities-b-fault','capabilities-rollback')
FEATURES.update({name:"test-"+name for name in CAP_CASES})
FEATURES["native-cap-disabled"]="test-native-cap-disabled"
NATIVE_CASES["native-cap-disabled"]=1
AS_CASES=('address-space','address-space-xapic','address-space-a-fault','address-space-a2-fault','address-space-b-fault','address-space-rollback')
FEATURES.update({name:'test-'+name for name in AS_CASES})
FEATURES['native-abi02-disabled']='test-native-abi02-disabled'
NATIVE_CASES['native-abi02-disabled']=1
IPC_CASES=('channels','channels-xapic','channels-a-fault','channels-b-fault','channels-rollback','channels-stress')
FEATURES.update({name:'test-'+name for name in IPC_CASES})
FEATURES['native-abi03-disabled']='test-native-abi03-disabled';NATIVE_CASES['native-abi03-disabled']=1
AS_CASES+=('address-space-channel-disabled',)
FEATURES['address-space-channel-disabled']='test-address-space-channel-disabled'
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
            if label.startswith(("neu_test_", "neu_user_", "__user_kernel_", "neu_native_", "__native_", "neu_abi_", "neu_node_", "neu_irq_", "neu_timer_", "neu_switch_", "neu_context_", "neu_preempt_", "neu_multi_", "neu_cap_", "neu_as_", "neu_ipc_")) or label in ("neu_halt_begin", "neu_halt_end", "__stack_bottom", "__stack_top",
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
        # Terminal pre-entry Phase 4C negatives have no reachable user fixture;
        # the linker removes those payload sections. Earlier image checks stay strict.
        if symbols.get("neu_preempt_image")==1 and prefix+"_payload_end" not in symbols:
            prefix=None
        elif not 0 < symbols[prefix + "_payload_end"] - symbols[prefix + "_payload"] <= 4096:
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
    if "neu_preempt_payload" in symbols:
        if not 0<symbols["neu_preempt_payload_end"]-symbols["neu_preempt_payload"]<=4096:raise Failure("Preemption payload exceeds its owned RX page.")
        for name,opcode in (("neu_preempt_syscall_site",b"\x0f\x05"),("neu_context_iret",b"\x48\xcf")):
            address=symbols[name];segment=next((item for item in loads if item[3]<=address<item[3]+item[5]),None)
            if segment is None or data[segment[2]+address-segment[3]:segment[2]+address-segment[3]+len(opcode)]!=opcode:raise Failure("Missing preemption architectural transition instruction.")
    if "neu_multi_payload" in symbols:
        if not 0<symbols["neu_multi_payload_end"]-symbols["neu_multi_payload"]<=4096:raise Failure("Multiple-Thread payload exceeds its shared RX page.")
        for name,opcode in (("neu_multi_atomic_site",b"\xf0\x48\x0f\xc1"),("neu_multi_syscall_site",b"\x0f\x05"),("neu_context_iret",b"\x48\xcf")):
            address=symbols[name];segment=next((item for item in loads if item[3]<=address<item[3]+item[5]),None)
            if segment is None or data[segment[2]+address-segment[3]:segment[2]+address-segment[3]+len(opcode)]!=opcode:raise Failure("Missing actual LOCK XADD/SYSCALL/IRETQ in the multiple-Thread image.")
    if "neu_ipc_syscall_site" in symbols:
        address=symbols['neu_ipc_syscall_site'];segment=next((item for item in loads if item[3]<=address<item[3]+item[5]),None)
        if segment is None or data[segment[2]+address-segment[3]:segment[2]+address-segment[3]+2]!=b"\x0f\x05":raise Failure('Channel fixture requires real SYSCALL.')
    if "neu_as_syscall_site" in symbols:
        address=symbols["neu_as_syscall_site"];segment=next((item for item in loads if item[3]<=address<item[3]+item[5]),None)
        if segment is None or data[segment[2]+address-segment[3]:segment[2]+address-segment[3]+2]!=b"\x0f\x05":raise Failure("ABI 0.2 fixture must use actual SYSCALL.")
    if "neu_cap_syscall_site" in symbols:
        address=symbols["neu_cap_syscall_site"];segment=next((item for item in loads if item[3]<=address<item[3]+item[5]),None)
        if segment is None or data[segment[2]+address-segment[3]:segment[2]+address-segment[3]+2]!=b"\x0f\x05":raise Failure("Capability requests must use an actual SYSCALL instruction.")
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


for name in PREEMPT_CASES:
    tail=["[NEU] Preempt two runnable:","[NEU] Preempt selected:","[NEU] Preempt timer stopped:","[NEU] Preempt accounting restored:","[NEU] Preemption test passed:","[NEU] Kernel bootstrap successful","[NEU] Idle: interrupts disabled; halting"]
    if name in PREEMPT_NEGATIVE:
        vector=65 if name=="preempt-unexpected" else 48
        tail=["[NEU] Interrupt failure: vector="+str(vector)]
    EXPECT[name]=(COMMON+PROTOCOL+OWNED+PAGING+["[NEU] Preempt baseline:","[NEU] Timer controller verified:","[NEU] Timer calibrated:"]+tail,
                  ["BOOT FAILURE:","PANIC:","EXCEPTION:","Cooperative switch test passed:"]+([] if name in PREEMPT_NEGATIVE else ["Interrupt failure:"]))


for name in MULTI_CASES:
    EXPECT[name]=(COMMON+PROTOCOL+OWNED+PAGING+["[NEU] Multi baseline:","[NEU] Timer controller verified:","[NEU] Timer calibrated:",
        "[NEU] Multi Thread ready: label=1", "[NEU] Multi Thread ready: label=3", "[NEU] Multi Thread ready: label=2",
        "[NEU] Multi three runnable:","[NEU] Multi selected:","[NEU] Preempt timer stopped:","[NEU] Multi accounting restored:",
        "[NEU] Multiple Thread test passed:","[NEU] Kernel bootstrap successful","[NEU] Idle: interrupts disabled; halting"],
        ["BOOT FAILURE:","PANIC:","EXCEPTION:","Interrupt failure:","Switch yield saved:","verified=false"])


for name in CAP_CASES:
    base,forbidden=EXPECT['threads'];required=[]
    for marker in base:
        if marker=='[NEU] Preempt timer stopped:':required+=['[NEU] Cap regrant:','[NEU] Cap Node cleanup:','[NEU] Cap accounting restored:']
        required.append(marker)
        if marker=='[NEU] Multi Thread ready: label=2':required.append('[NEU] Cap bootstrap:')
    EXPECT[name]=(required,forbidden)

for name in AS_CASES:
    base,forbidden=EXPECT['threads']
    required=[]
    for marker in base:
        if marker=='[NEU] Preempt timer stopped:':required+=['[NEU] AS cleanup:','[NEU] AS accounting restored:']
        required.append(marker)
        if marker=='[NEU] Multi Thread ready: label=2':required.append('[NEU] AS bootstrap:')
    EXPECT[name]=(required,forbidden)

for name in IPC_CASES:
    required,forbidden=EXPECT['threads']
    result=[]
    for marker in required:
        if marker=='[NEU] Preempt timer stopped:':result+=['[NEU] IPC accounting restored:']
        result.append(marker)
        if marker=='[NEU] Multi Thread ready: label=2':result.append('[NEU] IPC bootstrap:')
    EXPECT[name]=(result,forbidden)


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
        snapshot=getattr(qmp,'_walk_snapshot',None)
        if snapshot is None:
            value=debug_words(qmp,table+((address>>shift)&511)*8,1,True)[0]
        else:
            # Exactly one stopped CPU: all walks in this observation read a
            # coherent physical table-page snapshot. Discard before CONT.
            if table not in snapshot:snapshot[table]=debug_words(qmp,table,512,True)
            value=snapshot[table][(address>>shift)&511]
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


def preempt_site(symbols,name):
    return USER_CODE+symbols[name]-symbols['neu_preempt_payload']


def logged_frames(text,event):
    pattern=r"Switch frame: event="+re.escape(event)+r" thread=(\d+) RIP=(0x[0-9a-f]+) RSP=(0x[0-9a-f]+) flags=(0x[0-9a-f]+) regs=([^\n]+)\n"
    result=[]
    for m in re.finditer(pattern,text):
        thread,rip,rsp,flags=[int(v,0) for v in m.groups()[:4]]
        regs=[int(v,0) for v in m[5].split(',')]
        if len(regs)!=15:raise Failure('Incomplete saved integer context.')
        result.append((thread,regs+[rip,0x3b,flags,rsp,0x33]))
    return result


def observe_preempt(qmp,serial,symbols,start,sequence,label,root,bottom,top,deadline):
    result={'sequence':sequence,'label':label,'root':root,'bottom':bottom,'top':top}
    seed=35 if label==1 else 77
    six=[0x1122334455667788,0x2233445566778899,0x33445566778899aa,0x445566778899aabb,0x5566778899aabbcc,0x66778899aabbccdd]
    markers=[v^seed for v in (0x11225566778899aa,0xaabbccddeeff1234,0x1234567887654321)]
    while time.monotonic()<deadline:
        if 'Switch common restored:' in transcript(serial)[start:]:break
        qmp.call('stop')
        try:
            cpu=qmp.call('human-monitor-command',{'command-line':'info registers'});reg=lambda key:native_register(cpu,key)
            if reg('CR3')!=root:continue
            rip=reg('RIP')
            stage=None
            if symbols['neu_preempt_restore_wait']<=rip<symbols['neu_preempt_restore_wait_end']:stage='restore'
            elif 'CPL=3' in cpu and preempt_site(symbols,'neu_preempt_loop')<=rip<preempt_site(symbols,'neu_preempt_loop_end'):stage='user'
            elif symbols['neu_preempt_irq_wait']<=rip<symbols['neu_preempt_irq_wait_end']:stage='irq'
            if stage is None or stage in result:continue
            if reg('CR0')&10!=10 or reg('CR4')&0x50600 or reg('EFER')&1!=1 or not re.search(r'FS =0000 0000000000000000',cpu) or not re.search(r'GS =0000 0000000000000000',cpu):
                raise Failure('Preemption exposed unsupported CPU/TLS/extended state.')
            tr=re.search(r'TR =0018 ([0-9a-fA-F]+)',cpu)
            if not tr or debug_words(qmp,int(tr[1],16)+4,1)[0]!=top or debug_words(qmp,symbols['neu_native_selected_top'],1)[0]!=top:
                raise Failure('Hardware TSS and selected entry stack disagree with the current Thread.')
            common=debug_words(qmp,symbols['neu_user_saved_rsp'],1)[0]
            if not symbols['__stack_bottom']<=common<symbols['__stack_top']:raise Failure('IRQ continuation lost its permanent common stack.')
            record={'cpu':cpu,'tss_rsp0':top,'common_rsp':common}
            if stage=='restore':
                if 'CPL=0' not in cpu or reg('RSP')!=top-160 or reg('RFL')&0x600:raise Failure('Invalid non-preemptible restore boundary.')
                record['frame']=debug_words(qmp,top-160,20)
                walks={str(va):switch_walk(qmp,root,va) for va in (bottom-4096,bottom,bottom+4096,top)}
                if any(walks[str(va)]['present'] for va in (bottom-4096,top)):raise Failure('Mapped private kernel stack guard.')
                for va in (bottom,bottom+4096):
                    w=walks[str(va)]
                    if not w['present'] or any(e&4 for e in w['entries']) or w['entries'][-1]&((1<<63)|0x82)!=(1<<63)|2:raise Failure('Preempted Thread stack lacks supervisor RW/NX protection.')
                record['stack_walks']=walks
                if 'x2APIC=false' in transcript(serial):
                    device=switch_walk(qmp,root,0xffffb00000000000)
                    if not device['present'] or any(e&4 for e in device['entries']) or device['entries'][-1]&((1<<63)|0x9f)!=(1<<63)|0x1b or device['physical']!=0xfee00000:
                        raise Failure('Private xAPIC mapping is not supervisor UC RW/NX.')
                    record['device_walk']=device
            elif stage=='user':
                if reg('CS')!=0x3b or reg('SS')!=0x33 or not reg('RFL')&0x200 or reg('RFL')&~0x10ad7:
                    raise Failure('Preemptible user execution has wrong privilege or unsafe flags.')
                if [reg(k) for k in ('RBX','RBP','R12','R13','R14','R15')]!=[v^seed for v in six]:raise Failure('Preempted user callee registers were corrupted.')
                if debug_words(qmp,USER_RSP-24,3)!=markers:raise Failure('Preemption lost persistent private user-stack witnesses.')
                words=debug_words(qmp,0x402000,4)
                if words[0]==0 or words[1]!=1 or words[2]!=markers[0]:raise Failure('Thread restarted or failed to make independent CPU progress.')
                record.update(words=words,markers=markers)
            else:
                if 'CPL=0' not in cpu or reg('CS')!=8 or reg('SS')!=0 or reg('RSP')!=top-176 or reg('RFL')&0x600:
                    raise Failure('Hardware user IRQ did not select the trusted private stack:\n'+cpu)
                frame=debug_words(qmp,top-176,22)
                if frame[15:17]!=[48,0] or frame[18]!=0x3b or frame[21]!=0x33 or frame[19]&0x202!=0x202 or frame[19]&~0x10ad7:
                    raise Failure('Hardware IRQ frame lost vector/origin/flags/stack.')
                if not USER_CODE<=frame[17]<preempt_site(symbols,'neu_preempt_payload_end') or not 0x404000<=frame[20]<0x405000:
                    raise Failure('Actual interrupted RIP/RSP is outside owned code/stack.')
                lapic=qmp.call('human-monitor-command',{'command-line':'info lapic'})
                if not re.search(r'LVTT\s+0x00000030',lapic) or not re.search(r'ISR\s+48\b',lapic) or 'APIC enabled' not in lapic or 'TPR 0x00' not in lapic:
                    raise Failure('IRQ capture lacks independent unmasked LAPIC/in-service vector 48 evidence:\n'+lapic)
                if debug_words(qmp,symbols['neu_native_busy'],1)[0]!=0:raise Failure('Timer interrupted an unfinished native kernel entry.')
                record.update(frame=frame,lapic=lapic,markers=debug_words(qmp,USER_RSP-24,3))
                if record['markers']!=markers:raise Failure('IRQ selected another Thread user stack.')
            result[stage]=record
            if all(key in result for key in ('restore','user','irq')):return result
        finally:
            qmp.call('cont')
        time.sleep(0.001)
    else:raise Failure('Timed out observing a preemption slice.')
    if 'restore' not in result:raise Failure('Missing actual restore-frame observation.')
    return result


def check_preempt_loop_registers(case,frame,seed,symbols):
    if case=='preempt-native' or not preempt_site(symbols,'neu_preempt_loop')<=frame[15]<preempt_site(symbols,'neu_preempt_loop_end'):return
    # All fourteen other GPRs are invariant in the ordinary CPU-bound loop.
    # Checking actual IRQ frames after resumption detects a wrong POP order even
    # when the copied privileged restore-frame bytes themselves were correct.
    expected=[0x66778899aabbccdd^seed,0x5566778899aabbcc^seed,0x445566778899aabb^seed,0x33445566778899aa^seed,
              0xaabbccddeeff0011,44,66,55,11,22,0x2233445566778899^seed,0x778899aabbccddee,0x401000,0x1122334455667788^seed]
    if frame[:14]!=expected or frame[14]==0:raise Failure('An ordinary interrupted/resumed loop lost a supported integer register.')


def check_preempt(case,text,symbols,observations,kernel_root,qmp):
    final_lapic=qmp.call('human-monitor-command',{'command-line':'info lapic'})
    calibrated=re.search(r'Timer calibrated: PIT_Hz=(\d+) reference=(\d+) samples=(\d+),(\d+),(\d+) divided_Hz=(\d+) initial=(\d+) divide=16 period_us=10000',text)
    if not calibrated:raise Failure('Missing provisional 10 ms reference calibration.')
    hz,ref,a,b,c,divided,initial=map(int,calibrated.groups())
    if hz!=1193182 or ref!=11932 or divided!=sorted((a,b,c))[1]*hz//ref or initial!=(divided*10000+500000)//1000000 or max(a,b,c)-min(a,b,c)>sorted((a,b,c))[1]//4:raise Failure('Invalid measured clockevent calibration.')
    if case in PREEMPT_NEGATIVE:
        vector=65 if case=='preempt-unexpected' else 48
        reason='unsupported kernel origin' if case=='preempt-origin' else 'unexpected or non-hardware event'
        if f'Interrupt failure: vector={vector} reason={reason} ticks=0 eois=0 IF=0' not in text or 'Preempt selected:' in text or 'Preemption test passed:' in text:
            raise Failure('Preemption negative was not rejected at the intended invariant.')
        if not re.search(r'LVTT\s+0x00010030.*masked',final_lapic) or 'TPR 0xff' not in final_lapic or not re.search(r'initial_count = 0 current_count = 0',final_lapic):raise Failure('Fatal IRQ rejection did not mask/stop the source.')
        if case=='preempt-origin':
            if not re.search(r'ISR\s+48\b',final_lapic):raise Failure('Unsupported-origin test did not use a real hardware event.')
        elif not re.search(r'ISR\s+\(none\)',final_lapic):raise Failure('Software negative unexpectedly acknowledged a real IRQ.')
        return {'expected_rejection':reason,'vector':vector,'final_lapic':final_lapic}
    ready=re.findall(r'Switch Thread ready: label=(\d+) node=(\d+) thread=(\d+) space=(\d+) root=(0x[0-9a-f]+) bottom=(0x[0-9a-f]+) top=(0x[0-9a-f]+) pa0=(0x[0-9a-f]+) pa1=(0x[0-9a-f]+) code_pa=(0x[0-9a-f]+) data_pa=(0x[0-9a-f]+) user_stack_pa=(0x[0-9a-f]+) seed=(\d+)',text)
    if len(ready)!=2:raise Failure('Missing two independently owned initial Threads.')
    threads={int(v[0]):dict(zip(('label','node','thread','space','root','bottom','top','pa0','pa1','code','data','user_stack','seed'),map(lambda x:int(x,0),v))) for v in ready}
    if set(threads)!={1,2} or any(threads[1][k]==threads[2][k] for k in ('node','thread','space','root','bottom','top')):raise Failure('Preemptible Threads share identity or address space.')
    if {threads[1][k] for k in ('pa0','pa1','code','data','user_stack')} & {threads[2][k] for k in ('pa0','pa1','code','data','user_stack')}:raise Failure('Cross-Node backing aliases.')
    selected=list(re.finditer(r'Preempt selected: sequence=(\d+) label=(\d+) node=(\d+) thread=(\d+) space=(\d+) root=(0x[0-9a-f]+) fresh=(true|false) peer=(true|false)\n',text))
    fifo=[1,2];saved={};counts={1:0,2:0};work={1:0,2:0};outcomes={};events=[];order=[];user_samples=[]
    for i,m in enumerate(selected):
        seq,label,node,thread,space,root=map(lambda x:int(x,0),m.groups()[:6]);record=threads[label]
        if seq!=i+1 or not fifo or fifo.pop(0)!=label or tuple((node,thread,space,root))!=tuple(record[k] for k in ('node','thread','space','root')) or (m[8]=='true')!=bool(fifo):raise Failure('Preemption departed from single-Running FIFO ownership.')
        order.append(label);episode=text[m.end():selected[i+1].start() if i+1<len(selected) else len(text)]
        taken=logged_frames(episode,'preempt-taken')
        if len(taken)!=1 or taken[0][0]!=thread:raise Failure('Missing unique consumed saved context.')
        frame=taken[0][1]
        if label in saved:
            if frame!=saved[label] or m[7]!='false':raise Failure('A preempted Thread restarted or changed its full saved context.')
        elif m[7]!='true' or frame!=[0]*15+[USER_CODE,0x3b,0x202,USER_RSP,0x33]:raise Failure('Invalid initial preemptible context.')
        rejected=re.search(r'Preempt rejected before entry: kind=(\d+) thread=(\d+) root=(0x[0-9a-f]+) IF=0',episode)
        if rejected:
            kind={'preempt-invalid-context':1,'preempt-stack-mismatch':2,'preempt-unsafe':3}.get(case)
            if not kind or int(rejected[1])!=kind or int(rejected[2])!=thread or int(rejected[3],16)!=kernel_root or seq in observations or 'Switch native ready:' in episode:raise Failure('Invalid saved publication reached user/native activation.')
        else:
            if seq not in observations or observations[seq]['restore']['frame']!=frame:raise Failure('Actual privileged restore frame differs from the owned saved context.')
            obs=observations[seq]
            if 'user' in obs:user_samples.append((label,seq,obs['user']['words'][0]))
            if 'irq' in obs:
                raw=obs['irq']['frame'];actual=raw[:15]+raw[17:]
                logged=logged_frames(episode,'irq')
                if (thread,actual) not in logged:raise Failure('QMP hardware frame was not the actual saved IRQ context.')
        irq_matches=list(re.finditer(r'Preempt IRQ: tick=(\d+) sequence=(\d+) node=(\d+) thread=(\d+) space=(\d+) root=(0x[0-9a-f]+) RIP=(0x[0-9a-f]+) RSP=(0x[0-9a-f]+) flags=(0x[0-9a-f]+) CS=(0x[0-9a-f]+) SS=(0x[0-9a-f]+) handler=(0x[0-9a-f]+) vector=48 ISR_before=1 ISR_after=0 switch=(true|false)\n',episode))
        irq_frames=logged_frames(episode,'irq')
        if len(irq_frames)!=len(irq_matches):raise Failure('IRQ count and integer-context trace differ.')
        for irq,(owner,words) in zip(irq_matches,irq_frames):
            tick,sequence,n,t,s,r,rip,rsp,flags,cs,ss,handler=map(lambda x:int(x,0),irq.groups()[:-1]);switch=irq[13]=='true'
            if tick!=len(events)+1 or (sequence,n,t,s,r)!=(seq,node,thread,space,root) or cs!=0x3b or ss!=0x33 or flags&0x202!=0x202 or flags&~0x10ad7 or not record['bottom']<=handler<record['top']-176 or switch!=bool(fifo) or owner!=thread or words[15:]!=[rip,cs,flags,rsp,ss]:raise Failure('IRQ ownership/origin/acknowledgment/frame mismatch.')
            check_preempt_loop_registers(case,words,record['seed'],symbols)
            events.append({'tick':tick,'sequence':seq,'label':label,'frame':words,'switched':switch})
        yielded=logged_frames(episode,'preempt-saved')
        if yielded:
            if len(yielded)!=1 or not irq_matches or not fifo or yielded[0]!=(thread,irq_frames[-1][1]):raise Failure('Timer handoff did not save the true last interrupt frame.')
            if seq not in observations or 'irq' not in observations[seq]:raise Failure('A scheduling handoff lacks independent hardware IRQ evidence.')
            saved[label]=yielded[0][1];counts[label]+=1;fifo.append(label)
            progress=re.search(r'Preempt progress: label=(\d+) preemptions=(\d+) work=(\d+) boots=1 queue=(\d+)',episode)
            if not progress or tuple(map(int,(progress[1],progress[2],progress[4])))!=(label,counts[label],len(fifo)) or int(progress[3])<=work[label]:raise Failure('Non-yielding computation failed to progress independently.')
            work[label]=int(progress[3])
            if counts[label]<=4 and case!='preempt-native' and re.search(r'Native terminal:.*entries=[1-9]',episode):raise Failure('Cooperative/syscall work caused the purported non-yielding alternation.')
        else:
            outcome=re.search(r'Preempt outcome: label=(\d+) thread=(\d+) work=(\d+) boots=1 reason=(normal|fault) terminal_dispatch=rejected',episode)
            if not outcome or int(outcome[1])!=label or int(outcome[2])!=thread or int(outcome[3])<work[label] or (not rejected and int(outcome[3])==work[label]):raise Failure('Missing current-Thread terminal outcome/progress.')
            if f'Switch reclaimed: label={label} thread={thread} space={space} saved=false queued=false outcome=retained' not in episode:raise Failure('Terminated context still owns reclaimed resources.')
            if fifo and 'Switch survivor retained:' not in episode:raise Failure('Healthy suspended context did not survive sibling teardown.')
            expected_fault=(label==1 and case in ('preempt-a-fault','preempt-cross','preempt-invalid-context','preempt-stack-mismatch','preempt-unsafe','preempt-fpu')) or (label==2 and case=='preempt-b-fault')
            if (outcome[4]=='fault')!=expected_fault:raise Failure('Wrong Node terminal disposition.')
            if expected_fault and not rejected:
                fault=re.search(r'Node user fault: node=(\d+) thread=(\d+) space=(\d+) root=(0x[0-9a-f]+) vector=(\d+) error=(0x[0-9a-f]+) RIP=(0x[0-9a-f]+) CR2=(0x[0-9a-f]+) CS=(0x[0-9a-f]+) SS=(0x[0-9a-f]+) RSP=(0x[0-9a-f]+) RFLAGS=(0x[0-9a-f]+) handler_rsp=(0x[0-9a-f]+)',episode)
                vector,target,site=(14,0x406000,'neu_preempt_cross_site') if case=='preempt-cross' else (7,0,'neu_preempt_fpu_site') if case=='preempt-fpu' else (6,0,'neu_preempt_fault_site')
                if not fault:raise Failure('Missing actual preempted user CPU fault.')
                values=list(map(lambda x:int(x,0),fault.groups()))
                if tuple(values[:7])!=(node,thread,space,root,vector,4 if vector==14 else 0,preempt_site(symbols,site)) or values[8:11]!=[0x3b,0x33,USER_RSP-24] or (vector==14 and values[7]!=target) or not record['bottom']<=values[12]<record['top']:raise Failure('Fault was fabricated or bound to the wrong preempted Node.')
            if expected_fault and not rejected and not fifo:raise Failure('Fault test did not leave a healthy suspended sibling.')
            outcomes[label]={'reason':outcome[4],'work':int(outcome[3])}
    if fifo or set(outcomes)!={1,2} or order[:4]!=[1,2,1,2]:raise Failure('Missing repeated genuine A/B preemption or final empty FIFO.')
    minimum=1 if case in ('preempt-invalid-context','preempt-stack-mismatch','preempt-unsafe') else 4
    if min(counts.values())<minimum or any(sum(label==owner for owner,_,_ in user_samples)<(1 if minimum==1 and label==1 else 2) for label in (1,2)):raise Failure('Insufficient independently observed interrupted/resumed Ring 3 progress.')
    if not any(not event['switched'] for event in events):raise Failure('Missing no-peer hardware return without a scheduler switch.')
    if case=='preempt-handoff' and 'Preempt handoff negatives: duplicate=Busy wrong_owner=Identity original=retained' not in text:raise Failure('Missing duplicate/wrong-owner handoff rejection.')
    if case=='preempt-stale' and 'Switch stale identity rejected:' not in text:raise Failure('Missing genuine reused-generation rejection.')
    stopped=re.search(r'Preempt timer stopped: ticks=(\d+) eois=(\d+) user=(\d+) drain=(\d+) ISR=0 IRR=0 initial=0 current=0 SVR=disabled IF=0',text)
    done=re.search(r'Preemption test passed: dispatches=(\d+) captures=(\d+) queue=empty running=none IF=0',text)
    baseline=re.search(r'Preempt baseline: allocated=(\d+) free=(\d+) tables=(\d+) root=(0x[0-9a-f]+)',text)
    restored=re.search(r'Preempt accounting restored: allocated=(\d+) free=(\d+) tables=(\d+)',text)
    if not stopped or not done or not baseline or not restored or baseline.groups()[:3]!=restored.groups() or int(baseline[4],16)!=kernel_root:raise Failure('Missing exact resource and timer cleanup.')
    ticks,eois,user,drain=map(int,stopped.groups())
    if ticks!=eois or user!=len(events) or ticks!=user+drain or drain>1 or tuple(map(int,done.groups()))!=(len(selected),len(events)):raise Failure('Timer events, EOI and scheduler counts were conflated.')
    if not re.search(r'LVTT\s+0x00010030.*masked',final_lapic) or 'APIC disabled' not in final_lapic or 'TPR 0xff' not in final_lapic or not re.search(r'ISR\s+\(none\)',final_lapic) or not re.search(r'IRR\s+\(none\)',final_lapic) or not re.search(r'initial_count = 0 current_count = 0',final_lapic):raise Failure('Timer did not stop and drain before final halt.')
    for name in ('neu_native_busy','neu_native_selected_top','neu_native_saved_user_rsp','neu_user_saved_rsp'):
        if debug_words(qmp,symbols[name],1)[0]!=0:raise Failure('Stale active native/current stack state at halt.')
    if debug_words(qmp,symbols['neu_preempt_busy'],1)[0]&255:raise Failure('Stale IRQ handoff guard at halt.')
    if int(re.search(r'EFER=([0-9a-fA-F]+)',qmp.call('human-monitor-command',{'command-line':'info registers'}))[1],16)&1:raise Failure('Preemption left native SCE enabled.')
    return {'threads':threads,'order':order,'preemptions':counts,'events':events,'outcomes':outcomes,'user_samples':user_samples,'observations':observations,'calibration':{'samples':[a,b,c],'divided_hz':divided,'initial':initial},'ticks':ticks,'eois':eois,'drain_ticks':drain,'accounting':list(map(int,restored.groups())),'final_lapic':final_lapic}


def multi_site(symbols,name):
    return USER_CODE+symbols[name]-symbols['neu_multi_payload']


def multi_committed(stored,frame,symbols):
    if frame is not None:
        if frame[15]==multi_site(symbols,'neu_multi_after_atomic'):
            if frame[14]!=stored:raise Failure('Atomic commit-before-INC witness is inconsistent.')
            return stored+1
        if frame[15]==multi_site(symbols,'neu_multi_after_inc'):
            if frame[14]!=stored+1:raise Failure('Atomic INC-before-store witness is inconsistent.')
            return frame[14]
    return stored


def multi_registers(frame,record,symbols):
    if not multi_site(symbols,'neu_multi_loop')<=frame[15]<multi_site(symbols,'neu_multi_loop_end'):return
    seed=record['seed'];label=record['label']
    expected={0:0x66778899aabbccdd^seed,1:0x5566778899aabbcc^seed,2:0x445566778899aabb^seed,3:0x33445566778899aa^seed,
              4:0xaabbccddeeff0011^seed,5:44,6:66,7:55,8:11,9:22,10:0x2233445566778899^seed,11:0x402040+(label-1)*32,13:0x1122334455667788^seed}
    if any(frame[index]!=value for index,value in expected.items()):raise Failure('Independent Thread GPR witness was corrupted across preemption.')


def observe_cap_request(qmp,cpu,symbols,record):
    reg=lambda key:native_register(cpu,key)
    words=debug_words(qmp,symbols['neu_cap_witness'],24)
    node,thread,space,root=(record[k] for k in ('node','thread','space','root'))
    if words[1:6]!=[node,thread,space,root,node] or 'CPL=0' not in cpu or reg('CS')!=8 or reg('SS')!=0x10 or reg('RFL')&0x77700 or reg('CR3')!=root or not record['bottom']<=reg('RSP')<record['top']-160:raise Failure('Native capability request lost its trusted Node/root/stack binding.')
    if reg('CR0')&10!=10 or reg('CR4')&0x50600 or reg('EFER')&1!=1:raise Failure('Capability request exposed unsupported CPU state.')
    if words[20:22]!=[record['top'],record['top']-160] or words[22]&0x600 or words[23]!=1:raise Failure('Capability witness was not published in an exclusive IF=0 native transaction.')
    tr=re.search(r'TR =0018 ([0-9a-fA-F]+)',cpu)
    if not tr or debug_words(qmp,int(tr[1],16)+4,1)[0]!=record['top'] or debug_words(qmp,symbols['neu_native_selected_top'],1)[0]!=record['top'] or debug_words(qmp,symbols['neu_native_busy'],1)[0]!=1:raise Failure('Capability operation used the wrong TSS/native entry ownership.')
    frame=debug_words(qmp,words[21],20)
    if frame[14]!=words[6] or frame[8:10]!=words[7:9] or frame[6]!=0x100000001 or frame[15:17]!=[words[18],0x3b] or frame[18:]!=[words[19],0x33] or frame[17]&0x202!=0x202 or words[18]!=multi_site(symbols,'neu_cap_syscall_resume') or not record['user_base']<=words[19]<=record['initial_rsp']:raise Failure('Capability request did not originate in the selected real Ring 3 syscall frame.')
    walks={}
    storage=symbols['neu_cap_storage'];size=debug_words(qmp,symbols['neu_cap_storage_bytes'],1)[0]
    for address in (symbols['neu_cap_witness'],storage):
        w=switch_walk(qmp,root,address)
        if not w['present'] or any(e&4 for e in w['entries']) or w['entries'][-1]&((1<<63)|0x82)!=(1<<63)|2:raise Failure('Capability metadata is not supervisor-only RW/NX storage.')
        walks[str(address)]=w
    if words[16]:
        if not storage<=words[16]<storage+size:raise Failure('Object value is outside the actual fixed registry storage.')
        actual=debug_words(qmp,words[16],1)[0]
        if words[12]==2:actual&=255
        if actual!=words[17] or (words[9]==0 and words[15]!=1):raise Failure('Observed object storage/value/pin differs from the authorized operation.')
        walks[str(words[16])]=switch_walk(qmp,root,words[16])
    elif words[9]==0:raise Failure('Successful observed operation did not use real typed object storage.')
    return {'cpu':cpu,'words':words,'frame':frame,'walks':walks,'storage':storage,'storage_bytes':size}


def observe_as_request(qmp,cpu,symbols,record):
    reg=lambda k:native_register(cpu,k)
    w=debug_words(qmp,symbols['neu_as_witness'],24)
    if w[1:6]!=[record[k] for k in ('node','thread','space','root')]+[record['node']] or 'CPL=0' not in cpu or reg('CS')!=8 or reg('SS')!=0x10 or reg('CR3')!=record['root'] or reg('RFL')&0x77700 or not record['bottom']<=reg('RSP')<record['top']-160:raise Failure('ABI 0.2 lost trusted caller/root/privilege binding.')
    if reg('CR0')&10!=10 or reg('CR4')&0x50600 or reg('EFER')&1!=1:raise Failure('ABI 0.2 exposed unsupported CPU state.')
    tr=re.search(r'TR =0018 ([0-9a-fA-F]+)',cpu)
    if not tr or debug_words(qmp,int(tr[1],16)+4,1)[0]!=record['top'] or debug_words(qmp,symbols['neu_native_selected_top'],1)[0]!=record['top'] or debug_words(qmp,symbols['neu_native_busy'],1)[0]!=1:raise Failure('ABI 0.2 lost selected TSS/entry stack.')
    f=debug_words(qmp,w[21],20)
    if w[20:22]!=[record['top'],record['top']-160] or w[22]&0x600 or w[23]!=1 or f[14]!=w[6] or f[8:10]!=w[7:9] or f[15:17]!=[w[18],0x3b] or f[18:]!=[w[19],0x33] or f[17]&0x202!=0x202 or w[18]!=multi_site(symbols,'neu_as_syscall_resume'):raise Failure('ABI 0.2 did not use actual Ring 3 SYSCALL frame.')
    metadata=switch_walk(qmp,record['root'],symbols['neu_as_witness'])
    if not metadata['present'] or any(e&4 for e in metadata['entries']) or metadata['entries'][-1]&((1<<63)|2)!=(1<<63)|2:raise Failure('ABI evidence storage is not supervisor RW/NX.')
    walk=None
    if w[6]==0x12 and w[9]==0:
        if w[12:14]!=[3,16] or w[15]!=1 or w[16:18]!=[record['space'],record['root']]:raise Failure('Inspection did not pin the live typed AddressSpace.')
        walk=switch_walk(qmp,record['root'],w[8]);actual=0
        if walk['present']:
            e=walk['entries'];actual=1|(2 if all(v&4 for v in e) else 0)|(4 if all(v&2 for v in e) else 0)|(8 if not any(v&(1<<63) for v in e) else 0)
        if actual!=w[10]:raise Failure('Returned mapping attributes differ from independent live physical PTE walk.')
    elif w[10]!=0:raise Failure('Rejected inspection disclosed a mapping result.')
    return {'cpu':cpu,'words':w,'frame':f,'mapping_walk':walk,'metadata_walk':metadata}


def check_address_space(case,text,symbols,members,state):
    a=members['threads'][1]['node'];b=members['threads'][3]['node']
    if f'AS bootstrap: A={a} B={b} grants=INSPECT,INSPECT,NONE,Flag.READ handles=4,4 objects=4 references=8 root_refs=2 ABI=0.2' not in text:raise Failure('Missing explicit separate bootstrap authority.')
    rows=[list(map(lambda v:int(v,0),m.split(','))) for m in re.findall(r'AS record: words=([^\n]+)',text)]
    if not rows or any(len(w)!=24 or w[0]!=i+1 for i,w in enumerate(rows)):raise Failure('Invalid structured ABI request ledger.')
    tables={a:{},b:{}};objects={};grants={};errors=set();native=state['native'];closed=False;shared=False;post=False;different={};before_stack=False;post_b=False
    for w in rows:
        seq,node,thread,space,root,table,op,arg,va,status,result,obj,kind,rights,refs,pins,sid,realroot,rip,rsp,top,frame,flags,probe=w
        status=status-(1<<64) if status>>63 else status
        owner=next((r for r in members['threads'].values() if r['thread']==thread),None)
        if owner is None or (node,space,root,table)!=(owner['node'],owner['space'],owner['root'],node) or top!=owner['top'] or frame!=top-160 or flags&0x600 or rip!=multi_site(symbols,'neu_as_syscall_resume'):raise Failure('ABI record has invalid active identity or frame.')
        if op==0x10:
            if status==0:
                if arg>=4 or not result or result>>32!=1 or result&0xffffffff!=arg+1:raise Failure('Bootstrap did not return the existing bounded local grant.')
                if node==a and arg==0 and closed:raise Failure('Discovery recreated closed authority.')
                if (node,arg) in grants and grants[node,arg]!=result:raise Failure('Discovery changed existing authority.')
                grants[node,arg]=result;tables[node][result]=arg
            elif arg>=4 and status!=-16 or arg<4 and status!=-17:raise Failure('Bootstrap failure contract differs.')
        elif op==0x11:
            if status==0:
                if arg not in tables[node]:raise Failure('Close succeeded without local authority.')
                del tables[node][arg]
                if node==a:closed=True
            elif status!=-17:raise Failure('Double close did not reject a stale generation.')
        elif op==0x12:
            if status==0:
                if arg not in tables[node] or tables[node][arg] not in (0,1) or kind!=3 or rights!=16 or pins!=1 or refs not in (2,3) or (sid,realroot)!=(space,root):raise Failure('Inspection succeeded without typed local INSPECT authority and live root pin.')
                if obj in objects and objects[obj]!=(node,space,root):raise Failure('Equal local handles aliased foreign AddressSpace authority.')
                objects[obj]=(node,space,root)
                expected={0x400000:11,0x401000:3,0x402000:7,0x406000:3 if node==b else 0,0x410000:0 if node==b else 7,0x404000:7 if not post else 0}.get(va)
                if va==0x404000 and result==0:
                    # Actual mapping changed following precise Thread-stack retirement.
                    post=True;expected=0
                    if not before_stack or 'Multi Thread retired: label=1' not in text:raise Failure('No before/after live stack inspection.')
                elif va==0x404000:before_stack=True
                if expected is None or result!=expected:raise Failure('Wrong actual mapping result.')
                if va==0x406000:different[node]=result
                if owner['label']==2 and tables[node][arg]==0:shared=True
                if owner['label']==3 and rsp==owner['initial_rsp']-80 and va==0x406000:
                    if not closed or result!=3 or not probe:raise Failure('B did not retain independently observed authority after A closed its handle.')
                    post_b=True
                    if case in ('address-space-a-fault','address-space-a2-fault'):
                        cleanup_position=text.find(f'AS cleanup: node={a} ')
                        probe_position=text.find(f'AS probe: sequence={seq} node={b} ')
                        if cleanup_position<0 or probe_position<cleanup_position:raise Failure('Healthy B did not inspect its live root after faulted A capability cleanup.')
            else:
                slot=arg&0xffffffff;generation=arg>>32
                if slot==0 or slot>8 or generation==0:expected_status=-16
                elif slot<=4 and (generation!=1 or (node,slot-1) in grants and grants[node,slot-1] not in tables[node]):expected_status=-17
                elif slot>4:expected_status=-17 if case=='address-space-rollback' and node==a else -16
                elif slot==4:expected_status=-19
                elif slot==3:expected_status=-18
                elif va%4096 or not 0x400000<=va<0x800000:expected_status=-2
                else:raise Failure('A valid inspection was rejected without a policy reason.')
                if status!=expected_status or result!=0:raise Failure('Rejected ABI request differed from independent handle/type/rights/range policy or disclosed mapping details.')
        else:raise Failure('Private operation was published as ABI 0.2.')
        if status<0:errors.add(status)
        if probe:
            if seq not in native or native[seq]['words']!=w:raise Failure('Missing matching paused QMP CPU/PTE observation for published probe.')
    if not {-2,-16,-17,-18,-19}<=errors or not closed or not shared or different!={a:0,b:3} or len(objects)!=2:raise Failure('Incomplete real object authorization, shared authority or numeric-handle isolation.')
    if case not in ('address-space-a-fault','address-space-a2-fault') and not post:raise Failure('No inspection after A1 exit and stack retirement.')
    if case!='address-space-b-fault' and not post_b:raise Failure('Missing B inspection after A close/cleanup.')
    cleanup=list(re.finditer(r'AS cleanup: node=(\d+) handles=(\d+) objects_before=(\d+) after=(\d+) references_before=(\d+) after=(\d+) pins=0 root_ref=released lease=none',text))
    if len(cleanup)!=2 or {int(m[1]) for m in cleanup}!={a,b} or sum(int(m[2]) for m in cleanup)!=7 or tuple(map(int,cleanup[-1].groups()[3::2]))!=(0,0):raise Failure('AddressSpace cleanup leaked references or crossed Node ownership.')
    for m in cleanup:
        if text.index(f'Multi Node reclaimed: node={m[1]}')<m.end():raise Failure('Page tables reclaimed before capability root references.')
    if f'AS accounting restored: objects=0 references=0 pins=0 root_refs=0 leases=0 requests={len(rows)}' not in text:raise Failure('Missing final object/reference/pin accounting.')
    if 'RDX=0x20000001f' not in text or 'op=0xfffffffffffffd00' not in text:raise Failure('Missing ABI 0.2 feature query or private-operation rejection.')
    if case=='address-space-rollback' and 'AS rollback: objects=full table=full manifest=duplicate references=restored root_refs=2' not in text:raise Failure('Missing actual bounded bootstrap rollback.')
    return {'requests':rows,'native':native,'errors':sorted(errors),'post_a1_exit':post,'b_after_a_close_or_fault':post_b,'different_va_406000':different,'objects_final':0,'references_final':0,'root_refs_final':0,'pins_final':0}


def observe_ipc_request(qmp,cpu,symbols,record):
    reg=lambda k:native_register(cpu,k);w=debug_words(qmp,symbols['neu_ipc_witness'],64)
    if w[1:6]!=[record[k] for k in ('node','thread','space','root')]+[record['node']] or 'CPL=0' not in cpu or reg('CS')!=8 or reg('SS')!=0x10 or reg('CR3')!=record['root'] or reg('RFL')&0x77700 or not record['bottom']<=reg('RSP')<record['top']-160:raise Failure('Channel operation lost trusted caller/root/privilege binding.')
    if reg('CR0')&10!=10 or reg('CR4')&0x50600 or reg('EFER')&1!=1:raise Failure('Unsupported Channel CPU state.')
    tr=re.search(r'TR =0018 ([0-9a-fA-F]+)',cpu)
    if not tr or debug_words(qmp,int(tr[1],16)+4,1)[0]!=record['top'] or debug_words(qmp,symbols['neu_native_selected_top'],1)[0]!=record['top'] or debug_words(qmp,symbols['neu_native_busy'],1)[0]!=1:raise Failure('Channel TSS/native ownership mismatch.')
    frame=debug_words(qmp,w[21],20)
    if w[20:22]!=[record['top'],record['top']-160] or w[22]&0x600 or w[23]!=1 or frame[14]!=w[6] or frame[8:10]!=w[7:9] or frame[15:17]!=[w[18],0x3b] or frame[18:]!=[w[19],0x33] or frame[17]&0x202!=0x202 or w[18]!=multi_site(symbols,'neu_ipc_syscall_resume'):raise Failure('Missing actual Ring 3 Channel SYSCALL frame.')
    queues=[];walks=[]
    if w[26]==w[27] or not w[26] or not w[27]:raise Failure('Directional queues alias or have no real storage.')
    for i,address in enumerate(w[26:28]):
        walk=switch_walk(qmp,record['root'],address)
        if not walk['present'] or any(e&4 for e in walk['entries']) or walk['entries'][-1]&((1<<63)|2)!=(1<<63)|2:raise Failure('Channel FIFO is not supervisor-owned RW/NX storage.')
        actual=debug_words(qmp,address,5)
        if actual!=w[34+5*i:39+5*i]:raise Failure('Actual FIFO head/tail/length/payload differs from operation witness.')
        queues.append(actual);walks.append(walk)
    if w[15] and (w[15]!=1 or w[12]!=4 or w[24]>1 or w[25]!=record['node']):raise Failure('Authorized Channel operation did not pin its owned typed endpoint.')
    return {'cpu':cpu,'words':w,'frame':frame,'queues':queues,'walks':walks}


def check_ipc(case,text,symbols,members,state):
    a=members['threads'][1]['node'];b=members['threads'][3]['node'];owners=[a,b]
    if f'IPC bootstrap: A={a} B={b} objects=6 references=16 handles=8,8 endpoints=2 channels=1 queues=2 capacity=2 ABI=0.3' not in text:raise Failure('Missing complete trusted Channel bootstrap.')
    records=[list(map(lambda v:int(v,0),s.split(','))) for s in re.findall(r'IPC record: words=([^\n]+)',text)]
    caps={};generations={}
    for node in owners:
        generations[node]=[1]*8
        if case=='channels-rollback' and node==b:generations[node][4]=2
        caps[node]={((generations[node][slot]<<32)|(slot+1)):slot for slot in range(8)}
    queues=[[0,0,0,0,0],[0,0,0,0,0]];refs=[4,4];ends=[0x101,0x102];sent=[0,0];received=[0,0];discard=[0,0];closed=set();errors=set();native=state['native'];successful=[];after_exit=False;after_fault=False
    def retire(side):
        discard[side]+=queues[side][2];queues[side]=[0,0,0,0,0];ends[side]=0
    def snapshot():return [0x101,int(any(ends)),*ends,a,b,*queues[0],*queues[1],*sent,*received,*discard]
    for seq,w in enumerate(records,1):
        if len(w)!=64 or w[0]!=seq:raise Failure('Malformed Channel transaction ledger.')
        node,thread,space,root,table,op,h,value,status,scalar=w[1:11];status=status-(1<<64) if status>>63 else status
        side=owners.index(node)
        if w[50:60]!=queues[0]+queues[1] or w[60]!=int(bool(ends[0]))+2*int(bool(ends[1])):raise Failure('Channel request before-state differs from prior committed messages/close.')
        if op==(1<<64)-2:
            if node in closed:raise Failure('Duplicate Node cleanup.')
            for slot in list(caps[node].values()):
                if slot>=4:refs[side]-=1
            caps[node].clear();closed.add(node)
            if refs[side]==0 and ends[side]:retire(side)
        else:
            owner=next((r for r in members['threads'].values() if r['thread']==thread),None)
            if owner is None or node in closed or (node,space,root,table)!=(owner['node'],owner['space'],owner['root'],node) or w[20:22]!=[owner['top'],owner['top']-160] or w[22]&0x600 or w[18]!=multi_site(symbols,'neu_ipc_syscall_resume'):raise Failure('IPC used foreign or terminal execution authority.')
            slot=h&0xffffffff;generation=h>>32;entry=caps[node].get(h)
            expected=0
            if slot==0 or slot>8 or generation==0:expected=-16
            elif generation!=generations[node][slot-1] or entry is None:expected=-17
            if op==0x11:
                if expected==0:
                    del caps[node][h]
                    if entry>=4:
                        refs[side]-=1
                        if refs[side]==0:retire(side)
            elif op in (0x13,0x14):
                if expected==0 and entry<4:expected=-19
                if expected==0 and (op==0x13 and entry==7 or op==0x14 and entry==6):expected=-18
                if expected==0:
                    if w[11]!=((4<<40)|(1<<8)|(5+side)) or w[12]!=4 or w[13]!={4:96,5:96,6:32,7:64}[entry] or w[14:18]!=[refs[side],1,0x101+side,0x101] or w[24:26]!=[side,node]:raise Failure('Endpoint identity/rights/reference/Node pin mismatch.')
                    q=queues[1-side if op==0x13 else side]
                    if op==0x13:
                        if not ends[1-side]:expected=-34
                        elif q[2]==2:expected=-33
                        else:q[3+q[1]]=value;q[1]=(q[1]+1)%2;q[2]+=1;sent[side]+=1;successful.append((seq,'send',side,value,owner['label']))
                    else:
                        if not q[2]:expected=-32 if ends[1-side] else -34
                        else:
                            delivered=q[3+q[0]];q[3+q[0]]=0;q[0]=(q[0]+1)%2;q[2]-=1;received[side]+=1
                            if scalar!=delivered:raise Failure('Receive scalar has no corresponding successful FIFO send.')
                            successful.append((seq,'receive',side,delivered,owner['label']))
                            if owner['label']==2 and delivered==55:
                                after_exit='Multi Thread retired: label=1' in text and w[19]==owner['initial_rsp']-200
                                after_fault=case=='channels-b-fault' and b in closed
            else:raise Failure('Unexpected Channel ABI operation.')
            if status!=expected or (op!=0x14 or expected!=0) and scalar!=0:raise Failure('IPC status/scalar differs from independent nonblocking capability/FIFO model.')
            if expected:errors.add(expected)
        if w[28:50]!=snapshot() or w[61]!=int(bool(ends[0]))+2*int(bool(ends[1])):raise Failure('Actual queue/endpoint state lost or invented a committed message.')
        if any(sent[1-side]!=received[side]+queues[side][2]+discard[side] for side in (0,1)):raise Failure('Directional message conservation failed.')
        if w[62]!=sum(len(t) for t in caps.values()):raise Failure('Capability reference accounting differs from live handles.')
        if w[23] and (seq not in native or native[seq]['words']!=w):raise Failure('Missing coherent native CPU/actual FIFO observation.')
    if closed!={a,b} or refs!=[0,0] or any(ends) or any(q[2] for q in queues):raise Failure('Channel/endpoint/handle/queue teardown incomplete.')
    extra=[6,3] if case=='channels-stress' else [0,0]
    if sent!=[4+extra[0],3+extra[1]] or received[1]!=4+extra[0] or received[0]!=(2 if case=='channels-a-fault' else 3)+extra[1] or discard!=([1,0] if case=='channels-a-fault' else [0,0]):raise Failure('Message lifetime accounting differs from selected close/fault policy.')
    if case=='channels-stress':
        for i in range(3):
            for side,kind,value in ((0,'send',0x6000+i),(0,'send',0x6100+i),(1,'receive',0x6000+i),(1,'receive',0x6100+i),(1,'send',0x6200+i),(0,'receive',0x6200+i)):
                if not any(e[1:4]==(kind,side,value) for e in successful):raise Failure('Missing repeated real FIFO wraparound transaction.')
            if not any(w[6]==0x13 and w[8]==0x7000+i and w[9]==(1<<64)-33 for w in records):raise Failure('Missing repeated Full rejection.')
        if sum(1 for w in records if w[23] and (w[8] in (0x6000,0x6002,0x7000) or w[10] in (0x6000,0x6102,0x6202)))<6:raise Failure('Missing stopped-CPU wraparound FIFO observations.')
    if not {-16,-17,-18,-19,-32,-33,-34}<=errors or len(native)<12:raise Failure('Incomplete negative or physical FIFO evidence.')
    if {e[4] for e in successful if e[1]=='send' and e[2]==0}!={1,2}:raise Failure('A1/A2 did not send through shared Node authority.')
    if case!='channels-a-fault' and not after_exit:raise Failure('A2 did not use its endpoint after A1 exit.')
    if case=='channels-b-fault' and not after_fault:raise Failure('Healthy A2 did not drain the pending reply after B fault cleanup.')
    if case=='channels-a-fault' and not any(w[1]==b and w[9]==(1<<64)-34 and a in closed for w in records):raise Failure('Healthy B did not observe A closure.')
    if 'RDX=0x30000003f' not in text or f'IPC accounting restored: channels=0 endpoints=0 queued=0 references=0 pins=0 requests={len(records)}' not in text:raise Failure('Missing ABI 0.3 features or final accounting.')
    if case=='channels-rollback' and 'IPC rollback: table=full pair=unpublished queues=unchanged references=restored' not in text:raise Failure('Missing initial grant rollback.')
    return {'records':records,'native':native,'committed':successful,'sent':sent,'received':received,'discarded':discard,'errors':sorted(errors),'after_a1_exit':after_exit,'after_b_fault':after_fault,'final_channels':0,'final_endpoints':0,'final_messages':0}


def observe_multi(qmp,serial,symbols,start,sequence,record,deadline,cap_state=None):
    abi02=cap_state is not None and cap_state.get('abi02',False)
    ipc=cap_state is not None and cap_state.get('ipc',False)
    probe_prefix='neu_ipc' if ipc else 'neu_as' if abi02 else 'neu_cap'
    root,bottom,top,initial=tuple(record[k] for k in ('root','bottom','top','initial_rsp'))
    local_rsp=initial-64;user_base=initial&~4095;seed=record['seed'];label=record['label']
    result={'sequence':sequence,'label':label,'root':root,'bottom':bottom,'top':top,'initial_rsp':initial}
    markers=[v^seed for v in (0x11225566778899aa,0xaabbccddeeff1234,0x1234567887654321)]
    reg_order=('R15','R14','R13','R12','R11','R10','R9','R8','RDI','RSI','RBP','RDX','RCX','RBX','RAX')
    while time.monotonic()<deadline:
        if 'Switch common restored:' in transcript(serial)[start:]:break
        if cap_state is not None:
            # A running snapshot is only a location hint. Stop and reread before
            # asserting any CPU/frame/object property. Avoid starving the guest
            # with stop/continue polls during unrelated kernel validation.
            hint=qmp.call('human-monitor-command',{'command-line':'info registers'})
            hint_rip=native_register(hint,'RIP');wanted=False
            if native_register(hint,'CR3')==root:
                wanted=('restore' not in result and symbols['neu_preempt_restore_wait']<=hint_rip<symbols['neu_preempt_restore_wait_end']) or ('user' not in result and 'CPL=3' in hint and multi_site(symbols,'neu_multi_loop')<=hint_rip<multi_site(symbols,'neu_multi_loop_end') and native_register(hint,'RSP')==local_rsp) or ('irq' not in result and symbols['neu_preempt_irq_wait']<=hint_rip<symbols['neu_preempt_irq_wait_end'])
                if symbols[probe_prefix+'_request_wait']<=hint_rip<symbols[probe_prefix+'_request_wait_end']:
                    wanted=debug_words(qmp,symbols[probe_prefix+'_witness'],1)[0] not in cap_state['native']
            if not wanted:
                time.sleep(0.001);continue
        qmp.call('stop')
        qmp._walk_snapshot={}
        try:
            cpu=qmp.call('human-monitor-command',{'command-line':'info registers'});reg=lambda key:native_register(cpu,key)
            if reg('CR3')!=root:continue
            rip=reg('RIP');stage=None
            if cap_state is not None and symbols[probe_prefix+'_request_wait']<=rip<symbols[probe_prefix+'_request_wait_end']:
                seq=debug_words(qmp,symbols[probe_prefix+'_witness'],1)[0]
                if seq not in cap_state['native']:cap_state['native'][seq]=(observe_ipc_request if ipc else observe_as_request if abi02 else observe_cap_request)(qmp,cpu,symbols,record)
                continue
            if symbols['neu_preempt_restore_wait']<=rip<symbols['neu_preempt_restore_wait_end'] and reg('RSP')==top-160:stage='restore'
            elif 'CPL=3' in cpu and multi_site(symbols,'neu_multi_loop')<=rip<multi_site(symbols,'neu_multi_loop_end') and reg('RSP')==local_rsp:stage='user'
            elif symbols['neu_preempt_irq_wait']<=rip<symbols['neu_preempt_irq_wait_end'] and reg('RSP')==top-176:stage='irq'
            if stage is None or stage in result:continue
            if reg('CR0')&10!=10 or reg('CR4')&0x50600 or reg('EFER')&1!=1 or not re.search(r'FS =0000 0000000000000000',cpu) or not re.search(r'GS =0000 0000000000000000',cpu):raise Failure('Unsupported CPU/TLS/extended state in member execution.')
            tr=re.search(r'TR =0018 ([0-9a-fA-F]+)',cpu)
            if not tr or debug_words(qmp,int(tr[1],16)+4,1)[0]!=top or debug_words(qmp,symbols['neu_native_selected_top'],1)[0]!=top:raise Failure('Shared-root dispatch selected a wrong Thread TSS/entry stack.')
            common=debug_words(qmp,symbols['neu_user_saved_rsp'],1)[0]
            if not symbols['__stack_bottom']<=common<symbols['__stack_top']:raise Failure('Member IRQ lost the common continuation.')
            sample={'cpu':cpu,'tss_rsp0':top,'common_rsp':common}
            if stage=='restore':
                if 'CPL=0' not in cpu or reg('RFL')&0x600:raise Failure('Unsafe member restore boundary.')
                sample['frame']=debug_words(qmp,top-160,20)
                epoch=len(re.findall(r'Multi Thread retired:',transcript(serial)))
                cache_key=(record['thread'],root,epoch)
                cached=None if cap_state is None else cap_state['maps'].get(cache_key)
                if cached is not None:
                    sample.update(cached);sample['maps_observed']=False
                else:
                    sample['data_walk']=switch_walk(qmp,root,0x402000);sample['code_walk']=switch_walk(qmp,root,0x400000)
                    if not sample['data_walk']['present'] or sample['data_walk']['physical']!=record['data_pa']:raise Failure('Actual shared-data backing differs from the owned Node frame.')
                    data=sample['data_walk'];code=sample['code_walk']
                    if data['entries'][-1]&((1<<63)|0x82)!=(1<<63)|2 or not all(e&4 for e in data['entries']) or not code['present'] or code['entries'][-1]&((1<<63)|0x82) or not all(e&4 for e in code['entries']):raise Failure('Shared Node data/code permissions differ from RW/NX and RX.')
                    walks={str(va):switch_walk(qmp,root,va) for va in (user_base-4096,user_base,user_base+4096,bottom-4096,bottom,bottom+4096,top)}
                    for guard in (user_base-4096,user_base+4096,bottom-4096,top):
                        if walks[str(guard)]['present']:raise Failure('A member stack guard is mapped.')
                    for va,pa,user in ((user_base,record['user_pa'],True),(bottom,record['k0'],False),(bottom+4096,record['k1'],False)):
                        w=walks[str(va)]
                        if not w['present'] or w['physical']!=pa or w['entries'][-1]&((1<<63)|0x82)!=(1<<63)|2 or (all(entry&4 for entry in w['entries']))!=user:raise Failure('Member stack backing/permissions are incorrect.')
                    sample['walks']=walks
                    sample['all_user_stacks']={str(slot):switch_walk(qmp,root,0x404000+slot*0x4000) for slot in range(3)}
                    sample['all_kernel_stacks']={str(slot):switch_walk(qmp,root,0xffffc00000001000+slot*0x4000) for slot in range(3)}
                    probe=switch_walk(qmp,root,0x410000);sample['probe_walk']=probe
                    if label in (1,2):
                        if not probe['present'] or probe['physical']!=record['data_pa']:raise Failure('Node A private data probe is not an alias of its shared frame.')
                    elif probe['present']:raise Failure('Node B has an unauthorized mapping of Node A private data.')
                    if 'x2APIC=false' in transcript(serial):
                        device=switch_walk(qmp,root,0xffffb00000000000)
                        if not device['present'] or any(e&4 for e in device['entries']) or device['entries'][-1]&((1<<63)|0x9f)!=(1<<63)|0x1b or device['physical']!=0xfee00000:raise Failure('Member xAPIC view is not supervisor UC RW/NX.')
                        sample['device_walk']=device
                    sample['maps_observed']=True
                    if cap_state is not None:cap_state['maps'][cache_key]={k:v for k,v in sample.items() if k not in ('cpu','frame','tss_rsp0','common_rsp','maps_observed')}
            elif stage=='user':
                if reg('CS')!=0x3b or reg('SS')!=0x33 or reg('RFL')&0x202!=0x202 or reg('RFL')&~0x10ad7:raise Failure('Member execution has wrong privilege/flags.')
                actual=[reg(key) for key in reg_order]+[rip,reg('CS'),reg('RFL'),reg('RSP'),reg('SS')]
                multi_registers(actual,record,symbols)
                words=debug_words(qmp,0x402000,32);offset=8+(label-1)*4
                if words[offset+2]!=1 or words[offset+3]!=markers[0] or debug_words(qmp,local_rsp,3)!=markers:raise Failure('A shared-root member restarted or lost its private stack witnesses.')
                if words[offset]==0:continue
                multi_committed(words[offset],actual,symbols)
                sample.update(frame=actual,words=words,markers=markers)
            else:
                if 'CPL=0' not in cpu or reg('CS')!=8 or reg('SS')!=0 or reg('RFL')&0x600:raise Failure('Invalid member hardware IRQ origin/stack.')
                raw=debug_words(qmp,top-176,22);frame=raw[:15]+raw[17:]
                if raw[15:17]!=[48,0] or frame[16]!=0x3b or frame[19]!=0x33 or frame[17]&0x202!=0x202 or frame[17]&~0x10ad7 or not user_base<=frame[18]<=initial:raise Failure('Actual member IRQ frame lost its selected user-stack contract.')
                multi_registers(frame,record,symbols)
                lapic=qmp.call('human-monitor-command',{'command-line':'info lapic'})
                if not re.search(r'LVTT\s+0x00000030',lapic) or not re.search(r'ISR\s+48\b',lapic) or 'APIC enabled' not in lapic or 'TPR 0x00' not in lapic:raise Failure('Member capture lacks a real in-service LAPIC timer.')
                if debug_words(qmp,symbols['neu_native_busy'],1)[0]!=0:raise Failure('An unsafe native call chain was preempted.')
                sample.update(raw_frame=raw,frame=frame,lapic=lapic,words=debug_words(qmp,0x402000,32),markers=debug_words(qmp,local_rsp,3))
                if sample['markers']!=markers:raise Failure('IRQ used another member user stack.')
            result[stage]=sample
            if (cap_state is None and all(key in result for key in ('restore','user','irq'))) or (cap_state is not None and record.get('peer') and stage=='irq' and 'restore' in result):return result
        finally:
            qmp._walk_snapshot=None
            qmp.call('cont')
        time.sleep(0.001)
    else:raise Failure('Timed out observing three-Thread preemption.')
    if 'restore' not in result:raise Failure('Missing actual member restore frame.')
    return result


def check_multi(case,text,symbols,observations,kernel_root,qmp,capabilities=False):
    calibrated=re.search(r'Timer calibrated: PIT_Hz=(\d+) reference=(\d+) samples=(\d+),(\d+),(\d+) divided_Hz=(\d+) initial=(\d+) divide=16 period_us=10000',text)
    if not calibrated:raise Failure('Missing three-Thread 10 ms calibration.')
    hz,ref,a,b,c,divided,initial=map(int,calibrated.groups())
    if hz!=1193182 or ref!=11932 or divided!=sorted((a,b,c))[1]*hz//ref or initial!=(divided*10000+500000)//1000000 or max(a,b,c)-min(a,b,c)>sorted((a,b,c))[1]//4:raise Failure('Invalid three-Thread clockevent calibration.')
    pattern=r'Multi Thread ready: label=(\d+) node=(\d+) thread=(\d+) space=(\d+) root=(0x[0-9a-f]+) user_base=(0x[0-9a-f]+) initial_rsp=(0x[0-9a-f]+) bottom=(0x[0-9a-f]+) top=(0x[0-9a-f]+) data_pa=(0x[0-9a-f]+) user_pa=(0x[0-9a-f]+) k0=(0x[0-9a-f]+) k1=(0x[0-9a-f]+) seed=(\d+)'
    ready=list(re.finditer(pattern,text));keys=('label','node','thread','space','root','user_base','initial_rsp','bottom','top','data_pa','user_pa','k0','k1','seed')
    if len(ready)!=3 or [int(m[1]) for m in ready]!=[1,3,2]:raise Failure('Three live member resources were not constructed before execution.')
    records={int(m[1]):dict(zip(keys,map(lambda v:int(v,0),m.groups()))) for m in ready}
    if any(records[1][key]!=records[2][key] for key in ('node','space','root','data_pa')) or any(records[1][key]==records[3][key] for key in ('node','space','root','data_pa')):raise Failure('Same-Node sharing or different-Node isolation is missing.')
    if any(len({r[key] for r in records.values()})!=3 for key in ('thread','user_base','initial_rsp','bottom','top','user_pa','k0','k1')):raise Failure('Threads share a private stack or execution identity.')
    all_stack_pa=[r[key] for r in records.values() for key in ('user_pa','k0','k1')]
    if len(set(all_stack_pa))!=9 or set(all_stack_pa)&{records[1]['data_pa'],records[3]['data_pa']}:raise Failure('Private stacks alias shared data or another stack.')
    for label,r in records.items():
        slot=(r['thread']&0xffffffff)-1
        if slot>=3 or r['bottom']!=0xffffc00000001000+slot*0x4000 or r['top']!=r['bottom']+8192 or r['user_base']!=0x404000+slot*0x4000 or r['initial_rsp']!=r['user_base']+4088 or r['seed']!={1:35,2:53,3:77}[label]:raise Failure('Invalid bounded member stack/generation binding.')
    selected=list(re.finditer(r'Multi selected: sequence=(\d+) label=(\d+) node=(\d+) thread=(\d+) space=(\d+) root=(0x[0-9a-f]+) fresh=(true|false) peer=(true|false)\n',text))
    if not selected or ready[-1].end()>selected[0].start() or 'Multi three runnable: order=1,3,2 pins=2,1 data_owners=16 quantum_us=10000' not in text:raise Failure('The scheduler did not admit all three before dispatch.')
    peak=re.search(r'Multi three runnable: order=1,3,2 pins=2,1 data_owners=16 quantum_us=10000 allocated=(\d+) free=(\d+) tables=(\d+)',text)
    if not peak or int(peak[1])!=int(peak[3])+16 or int(peak[3])!=(42 if case=='threads-xapic' else 33):raise Failure('Three-member capacity differs from its bounded accounting design.')
    calibration={'samples':[a,b,c],'divided_hz':divided,'initial':initial}
    fifo=[1,3,2];saved={};suspended_counts={};code_frames={};outcomes={};captures={1:0,2:0,3:0};last_work={1:0,2:0,3:0};events=[];order=[];shared_audits=[];user_samples=[];node_results={}
    fault_label={'threads-a1-fault':1,'threads-a2-fault':2,'threads-b-fault':3,'threads-cross':3,'threads-a-cross':1,'threads-stack':1,'threads-user-guard':2,'threads-return-stack':2}.get(case)
    for i,m in enumerate(selected):
        seq,label,node,thread,space,root=map(lambda v:int(v,0),m.groups()[:6]);r=records[label]
        if seq!=i+1 or not fifo or fifo.pop(0)!=label or (node,thread,space,root)!=tuple(r[k] for k in ('node','thread','space','root')) or (m[8]=='true')!=bool(fifo) or label in outcomes:raise Failure('Member selection violated Thread FIFO/lifecycle ownership.')
        order.append(label);episode=text[m.end():selected[i+1].start() if i+1<len(selected) else len(text)]
        taken=logged_frames(episode,'multi-taken')
        if len(taken)!=1 or taken[0][0]!=thread:raise Failure('Missing unique member context consumption.')
        frame=taken[0][1]
        if label in saved:
            if frame!=saved[label] or m[7]!='false':raise Failure('A member restarted or lost its complete interrupted state.')
        elif m[7]!='true' or frame!=[0]*15+[USER_CODE,0x3b,0x202,r['initial_rsp'],0x33]:raise Failure('Invalid independent initial context.')
        rejected='Multi return stack rejected:' in episode
        if rejected:
            if case!='threads-return-stack' or label!=2 or seq in observations or 'Switch native ready:' in episode:raise Failure('A sibling user RSP reached native/root activation.')
        else:
            if seq not in observations or observations[seq]['restore']['frame']!=frame:raise Failure('Actual selected restore frame differs from saved owner state.')
            obs=observations[seq]
            code_pa=obs['restore']['code_walk']['physical']
            if node in code_frames and code_frames[node]!=code_pa:raise Failure('Sibling Threads did not share Node RX backing.')
            if node not in code_frames and code_pa in code_frames.values():raise Failure('Different Nodes alias private code backing.')
            code_frames[node]=code_pa
            for slot in range(3):
                owner=next(k for k,v in records.items() if (v['thread']&0xffffffff)-1==slot)
                expected=records[owner]['node']==node and owner not in outcomes
                if obs['restore']['all_user_stacks'][str(slot)]['present']!=expected or obs['restore']['all_kernel_stacks'][str(slot)]['present']!=expected:raise Failure('Retired/foreign member stack is mapped or a healthy sibling was unmapped.')
            for stage in ('user','irq'):
                if stage not in obs:continue
                sample=obs[stage];words=sample['words'];total=0
                for owner,record in records.items():
                    if record['node']!=node:continue
                    stored=words[8+(owner-1)*4]
                    count=outcomes[owner]['committed'] if owner in outcomes else multi_committed(stored,sample['frame'] if owner==label else saved.get(owner),symbols)
                    if count<stored or count-stored>1:raise Failure('Atomic completion witness is outside one interruptible instruction sequence.')
                    total+=count
                if total!=words[0]:raise Failure('QMP shared counter differs from actual committed atomic increments.')
                if stage=='user':user_samples.append({'sequence':seq,'label':label,'work':words[8+(label-1)*4],'shared_total':words[0]})
        irq=list(re.finditer(r'Preempt IRQ: tick=(\d+) sequence=(\d+) node=(\d+) thread=(\d+) space=(\d+) root=(0x[0-9a-f]+) RIP=(0x[0-9a-f]+) RSP=(0x[0-9a-f]+) flags=(0x[0-9a-f]+) CS=(0x[0-9a-f]+) SS=(0x[0-9a-f]+) handler=(0x[0-9a-f]+) vector=48 ISR_before=1 ISR_after=0 switch=(true|false)\n',episode))
        irq_frames=logged_frames(episode,'irq')
        if len(irq)!=len(irq_frames):raise Failure('Member IRQ event/frame count mismatch.')
        for event,(owner,words) in zip(irq,irq_frames):
            tick,sequence,n,t,s,cr3,rip,rsp,flags,cs,ss,handler=map(lambda v:int(v,0),event.groups()[:-1]);switched=event[13]=='true'
            if sequence!=seq or (n,t,s,cr3)!=(node,thread,space,root) or tick!=(events[-1]['tick']+1 if events and events[-1]['sequence']==seq else tick) or owner!=thread or words[15:]!=[rip,cs,flags,rsp,ss] or cs!=0x3b or ss!=0x33 or flags&0x202!=0x202 or flags&~0x10ad7 or not r['user_base']<=rsp<=r['initial_rsp'] or not r['bottom']<=handler<r['top']-176 or switched!=bool(fifo):raise Failure('Member IRQ used wrong CPU/root/stack/acknowledgment ownership.')
            if events and tick<=events[-1]['tick']:raise Failure('Hardware event accounting is not monotonic.')
            multi_registers(words,r,symbols);events.append({'tick':tick,'sequence':seq,'label':label,'frame':words,'switched':switched})
        if not rejected and 'irq' in observations[seq]:
            if (thread,observations[seq]['irq']['frame']) not in irq_frames:raise Failure('Recorded member handoff was not the actual QMP hardware frame.')
        yielded=logged_frames(episode,'multi-saved')
        if yielded:
            if len(yielded)!=1 or not fifo or not irq_frames or yielded[0]!=(thread,irq_frames[-1][1]) or seq not in observations or 'irq' not in observations[seq]:raise Failure(f'Member scheduling lacks a real copied timer context: sequence={seq}, yielded={len(yielded)}, peer={bool(fifo)}, IRQs={len(irq_frames)}, observed={list(observations.get(seq,{}))}.')
            saved[label]=yielded[0][1];captures[label]+=1;fifo.append(label)
            progress=re.search(r'Multi progress: label=(\d+) captures=(\d+) stored=(\d+) committed=(\d+) boots=1 total=(\d+)',episode)
            if not progress or (int(progress[1]),int(progress[2]))!=(label,captures[label]) or (int(progress[4])<last_work[label] if capabilities else int(progress[4])<=last_work[label]) or multi_committed(int(progress[3]),saved[label],symbols)!=int(progress[4]):raise Failure('A member failed to make independent atomic progress.')
            actual_stored=observations[seq]['irq']['words'][8+(label-1)*4]
            if actual_stored!=int(progress[3]):raise Failure('Saved member progress was not the actual observed IRQ memory.')
            suspended_counts[label]=multi_committed(actual_stored,saved[label],symbols)
            last_work[label]=int(progress[4])
            if not capabilities and captures[label]<=4 and re.search(r'Native terminal:.*entries=[1-9]',episode):raise Failure('Cooperative/native calls caused the claimed three-Thread alternation.')
        else:
            outcome=re.search(r'Multi outcome: label=(\d+) thread=(\d+) reason=(normal|fault) status=(\d+) committed=(\d+) node_state=(Active|Exited)',episode)
            if not outcome or int(outcome[1])!=label or int(outcome[2])!=thread or (outcome[3]=='fault')!=(fault_label==label) or int(outcome[5])<last_work[label]:raise Failure('Missing correct independent Thread outcome.')
            if outcome[3]=='normal' and int(outcome[4])!=label:raise Failure('thread_exit did not retain the calling Thread status.')
            if rejected and int(outcome[5])!=suspended_counts[label]:raise Failure('Rejected return lost a previously committed atomic increment.')
            outcomes[label]={'reason':outcome[3],'status':int(outcome[4]),'committed':int(outcome[5])};saved.pop(label,None)
            canceled=list(re.finditer(r'Multi cancelled: label=(\d+) thread=(\d+) by=(\d+) committed=(\d+) saved=false queued=false',episode))
            expected_cancel=[owner for owner in fifo if records[owner]['node']==node] if outcome[3]=='fault' else []
            if {int(c[1]) for c in canceled}!=set(expected_cancel):raise Failure('Node-wide fault did not cancel exactly its eligible siblings.')
            for c in canceled:
                owner=int(c[1])
                if int(c[2])!=records[owner]['thread'] or int(c[3])!=thread or int(c[4])!=suspended_counts[owner]:raise Failure('Cancellation retired a foreign Thread identity.')
                outcomes[owner]={'reason':'cancelled','by':thread,'committed':int(c[4])};saved.pop(owner,None);fifo.remove(owner)
            remaining_same=any(records[owner]['node']==node for owner in fifo)
            if (outcome[6]=='Active')!=remaining_same:raise Failure('Node lifetime was equated with an individual Thread exit.')
            if outcome[3]=='fault' and fault_label in (1,2) and not expected_cancel:raise Failure('Node A fault did not leave a suspended sibling to cancel.')
            if outcome[3]=='fault' and fault_label==3 and not all(owner in fifo for owner in (1,2)):raise Failure('Node B fault did not preserve both live A members.')
            if outcome[3]=='fault' and not rejected:
                fault=re.search(r'Node user fault: node=(\d+) thread=(\d+) space=(\d+) root=(0x[0-9a-f]+) vector=(\d+) error=(0x[0-9a-f]+) RIP=(0x[0-9a-f]+) CR2=(0x[0-9a-f]+) CS=(0x[0-9a-f]+) SS=(0x[0-9a-f]+) RSP=(0x[0-9a-f]+) RFLAGS=(0x[0-9a-f]+) handler_rsp=(0x[0-9a-f]+)',episode)
                expected={'threads-cross':(14,4,0x410000,'neu_multi_cross_b_site'),'threads-a-cross':(14,4,0x406000,'neu_multi_cross_a_site'),'threads-stack':(14,5,0xffffc00000009000,'neu_multi_kernel_stack_site'),'threads-user-guard':(14,4,0x40b000,'neu_multi_user_guard_site')}.get(case,(6,0,0,'neu_multi_ud_site'))
                if not fault:raise Failure('Missing actual current-Thread fault frame.')
                v=list(map(lambda x:int(x,0),fault.groups()));vector,error,cr2,site=expected
                if tuple(v[:7])!=(node,thread,space,root,vector,error,multi_site(symbols,site)) or v[8:11]!=[0x3b,0x33,r['initial_rsp']-64] or (vector==14 and v[7]!=cr2) or not r['bottom']<=v[12]<r['top']:raise Failure('Node fault metadata does not match actual selected execution.')
            retired=list(re.finditer(r'Multi Thread retired: label=(\d+) thread=(\d+) freed=(\d+) pins=(\d+) root=(0x[0-9a-f]+) outcome=retained',episode))
            expected_retired={label}|set(expected_cancel)
            if {int(t[1]) for t in retired}!=expected_retired:raise Failure('Thread exit/cancellation did not retire exactly its own stack resources.')
            for t in retired:
                if int(t[2])!=records[int(t[1])]['thread'] or int(t[3])<3 or int(t[5],16)!=root:raise Failure('Wrong Thread stack release or missing backing cleanup.')
            live_count=sum(records[owner]['node']==node for owner in fifo)
            if retired and int(retired[-1][4])!=live_count:raise Failure('Shared root pins do not match its surviving resumable members.')
            reclaimed=f'Multi Node reclaimed: node={node} final_outcome=retained' in episode
            if reclaimed != (not remaining_same):raise Failure('Shared root was freed before its last Thread or leaked afterward.')
            if reclaimed:node_results[node]=outcome[3]
            if fifo and 'Multi survivor retained:' not in episode:raise Failure('A healthy same/different-Node continuation did not survive teardown.')
        audits=re.findall(r'Multi shared audit: node=(\d+) total=(\d+) committed=(\d+) pending=(\d+) pins=(\d+)',episode)
        if len(audits)!=1 or int(audits[0][0])!=node or audits[0][1]!=audits[0][2] or not 0<=int(audits[0][3])<=2:raise Failure('Shared atomic sum was not reconciled before resource release.')
        shared_audits.append(list(map(int,audits[0])))
    if fifo or set(outcomes)!={1,2,3} or len(node_results)!=2 or order[:6]!=[1,3,2,1,3,2]:raise Failure('Missing repeated three-Thread preemption or terminal/empty completion.')
    minimum=1 if case=='threads-return-stack' else 4
    if min(captures.values())<minimum or any(sum(row['label']==label for row in user_samples)<(1 if case=='threads-return-stack' and label==2 else 2) for label in (1,2,3)):raise Failure('Insufficient actual interrupted/resumed CPU observations for all three members.')
    if not capabilities and not any(not e['switched'] for e in events):raise Failure('Missing direct hardware return for the final runnable member.')
    if case=='threads-rollback':
        budgets=re.findall(r'Multi rollback: budget=(\d+) accounting=restored sibling_contexts=unchanged pins=1,1',text)
        if list(map(int,budgets))!=[0,1,2,3] or 'Multi context rollback: rejected=Association accounting=restored' not in text:raise Failure('A2 construction did not roll back while A1/B1 remained runnable and pinned.')
    if case=='threads-stale':
        stale=re.search(r'Multi stale rejected: old=(\d+) replacement=(\d+) root_pins=1 accounting=restored',text)
        if not stale or int(stale[1])!=records[1]['thread'] or int(stale[1])==int(stale[2]) or (int(stale[1])&0xffffffff)!=(int(stale[2])&0xffffffff):raise Failure('Old Thread slot generation was not rejected after genuine reuse.')
    drains=re.findall(r'Preempt deferred drained: before=(\d+) after=(\d+) eois=(\d+) root=(0x[0-9a-f]+) IF=0 kernel_switch=false',text)
    for before,after,eois,root in drains:
        if int(after)!=int(before)+1 or after!=eois or int(root,16)!=kernel_root:raise Failure('Deferred kernel drain changed scheduling or violated acknowledgment.')
    stopped=re.search(r'Preempt timer stopped: ticks=(\d+) eois=(\d+) user=(\d+) drain=(\d+) ISR=0 IRR=0 initial=0 current=0 SVR=disabled IF=0',text)
    done=re.search(r'Multiple Thread test passed: dispatches=(\d+) captures=(\d+) queue=empty running=none IF=0',text)
    baseline=re.search(r'Multi baseline: allocated=(\d+) free=(\d+) tables=(\d+) root=(0x[0-9a-f]+)',text);restored=re.search(r'Multi accounting restored: allocated=(\d+) free=(\d+) tables=(\d+)',text)
    if not stopped or not done or not baseline or not restored or baseline.groups()[:3]!=restored.groups() or int(baseline[4],16)!=kernel_root:raise Failure('Missing exact final shared-root accounting.')
    ticks,eois,user,drain=map(int,stopped.groups())
    if ticks!=eois or user!=len(events) or ticks!=user+drain or not len(drains)<=drain<=len(drains)+1 or tuple(map(int,done.groups()))!=(len(selected),len(events)):raise Failure('User IRQs, safe drains, EOIs and selections were conflated.')
    observed_ticks={e['tick'] for e in events}|{int(row[1]) for row in drains}
    if observed_ticks!={*range(1,ticks+1)} and not (drain==len(drains)+1 and observed_ticks==set(range(1,ticks))):raise Failure('Missing hardware event accounting between user and kernel windows.')
    final=qmp.call('human-monitor-command',{'command-line':'info lapic'})
    if not re.search(r'LVTT\s+0x00010030.*masked',final) or 'APIC disabled' not in final or 'TPR 0xff' not in final or not re.search(r'ISR\s+\(none\)',final) or not re.search(r'IRR\s+\(none\)',final) or not re.search(r'initial_count = 0 current_count = 0',final):raise Failure('Three-Thread timer did not quiesce before the controlled halt.')
    for name in ('neu_native_busy','neu_native_selected_top','neu_native_saved_user_rsp','neu_user_saved_rsp'):
        if debug_words(qmp,symbols[name],1)[0]!=0:raise Failure('Stale native/shared-root entry state at final halt.')
    if debug_words(qmp,symbols['neu_preempt_busy'],1)[0]&255 or int(re.search(r'EFER=([0-9a-fA-F]+)',qmp.call('human-monitor-command',{'command-line':'info registers'}))[1],16)&1:raise Failure('Stale IRQ busy state or SCE at halt.')
    return {'threads':records,'peak':list(map(int,peak.groups())),'calibration':calibration,'order':order,'captures':captures,'events':events,'outcomes':outcomes,'node_results':node_results,'shared_audits':shared_audits,'user_samples':user_samples,'observations':observations,'ticks':ticks,'eois':eois,'drain_ticks':drain,'accounting':list(map(int,restored.groups())),'final_lapic':final}


def check_capabilities(case,text,symbols,members,state):
    records=[]
    for line in re.finditer(r'Cap record: words=([^\n]+)\n',text):
        words=[int(v,0) for v in line[1].split(',')]
        if len(words)!=24 or words[0]!=len(records)+1:raise Failure('Incomplete/out-of-order capability transaction record.')
        records.append(words)
    boot=re.search(r'Cap bootstrap: A=(\d+) table=(\d+) B=(\d+) table=(\d+) counter=(\d+) other=(\d+) solo=(\d+) objects=4 references=6 handles=5,1 ABI=0.1 test_only=true',text)
    done=re.search(r'Cap accounting restored: objects=0 references=0 pins=0 leases=0 requests=(\d+) public_ABI=unchanged',text)
    if not boot or not done or int(done[1])!=len(records):raise Failure('Missing exact capability bootstrap/final accounting.')
    a,at,b,bt,aid,bid,solo=map(int,boot.groups());h1=0x100000001;new_h=0x200000001
    if (a,at,b,bt)!=(members['threads'][1]['node'],a,members['threads'][3]['node'],b) or a==b or (aid,bid,solo)!=(0x10000000101,0x10000000102,0x10000000104):raise Failure('Capability tables/object generations do not match the actual Node profile.')
    flag=0x20000000103
    objects={aid:{'kind':1,'value':10,'refs':3},bid:{'kind':1,'value':200,'refs':1},flag:{'kind':2,'value':1,'refs':1},solo:{'kind':1,'value':9,'refs':1}}
    tables={a:{h1:(aid,3),h1+1:(aid,1),h1+2:(aid,2),h1+3:(flag,1),h1+4:(solo,1)},b:{h1:(bid,3)}}
    generations={a:[1]*5+[1 if case=='capabilities-rollback' else 0]*3,b:[1]+[0]*7}
    entries=[]
    for match in re.finditer(r'Native entry: seq=\d+ op=(0x[0-9a-f]+) arg1=(0x[0-9a-f]+) RIP=(0x[0-9a-f]+) RSP=(0x[0-9a-f]+) R11=(0x[0-9a-f]+)',text):
        if 0xfffffffffffffd00<=int(match[1],16)<=0xfffffffffffffd05:entries.append(match)
    if len(entries)!=len(records):raise Failure('Capability records were not genuine native scalar requests.')
    timeline=[(m.start(),'request',(m,r)) for m,r in zip(entries,records)]
    for m in re.finditer(r'Cap regrant: node=(\d+) old_object=(\d+) new_object=(\d+) old_handle=(0x[0-9a-f]+) new_handle=(0x[0-9a-f]+) initial=77',text):timeline.append((m.start(),'regrant',m))
    for m in re.finditer(r'Cap Node cleanup: node=(\d+) handles=(\d+) objects_before=(\d+) after=(\d+) references_before=(\d+) after=(\d+) pins=0 lease=none',text):timeline.append((m.start(),'cleanup',m))
    regranted=False;cleaned=set();errors=set();used=set();native=state['native'];successful_pins=0;post_exit=False
    normal_a1=re.search(r'Multi outcome: label=1 .*reason=normal',text)
    selections=list(re.finditer(r'Multi selected: sequence=\d+ label=(\d+) node=(\d+) thread=(\d+) space=(\d+) root=(0x[0-9a-f]+)',text))
    def release(target):
        objects[target]['refs']-=1
        if objects[target]['refs']==0:del objects[target]
    for position,kind,event in sorted(timeline,key=lambda item:item[0]):
        if kind=='regrant':
            if regranted or h1 in tables[a] or solo in objects or tuple(int(v,0) for v in event.groups())!=(a,solo,0x10000000204,h1,new_h):raise Failure('Trusted slot reuse did not follow effective close/last-reference cleanup.')
            objects[0x10000000204]={'kind':1,'value':77,'refs':1};tables[a][new_h]=(0x10000000204,3);generations[a][0]=2;regranted=True;continue
        if kind=='cleanup':
            node,count,before,after,refs_before,refs_after=map(int,event.groups())
            if node in cleaned or node not in tables or count!=len(tables[node]) or before!=len(objects) or refs_before!=sum(o['refs'] for o in objects.values()):raise Failure('Node capability teardown started with incorrect table/reference ownership.')
            for target,_ in tables[node].values():release(target)
            tables[node].clear();cleaned.add(node)
            if after!=len(objects) or refs_after!=sum(o['refs'] for o in objects.values()):raise Failure('Node cleanup released foreign authority or leaked references.')
            continue
        entry,r=event;seq,node,thread,space,root,table,op,handle,value,status,scalar,target,obj_kind,rights,refs,pins,source,actual_value,rip,rsp,top,frame_ptr,flags,probed=r
        selected=next((m for m in reversed(selections) if m.start()<position),None)
        if not selected:raise Failure('Capability request has no selected execution context.')
        label=int(selected[1]);owner=members['threads'][label]
        if (node,thread,space,root)!=(owner['node'],owner['thread'],owner['space'],owner['root']) or table!=node or node in cleaned or top!=owner['top'] or frame_ptr!=top-160 or flags&0x600:raise Failure('A userspace argument selected foreign or terminal authority.')
        if (op,handle,rip,rsp)!=tuple(int(v,16) for v in entry.groups()[:4]):raise Failure('Capability request did not match its actual native entry scalars.')
        following=text[entry.end():next((m.start() for m in entries if m.start()>entry.start()),len(text))]
        returned=re.search(r'Native return prepared: seq=\d+ RAX=(0x[0-9a-f]+) RDX=(0x[0-9a-f]+)',following)
        if not returned or tuple(int(v,16) for v in returned.groups())!=(status,scalar):raise Failure('Capability result did not return through the validated native frame.')
        used.add(label);slot=(handle&0xffffffff)-1;generation=handle>>32;expected_status=0;expected_scalar=0;old=tables[node].get(handle)
        if not 0<=slot<8 or generation==0:expected_status=-16
        elif generations[node][slot]==0:expected_status=-16
        elif generation!=generations[node][slot] or old is None:expected_status=-17
        expected_info=None
        if expected_status==0:
            object_id,granted=old;obj=objects[object_id]
            if (target,obj_kind,rights)!=(object_id,obj['kind'],granted):raise Failure('A handle resolved to the wrong typed kernel object or rights.')
            required=2 if op==0xfffffffffffffd01 else 3 if op==0xfffffffffffffd03 else 1
            requested_type=2 if op==0xfffffffffffffd04 else 1
            if op!=0xfffffffffffffd02 and obj['kind']!=requested_type:expected_status=-19
            elif op!=0xfffffffffffffd02 and granted&required!=required:expected_status=-18
            if expected_status:
                expected_info=(obj['refs'],0,obj['value'])
            elif op==0xfffffffffffffd02:
                del tables[node][handle];release(object_id)
                expected_info=(objects[object_id]['refs'],0,objects[object_id]['value']) if object_id in objects else (0,0,0)
            else:
                if op in (0xfffffffffffffd00,0xfffffffffffffd04):expected_scalar=obj['value']
                elif op==0xfffffffffffffd01:obj['value']=value
                elif op==0xfffffffffffffd03:expected_scalar=obj['value'];obj['value']=value
                elif op==0xfffffffffffffd05:
                    expected_scalar=obj['value'];del tables[node][handle];obj['refs']-=1
                expected_info=(obj['refs'],1,obj['value']);successful_pins+=1
                if op==0xfffffffffffffd05 and obj['refs']==0:del objects[object_id]
        elif (target,obj_kind,rights,refs,pins,source,actual_value)!=(0,0,0,0,0,0,0):raise Failure('Invalid handle exposed foreign object contents or metadata.')
        signed=status-(1<<64) if status>>63 else status
        if signed!=expected_status or scalar!=(0 if expected_status else expected_scalar):raise Failure('Capability result violated the independent type/rights/lifetime oracle.')
        if expected_info and (refs,pins,actual_value)!=expected_info:raise Failure('Capability pin/reference/value snapshot differs from actual object ownership.')
        if signed:errors.add(signed)
        if probed:
            if seq not in native or native[seq]['words']!=r:raise Failure('Missing actual CPL=0/object-storage observation for a required capability probe.')
        elif seq in native:raise Failure('An unrequested native probe was conflated with another operation.')
        if normal_a1 and label==2 and position>normal_a1.end() and signed==0:post_exit=True
    if cleaned!={a,b} or objects or any(tables.values()) or not regranted or used!={1,2,3} or not {-16,-17,-18,-19}<=errors or successful_pins<12:raise Failure('Incomplete Node-local authorization/isolation/lifetime coverage.')
    if case in ('capabilities','capabilities-xapic','capabilities-rollback','capabilities-b-fault') and not post_exit:raise Failure('Surviving A2 did not use retained authority after A1 normal exit.')
    if case=='capabilities-rollback' and 'Cap rollback: table=full objects=full invalid_rights=rejected references=restored entries=5,1' not in text:raise Failure('Missing bounded registry/table installation rollback.')
    if set(native)!={r[0] for r in records if r[23]}:raise Failure('Native capability probes were missed or duplicated.')
    return {'requests':records,'native':native,'errors':sorted(errors),'post_a1_exit':post_exit,'node_tables':{a:at,b:bt},'objects_final':0,'references_final':0,'pins_final':0,'closed_nodes':sorted(cleaned)}


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
    operations = [query, query, exit_op] if kind == 0 else ([query, (0x13 if case=="native-abi03-disabled" else (0x12 if case=="native-abi02-disabled" else (0xfffffffffffffd00 if case=="native-cap-disabled" else (1 << 64) - 1))), query, exit_op] if kind == 1 else ([query, exit_op, query, exit_op] if kind == 2 else ([] if kind == 7 else [query])))
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
        if case in ("timer-xapic","preempt-xapic","threads-xapic","capabilities-xapic","address-space-xapic","channels-xapic"):
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
                preempt_observations = {}
                multi_observations = {}
                cap_state={"native":{},"maps":{}} if case in CAP_CASES or case in AS_CASES or case in IPC_CASES else None
                if case in AS_CASES:cap_state['abi02']=True
                if case in IPC_CASES:cap_state['ipc']=True
                if cap_state is not None:result.update(capability_observations=cap_state["native"],member_observations=multi_observations)
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
                    if case in PREEMPT_CASES and case not in PREEMPT_NEGATIVE:
                        current=transcript(serial)
                        runs=list(re.finditer(r"Preempt selected: sequence=(\d+) label=(\d+) node=(\d+) thread=(\d+) space=(\d+) root=(0x[0-9a-f]+) fresh=(true|false) peer=(true|false)\n",current))
                        for index,selected in enumerate(runs):
                            sequence,label,_,_,_,root=map(lambda v:int(v,0),selected.groups()[:6])
                            episode=current[selected.start():runs[index+1].start() if index+1<len(runs) else len(current)]
                            ready=re.search(r"Switch native ready:.*stack_bottom=(0x[0-9a-f]+) stack_top=(0x[0-9a-f]+).*fresh=(true|false)\n",episode)
                            if sequence not in preempt_observations and ready:
                                if qmp is None:qmp=QMP(endpoint,started+timeout)
                                preempt_observations[sequence]=observe_preempt(qmp,serial,symbols,selected.start(),sequence,label,root,int(ready[1],16),int(ready[2],16),started+timeout)
                    if case in MULTI_CASES or case in CAP_CASES or case in AS_CASES or case in IPC_CASES:
                        current=transcript(serial)
                        resource=re.findall(r"Multi Thread ready: label=(\d+) node=(\d+) thread=(\d+) space=(\d+) root=(0x[0-9a-f]+) user_base=(0x[0-9a-f]+) initial_rsp=(0x[0-9a-f]+) bottom=(0x[0-9a-f]+) top=(0x[0-9a-f]+) data_pa=(0x[0-9a-f]+) user_pa=(0x[0-9a-f]+) k0=(0x[0-9a-f]+) k1=(0x[0-9a-f]+) seed=(\d+)",current)
                        keys=('label','node','thread','space','root','user_base','initial_rsp','bottom','top','data_pa','user_pa','k0','k1','seed')
                        records={int(row[0]):dict(zip(keys,map(lambda v:int(v,0),row))) for row in resource}
                        runs=list(re.finditer(r"Multi selected: sequence=(\d+) label=(\d+) node=(\d+) thread=(\d+) space=(\d+) root=(0x[0-9a-f]+) fresh=(true|false) peer=(true|false)\n",current))
                        for index,selected in enumerate(runs):
                            sequence,label=map(int,selected.groups()[:2]);episode=current[selected.start():runs[index+1].start() if index+1<len(runs) else len(current)]
                            if sequence not in multi_observations and re.search(r"Switch native ready:.*fresh=(true|false)\n",episode):
                                if qmp is None:qmp=QMP(endpoint,started+timeout)
                                multi_observations[sequence]=observe_multi(qmp,serial,symbols,selected.start(),sequence,{**records[label],'peer':selected[8]=='true'},started+timeout,cap_state)
                    time.sleep(0.01 if case in USER_CASES or case in NATIVE_CASES or case in NODE_CASES or case in TIMER_CASES or case in SWITCH_CASES or case in PREEMPT_CASES or case in MULTI_CASES or case in CAP_CASES or case in AS_CASES or case in IPC_CASES else 0.05)
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
                preemption=check_preempt(case,text,symbols,preempt_observations,paging["root"],qmp) if case in PREEMPT_CASES else None
                member_case={'capabilities-xapic':'threads-xapic','capabilities-a-fault':'threads-a1-fault','capabilities-a2-fault':'threads-a2-fault','capabilities-b-fault':'threads-b-fault','address-space-xapic':'threads-xapic','address-space-a-fault':'threads-a1-fault','address-space-a2-fault':'threads-a2-fault','address-space-b-fault':'threads-b-fault','channels-xapic':'threads-xapic','channels-a-fault':'threads-a1-fault','channels-b-fault':'threads-b-fault'}.get(case,'threads' if case in CAP_CASES or case in AS_CASES or case in IPC_CASES else case)
                members=check_multi(member_case,text,symbols,multi_observations,paging["root"],qmp,case in CAP_CASES or case in AS_CASES or case in IPC_CASES) if case in MULTI_CASES or case in CAP_CASES or case in AS_CASES or case in IPC_CASES else None
                qmp.call("quit")  # A host-controlled exit after the observed halt.
                if process.wait(timeout=5) != 0:
                    raise Failure(f"QEMU did not exit cleanly after QMP quit: {process.returncode}")
                capabilities=check_capabilities(case,text,symbols,members,cap_state) if case in CAP_CASES else None
                address_space=check_address_space(case,text,symbols,members,cap_state) if case in AS_CASES else None
                ipc=check_ipc(case,text,symbols,members,cap_state) if case in IPC_CASES else None
                result.update(ipc=ipc,address_space=address_space,timer=timer,switching=switching,preemption=preemption,members=members,capabilities=capabilities)
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
