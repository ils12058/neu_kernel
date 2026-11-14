# Reusable per-CPU Ring 3 bridge. Every Frame is an owned 160-byte value.
# Bridge headers are 16 words and owned by the executing CPU alone.
.section .text.smp_scheduler,"ax"
.balign 16
.global neu_smp_sched_enter
.type neu_smp_sched_enter,@function
# rdi=Frame, rsi=user CR3, rdx=bridge, rcx=kernel CR3
neu_smp_sched_enter:
    cli
    cld
    push rbp
    push rbx
    push r12
    push r13
    push r14
    push r15
    mov r8, rdx
    mov [r8 + 16], rsp
    mov [r8 + 24], rcx
    mov [r8 + 64], rdi
    mov [r8 + 72], rsi
    mov rax, cr0
    mov [r8 + 32], rax
    or rax, 12                 # no unsaved FPU/SIMD state
    and rax, -3
    mov cr0, rax
    mov ecx, 0xc0000080
    rdmsr
    shl rdx, 32
    or rax, rdx
    mov [r8 + 40], rax
    or eax, 1
    mov edx, 0
    wrmsr
    mov ecx, 0xc0000101
    rdmsr
    shl rdx, 32
    or rax, rdx
    mov [r8 + 48], rax
    inc ecx
    rdmsr
    shl rdx, 32
    or rax, rdx
    mov [r8 + 56], rax
    mov rax, r8
    mov rdx, rax
    shr rdx, 32
    wrmsr                     # KERNEL_GS_BASE=trusted per-CPU bridge
    dec ecx
    xor eax, eax
    xor edx, edx
    wrmsr                     # user GS_BASE=0
    mov ecx, 0xc0000081
    rdmsr
    shl rdx, 32
    or rax, rdx
    mov [r8 + 88], rax
    movabs rax, 0x0028000800000000
    mov rdx, rax
    shr rdx, 32
    wrmsr
    inc ecx
    rdmsr
    shl rdx, 32
    or rax, rdx
    mov [r8 + 96], rax
    lea rax, [rip + neu_smp_sched_syscall]
    mov rdx, rax
    shr rdx, 32
    wrmsr
    mov ecx, 0xc0000084       # SFMASK; 0x83 is CSTAR
    rdmsr
    shl rdx, 32
    or rax, rdx
    mov [r8 + 104], rax
    mov eax, 0x77700           # ABI ENTRY_MASK
    xor edx, edx
    wrmsr
    mov ecx, 0x174             # SYSENTER has no entry contract
    rdmsr
    shl rdx, 32
    or rax, rdx
    mov [r8 + 112], rax
    xor eax, eax
    xor edx, edx
    wrmsr
    mov ax, 0x33
    mov ds, ax
    mov es, ax
    mov rax, [r8 + 64]
    mov rdx, [r8 + 72]
    mov cr3, rdx
    push qword ptr [rax + 152]
    push qword ptr [rax + 144]
    push qword ptr [rax + 136]
    push qword ptr [rax + 128]
    push qword ptr [rax + 120]
    mov r15, [rax]
    mov r14, [rax + 8]
    mov r13, [rax + 16]
    mov r12, [rax + 24]
    mov r11, [rax + 32]
    mov r10, [rax + 40]
    mov r9,  [rax + 48]
    mov r8,  [rax + 56]
    mov rdi, [rax + 64]
    mov rsi, [rax + 72]
    mov rbp, [rax + 80]
    mov rdx, [rax + 88]
    mov rcx, [rax + 96]
    mov rbx, [rax + 104]
    mov rax, [rax + 112]
    iretq

.macro SAVE_GPRS
    push rax
    push rbx
    push rcx
    push rdx
    push rbp
    push rsi
    push rdi
    push r8
    push r9
    push r10
    push r11
    push r12
    push r13
    push r14
    push r15
.endm
.macro RESTORE_GPRS
    pop r15
    pop r14
    pop r13
    pop r12
    pop r11
    pop r10
    pop r9
    pop r8
    pop rdi
    pop rsi
    pop rbp
    pop rdx
    pop rcx
    pop rbx
    pop rax
.endm

