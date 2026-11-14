# Phase 9C-1 static two-CPU Ring 3 execution support.
#
# Each CPU owns one 16 KiB kernel entry arena, one bridge record and one
# exception-frame stack. The two arenas, their guard pages and the shared
# barrier page are image storage: the barrier is aliased user RW in both
# private roots, the arenas stay supervisor-only.
#
# No SWAPGS is used. CPU identity is re-derived from CPUID on every exit, and
# the bridge restores the owning CPU's IA32_GS_BASE before the kernel resumes.

.section .bss.smp_user,"aw",@nobits
.balign 4096

.global neu_smp_user_guard0_low, neu_smp_user_arena0, neu_smp_user_guard0_high
.global neu_smp_user_guard1_low, neu_smp_user_arena1, neu_smp_user_guard1_high
neu_smp_user_guard0_low:  .skip 4096
neu_smp_user_arena0:      .skip 16384
neu_smp_user_guard0_high: .skip 4096
neu_smp_user_guard1_low:  .skip 4096
neu_smp_user_arena1:      .skip 16384
neu_smp_user_guard1_high: .skip 4096

.balign 4096
.global neu_smp_user_barrier
neu_smp_user_barrier: .skip 4096

.section .text.smp_user,"ax"

# One validated Ring 3 entry on the calling CPU.
#   rdi = user RIP, rsi = user RSP, rdx = user CR3, rcx = kernel CR3,
#   r8  = user RFLAGS, r9 = this CPU's bridge record.
# The kernel continuation is suspended here and resumed exactly once by
# neu_smp_user_resume after the same CPU takes a Ring 3 exit.
.balign 16
.global neu_smp_user_enter
.type neu_smp_user_enter,@function
neu_smp_user_enter:
    cli
    cld
    push rbp
    push rbx
    push r12
    push r13
    push r14
    push r15
    mov qword ptr [r9 + 40], rdi   # user RIP
    mov qword ptr [r9 + 48], rsi   # user RSP
    mov qword ptr [r9 + 56], r8    # user RFLAGS
    mov qword ptr [r9 + 64], rdx   # user CR3
    mov qword ptr [r9 + 0], rsp    # kernel continuation RSP
    mov qword ptr [r9 + 8], rcx    # kernel CR3
    mov rax, cr0
    mov qword ptr [r9 + 16], rax
    or rax, 0x0a                   # MP+TS: no FPU/SIMD state is exposed.
    mov cr0, rax
    mov ecx, 0xc0000080
    rdmsr
    shl rdx, 32
    or rax, rdx
    mov qword ptr [r9 + 24], rax   # kernel EFER
    and eax, -2                    # no SYSCALL entry for this profile.
    wrmsr
    mov ecx, 0x174                 # no SYSENTER entry either.
    xor eax, eax
    xor edx, edx
    wrmsr
    mov ecx, 0xc0000101            # publish this CPU's identity, then clear it.
    rdmsr
    shl rdx, 32
    or rax, rdx
    mov qword ptr [r9 + 32], rax
    mov ecx, 0xc0000100
    xor eax, eax
    xor edx, edx
    wrmsr
    inc ecx
    wrmsr
    inc ecx
    wrmsr
    mov rax, qword ptr [r9 + 64]
    mov cr3, rax
    mov rdi, qword ptr [r9 + 40]
    mov rsi, qword ptr [r9 + 48]
    mov r8, qword ptr [r9 + 56]
    mov ax, 0x33
    mov ds, ax
    mov es, ax
    push 0x33
    push rsi
    push r8
    push 0x3b
    push rdi
    xor eax, eax
    xor ebx, ebx
    xor ecx, ecx
    xor edx, edx
    xor ebp, ebp
    xor esi, esi
    xor edi, edi
    xor r8d, r8d
    xor r9d, r9d
    xor r10d, r10d
    xor r11d, r11d
    xor r12d, r12d
    xor r13d, r13d
    xor r14d, r14d
    xor r15d, r15d
    iretq

# Resumes the suspended kernel continuation of the CPU that owns `rdi`.
# Ring 3 is already gone, so the kernel root, CR0, EFER and CPU-local GS base
# are restored before the original stack becomes live again.
.balign 16
.global neu_smp_user_resume
.type neu_smp_user_resume,@function
neu_smp_user_resume:
    cli
    cld
    mov rax, qword ptr [rdi + 8]
    mov cr3, rax
    mov rax, qword ptr [rdi + 16]
    mov cr0, rax
    mov rax, qword ptr [rdi + 24]
    mov rdx, rax
    shr rdx, 32
    mov ecx, 0xc0000080
    wrmsr
    mov rax, qword ptr [rdi + 32]
    mov rdx, rax
    shr rdx, 32
    mov ecx, 0xc0000101
    wrmsr
    mov rsp, qword ptr [rdi + 0]
    mov qword ptr [rdi + 0], 0
    pop r15
    pop r14
    pop r13
    pop r12
    pop rbx
    pop rbp
    ret

# Bounded BSP observation window between the prepared publication and the
# release of either CPU. QMP matches live RIP against these labels.
.balign 16
.global neu_smp_user_prepared_begin, neu_smp_user_prepared_end
.type neu_smp_user_prepared_dwell,@function
neu_smp_user_prepared_dwell:
neu_smp_user_prepared_begin:
    mov ecx, 0x04000000
1:
    dec ecx
    jne 1b
neu_smp_user_prepared_end:
    ret

