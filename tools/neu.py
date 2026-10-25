#!/usr/bin/env python3
"""Small stdlib-only build/run/boot-test runner for Neu Phase 0."""

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
FEATURES = {"boot": None, "panic": "test-panic", "missing-map": "test-missing-memory-map"}


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
            if label in ("neu_halt_begin", "neu_halt_end", "__stack_bottom", "__stack_top"):
                symbols[label] = value
    if symbols.get("__stack_top", 0) - symbols.get("__stack_bottom", 0) != 65536:
        raise Failure("Kernel ELF is missing its 64 KiB stack.")
    if not symbols.get("neu_halt_begin", 0) < symbols.get("neu_halt_end", 0):
        raise Failure("Kernel ELF is missing halt-loop symbols.")
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
          "[NEU] CPU and kernel stack validated"]
PROTOCOL = ["[NEU] Boot protocol initialized: Limine base 6, UEFI64", "[NEU] Memory map validated:"]
EXPECT = {
    "boot": (COMMON + PROTOCOL + ["[NEU] Kernel bootstrap successful", "[NEU] Idle: interrupts disabled; halting"],
             ["[NEU] PANIC:", "[NEU] BOOT FAILURE:"]),
    "panic": (COMMON + PROTOCOL + ["[NEU] PANIC:", "controlled Phase 0 test panic"],
              ["[NEU] Kernel bootstrap successful", "[NEU] BOOT FAILURE:"]),
    "missing-map": (COMMON + ["[NEU] BOOT FAILURE: required Limine memory-map response is missing"],
                    ["[NEU] Kernel bootstrap successful", "[NEU] Boot protocol initialized", "[NEU] PANIC:"]),
}


def transcript(path):
    return path.read_text(errors="replace") if path.exists() else ""


def check_halt(qmp, symbols):
    if qmp.call("query-status")["status"] != "running":
        raise Failure("VM reset, shut down, or stopped before the controlled halt.")
    registers = qmp.call("human-monitor-command", {"command-line": "info registers"})
    rip = re.search(r"\bRIP=([0-9a-fA-F]+)", registers)
    flags = re.search(r"\b(?:RFL|EFL)=([0-9a-fA-F]+)", registers)
    if not rip or not flags or "HLT=1" not in registers or "CPL=0" not in registers:
        raise Failure("CPU is not halted at Ring 0 (QMP register inspection):\n" + registers)
    if not symbols["neu_halt_begin"] <= int(rip[1], 16) < symbols["neu_halt_end"] or int(flags[1], 16) & (1 << 9):
        raise Failure("CPU is outside Neu's halt loop or interrupts are enabled.")
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
        result["command"] = arguments
        print("+ " + shlex.join(arguments), flush=True)
        qmp = None
        with errors.open("w") as error_log:
            process = subprocess.Popen(arguments, cwd=ROOT, stdout=error_log, stderr=error_log)
            try:
                required, forbidden = EXPECT[case]
                while required[-1] not in transcript(serial):
                    if process.poll() is not None:
                        raise Failure(f"QEMU exited early with status {process.returncode}.")
                    if time.monotonic() - started >= timeout:
                        raise Failure(f"{case}: timed out after {timeout:g}s waiting for the expected serial result.")
                    time.sleep(0.05)
                qmp = QMP(endpoint, started + timeout)
                symbols = elf_info(temporary / "esp/boot/neu-kernel")["symbols"]
                # UART output precedes HLT. Allow only the remaining test budget
                # for the CPU to arrive at the halt loop, then verify it twice.
                while True:
                    try:
                        first = check_halt(qmp, symbols)
                        break
                    except Failure:
                        if time.monotonic() - started >= timeout:
                            raise
                        time.sleep(0.05)
                time.sleep(0.1)
                second = check_halt(qmp, symbols)
                text = transcript(serial)
                cursor = 0
                for marker in required:
                    position = text.find(marker, cursor)
                    if position < 0:
                        raise Failure(f"Missing or out-of-order serial marker: {marker}")
                    cursor = position + len(marker)
                if any(marker in text for marker in forbidden):
                    raise Failure(f"Unexpected panic/failure/success marker in {case} transcript.")
                qmp.call("quit")  # A host-controlled exit after the observed halt.
                if process.wait(timeout=5) != 0:
                    raise Failure(f"QEMU did not exit cleanly after QMP quit: {process.returncode}")
                result.update(passed=True, halt_observations=[first, second])
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
    parser.add_argument("action", choices=("build", "run", "test", "doctor"))
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