.global neu_smp_sched_syscall
neu_smp_sched_syscall:
    cli
    swapgs
    mov qword ptr gs:[0], rsp
    mov rsp, qword ptr gs:[8]
    push 0x33
    push qword ptr gs:[0]
    push r11
    push 0x3b
    push rcx
    push 0
    push 128
    SAVE_GPRS
    swapgs
    cld
    mov ax, 0x10
    mov ds, ax
    mov es, ax
    mov rdi, rsp
    xor esi, esi
    and rsp, -16
    call neu_smp_sched_boundary
    ud2

.global neu_smp_sched_timer, neu_smp_sched_ipi, neu_smp_sched_spurious
neu_smp_sched_timer:
    push 0
    push 224
    jmp neu_smp_sched_irq_common
neu_smp_sched_ipi:
    push 0
    push 241
    jmp neu_smp_sched_irq_common
neu_smp_sched_spurious:
    push 0
    push 255
neu_smp_sched_irq_common:
    cli
    cld
    SAVE_GPRS
    mov ax, 0x10
    mov ds, ax
    mov es, ax
    mov rbx, rsp
    mov rdi, rsp
    xor esi, esi
    and rsp, -16
    call neu_smp_sched_interrupt
    mov rsp, rbx
    test byte ptr [rsp + 144], 3
    jz 2f
    mov ax, 0x33
    mov ds, ax
    mov es, ax
2:
    RESTORE_GPRS
    add rsp, 16
    iretq

.global neu_smp_sched_resume
# Same CPU only; abandons the normalized frame after Rust copied all values.
neu_smp_sched_resume:
    cli
    cld
    mov r8, rdi
    mov rax, [r8 + 24]
    mov cr3, rax
    mov rax, [r8 + 32]
    mov cr0, rax
    mov ax, 0x10
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov ecx, 0xc0000080
    mov rax, [r8 + 40]
    mov rdx, rax
    shr rdx, 32
    wrmsr
    mov ecx, 0xc0000101
    mov rax, [r8 + 48]
    mov rdx, rax
    shr rdx, 32
    wrmsr
    inc ecx
    mov rax, [r8 + 56]
    mov rdx, rax
    shr rdx, 32
    wrmsr
    mov ecx, 0xc0000081
    mov rax, [r8 + 88]
    mov rdx, rax
    shr rdx, 32
    wrmsr
    inc ecx
    mov rax, [r8 + 96]
    mov rdx, rax
    shr rdx, 32
    wrmsr
    mov ecx, 0xc0000084
    mov rax, [r8 + 104]
    mov rdx, rax
    shr rdx, 32
    wrmsr
    mov ecx, 0x174
    mov rax, [r8 + 112]
    mov rdx, rax
    shr rdx, 32
    wrmsr
    mov rsp, [r8 + 16]
    pop r15
    pop r14
    pop r13
    pop r12
    pop rbx
    pop rbp
    ret

.global neu_smp_sched_idle, neu_smp_sched_idle_begin, neu_smp_sched_idle_end
neu_smp_sched_idle:
    cli
    cmp [rdi], rsi
    jne 1f
neu_smp_sched_idle_begin:
    sti
    hlt
    cli
neu_smp_sched_idle_end:
1:  ret

.global neu_smp_sched_drain
neu_smp_sched_drain:
    sti
    nop
    nop
    cli
    ret

.global neu_smp_sched_probe_wait, neu_smp_sched_probe_begin, neu_smp_sched_probe_end
neu_smp_sched_probe_wait:
neu_smp_sched_probe_begin:
    mov ecx, dword ptr [rdi]
    test ecx, ecx
    jz neu_smp_sched_probe_end
    pause
    jmp neu_smp_sched_probe_begin
neu_smp_sched_probe_end:
    ret

# QMP can observe these exact instructions and the following masked timer
# window before Rust publishes this CPU's invalidation acknowledgement.
# rdi=address, rsi=actual root, rdx=mode, rcx=timer, r8=epoch.
.global neu_smp_sched_invalidate, neu_smp_sched_invlpg_done
.global neu_smp_sched_root_flush_done, neu_smp_sched_tlb_probe_begin, neu_smp_sched_tlb_probe_end
neu_smp_sched_invalidate:
    cmp rdx, 1
    jne 1f
    invlpg [rdi]
neu_smp_sched_invlpg_done:
    jmp neu_smp_sched_tlb_probe_begin
1:  mov cr3, rsi
neu_smp_sched_root_flush_done:
neu_smp_sched_tlb_probe_begin:
    mov eax, dword ptr [rcx]
    test eax, eax
    jz neu_smp_sched_tlb_probe_end
    pause
    jmp neu_smp_sched_tlb_probe_begin
neu_smp_sched_tlb_probe_end:
    ret

.section .bss.smp_sched_barrier,"aw",@nobits
.balign 4096
.global neu_smp_sched_barrier
neu_smp_sched_barrier: .skip 4096

# Same payload for every Thread; bootstrap stack words select tag and action.
# DATA has 128-byte private records per tag: progress, canary, ABI, received
# value, terminal tag. The two Threads of Node A share DATA but own disjoint
# records. BAR+0 releases work, +16 triggers a Node fault, +32 records containment.
.section .rodata.smp_sched_fixture,"a"
.balign 16
.global neu_smp_sched_payload, neu_smp_sched_payload_end
.global neu_smp_sched_user_loop_begin, neu_smp_sched_user_loop_end
.global neu_smp_sched_fault_site, neu_smp_sched_yield_resume
.global neu_smp_sched_receive_resume, neu_smp_sched_done_resume
neu_smp_sched_payload:
    mov r12, [rsp]
    mov r13, [rsp - 8]
    mov r10, [rsp - 16]         # endpoint bootstrap handle
    mov r11, [rsp - 24]         # action
    movabs r14, 0x402000
    movabs r15, 0x417000
    mov rbp, r12
    dec rbp
    shl rbp, 7
    add rbp, r14
    mov [rbp + 40], r11
    mov [rbp + 48], r10
    mov eax, 1
    syscall
.global neu_smp_sched_query_resume
neu_smp_sched_query_resume:
    test rax, rax
    jne neu_smp_sched_bad
    mov [rbp + 16], rdx
    cmp qword ptr [rbp + 40], 20
    jae neu_authority_prepare
    # The two same-Node Threads inspect distinct grants while another CPU
    # may transact against their shared authority table.
    xor edi, edi
    cmp r12, 3
    jne 9f
    inc edi
9:  mov eax, 0x10
    syscall
    test rax, rax
    jne neu_smp_sched_bad
    mov [rbp + 56], rdx
    mov rdi, rdx
    mov esi, 0x400000
    mov eax, 0x12
    syscall
    test rax, rax
    jne neu_smp_sched_bad
    cmp rdx, 11               # actual RX/user CODE PTE flags
    jne neu_smp_sched_bad
    mov [rbp + 80], rdx
    mov rdi, [rbp + 56]
    mov eax, 0x11
    syscall
    test rax, rax
    jne neu_smp_sched_bad
    cmp qword ptr [rbp + 40], 4
    je neu_smp_sched_tlb_warm
    cmp qword ptr [rbp + 40], 7
    jne 8f
neu_smp_sched_tlb_warm:
    lea r9, [r12 - 1]
    shl r9, 3
    add r9, 0x410200
    mov [r9], r12
    mov rax, [r9]
    mov [rbp + 64], rax        # outside all three 128-byte DATA rows
8:
    cmp qword ptr [rbp + 40], 3
    jne 1f
    mov eax, 0x16
    mov rdi, [rbp + 48]
    syscall
neu_smp_sched_receive_resume:
    test rax, rax
    jne neu_smp_sched_bad
    mov [rbp + 24], rdx
1:
    movabs r8, 0x1122334455667788
    xor ebx, ebx
neu_smp_sched_user_loop_begin:
    inc rbx
    mov [rbp], rbx
    mov [rbp + 8], r8
    cmp qword ptr [rbp + 40], 8
    jne 6f
    lock inc qword ptr [r14 + 0x300] # real same-Node atomic shared counter
6:
    cmp qword ptr [r15], 1
    jne neu_smp_sched_user_loop_begin
neu_smp_sched_user_loop_end:
    cmp qword ptr [rbp + 40], 19
    je neu_authority_disabled
    cmp qword ptr [rbp + 40], 20
    jae neu_authority_action
    cmp qword ptr [rbp + 40], 1
    je neu_smp_sched_user_fault_wait
    cmp qword ptr [rbp + 40], 4
    je neu_smp_sched_user_tlb_wait
    cmp qword ptr [rbp + 40], 5
    je neu_smp_sched_user_survivor_wait
    cmp qword ptr [rbp + 40], 6
    je neu_smp_sched_user_sibling
    cmp qword ptr [rbp + 40], 7
    je neu_smp_sched_user_sibling
    cmp qword ptr [rbp + 40], 10
    je neu_smp_sched_user_reject_wait
    cmp qword ptr [rbp + 40], 3
    je neu_smp_sched_receive_cap_loop
    cmp qword ptr [rbp + 40], 2
    jne 2f
    mov eax, 0x13
    mov rdi, [rbp + 48]
    mov rsi, r13
    syscall
    test rax, rax
    jne neu_smp_sched_bad
    mov eax, 0x10
    mov edi, 3
    syscall
    test rax, rax
    jne neu_smp_sched_bad
    mov [rbp + 72], rdx
    mov rsi, rdx
    mov rdi, [rbp + 48]
    mov eax, 0x17
    syscall
    test rax, rax
    jne neu_smp_sched_bad
    mov rdi, [rbp + 72]
    mov rax, -1024
    syscall
    cmp rax, -17              # moved-from Flag handle is stale
    jne neu_smp_sched_bad
    mov [rbp + 88], rax
neu_smp_sched_move_ack_wait:
    cmp qword ptr [r15 + 40], 1
    jne neu_smp_sched_move_ack_wait
    jmp 2f
neu_smp_sched_receive_cap_loop:
    mov rdi, [rbp + 48]
    mov eax, 0x18
    syscall
    cmp rax, -32
    jne 7f
    mov rax, -256
    syscall
    jmp neu_smp_sched_receive_cap_loop
7:  test rax, rax
    jne neu_smp_sched_bad
    mov [rbp + 72], rdx
    mov rdi, rdx
    mov rax, -1024
    syscall
    test rax, rax
    jne neu_smp_sched_bad
    cmp rdx, 1
    jne neu_smp_sched_bad
    mov [rbp + 88], rdx
    mov rdi, [rbp + 72]
    mov eax, 0x11
    syscall
    test rax, rax
    jne neu_smp_sched_bad
    mov qword ptr [r15 + 40], 1
2:
.global neu_smp_sched_user_done
neu_smp_sched_user_done:
    # Internal yield uses the same full saved context and validated native exit.
    mov rax, -256
    syscall
neu_smp_sched_yield_resume:
    test rax, rax
    jne neu_smp_sched_bad
    mov [rbp + 32], r12
    mov eax, 2
    mov edi, 128
    syscall
neu_smp_sched_done_resume:
    ud2
neu_smp_sched_user_fault_wait:
    cmp qword ptr [r15 + 16], 1
    jne neu_smp_sched_user_fault_wait
    mov rax, [r15 + 24]
neu_smp_sched_fault_site:
    mov byte ptr [rax], 1
    jmp neu_smp_sched_bad
neu_smp_sched_user_tlb_wait:
    cmp qword ptr [r15 + 16], 1
    jne neu_smp_sched_user_tlb_wait
.global neu_smp_sched_tlb_fault_site
neu_smp_sched_tlb_fault_site:
    mov qword ptr [r9], 255    # must #PF at 0x410210 after remote INVLPG
    jmp neu_smp_sched_bad
neu_smp_sched_user_reject_wait:
    cmp qword ptr [r15 + 16], 1
    jne neu_smp_sched_user_reject_wait
    inc rsp                   # unsafe, unaligned/out-of-range user return stack
    std                       # unsafe user return flags; entry must clear DF
    mov eax, 1
    syscall
.global neu_smp_sched_reject_resume
neu_smp_sched_reject_resume:
    ud2                       # rejected frame must never enter Ring 3 again
neu_smp_sched_user_sibling:
    mov qword ptr [r15 + 16], 1 # published only by a live Ring 3 sibling
neu_smp_sched_user_sibling_loop:
    inc rbx
    mov [rbp], rbx
    jmp neu_smp_sched_user_sibling_loop
neu_smp_sched_user_survivor_wait:
    inc rbx
    mov [rbp], rbx
    cmp qword ptr [r15 + 32], 1
    jne neu_smp_sched_user_survivor_wait
    jmp 2b
neu_smp_sched_bad:
    mov eax, 2
    mov edi, 255
    syscall
    ud2

# K10 operations share the same native/timer bridge and full context checks.
# Upper argument registers are explicitly zero for the versioned authority ABI.
.macro AUTH_ZERO
    xor edx, edx
    xor r10d, r10d
    xor r8d, r8d
    xor r9d, r9d
.endm
.global neu_authority_prepare, neu_authority_action
neu_authority_prepare:
    mov eax, 0x10
    mov edi, 3
    syscall
    test rax, rax
    jne neu_smp_sched_bad
    mov [rbp + 56], rdx
    cmp r12, 2
    je neu_authority_warm_b
    cmp r12, 3
    jne neu_authority_warm_a
    test qword ptr [r15 + 96], 4
    jz neu_authority_warm_a
    mov rdi, [rbp + 48]
    mov esi, 64
    AUTH_ZERO
    mov eax, 0x19
    syscall
    test rax, rax
    jne neu_smp_sched_bad
    mov [rbp + 96], rdx
neu_authority_warm_a:
    mov rax, [0x410000]
    mov [rbp + 64], rax
    mov rax, [0x411000]
    mov [rbp + 72], rax
    jmp neu_authority_loop
neu_authority_warm_b:
    mov rax, [0x420000]
    mov [rbp + 64], rax
    mov rax, [0x421000]
    mov [rbp + 72], rax
    mov [0x430008], r13
    mov [0x431008], r13
neu_authority_loop:
    movabs r8, 0x1122334455667788
    xor ebx, ebx
    jmp neu_smp_sched_user_loop_begin
neu_authority_action:
    cmp r12, 2
    je neu_authority_receiver
    cmp r12, 3
    je neu_authority_sibling
    test qword ptr [r15 + 96], 4
    jz neu_authority_derive
neu_authority_wait_registered:
    cmp qword ptr [r15 + 80], 2
    jne neu_authority_wait_registered
    mov rdi, [rbp + 48]
    xor esi, esi
    AUTH_ZERO
    mov eax, 0x1a
    syscall
    test rax, rax
    jne neu_smp_sched_bad
    cmp rdx, 1
    jne neu_smp_sched_bad
    mov [rbp + 112], rdx
neu_authority_derive:
    mov rdi, [rbp + 56]
    mov esi, 0x105            # READ | MAP | DERIVE
    AUTH_ZERO
    mov eax, 0x19
    syscall
    test rax, rax
    jne neu_smp_sched_bad
    mov [rbp + 96], rdx
    mov rdi, rdx
    mov esi, 2                # WRITE amplification must fail
    AUTH_ZERO
    mov eax, 0x19
    syscall
    cmp rax, -18
    jne neu_smp_sched_bad
    test rdx, rdx
    jne neu_smp_sched_bad
    inc qword ptr [rbp + 104]
    mov rdi, [rbp + 96]
    mov esi, 1
    AUTH_ZERO
    mov eax, 0x19
    syscall
    test rax, rax
    jne neu_smp_sched_bad
    mov [rbp + 120], rdx
    mov rdi, rdx              # no DERIVE on this descendant
    mov esi, 1
    AUTH_ZERO
    mov eax, 0x19
    syscall
    cmp rax, -18
    jne neu_smp_sched_bad
    inc qword ptr [rbp + 104]
    mov rdi, [rbp + 120]
    mov eax, 0x11
    syscall
    test rax, rax
    jne neu_smp_sched_bad
    mov rdi, [rbp + 96]
    mov eax, 0x11
    syscall
    test rax, rax
    jne neu_smp_sched_bad
    mov rdi, [rbp + 56]
    mov esi, 0x400            # unknown authority bit
    AUTH_ZERO
    mov eax, 0x19
    syscall
    cmp rax, -18
    jne neu_smp_sched_bad
    inc qword ptr [rbp + 104]
    mov rdi, [rbp + 56]
    mov esi, 1
    AUTH_ZERO
    mov r9d, 1               # reserved ABI input
    mov eax, 0x19
    syscall
    cmp rax, -2
    jne neu_smp_sched_bad
    inc qword ptr [rbp + 104]
    test qword ptr [r15 + 96], 1
    jz neu_authority_revoke
    mov rdi, [rbp + 56]
    mov esi, 13               # READ | MAP | TRANSFER
    AUTH_ZERO
    mov eax, 0x19
    syscall
    test rax, rax
    jne neu_smp_sched_bad
    mov rsi, rdx
    mov rdi, [rbp + 48]
    mov eax, 0x17
    syscall
    test rax, rax
    jne neu_smp_sched_bad
neu_authority_revoke:
    mov rdi, [rbp + 56]
    xor esi, esi
    AUTH_ZERO
    mov eax, 0x1a
    syscall
.global neu_authority_revoke_resume
neu_authority_revoke_resume:
    test rax, rax
    jne neu_smp_sched_bad
    mov [rbp + 88], rdx
    mov ecx, 1
    test qword ptr [r15 + 96], 8
    jz neu_authority_rederive_count
    mov ecx, 16
neu_authority_rederive_count:
    mov [rbp + 96], rcx
neu_authority_rederive:
    mov rdi, [rbp + 56]
    mov esi, 5
    AUTH_ZERO
    mov eax, 0x19
    syscall
    test rax, rax
    jne neu_smp_sched_bad
    mov rdi, rdx
    mov eax, 0x11
    syscall
    test rax, rax
    jne neu_smp_sched_bad
    dec qword ptr [rbp + 96]
    jnz neu_authority_rederive
    mov qword ptr [r15 + 64], 1
    test qword ptr [r15 + 96], 2
    jnz neu_smp_sched_user_done
    mov qword ptr [r15 + 16], 1
    jmp neu_smp_sched_user_sibling_loop
neu_authority_sibling:
    test qword ptr [r15 + 96], 16
    jnz neu_authority_pending_fault_wait
    test qword ptr [r15 + 96], 4
    jz neu_authority_sibling_wait
    mov rdi, [rbp + 96]
    mov eax, 0x16
    syscall
    cmp rax, -38
    jne neu_smp_sched_bad
    test rdx, rdx
    jne neu_smp_sched_bad
    mov [rbp + 88], rax
neu_authority_sibling_wait:
    cmp qword ptr [r15 + 64], 1
    jne neu_authority_sibling_wait
    test qword ptr [r15 + 96], 2
    jnz neu_smp_sched_user_done
    mov eax, 0x410010
.global neu_authority_fault_site
neu_authority_fault_site:
    mov qword ptr [rax], 255   # a completed revoke must cause user #PF, error 6
    jmp neu_smp_sched_bad
neu_authority_receiver:
    cmp qword ptr [r15 + 64], 1
    jne neu_authority_receiver
    movabs rdi, 0x200000002  # delivered descendant was purged from Node B
    mov esi, 1
    AUTH_ZERO
    mov eax, 0x19
    syscall
    cmp rax, -17
    jne neu_smp_sched_bad
    mov [rbp + 88], rax
    mov rax, [0x430008]
    cmp rax, r13
    jne neu_smp_sched_bad
    mov rax, [0x431008]
    cmp rax, r13
    jne neu_smp_sched_bad
    mov [rbp + 80], rax
    mov rdi, [rbp + 56]
    mov esi, 5
    AUTH_ZERO
    mov eax, 0x19
    syscall
    test rax, rax
    jne neu_smp_sched_bad
    mov rdi, rdx
    mov eax, 0x11
    syscall
    test rax, rax
    jne neu_smp_sched_bad
    test qword ptr [r15 + 96], 1
    jz neu_authority_receiver_done
    mov rdi, [rbp + 48]
    mov eax, 0x18
    syscall
    cmp rax, -32
    jne neu_smp_sched_bad
    mov [rbp + 112], rax
neu_authority_receiver_done:
    test qword ptr [r15 + 96], 2
    jnz neu_smp_sched_user_done
    jmp neu_smp_sched_user_survivor_wait
neu_authority_pending_fault_wait:
    cmp qword ptr [r15 + 112], 1
    jne neu_authority_pending_fault_wait
    mov eax, 0x440000
.global neu_authority_pending_fault_site
neu_authority_pending_fault_site:
    mov qword ptr [rax], 255   # cancels requester while its revoke is pending
    jmp neu_smp_sched_bad
neu_authority_disabled:
    mov rdi, [rbp + 56]
    mov esi, 1
    AUTH_ZERO
    mov eax, 0x19
    syscall
    cmp rax, -1
    jne neu_smp_sched_bad
    test rdx, rdx
    jne neu_smp_sched_bad
    inc qword ptr [rbp + 104]
    mov rdi, [rbp + 56]
    xor esi, esi
    AUTH_ZERO
    mov eax, 0x1a
    syscall
    cmp rax, -1
    jne neu_smp_sched_bad
    test rdx, rdx
    jne neu_smp_sched_bad
    inc qword ptr [rbp + 104]
    jmp neu_smp_sched_user_done
neu_smp_sched_payload_end:
