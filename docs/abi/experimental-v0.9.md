# Experimental Neu x86-64 ABI 0.9 — x87/MMX/SSE2 and Thread TLS

Status: experimental; implemented by `cpu-state-test` profiles. Acceptance is
recorded in [CPU-state completion](../CPU_STATE_COMPLETION.md).
[ADR 0033](../adr/0033-eager-fpu-simd-thread-tls.md) defines the execution contract.

## Version and features

ABI_QUERY (`RAX=1`) returns minor version 9 with FPU/SSE2=`0x800` and
Thread TLS=`0x1000`, plus only the features implemented by the underlying
profile. The conventional CPU-state profiles return `RDX=0x00000009000018ff`;
the combined DMA profile returns `0x0000000900001fff`. The former supports the
ABI 0.5 operations; the latter also supports [ABI 0.8](experimental-v0.8.md).
The version does not itself imply device, DMA or derived-MemoryObject support.
Earlier profiles retain their versions, masks and NotSupported (`-1`, RDX=0)
for the new TLS operations.

RAX selects the operation and returns signed status. RDI/RSI/RDX/R10/R8/R9
carry six arguments; RDX returns the scalar result and is zero on error.
The existing GPR, kernel-stack, native-return and blocked-continuation contract
continues to apply.

| RAX | Operation | Arguments | Successful RDX |
| --- | --- | --- | --- |
| `0x29` | THREAD_TLS_SET | RDI = full-width FS_BASE; all remaining arguments zero | 0 |
| `0x2a` | THREAD_TLS_GET | All arguments zero | Current Thread's FS_BASE |

These operations affect only the executing Thread. There is no handle, target
Thread argument, cross-Node operation or physical address. They require no new
capability grant; the caller controls its own TLS register, not another Thread.

## TLS contract

A new Thread starts with FS selector/base zero. THREAD_TLS_SET accepts zero or
any address below `0x0000800000000000`; noncanonical and upper-half addresses
return InvalidArgument (`-2`, RDX=0). Every reserved argument is checked at its
full width before any mutation. Failed SET/GET operations preserve the base.

The accepted base need not already be mapped. FS-relative loads/stores remain
subject to that Node's ordinary user page tables and permissions. An unmapped
access causes the existing contained user fault. Equal virtual TLS addresses
in unrelated Nodes refer to their different physical backing; sibling Threads
share the Node's AddressSpace and must choose separate TLS storage themselves.

FS_BASE survives native calls, timer/IPI/device interrupts, blocking/resumption
and migration. User returns normalize the FS selector to zero and install the
owned base. Direct FSGSBASE instructions remain disabled; changing segment
selectors is not an alternative TLS ABI. GS is reserved for the existing entry
mechanism and supplies no userspace TLS operation. Retirement clears the saved
base and the departing CPU's live user transport.

TLS allocation, ELF `PT_TLS` templates, thread-control-block layout, dynamic
linking and language/runtime conventions belong to userspace and are not
implemented by these two kernel operations.

## Floating-point execution contract

Boot checks every participating CPU for x87, MMX, FXSR, SSE and SSE2 and a
compatible MXCSR mask. Missing support refuses this runtime before constructing
or entering an extended-state Node. CPUs reporting a zero mask use the
architectural `0xffbf` fallback; MXCSR reserved bits cannot reach FXRSTOR.

Each Thread owns an aligned 512-byte FXSAVE64 image bound to its complete
Thread/Node/CR3/AddressSpace identity, including generations. New images have
FCW=`0x037f`, FSW/FTW/pointers zero, MXCSR=`0x1f80`, all eight x87/MMX slots and
all sixteen XMM registers zero, and initialized reserved bytes.

Ownership is eager: one loaded Thread per CPU and one CPU per Thread. CR0
EM/TS are clear and MP/NE set during userspace; CR4 OSFXSR/OSXMMEXCPT are set.
Assembly saves user FP and FS_BASE before Rust executes and restores the
original kernel FP image, FS selector/base and CR0/CR4 controls. A returning
IRQ reinstalls the same user image/base; a scheduling boundary publishes the
captured state under the shared transaction before another CPU may load it.
Retirement requires balanced saves/loads and no CPU owner, then scrubs the
image, identity, TLS and counters while retaining a generation watermark.

Architectural x87 `#MF` and SSE `#XM` exceptions follow the existing Node containment
and sibling-stop protocol. CR4 OSXSAVE/FSGSBASE remain clear in userspace; AVX,
AVX-512, XSAVE-managed extensions and direct FS/GS-base instructions are not
exposed. The compiler's kernel target continues to avoid implicit FP/SIMD use.

The stock QEMU 11 TCG acceptance verifies real `#MF`, SIMD state/status and
`#UD` containment. It records unmasked SSE error flags without delivering
`#XM`; hardware `#XM` delivery is not validated by this backend. See the
[QMP limitation and retained diagnosis](../testing/cpu-state-qmp.md).
