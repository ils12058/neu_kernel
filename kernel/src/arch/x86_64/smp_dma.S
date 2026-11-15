# Ring 3 ABI 0.8 exercise. All hardware access is mediated by typed handles.
# Shared barrier words only coordinate this finite test, never grant authority.
.section .rodata.smp_dma_fixture,"a"
.balign 16
.global neu_smp_sched_payload, neu_smp_sched_payload_end
.global neu_smp_sched_user_loop_begin, neu_smp_sched_user_loop_end
.global neu_smp_sched_fault_site, neu_smp_sched_yield_resume
.global neu_smp_sched_query_resume, neu_smp_sched_done_resume
.macro Z
    xor esi, esi
    xor edx, edx
    xor r10d, r10d
    xor r8d, r8d
    xor r9d, r9d
.endm
.macro OK
    test rax, rax
    jne dma_bad
.endm
.macro ERR value
    cmp rax, \value
    jne dma_bad
    test rdx, rdx
    jne dma_bad
    inc qword ptr [rbp + 104]
.endm
neu_smp_sched_payload:
    mov r12, [rsp]
    mov r13, [rsp - 8]
    mov r10, [rsp - 16]
    mov r11, [rsp - 24]
    mov r14d, 0x402000
    mov r15d, 0x417000
    mov rbp, r12
    dec rbp
    shl rbp, 7
    add rbp, r14
    mov [rbp + 40], r11
    mov [rbp + 48], r10
.if NEU_CPU_STATE
    lea rdi, [r12 - 1]
    shl rdi, 9
    lea rdi, [rdi + r14 + 0x400]
    fxsave64 [rdi]
.endif
    mov eax, 1
    syscall
neu_smp_sched_query_resume:
    OK
    mov [rbp + 16], rdx
.if NEU_CPU_STATE
    xor edi, edi
    Z
    mov eax, 0x2a
    syscall
    or rax, rdx
    jne dma_bad
    mov edi, 0x402200
    cmp r12, 3
    jne 20f
    add edi, 0x20
20: Z
    mov eax, 0x29
    syscall
    or rax, rdx
    jne dma_bad
    mov qword ptr fs:[0], r13
    mov qword ptr fs:[8], r12
    mov word ptr fs:[16], 0x37f
    fldcw word ptr fs:[16]
    mov dword ptr fs:[16], 0x1f80
    ldmxcsr dword ptr fs:[16]
.macro DMA_CPU_ST index
    mov rax, r12
    imul rax, 100
    add rax, \index
    mov qword ptr fs:[16], rax
    fild qword ptr fs:[16]
.endm
    DMA_CPU_ST 1
    DMA_CPU_ST 2
    DMA_CPU_ST 3
    DMA_CPU_ST 4
    DMA_CPU_ST 5
    DMA_CPU_ST 6
    DMA_CPU_ST 7
    DMA_CPU_ST 8
.macro DMA_CPU_XMM register, index
    mov rax, r12
    shl rax, 48
    mov ecx, \index
    shl rcx, 32
    or rax, rcx
    mov edx, 0x11223344
    or rax, rdx
    movq \register, rax
    punpcklqdq \register, \register
.endm
    DMA_CPU_XMM xmm0, 0
    DMA_CPU_XMM xmm1, 1
    DMA_CPU_XMM xmm2, 2
    DMA_CPU_XMM xmm3, 3
    DMA_CPU_XMM xmm4, 4
    DMA_CPU_XMM xmm5, 5
    DMA_CPU_XMM xmm6, 6
    DMA_CPU_XMM xmm7, 7
    DMA_CPU_XMM xmm8, 8
    DMA_CPU_XMM xmm9, 9
    DMA_CPU_XMM xmm10, 10
    DMA_CPU_XMM xmm11, 11
    DMA_CPU_XMM xmm12, 12
    DMA_CPU_XMM xmm13, 13
    DMA_CPU_XMM xmm14, 14
    DMA_CPU_XMM xmm15, 15
.global neu_cpu_dma_seed_done
neu_cpu_dma_seed_done:
.endif
    xor edi, edi
    mov eax, 0x10
    syscall
    OK
    mov rdi, rdx
    mov esi, 0x400000
    mov eax, 0x12
    syscall
    OK
    cmp rdx, 11
    jne dma_bad
    mov [rbp + 80], rdx
    mov edi, 5
    mov eax, 0x10
    syscall
    OK
    mov [rbp + 56], rdx
    movabs r8, 0x1122334455667788
    xor ebx, ebx
neu_smp_sched_user_loop_begin:
    inc rbx
    mov [rbp], rbx
    mov [rbp + 8], r8
    cmp qword ptr [r15], 1
    jne neu_smp_sched_user_loop_begin
neu_smp_sched_user_loop_end:
    cmp r12, 1
    je dma_helper
    cmp r12, 2
    je dma_successor
    call dma_open
    # Two attenuated children consume the last two bounded handle slots.
    mov rdi, [rbp + 64]
    Z
    mov esi, 4
    cmp qword ptr [r15 + 96], 7
    jne dma_map_child_rights
    mov esi, 32
dma_map_child_rights:
    mov eax, 0x19
    syscall
    OK
    mov [r15 + 224], rdx
    mov rdi, rdx
    Z
    mov eax, 0x23
    syscall
    ERR -18
    mov rdi, [rbp + 96]
    Z
    mov esi, 1
    mov eax, 0x19
    syscall
    OK
    mov [r15 + 232], rdx
    mov rdi, rdx
    Z
    mov edx, 1
    mov eax, 0x27
    syscall
    ERR -18
    mov rdi, [rbp + 56]
    Z
    mov eax, 0x22
    syscall
    ERR -19
    mov rdi, [rbp + 96]
    Z
    mov esi, 1
    mov eax, 0x26
    syscall
    ERR -49
    mov rdi, [rbp + 96]
    Z
    mov esi, 8192
    mov eax, 0x26
    syscall
    ERR -49
    mov rdi, [rbp + 64]
    Z
    mov rsi, [rbp + 96]
    mov edx, 3
    mov r9d, 1
    mov eax, 0x24
    syscall
    ERR -2
    # Mapping through a strict domain descendant makes revocation meaningful.
    mov rdi, [r15 + 224]
    cmp qword ptr [r15 + 96], 7
    jne dma_map_child
    mov rdi, [rbp + 64]
dma_map_child:
    call dma_map_rw
    mov rdi, [rbp + 64]
    Z
    mov rsi, [rbp + 96]
    mov edx, 3
    mov eax, 0x24
    syscall
    ERR -51
    call dma_arm
    mov rdi, [r15 + 224]
    Z
    mov rsi, [r15 + 232]
    mov r10d, 8
    mov r8d, 1
    mov eax, 0x28
    syscall
    ERR -18
    mov rdi, [rbp + 64]
    Z
    mov rsi, [rbp + 96]
    mov edx, 8191
    mov r10d, 2
    mov r8d, 1
    mov eax, 0x28
    syscall
    ERR -49
    mov rdi, [rbp + 64]
    Z
    mov rsi, [rbp + 96]
    mov r10d, 4097
    mov r8d, 1
    mov eax, 0x28
    syscall
    ERR -49
    mov rdi, [rbp + 64]
    Z
    mov rsi, [rbp + 96]
    mov r10d, 8
    mov r8d, 3
    mov eax, 0x28
    syscall
    ERR -50
    mov rax, [rbp + 64]
    mov [r15 + 112], rax
    mov qword ptr [r15 + 64], 1
dma_foreign_wait:
    cmp qword ptr [r15 + 192], 1
    jne dma_foreign_wait
    mov qword ptr [r15 + 208], 1
dma_mapped_wait:
    cmp qword ptr [r15 + 216], 1
    jne dma_mapped_wait
    call dma_roundtrip
    mov rax, [r15 + 96]
    cmp rax, 10
    jae dma_fault_probe
    cmp rax, 2
    jb dma_close
    cmp rax, 5
    je dma_quota
    mov qword ptr [r15 + 168], 1
    cmp rax, 2
    je dma_cancel_wait
    cmp rax, 3
    je dma_cancel_wait
    cmp rax, 7
    je dma_cancel_wait
    call dma_submit_read
    cmp qword ptr [r15 + 96], 6
    je dma_node_fault
dma_cancel_wait:
    mov rdi, [rbp + 88]
    Z
    mov eax, 0x20
    syscall
    cmp qword ptr [r15 + 96], 4
    je dma_timeout
    ERR -46
    jmp dma_retired
dma_timeout:
    OK
    jmp dma_retired
dma_node_fault:
    movabs rax, 0xffffb00000001000
neu_smp_sched_fault_site:
    mov byte ptr [rax], 1
    jmp dma_bad
dma_quota:
    mov ebx, 14
dma_quota_loop:
    call dma_submit_read
    call dma_wait_ack
    dec ebx
    jne dma_quota_loop
    call dma_arm
    mov rdi, [rbp + 64]
    Z
    mov rsi, [rbp + 96]
    mov edx, 4080
    mov r10d, 32
    mov r8d, 1
    mov eax, 0x28
    syscall
    ERR -53
    jmp dma_retired
dma_fault_probe:
    mov rdi, [rbp + 64]
    Z
    mov rsi, [rbp + 96]
    mov eax, 0x25
    syscall
    OK
    cmp qword ptr [r15 + 96], 11
    je dma_readonly_map
    cmp qword ptr [r15 + 96], 12
    jne dma_start_probe
    mov rdi, [rbp + 64]
    call dma_map_rw
    jmp dma_start_probe
dma_readonly_map:
    mov rdi, [rbp + 64]
    Z
    mov rsi, [rbp + 96]
    mov edx, 1
    mov eax, 0x24
    syscall
    OK
    call dma_arm
    mov rdi, [rbp + 64]
    Z
    mov rsi, [rbp + 96]
    mov r10d, 8
    mov r8d, 2
    mov eax, 0x28
    syscall
    ERR -50
dma_start_probe:
    call dma_arm
    mov qword ptr [r15 + 176], 1
dma_probe_wait:
    cmp qword ptr [r15 + 184], 1
    jne dma_probe_wait
dma_close:
    mov rdi, [rbp + 72]
    mov eax, 0x11
    syscall
    OK
dma_retired:
    inc qword ptr [rbp]
    cmp qword ptr [r15 + 152], 1
    jne dma_retired
    mov rdi, [rbp + 96]
    Z
    mov eax, 0x26
    syscall
    ERR -17
    jmp dma_done_wait

dma_helper:
    cmp qword ptr [r15 + 144], 1
    je dma_done_wait
    cmp qword ptr [r15 + 168], 1
    jne dma_helper
    cmp qword ptr [r15 + 88], 3
    jb dma_helper
    mov rax, [r15 + 96]
    cmp rax, 2
    je dma_helper_submit
    cmp rax, 3
    je dma_helper_submit
    cmp rax, 7
    jne dma_done_wait
dma_helper_submit:
    call dma_arm
    mov rdi, [r14 + 320]
    cmp qword ptr [r15 + 96], 7
    jne dma_helper_submit_anchor
    mov rdi, [r15 + 224]
dma_helper_submit_anchor:
    Z
    mov rsi, [r14 + 352]
    mov edx, 4080
    mov r10d, 32
    mov r8d, 1
    mov eax, 0x28
    syscall
    OK
    mov rax, [r15 + 96]
    cmp rax, 2
    je dma_helper_close
    cmp rax, 3
    je dma_helper_revoke
    cmp rax, 7
    jne dma_done_wait
dma_helper_revoke:
    mov rdi, [r14 + 320]    # original domain in the shared Node A table
    Z
    mov eax, 0x1a
    syscall
    OK
    cmp rdx, 1
    jne dma_bad
    mov [rbp + 112], rdx
    jmp dma_done_wait
dma_helper_close:
    mov rdi, [r14 + 352]    # original DMA buffer; closes a live in-flight lease
    mov eax, 0x11
    syscall
    OK
    jmp dma_done_wait

dma_successor:
    cmp qword ptr [r15 + 64], 1
    jne dma_successor
    mov rdi, [rbp + 56]
    Z
    mov eax, 0x1c
    syscall
    ERR -40
    mov rdi, [r15 + 112]
    Z
    mov eax, 0x23
    syscall
    ERR -17
    mov qword ptr [r15 + 192], 1
dma_successor_wait:
    inc qword ptr [rbp]
    cmp qword ptr [r15 + 152], 1
    jne dma_successor_wait
    call dma_open
    mov rdi, [rbp + 64]
    call dma_map_rw
    # Read all retained device SRAM before writing anything as the new owner.
    call dma_arm
    mov rdi, [rbp + 64]
    Z
    mov rsi, [rbp + 96]
    mov r10d, 4096
    mov r8d, 2
    mov eax, 0x28
    syscall
    OK
    call dma_wait_ack
    mov rdi, [rbp + 96]
    Z
    mov eax, 0x26
    syscall
    OK
    test rdx, rdx
    jne dma_bad
    mov rdi, [rbp + 96]
    Z
    mov esi, 4088
    mov eax, 0x26
    syscall
    OK
    test rdx, rdx
    jne dma_bad
    call dma_roundtrip
    mov rdi, [rbp + 64]
    Z
    mov rsi, [rbp + 96]
    mov eax, 0x25
    syscall
    OK
    mov rdi, [rbp + 72]
    mov eax, 0x11
    syscall
    OK
dma_successor_release:
    cmp qword ptr [r15 + 128], 1
    jne dma_successor_release
    mov qword ptr [r15 + 144], 1
dma_done_wait:
    inc qword ptr [rbp]
    cmp qword ptr [r15 + 144], 1
    jne dma_done_wait
.if NEU_CPU_STATE
    lea rdi, [r12 - 1]
    shl rdi, 9
    lea rdi, [rdi + r14 + 0xa00]
    fxsave64 [rdi]
    mov rax, qword ptr fs:[0]
    cmp rax, r13
    jne dma_bad
    cmp qword ptr fs:[8], r12
    jne dma_bad
.endif
    mov rax, -256
    syscall
neu_smp_sched_yield_resume:
    OK
    mov [rbp + 32], r12
    mov eax, 2
    mov edi, 128
    syscall
neu_smp_sched_done_resume:
    ud2

dma_open:
    mov edi, 3
    mov eax, 0x10
    syscall
    OK
    mov rdi, rdx
    mov eax, 0x11
    syscall
    OK
    mov edi, 4
    mov eax, 0x10
    syscall
    OK
    mov rdi, rdx
    mov eax, 0x11
    syscall
    OK
    mov rdi, [rbp + 56]
    Z
    mov eax, 0x1c
    syscall
    OK
    mov [rbp + 72], rdx
    mov rdi, rdx
    Z
    mov eax, 0x1d
    syscall
    OK
    mov [rbp + 88], rdx
    mov rdi, [rbp + 72]
    Z
    mov eax, 0x22
    syscall
    OK
    mov [rbp + 64], rdx
    mov rdi, rdx
    Z
    mov eax, 0x23
    syscall
    OK
    mov [rbp + 96], rdx
    ret
dma_arm:
    cmp qword ptr [r15 + 128], 3
    jne dma_arm
    ret
dma_map_rw:
    Z
    mov rsi, [rbp + 96]
    mov edx, 3
    mov eax, 0x24
    syscall
    OK
    cmp rdx, 0x100000
    jne dma_bad
    ret
dma_submit_read:
    call dma_arm
    mov rdi, [rbp + 64]
    cmp r12, 3
    jne dma_submit_anchor
    cmp qword ptr [r15 + 96], 7
    jne dma_submit_anchor
    cmp qword ptr [r15 + 168], 1
    jne dma_submit_anchor
    mov rdi, [r15 + 224]
dma_submit_anchor:
    Z
    mov rsi, [rbp + 96]
    mov edx, 4080
    mov r10d, 32
    mov r8d, 1
    mov eax, 0x28
    syscall
    OK
    ret
dma_wait_ack:
    mov rdi, [rbp + 88]
    Z
    mov eax, 0x20
    syscall
    OK
    mov [rbp + 24], rdx
    mov rdi, [rbp + 88]
    mov rsi, rdx
    xor edx, edx
    xor r10d, r10d
    xor r8d, r8d
    xor r9d, r9d
    mov eax, 0x21
    syscall
    OK
    ret
dma_roundtrip:
    xor ebx, ebx
dma_write_pattern:
    mov rdi, [rbp + 96]
    Z
    lea rsi, [rbx * 8 + 4080]
    movabs rdx, 0x1020304050607080
    add rdx, rbx
    add rdx, r12
    mov eax, 0x27
    syscall
    OK
    inc ebx
    cmp ebx, 4
    jne dma_write_pattern
    call dma_submit_read
    mov rdi, [rbp + 64]
    Z
    mov rsi, [rbp + 96]
    mov eax, 0x25
    syscall
    ERR -52
    call dma_wait_ack
    xor ebx, ebx
dma_clear_pattern:
    mov rdi, [rbp + 96]
    Z
    lea rsi, [rbx * 8 + 4080]
    mov eax, 0x27
    syscall
    OK
    inc ebx
    cmp ebx, 4
    jne dma_clear_pattern
    call dma_arm
    mov rdi, [rbp + 64]
    Z
    mov rsi, [rbp + 96]
    mov edx, 4080
    mov r10d, 32
    mov r8d, 2
    mov eax, 0x28
    syscall
    OK
    call dma_wait_ack
    xor ebx, ebx
dma_check_pattern:
    mov rdi, [rbp + 96]
    Z
    lea rsi, [rbx * 8 + 4080]
    mov eax, 0x26
    syscall
    OK
    movabs rax, 0x1020304050607080
    add rax, rbx
    add rax, r12
    cmp rdx, rax
    jne dma_bad
    mov [rbp + 120], rdx
    inc ebx
    cmp ebx, 4
    jne dma_check_pattern
    ret
dma_bad:
    mov eax, 2
    mov edi, 255
    syscall
    ud2
neu_smp_sched_payload_end:
