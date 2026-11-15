# Independent Ring 3 consumers of the eager x87/SSE2 and FS_BASE mechanisms.
# FXSAVE destinations live in user DATA; startup snapshots remain untouched.
.section .rodata.cpu_state_fixture,"a"
.balign 16
.macro CPU_ZERO
    xor edi, edi
    xor esi, esi
    xor edx, edx
    xor r10d, r10d
    xor r8d, r8d
    xor r9d, r9d
.endm
.global neu_smp_sched_payload, neu_smp_sched_payload_end
.global neu_smp_sched_query_resume, neu_smp_sched_receive_resume
.global neu_smp_sched_yield_resume, neu_smp_sched_done_resume
.global neu_smp_sched_user_loop_begin, neu_smp_sched_user_loop_end
.global neu_smp_sched_fault_site, neu_smp_sched_user_done
neu_smp_sched_payload:
    mov r12, [rsp]
    mov r13, [rsp - 8]
    mov r10, [rsp - 16]
    mov r11, [rsp - 24]
    mov r14, 0x402000
    mov r15, 0x417000
    lea rbp, [r12 - 1]
    shl rbp, 7
    add rbp, r14
    mov [rbp + 40], r11
    mov [rbp + 48], r10
    lea rdi, [r12 - 1]
    shl rdi, 9
    lea rdi, [rdi + r14 + 0x400]
.global neu_cpu_initial_save
neu_cpu_initial_save:
    fxsave64 [rdi]
    mov qword ptr [rbp + 96], 1
    CPU_ZERO
    mov eax, 1
    syscall
neu_smp_sched_query_resume:
    test rax, rax
    jne neu_cpu_bad
    mov [rbp + 16], rdx
    CPU_ZERO
    mov eax, 0x2a
    syscall
.global neu_cpu_tls_initial_resume
neu_cpu_tls_initial_resume:
    or rax, rdx
    jne neu_cpu_bad
    mov rdi, 0x402200         # same VA for distinct Node A1/B1
    cmp r12, 3
    jne 1f
    add rdi, 0x20            # sibling A2 gets separate storage
1:  mov [rbp + 112], rdi
    mov eax, 0x29
    syscall
    or rax, rdx
    jne neu_cpu_bad
    mov qword ptr fs:[0], r13
    mov qword ptr fs:[8], r12
    CPU_ZERO
    mov eax, 0x2a
    syscall
.global neu_cpu_tls_get_resume
neu_cpu_tls_get_resume:
    test rax, rax
    jne neu_cpu_bad
    cmp rdx, [rbp + 112]
    jne neu_cpu_bad
    cmp qword ptr [rbp + 40], 50
    jne neu_cpu_inspect
    jmp neu_cpu_negative
neu_cpu_inspect:
    xor edi, edi
    cmp r12, 3
    jne 2f
    inc edi
2:  mov eax, 0x10
    syscall
    test rax, rax
    jne neu_cpu_bad
    mov [rbp + 56], rdx
    mov rdi, rdx
    mov esi, 0x400000
    mov eax, 0x12
    syscall
    test rax, rax
    jne neu_cpu_bad
    cmp rdx, 11
    jne neu_cpu_bad
    mov [rbp + 80], rdx
    mov rdi, [rbp + 56]
    mov eax, 0x11
    syscall
    test rax, rax
    jne neu_cpu_bad
.macro CPU_MMX register, index
    mov rax, r12
    shl rax, 48
    mov ecx, \index
    shl rcx, 32
    or rax, rcx
    mov edx, 0x55667788
    or rax, rdx
    movq \register, rax
.endm
    # MMX aliases the eight x87 data registers. Exercise that image through an
    # actual native boundary before EMMS and the independent x87/SSE workload.
    CPU_MMX mm0, 0
    CPU_MMX mm1, 1
    CPU_MMX mm2, 2
    CPU_MMX mm3, 3
    CPU_MMX mm4, 4
    CPU_MMX mm5, 5
    CPU_MMX mm6, 6
    CPU_MMX mm7, 7
    CPU_ZERO
    mov eax, 0x2a
    syscall
.global neu_cpu_mmx_resume
neu_cpu_mmx_resume:
    test rax, rax
    jne neu_cpu_bad
    cmp rdx, [rbp + 112]
    jne neu_cpu_bad
    CPU_ZERO
    mov eax, 0x2a
    syscall
.global neu_cpu_mmx_restored_resume
neu_cpu_mmx_restored_resume:
    test rax, rax
    jne neu_cpu_bad
    cmp rdx, [rbp + 112]
    jne neu_cpu_bad
    emms
    mov eax, 0x37f
    lea rcx, [r12 - 1]
    shl rcx, 10
    add rax, rcx
    mov [rbp + 64], rax
    fldcw word ptr [rbp + 64]
    mov eax, 0x1f80
    shl rcx, 3
    add rax, rcx
    mov [rbp + 72], rax
    ldmxcsr [rbp + 72]
    # Execute actual SSE2 floating-point math before filling the register bank.
    cvtsi2sd xmm0, r12
    movsd xmm1, [rip + neu_cpu_quarter]
    addsd xmm0, xmm1
    movq [rbp + 88], xmm0