# Bounded BSP observation window after both raw Ring 3 exits were accepted and
# before completion/teardown destroys the user frames. QMP reads raw physical
# results while this CPU is stopped inside these labels.
.balign 16
.global neu_smp_user_results_begin, neu_smp_user_results_end
.type neu_smp_user_results_dwell,@function
neu_smp_user_results_dwell:
neu_smp_user_results_begin:
    mov ecx, 0x04000000
1:
    dec ecx
    jne 1b
neu_smp_user_results_end:
    ret

# AP wait for its own PHASE word. rdi = &PHASE, rsi = target, rdx = alternate.
# Returns 0 when the word became `target`, 1 when it became `alternate`, and
# -1 on timeout. QMP matches the AP's live RIP against these labels.
.balign 16
.global neu_smp_user_wait_begin, neu_smp_user_wait_end
.type neu_smp_user_wait,@function
neu_smp_user_wait:
neu_smp_user_wait_begin:
    mov r8, rdx
    xor eax, eax
1:
    mov rcx, qword ptr [rdi]
    cmp rcx, rsi
    je 2f
    cmp rcx, r8
    je 4f
    inc rax
    cmp rax, 1000000000
    jae 3f
    pause
    jmp 1b
2:
    xor eax, eax
    ret
3:
    mov rax, -1
    ret
4:
    mov rax, 1
    ret
neu_smp_user_wait_end:

# These bytes are data in the kernel ELF, then copied to a NEW zeroed frame.
# All intra-fixture branches are relative; absolute addresses are the narrow
# user layout the two private roots publish. The stage-A dwell is role-specific
# so a stopped VM can require two distinct live Ring 3 instruction pointers.
#
# CONSTANTS: +0 seed, +8 kind, +16 role, +24 fault flag, +32 fault target.
# DATA:      +0 result, +8 stage-A mask, +16 stage-B mask, +24 survivor,
#            +32 release, +40 stack round-trip, +48 stage-A spins,
#            +56 entry TSC, +64 exit TSC.
# BARRIER:   +0 stage-A bits, +8 stage-B bits, +16 release, +24 survivor.
.section .rodata.smp_user_fixture,"a"
.balign 16
.global neu_smp_user_payload, neu_smp_user_payload_end
.global neu_smp_user_done, neu_smp_user_done_resume
.global neu_smp_user_fault_site
.global neu_smp_user_dwell0_begin, neu_smp_user_dwell0_end
.global neu_smp_user_dwell1_begin, neu_smp_user_dwell1_end
neu_smp_user_payload:
    or rax, rbx
    or rax, rcx
    or rax, rdx
    or rax, rbp
    or rax, rsi
    or rax, rdi
    or rax, r8
    or rax, r9
    or rax, r10
    or rax, r11
    or rax, r12
    or rax, r13
    or rax, r14
    or rax, r15
    jne neu_smp_user_done
    movabs r14, 0x401000
    movabs r15, 0x402000
    movabs r13, 0x407000
    rdtsc
    shl rdx, 32
    or rax, rdx
    mov qword ptr [r15 + 56], rax
    mov rax, qword ptr [r14]
    mov rbx, qword ptr [r14 + 8]
    add rax, rbx
    mov qword ptr [r15], rax
    mov r8, qword ptr [r14 + 16]
    mov r9, qword ptr [r14 + 24]
    mov r10, qword ptr [r14 + 32]
    mov rcx, r8
    lock bts qword ptr [r13], rcx
    mov r12d, 0x20000000
1:
    mov rax, qword ptr [r13]
    and eax, 3
    cmp eax, 3
    je 2f
    dec r12d
    jne 1b
    jmp neu_smp_user_done
2:
    mov qword ptr [r15 + 8], rax
    mov eax, 0x20000000
    sub eax, r12d
    mov qword ptr [r15 + 48], rax
    mov ecx, 0x08000000
    test r8, r8
    jnz 9f
neu_smp_user_dwell0_begin:
    dec ecx
    jne neu_smp_user_dwell0_begin
neu_smp_user_dwell0_end:
    jmp 10f
9:
neu_smp_user_dwell1_begin:
    dec ecx
    jne neu_smp_user_dwell1_begin
neu_smp_user_dwell1_end:
10:
    mov rcx, r8
    lock bts qword ptr [r13 + 8], rcx
    mov r12d, 0x20000000
3:
    mov rax, qword ptr [r13 + 8]
    and eax, 3
    cmp eax, 3
    je 4f
    dec r12d
    jne 3b
    jmp neu_smp_user_done
4:
    mov qword ptr [r15 + 16], rax
    test r8, r8
    jnz 6f
    lock bts qword ptr [r13 + 16], 0
    movabs rax, 0x4e45555f55535231
    mov qword ptr [r13 + 24], rax
    mov qword ptr [r15 + 24], rax
    jmp 8f
6:
    mov r12d, 0x20000000
5:
    mov rax, qword ptr [r13 + 16]
    test eax, 1
    jnz 7f
    dec r12d
    jne 5b
    jmp neu_smp_user_done
7:
    mov qword ptr [r15 + 32], rax
    test r9, r9
    jz 8f
    mov rdi, r10
neu_smp_user_fault_site:
    mov byte ptr [rdi], 0
8:
    mov rax, 0x5a5aa5a5
    push rax
    pop rbx
    mov qword ptr [r15 + 40], rbx
    mov r8w, cs
    mov r9w, ss
    mov rax, qword ptr [r15]
    mov rbx, qword ptr [r15 + 8]
    rdtsc
    shl rdx, 32
    or rax, rdx
    mov qword ptr [r15 + 64], rax
    mov rax, qword ptr [r15]
    mov rbx, qword ptr [r15 + 8]
neu_smp_user_done:
    int 0x80
neu_smp_user_done_resume:
    ud2
neu_smp_user_payload_end:
