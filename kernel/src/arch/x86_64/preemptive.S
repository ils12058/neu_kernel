.section .rodata.preempt_payload,"a"
.balign 16
.global neu_preempt_payload, neu_preempt_payload_end
neu_preempt_payload:
    movabs rcx, 0x401000
    movabs rbx, 0x1122334455667788
    xor rbx, [rcx]
    movabs rbp, 0x2233445566778899
    xor rbp, [rcx]
    movabs r12, 0x33445566778899aa
    xor r12, [rcx]
    movabs r13, 0x445566778899aabb
    xor r13, [rcx]
    movabs r14, 0x5566778899aabbcc
    xor r14, [rcx]
    movabs r15, 0x66778899aabbccdd
    xor r15, [rcx]
    mov edi, 11
    mov esi, 22
    mov r10d, 44
    mov r8d, 55
    mov r9d, 66
    movabs rdx, 0x778899aabbccddee
    movabs r11, 0xaabbccddeeff0011
    sub rsp, 24
    movabs rax, 0x11225566778899aa
    xor rax, [rcx]
    mov [rsp], rax
    mov qword ptr [0x402010], rax
    movabs rax, 0xaabbccddeeff1234
    xor rax, [rcx]
    mov [rsp + 8], rax
    movabs rax, 0x1234567887654321
    xor rax, [rcx]
    mov [rsp + 16], rax
    inc qword ptr [0x402008] # Must remain exactly one, including after preemption.
    xor eax, eax
.global neu_preempt_loop, neu_preempt_loop_end
neu_preempt_loop:
    inc rax
    mov qword ptr [0x402000], rax
    cmp qword ptr [0x401008], 4
    jne 2f
    test eax, 0x7ffff
    jne 2f
    push rax
    push rdx
    push rcx
    push r11
    mov eax, 1
    call neu_preempt_invoke
    test rax, rax
    jne neu_preempt_failed
    movabs rcx, 0x100000003
    cmp rdx, rcx
    jne neu_preempt_failed
    pop r11
    pop rcx
    pop rdx
    pop rax
2:
    cmp rax, qword ptr [0x402018] # Test-only owned work limit, initially u64::MAX.
    jb neu_preempt_loop
neu_preempt_loop_end:
    cmp qword ptr [0x401008], 1
    je neu_preempt_fault_site
    cmp qword ptr [0x401008], 2
    je neu_preempt_cross
    cmp qword ptr [0x401008], 3
    je neu_preempt_fpu_site
    mov eax, 1
    call neu_preempt_invoke
    test rax, rax
    jne neu_preempt_failed
    movabs rcx, 0x100000003
    cmp rdx, rcx
    jne neu_preempt_failed
    xor edi, edi
    mov eax, 2
    call neu_preempt_invoke
    ud2
neu_preempt_invoke:
.global neu_preempt_syscall_site, neu_preempt_syscall_resume
neu_preempt_syscall_site:
    syscall
neu_preempt_syscall_resume:
    ret
neu_preempt_cross:
    mov ecx, 0x406000
.global neu_preempt_cross_site
neu_preempt_cross_site:
    mov rax, [rcx]
    jmp neu_preempt_failed
.global neu_preempt_fpu_site
neu_preempt_fpu_site:
    fninit # TS must force #NM before touching unsaved x87 state.
    jmp neu_preempt_failed
.global neu_preempt_fault_site
neu_preempt_fault_site:
    ud2
neu_preempt_failed:
    ud2
neu_preempt_payload_end:
.section .text.preempt_negative,"ax"
.global neu_preempt_kernel_origin
neu_preempt_kernel_origin:
    sti
    mov ecx, 0x10000000
1:  dec ecx
    jnz 1b
    cli
    ret

.global neu_preempt_image
.set neu_preempt_image, 1

.section .bss.preempt_probe,"aw",@nobits
.balign 8
.global neu_preempt_irq_observed
neu_preempt_irq_observed: .skip 8