.global neu_cpu_seed_begin
neu_cpu_seed_begin:
.macro CPU_ST index
    mov rax, r12
    imul rax, 100
    add rax, \index
    mov [rbp + 104], rax
    fild qword ptr [rbp + 104]
.endm
    CPU_ST 1
    CPU_ST 2
    CPU_ST 3
    CPU_ST 4
    CPU_ST 5
    CPU_ST 6
    CPU_ST 7
    CPU_ST 8
.macro CPU_XMM register, index
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
    CPU_XMM xmm0, 0
    CPU_XMM xmm1, 1
    CPU_XMM xmm2, 2
    CPU_XMM xmm3, 3
    CPU_XMM xmm4, 4
    CPU_XMM xmm5, 5
    CPU_XMM xmm6, 6
    CPU_XMM xmm7, 7
    CPU_XMM xmm8, 8
    CPU_XMM xmm9, 9
    CPU_XMM xmm10, 10
    CPU_XMM xmm11, 11
    CPU_XMM xmm12, 12
    CPU_XMM xmm13, 13
    CPU_XMM xmm14, 14
    CPU_XMM xmm15, 15
.global neu_cpu_seed_done
neu_cpu_seed_done:
    cmp qword ptr [rbp + 40], 3
    jne 3f
    mov rdi, [rbp + 48]
    mov eax, 0x16
    syscall
neu_smp_sched_receive_resume:
    test rax, rax
    jne neu_cpu_bad
    cmp rdx, 0x1101
    jne neu_cpu_bad
    mov [rbp + 24], rdx
3:  xor r10d, r10d
    cmp qword ptr [rbp + 40], 3
    jne 4f
    # Keep a resumed receiver in the real FP/TLS loop long enough for a stopped
    # hardware observation. This deadline paces only the bounded test payload.
    rdtsc
    shl rdx, 32
    or rax, rdx
    lea r10, [rax + 500000000]
4:  movabs r8, 0x1122334455667788
    xor ebx, ebx
neu_smp_sched_user_loop_begin:
    inc rbx
    mov [rbp], rbx
    mov [rbp + 8], r8
.global neu_cpu_x87_add, neu_cpu_x87_added, neu_cpu_x87_restored
neu_cpu_x87_add:
    fadd st(0), st(7)
neu_cpu_x87_added:
    fsub st(0), st(7)
neu_cpu_x87_restored:
    pxor xmm0, xmm15
.global neu_cpu_xmm_toggled, neu_cpu_xmm_restored
neu_cpu_xmm_toggled:
    pxor xmm0, xmm15
neu_cpu_xmm_restored:
    mov rax, qword ptr fs:[0]
    cmp rax, r13
    jne neu_cpu_bad
    cmp qword ptr fs:[8], r12
    jne neu_cpu_bad
    cmp qword ptr [r15], 1
    jne neu_smp_sched_user_loop_begin
    cmp qword ptr [rbp + 40], 3
    jne neu_smp_sched_user_loop_end
    rdtsc
    shl rdx, 32
    or rax, rdx
    cmp rax, r10
    jb neu_smp_sched_user_loop_begin
neu_smp_sched_user_loop_end:
    cmp qword ptr [rbp + 40], 6
    je neu_cpu_sibling
    cmp qword ptr [rbp + 40], 5
    je neu_cpu_survivor
    cmp qword ptr [rbp + 40], 40
    jae neu_cpu_fault_wait
    cmp qword ptr [rbp + 40], 2
    jne neu_cpu_final
    mov rdi, [rbp + 48]
    mov rsi, r13
    mov eax, 0x13
    syscall
    test rax, rax
    jne neu_cpu_bad
    jmp neu_cpu_final
neu_cpu_sibling:
    mov qword ptr [r15 + 16], 1
neu_cpu_sibling_loop:
    inc rbx
    mov [rbp], rbx
    jmp neu_cpu_sibling_loop
neu_cpu_survivor:
    inc rbx
    mov [rbp], rbx
    cmp qword ptr [r15 + 32], 1
    jne neu_cpu_survivor
neu_cpu_final:
    lea rdi, [r12 - 1]
    shl rdi, 9
    lea rdi, [rdi + r14 + 0xa00]
.global neu_cpu_final_save
neu_cpu_final_save:
    fxsave64 [rdi]
    mov rax, qword ptr fs:[0]
    cmp rax, r13
    jne neu_cpu_bad
neu_smp_sched_user_done:
    mov rax, -256
    syscall
neu_smp_sched_yield_resume:
    test rax, rax
    jne neu_cpu_bad
    mov [rbp + 32], r12
    mov eax, 2
    mov edi, 128
    syscall
neu_smp_sched_done_resume:
    ud2
neu_cpu_fault_wait:
    cmp qword ptr [rbp + 40], 50
    je neu_cpu_final
    cmp qword ptr [r15 + 16], 1
    jne neu_cpu_fault_wait
    cmp qword ptr [rbp + 40], 42
    je neu_cpu_avx
    cmp qword ptr [rbp + 40], 43
    je neu_cpu_fsgsbase
    cmp qword ptr [rbp + 40], 41
    je neu_cpu_sse_exception
    fninit
    mov word ptr [rbp + 64], 0x37b
    fldcw word ptr [rbp + 64]
    fld1
    fldz
    fdivp st(1), st(0)
.global neu_cpu_x87_fault
neu_cpu_x87_fault:
    fwait
    jmp neu_cpu_bad
neu_cpu_sse_exception:
    mov dword ptr [rbp + 72], 0x1d80
    ldmxcsr [rbp + 72]
    pxor xmm1, xmm1
    cvtsi2ss xmm0, r12
.global neu_cpu_sse_divide
neu_cpu_sse_divide:
    divss xmm0, xmm1
    # Stock QEMU 11 TCG records unmasked SSE status without delivering #XM.
    # Prove that actual status survives a native round trip, then raise a real
    # #UD for containment. No synthetic #XM frame or software gate is used.
    CPU_ZERO
    mov eax, 0x2a
    syscall
.global neu_cpu_sse_status_resume
neu_cpu_sse_status_resume:
    test rax, rax
    jne neu_cpu_bad
    cmp rdx, [rbp + 112]
    jne neu_cpu_bad
    stmxcsr dword ptr [rbp + 72]
    mov eax, [rbp + 72]
    and eax, 0x204
    cmp eax, 4
    jne neu_cpu_bad
.global neu_cpu_sse_status_fault
neu_cpu_sse_status_fault:
    ud2
neu_cpu_avx:
.global neu_cpu_avx_fault
neu_cpu_avx_fault:
    vxorps xmm0, xmm0, xmm0
    jmp neu_cpu_bad
neu_cpu_fsgsbase:
    mov rax, 0x402240
.global neu_cpu_fsgsbase_fault
neu_cpu_fsgsbase_fault:
    wrfsbase rax
    jmp neu_cpu_bad
neu_smp_sched_fault_site:
    ud2
neu_cpu_bad:
    mov eax, 2
    mov edi, 255
    syscall
    ud2

.macro CPU_BAD_BASE value
    CPU_ZERO
    movabs rdi, \value
    mov eax, 0x29
    syscall
    cmp rax, -2
    jne neu_cpu_bad
    inc qword ptr [rbp + 120]
.endm
.macro CPU_RESERVED operation, register
    CPU_ZERO
    .if \operation == 0x29
    mov rdi, [rbp + 112]
    .endif
    movabs \register, 0x8000000000000000
    mov eax, \operation
    syscall
    cmp rax, -2
    jne neu_cpu_bad
    inc qword ptr [rbp + 120]
.endm
neu_cpu_negative:
    CPU_BAD_BASE 0x800000000000
    CPU_BAD_BASE 0xffff800000000000
    CPU_BAD_BASE 0xffffffffffffffff
    CPU_RESERVED 0x29, rsi
    CPU_RESERVED 0x29, rdx
    CPU_RESERVED 0x29, r10
    CPU_RESERVED 0x29, r8
    CPU_RESERVED 0x29, r9
    CPU_RESERVED 0x2a, rdi
    CPU_RESERVED 0x2a, rsi
    CPU_RESERVED 0x2a, r10
    CPU_RESERVED 0x2a, r8
    CPU_RESERVED 0x2a, r9
    CPU_RESERVED 0x2a, rdx
    CPU_ZERO
    mov eax, 0x2a
    syscall
    test rax, rax
    jne neu_cpu_bad
    cmp rdx, [rbp + 112]
    jne neu_cpu_bad
    CPU_ZERO
    mov eax, 0x29
    syscall
    or rax, rdx
    jne neu_cpu_bad
    CPU_ZERO
    mov eax, 0x2a
    syscall
    or rax, rdx
    jne neu_cpu_bad
    CPU_ZERO
    movabs rdi, 0x7fffffffffff
    mov eax, 0x29
    syscall
    or rax, rdx
    jne neu_cpu_bad
    CPU_ZERO
    mov eax, 0x2a
    syscall
    test rax, rax
    jne neu_cpu_bad
    movabs rax, 0x7fffffffffff
    cmp rdx, rax
    jne neu_cpu_bad
    CPU_ZERO
    mov rdi, [rbp + 112]
    mov eax, 0x29
    syscall
    or rax, rdx
    jne neu_cpu_bad
    jmp neu_cpu_inspect
.balign 8
neu_cpu_quarter: .quad 0x3fd0000000000000
neu_smp_sched_payload_end:
