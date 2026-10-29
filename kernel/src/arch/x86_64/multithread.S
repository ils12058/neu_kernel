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
    .if {live}
    cmp rax, 8
    jae neu_multi_live_entry
    .endif
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
    .if {channels}
    mov qword ptr [rsp+56],0
    call neu_ipc_user_steps
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
    .if {channels}
    cmp qword ptr [rsp+56],99
    je 3f
    call neu_ipc_user_steps
3:
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

.if {live}
# Live Ring 3 MemoryObject roles. rbx addresses the caller's private 72-byte
# observation block in its own DATA page; the shared object VAs are fixed by
# the trusted mapping setup. The kernel never writes the tested payload.
.global neu_multi_live_entry
neu_multi_live_entry:
    mov rbx, qword ptr [rsp + 24]
    dec rbx
    lea rbx, [rbx + rbx * 8]
    shl rbx, 3
    add rbx, 0x402140
    mov rax, qword ptr [rsp + 48]
    cmp rax, 8
    je neu_multi_live_a
    cmp rax, 9
    je neu_multi_live_b
    cmp rax, 10
    je neu_multi_live_a2
    cmp rax, 12
    je neu_multi_live_b_wait
    cmp rax, 13
    je neu_multi_live_b
    cmp rax, 14
    je neu_multi_cap_a
    cmp rax, 15
    je neu_multi_cap_b
    jmp neu_multi_failed

neu_multi_live_a:
    movabs rax, 0x4e45554d4f4c4956
.global neu_multi_live_a_store_site
neu_multi_live_a_store_site:
    mov qword ptr [0x600000], rax
    mov [rbx], rax
    movabs rax, 0x5a5a0123456789ab
    mov qword ptr [0x600008], rax
    mov [rbx + 8], rax
    mov qword ptr [0x601000], 1
    mov qword ptr [rbx + 0x10], 1
.global neu_multi_live_a_gate,neu_multi_live_a_gate_end
neu_multi_live_a_gate:
    inc qword ptr [rbx + 0x28]
    cmp qword ptr [rbx + 0x20], 1
    jne neu_multi_live_a_gate
neu_multi_live_a_gate_end:
    mov rax, qword ptr [0x600000]
    mov [rbx + 0x30], rax
    movabs rcx, 0x4e45554d4f4c4956
    cmp rax, rcx
    jne neu_multi_failed
    mov rax, qword ptr [0x600008]
    movabs rcx, 0x5a5a0123456789ab
    cmp rax, rcx
    jne neu_multi_failed
    movabs rax, 0x4c4956454f4b0001
    mov [rbx + 0x18], rax
.global neu_multi_live_a_post_gate,neu_multi_live_a_post_gate_end
neu_multi_live_a_post_gate:
    inc qword ptr [rbx + 0x28]
    cmp qword ptr [rbx + 0x40], 1
    jne neu_multi_live_a_post_gate
neu_multi_live_a_post_gate_end:
    xor edi, edi
    jmp neu_multi_live_exit

neu_multi_live_a2:
.global neu_multi_live_a2_loop,neu_multi_live_a2_loop_end
neu_multi_live_a2_loop:
    inc qword ptr [rbx + 0x28]
    cmp qword ptr [0x601000], 1
    jne neu_multi_live_a2_loop
neu_multi_live_a2_loop_end:
    mov rax, qword ptr [0x600000]
    mov [rbx], rax
    movabs rcx, 0x4e45554d4f4c4956
    cmp rax, rcx
    jne neu_multi_failed
    mov rax, qword ptr [0x600008]
    mov [rbx + 8], rax
    movabs rcx, 0x5a5a0123456789ab
    cmp rax, rcx
    jne neu_multi_failed
    movabs rax, 0x4c4956454f4b0001
    mov [rbx + 0x18], rax
    xor edi, edi
    jmp neu_multi_live_exit

neu_multi_live_b:
.global neu_multi_live_b_ready,neu_multi_live_b_ready_end
neu_multi_live_b_ready:
    inc qword ptr [rbx + 0x28]
    cmp qword ptr [0x603000], 1
    jne neu_multi_live_b_ready
neu_multi_live_b_ready_end:
.global neu_multi_live_b_load_site
neu_multi_live_b_load_site:
    mov rax, qword ptr [0x602000]
    mov [rbx], rax
    movabs rcx, 0x4e45554d4f4c4956
    cmp rax, rcx
    jne neu_multi_failed
    mov rax, qword ptr [0x602008]
    mov [rbx + 8], rax
    movabs rcx, 0x5a5a0123456789ab
    cmp rax, rcx
    jne neu_multi_failed
    movabs rax, 0x4c4956454f4b0001
    mov [rbx + 0x18], rax
    cmp qword ptr [rsp + 48], 13
    je neu_multi_live_b_fault
.global neu_multi_live_b_gate,neu_multi_live_b_gate_end
neu_multi_live_b_gate:
    inc qword ptr [rbx + 0x28]
    cmp qword ptr [rbx + 0x20], 1
    jne neu_multi_live_b_gate
neu_multi_live_b_gate_end:
    xor edi, edi
    jmp neu_multi_live_exit

neu_multi_live_b_wait:
.global neu_multi_live_b_wait_gate,neu_multi_live_b_wait_gate_end
neu_multi_live_b_wait_gate:
    inc qword ptr [rbx + 0x28]
    cmp qword ptr [rbx + 0x20], 1
    jne neu_multi_live_b_wait_gate
neu_multi_live_b_wait_gate_end:
.global neu_multi_live_b_after_exit_load_site
neu_multi_live_b_after_exit_load_site:
    mov rax, qword ptr [0x602000]
    mov [rbx], rax
    movabs rcx, 0x4e45554d4f4c4956
    cmp rax, rcx
    jne neu_multi_failed
    mov rax, qword ptr [0x602008]
    mov [rbx + 8], rax
    movabs rcx, 0x5a5a0123456789ab
    cmp rax, rcx
    jne neu_multi_failed
    movabs rax, 0x4c4956454f4b0001
    mov [rbx + 0x18], rax
.global neu_multi_live_b_post_gate,neu_multi_live_b_post_gate_end
neu_multi_live_b_post_gate:
    inc qword ptr [rbx + 0x28]
    cmp qword ptr [rbx + 0x40], 1
    jne neu_multi_live_b_post_gate
neu_multi_live_b_post_gate_end:
    xor edi, edi
    jmp neu_multi_live_exit

neu_multi_live_b_fault:
    mov qword ptr [rbx + 0x38], 1
.global neu_multi_live_b_store_site
neu_multi_live_b_store_site:
    mov qword ptr [0x602000], 0
    ud2

# K8B capability-authority roles. A owns the bootstrap MemoryObject capability,
# proves a SEND-only endpoint cannot move it, then transfers it with the
# SEND_CAPS endpoint. B receives it, transfers it back, and exits without A
# consuming the queued return authority.
neu_multi_cap_a:
    mov rbx, 0x402240
    mov eax, 0x10
    mov edi, 3
    call neu_multi_invoke
    test rax, rax
    jne neu_multi_failed
    mov r12, rdx
    mov [rbx + 0x48], r12
    mov eax, 0x10
    mov edi, 6
    call neu_multi_invoke
    test rax, rax
    jne neu_multi_failed
    mov r13, rdx
    mov eax, 0x10
    mov edi, 4
    call neu_multi_invoke
    test rax, rax
    jne neu_multi_failed
    mov r14, rdx
    movabs rax, 0x4e45554d4f434150
    mov qword ptr [0x600000], rax
    mov [rbx + 0x00], rax
    movabs rax, 0x5a5a0123456789ac
    mov qword ptr [0x600008], rax
    mov [rbx + 0x08], rax
    mov rax, qword ptr [0x600000]
    mov [rbx + 0x30], rax
    movabs rcx, 0x4e45554d4f434150
    cmp rax, rcx
    jne neu_multi_failed
    mov rax, qword ptr [0x600008]
    movabs rcx, 0x5a5a0123456789ac
    cmp rax, rcx
    jne neu_multi_failed
    movabs rax, 0x4c49564543415001
    mov [rbx + 0x18], rax
    mov eax, 0x17
    mov rdi, r13
    mov rsi, r12
    call neu_multi_invoke
    mov [rbx + 0x38], rax
    cmp rax, -18
    jne neu_multi_failed
    mov qword ptr [rbx + 0x10], 1
.global neu_multi_cap_a_fail_gate,neu_multi_cap_a_fail_gate_end
neu_multi_cap_a_fail_gate:
    inc qword ptr [rbx + 0x28]
    cmp qword ptr [rbx + 0x20], 1
    jne neu_multi_cap_a_fail_gate
neu_multi_cap_a_fail_gate_end:
    mov eax, 0x17
    mov rdi, r14
    mov rsi, r12
    call neu_multi_invoke
    mov [rbx + 0x40], rax
    test rax, rax
    jne neu_multi_failed
    mov qword ptr [rbx + 0x10], 2
.global neu_multi_cap_a_send_gate,neu_multi_cap_a_send_gate_end
neu_multi_cap_a_send_gate:
    inc qword ptr [rbx + 0x28]
    cmp qword ptr [rbx + 0x58], 1
    jne neu_multi_cap_a_send_gate
neu_multi_cap_a_send_gate_end:
    xor edi, edi
    jmp neu_multi_live_exit

neu_multi_cap_b:
    mov rbx, 0x4022c0
    mov eax, 0x10
    mov edi, 7
    call neu_multi_invoke
    test rax, rax
    jne neu_multi_failed
    mov r12, rdx
    mov eax, 0x10
    mov edi, 4
    call neu_multi_invoke
    test rax, rax
    jne neu_multi_failed
    mov r13, rdx
    # The bootstrap table is full (eight handles). B does not need its SEND-only
    # endpoint, so close it to make the one slot the receive transaction needs.
    mov eax, 0x10
    mov edi, 6
    call neu_multi_invoke
    test rax, rax
    jne neu_multi_failed
    mov rdi, rdx
    mov eax, 0x11
    call neu_multi_invoke
    test rax, rax
    jne neu_multi_failed
.global neu_multi_cap_b_receive_gate,neu_multi_cap_b_receive_gate_end
neu_multi_cap_b_receive_gate:
    inc qword ptr [rbx + 0x28]
    cmp qword ptr [rbx + 0x20], 1
    jne neu_multi_cap_b_receive_gate
neu_multi_cap_b_receive_gate_end:
    mov eax, 0x18
    mov rdi, r12
    call neu_multi_invoke
    mov [rbx + 0x50], rdx
    test rax, rax
    jne neu_multi_failed
    movabs rax, 0x4c49564543415001
    mov [rbx + 0x18], rax
    mov qword ptr [rbx + 0x10], 1
.global neu_multi_cap_b_send_gate,neu_multi_cap_b_send_gate_end
neu_multi_cap_b_send_gate:
    inc qword ptr [rbx + 0x28]
    cmp qword ptr [rbx + 0x58], 1
    jne neu_multi_cap_b_send_gate
neu_multi_cap_b_send_gate_end:
    mov eax, 0x17
    mov rdi, r13
    mov rsi, qword ptr [rbx + 0x50]
    call neu_multi_invoke
    mov [rbx + 0x40], rax
    test rax, rax
    jne neu_multi_failed
    mov qword ptr [rbx + 0x10], 2
    xor edi, edi
    jmp neu_multi_live_exit

neu_multi_live_exit:
    mov eax, 2
    call neu_multi_invoke
    ud2
.endif

.if {capabilities}
    neu_cap_user_body
.endif
.if {abi02}
    neu_as_user_body
.endif
.if {channels}
    .if {ipc_move}
    neu_move_user_body
    .else
    neu_ipc_user_body
    .endif
.endif
neu_multi_payload_end:
