# A slice owns one common Rust continuation; yielded user frames live by value.
.section .text.context_bridge,"ax"
.global neu_enter_context, neu_resume_context, neu_context_iret
neu_enter_context:
    cli
    cld
    push rbp
    push rbx
    push r12
    push r13
    push r14
    push r15
    mov [rip + neu_user_saved_rsp], rsp
    mov [rip + neu_user_saved_root], rsi
    mov r12, rdi # immutable copied Frame on the common image stack
    mov r13, rdx # validated private stack top
    mov rax, cr0
    mov [rip + neu_user_saved_cr0], rax
    or rax, 10
    mov cr0, rax
    mov ecx, 0xc0000080
    rdmsr
    shl rdx, 32
    or rax, rdx
    mov [rip + neu_user_saved_efer], rax
    # Native session already owns SCE. Zero FS/GS, no TLS or SWAPGS.
    xor eax, eax
    mov fs, ax
    mov gs, ax
    xor edx, edx
    mov ecx, 0xc0000100
    wrmsr
    inc ecx
    wrmsr
    inc ecx
    wrmsr
    lea rdi, [r13 - 160]
    mov rsi, r12
    mov ecx, 20
    rep movsq # supervisor source and destination; never a user pointer
    lea rsp, [r13 - 160]
.if {preemptive}
    # Test observation occurs before arming, with the immutable user frame intact.
    mov ecx, 0x04000000
.global neu_preempt_restore_wait, neu_preempt_restore_wait_end
neu_preempt_restore_wait:
    dec ecx
    jne neu_preempt_restore_wait
neu_preempt_restore_wait_end:
    call neu_preempt_arm
.endif
    mov ax, 0x33
    mov ds, ax
    mov es, ax
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
neu_context_iret:
    iretq
neu_resume_context:
    cli
    cld
    # This normal image stack is mapped under BOTH old and common roots.
    # Move RSP before the CR3 change in the original terminal bridge.
    mov rsp, [rip + neu_user_saved_rsp]
.if {preemptive}
    # A long-mode Ring 3 IRQ installs null SS. Re-establish the normal kernel
    # segment contract before its common Rust continuation/drain window.
    mov ax, 0x10
    mov ss, ax
    mov ds, ax
    mov es, ax
.endif
    # The private entry call chain is now abandoned; only here release its guard.
    mov qword ptr [rip + neu_native_busy], 0
    jmp neu_resume_kernel

.section .rodata.switch_payload,"a"
.balign 16
.global neu_switch_payload, neu_switch_payload_end
# Payload appended below.
neu_switch_payload:
    movabs rcx, 0x401000
    movabs rbx, {canary_bx}
    xor rbx, [rcx]
    movabs rbp, {canary_bp}
    xor rbp, [rcx]
    movabs r12, {canary_12}
    xor r12, [rcx]
    movabs r13, {canary_13}
    xor r13, [rcx]
    movabs r14, {canary_14}
    xor r14, [rcx]
    movabs r15, {canary_15}
    xor r15, [rcx]
    mov edi, 11
    mov esi, 22
    mov edx, 33
    mov r10d, 44
    mov r8d, 55
    mov r9d, 66
    sub rsp, 24
    movabs rcx, {marker_0}
    xor rcx, qword ptr [0x401000]
    mov [rsp + 0], rcx
    movabs rcx, {marker_1}
    xor rcx, qword ptr [0x401000]
    mov [rsp + 8], rcx
    movabs rcx, {marker_2}
    xor rcx, qword ptr [0x401000]
    mov [rsp + 16], rcx
    mov rax, qword ptr [0x401000]
    add rax, 7
    mov qword ptr [0x402000], rax
    mov qword ptr [0x402008], 1
    mov rcx, [rsp]
    mov qword ptr [0x402010], rcx
    mov ecx, 0x04000000
.global neu_switch_phase0_wait, neu_switch_phase0_wait_end
neu_switch_phase0_wait:
    dec ecx
    jne neu_switch_phase0_wait
neu_switch_phase0_wait_end:
    mov eax, {query}
    call neu_switch_invoke
    test rax, rax
    jne neu_switch_failed
    movabs rcx, {info}
    cmp rdx, rcx
    jne neu_switch_failed
    call neu_switch_check
    movabs rax, {yield_op}
    call neu_switch_invoke
.global neu_switch_after_yield_0
neu_switch_after_yield_0:
    test rax, rax
    jne neu_switch_failed
    test rdx, rdx
    jne neu_switch_failed
    call neu_switch_check
    mov rax, qword ptr [0x401000]
    add rax, 14
    mov qword ptr [0x402000], rax
    mov qword ptr [0x402008], 2
    mov rcx, [rsp]
    mov qword ptr [0x402010], rcx
    mov ecx, 0x04000000
.global neu_switch_phase1_wait, neu_switch_phase1_wait_end
neu_switch_phase1_wait:
    dec ecx
    jne neu_switch_phase1_wait
neu_switch_phase1_wait_end:
    movabs rcx, 0x401000
    cmp qword ptr [rcx + 8], 1
    je neu_switch_ud_site
    cmp qword ptr [rcx + 8], 2
    je neu_switch_cross
    cmp qword ptr [rcx + 8], 3
    je neu_switch_stack_user
    cmp qword ptr [rcx + 8], 4
    je neu_switch_stack_guard
    movabs rax, {yield_op}
    call neu_switch_invoke
.global neu_switch_after_yield_1
neu_switch_after_yield_1:
    test rax, rax
    jne neu_switch_failed
    test rdx, rdx
    jne neu_switch_failed
    call neu_switch_check
    mov rax, qword ptr [0x401000]
    add rax, 21
    mov qword ptr [0x402000], rax
    mov qword ptr [0x402008], 3
    mov rcx, [rsp]
    mov qword ptr [0x402010], rcx
    mov ecx, 0x04000000
.global neu_switch_phase2_wait, neu_switch_phase2_wait_end
neu_switch_phase2_wait:
    dec ecx
    jne neu_switch_phase2_wait
neu_switch_phase2_wait_end:
    movabs rax, {yield_op}
    call neu_switch_invoke
.global neu_switch_after_yield_2
neu_switch_after_yield_2:
    test rax, rax
    jne neu_switch_failed
    test rdx, rdx
    jne neu_switch_failed
    call neu_switch_check
    mov rax, qword ptr [0x401000]
    add rax, 28
    mov qword ptr [0x402000], rax
    mov qword ptr [0x402008], 4
    mov rcx, [rsp]
    mov qword ptr [0x402010], rcx
    mov ecx, 0x04000000
.global neu_switch_phase3_wait, neu_switch_phase3_wait_end
neu_switch_phase3_wait:
    dec ecx
    jne neu_switch_phase3_wait
neu_switch_phase3_wait_end:
    cmp qword ptr [0x401000], 77
    jne 8f
    mov rax, qword ptr [0x406000]
    movabs rcx, {private_value}
    cmp rax, rcx
    jne neu_switch_failed
8:
    mov eax, {query}
    call neu_switch_invoke
    test rax, rax
    jne neu_switch_failed
    movabs rcx, {info}
    cmp rdx, rcx
    jne neu_switch_failed
    call neu_switch_check
    xor edi, edi
    mov eax, {exit}
    call neu_switch_invoke
    ud2
neu_switch_invoke:
.global neu_switch_syscall_site, neu_switch_resume
neu_switch_syscall_site:
    syscall
neu_switch_resume:
    ret
neu_switch_check:
    movabs rcx, {canary_bx}
    xor rcx, qword ptr [0x401000]
    cmp rbx, rcx
    jne neu_switch_failed
    movabs rcx, {canary_bp}
    xor rcx, qword ptr [0x401000]
    cmp rbp, rcx
    jne neu_switch_failed
    movabs rcx, {canary_12}
    xor rcx, qword ptr [0x401000]
    cmp r12, rcx
    jne neu_switch_failed
    movabs rcx, {canary_13}
    xor rcx, qword ptr [0x401000]
    cmp r13, rcx
    jne neu_switch_failed
    movabs rcx, {canary_14}
    xor rcx, qword ptr [0x401000]
    cmp r14, rcx
    jne neu_switch_failed
    movabs rcx, {canary_15}
    xor rcx, qword ptr [0x401000]
    cmp r15, rcx
    jne neu_switch_failed
    cmp rdi, 11
    jne neu_switch_failed
    cmp rsi, 22
    jne neu_switch_failed
    cmp r10, 44
    jne neu_switch_failed
    cmp r8, 55
    jne neu_switch_failed
    cmp r9, 66
    jne neu_switch_failed
    movabs rcx, {marker_0}
    xor rcx, qword ptr [0x401000]
    cmp [rsp + 8], rcx
    jne neu_switch_failed
    movabs rcx, {marker_1}
    xor rcx, qword ptr [0x401000]
    cmp [rsp + 16], rcx
    jne neu_switch_failed
    movabs rcx, {marker_2}
    xor rcx, qword ptr [0x401000]
    cmp [rsp + 24], rcx
    jne neu_switch_failed
    movabs rcx, 0x404fd8
    cmp rsp, rcx
    jne neu_switch_failed
    ret
neu_switch_cross:
    movabs rcx, 0x406000
.global neu_switch_cross_site
neu_switch_cross_site:
    mov rax, [rcx]
    jmp neu_switch_failed
neu_switch_stack_user:
    movabs rcx, {stack_bottom}
.global neu_switch_stack_user_site
neu_switch_stack_user_site:
    mov rax, [rcx]
    jmp neu_switch_failed
neu_switch_stack_guard:
    movabs rcx, {stack_guard}
.global neu_switch_stack_guard_site
neu_switch_stack_guard_site:
    mov rax, [rcx]
    jmp neu_switch_failed
.global neu_switch_ud_site
neu_switch_ud_site:
    ud2
neu_switch_failed:
    ud2
neu_switch_payload_end:
.global neu_switch_yield_op
.set neu_switch_yield_op, {yield_op}
.global neu_switch_canary_bx
.set neu_switch_canary_bx, {canary_bx}
.global neu_switch_canary_bp
.set neu_switch_canary_bp, {canary_bp}
.global neu_switch_canary_12
.set neu_switch_canary_12, {canary_12}
.global neu_switch_canary_13
.set neu_switch_canary_13, {canary_13}
.global neu_switch_canary_14
.set neu_switch_canary_14, {canary_14}
.global neu_switch_canary_15
.set neu_switch_canary_15, {canary_15}
.global neu_switch_marker_0
.set neu_switch_marker_0, {marker_0}
.global neu_switch_marker_1
.set neu_switch_marker_1, {marker_1}
.global neu_switch_marker_2
.set neu_switch_marker_2, {marker_2}
