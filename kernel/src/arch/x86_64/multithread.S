.section .rodata.multi_payload,"a"
.balign 16
.global neu_multi_payload,neu_multi_payload_end
neu_multi_payload:
    mov rdi, [rsp]
    mov rcx, [rsp - 8]
    mov rdx, [rsp - 16]
    mov rax, [rsp - 24]
    sub rsp, 64
    mov [rsp + 24], rdi
    mov [rsp + 32], rcx
    mov [rsp + 40], rdx
    mov [rsp + 48], rax
    movabs rbx, 0x1122334455667788
    xor rbx, rcx
    movabs rbp, 0x2233445566778899
    xor rbp, rcx
    movabs r12, 0x33445566778899aa
    xor r12, rcx
    movabs r13, 0x445566778899aabb
    xor r13, rcx
    movabs r14, 0x5566778899aabbcc
    xor r14, rcx
    movabs r15, 0x66778899aabbccdd
    xor r15, rcx
    movabs r11, 0xaabbccddeeff0011
    xor r11, rcx
    movabs rax, 0x11225566778899aa
    xor rax, rcx
    mov [rsp], rax
    mov [rdx + 24], rax
    movabs rax, 0xaabbccddeeff1234
    xor rax, rcx
    mov [rsp + 8], rax
    movabs rax, 0x1234567887654321
    xor rax, rcx
    mov [rsp + 16], rax
    inc qword ptr [rdx + 16]
    mov edi, 11
    mov esi, 22
    mov r10d, 44
    mov r8d, 55
    mov r9d, 66
    .if {capabilities}
    mov qword ptr [rsp+56],0
    call neu_cap_user_steps
    .endif
    .if {abi02}
    mov qword ptr [rsp+56],0
    call neu_as_user_steps
    .endif
    xor eax, eax
.global neu_multi_loop,neu_multi_atomic_site,neu_multi_after_atomic,neu_multi_after_inc,neu_multi_committed,neu_multi_loop_end
neu_multi_loop:
    mov ecx, 1
neu_multi_atomic_site:
    lock xadd qword ptr [0x402000], rcx
neu_multi_after_atomic:
    inc rax
neu_multi_after_inc:
    mov [rdx], rax
neu_multi_committed:
    .if {capabilities}
    cmp qword ptr [rsp+24],3
    je 8f
    cmp qword ptr [rsp+24],1
    jne 7f
    cmp qword ptr [rsp+56],1
    jne 8f
    cmp qword ptr [0x402100],2
    je 9f
    jmp 8f
7:
    cmp qword ptr [rsp+56],0
    jne 6f
    cmp qword ptr [0x402100],1
    je 9f
6:
    cmp qword ptr [rsp+56],1
    jne 5f
    cmp qword ptr [0x402100],3
    je 9f
5:
    cmp qword ptr [rsp+56],2
    jne 8f
    cmp qword ptr [0x402100],5
    jne 8f
9:
    call neu_cap_user_steps
8:
    .endif
    .if {abi02}
    cmp qword ptr [rsp+56],2
    je 4f
    cmp qword ptr [0x402100],0
    je 4f
    cmp qword ptr [0x402100],3
    ja 4f
    call neu_as_user_steps
4:
    .endif
    cmp rax, [rdx + 8]
    jb neu_multi_loop
neu_multi_loop_end:
    cmp qword ptr [rsp + 48], 1
    je neu_multi_ud_site
    cmp qword ptr [rsp + 48], 2
    je neu_multi_cross_a
    cmp qword ptr [rsp + 48], 3
    je neu_multi_cross_b
    cmp qword ptr [rsp + 48], 4
    je neu_multi_kernel_stack
    cmp qword ptr [rsp + 48], 5
    je neu_multi_user_guard
    cmp qword ptr [rsp + 24], 2
    jne 2f
    cmp qword ptr [0x402020], 1 # A1's normal exit left the Node and shared data alive.
    jne neu_multi_failed
2:
    cmp qword ptr [rsp + 24], 3
    jne 3f
    mov rax, qword ptr [0x406000]
    movabs rcx, 0x5152535461626364
    cmp rax, rcx
    jne neu_multi_failed
3:
    .if {capabilities}
    call neu_cap_user_final
    .endif
    .if {abi02}
    call neu_as_user_final
    .endif
    mov eax, 1
    call neu_multi_invoke
    test rax, rax
    jne neu_multi_failed
    movabs rcx, {abi_info}
    cmp rdx, rcx
    jne neu_multi_failed
    mov rdi, [rsp + 24]
    mov eax, 2
    call neu_multi_invoke
    ud2
neu_multi_invoke:
.global neu_multi_syscall_site,neu_multi_syscall_resume
neu_multi_syscall_site:
    syscall
neu_multi_syscall_resume:
    ret
neu_multi_cross_a:
    mov ecx, 0x406000
.global neu_multi_cross_a_site
neu_multi_cross_a_site:
    mov rax,[rcx]
    jmp neu_multi_failed
neu_multi_cross_b:
    mov ecx, 0x410000
.global neu_multi_cross_b_site
neu_multi_cross_b_site:
    mov rax,[rcx]
    jmp neu_multi_failed
neu_multi_kernel_stack:
    movabs rcx, 0xffffc00000009000
.global neu_multi_kernel_stack_site
neu_multi_kernel_stack_site:
    mov rax,[rcx]
    jmp neu_multi_failed
neu_multi_user_guard:
    mov ecx, 0x40b000
.global neu_multi_user_guard_site
neu_multi_user_guard_site:
    mov rax,[rcx]
    jmp neu_multi_failed
.global neu_multi_ud_site
neu_multi_ud_site:
    ud2
neu_multi_failed:
    ud2
.if {capabilities}
    neu_cap_user_body
.endif
.if {abi02}
    neu_as_user_body
.endif
neu_multi_payload_end:
