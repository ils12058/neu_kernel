.macro ipc_call op,handle,value,status,scalar
    mov rax,\op
    mov rdi,\handle
    mov rsi,\value
    xor edx,edx
    mov r9,0x100000001 # Cannot select authority; the trusted binding determines it.
    call neu_ipc_invoke
    cmp rax,\status
    jne neu_multi_failed
    mov rcx,\scalar
    cmp rdx,rcx
    jne neu_multi_failed
.endm
.macro ipc_receive handle,scalar
    mov eax,0x14
    mov rdi,\handle
    call neu_ipc_invoke
    cmp rax,-32
    je .Lipc_done
    test rax,rax
    jne neu_multi_failed
    mov rcx,\scalar
    cmp rdx,rcx
    jne neu_multi_failed
.endm
.macro ipc_stress_send status
    mov eax,0x13
    mov rdi,[0x402110]
    mov rsi,rbx
    call neu_ipc_invoke
    cmp rax,\status
    jne neu_multi_failed
    test rdx,rdx
    jne neu_multi_failed
.endm
.macro ipc_wait_call op,handle,value,status,scalar
    mov rax,\op
    mov rdi,\handle
    mov rsi,\value
    xor edx,edx
    mov r9,0x100000001
    call neu_ipc_invoke
    cmp rax,\status
    jne neu_multi_failed
    mov rcx,\scalar
    cmp rdx,rcx
    jne neu_multi_failed
.endm
.macro ipc_wait_error op,handle,value,status
    mov rax,\op
    mov rdi,\handle
    mov rsi,\value
    xor edx,edx
    call neu_ipc_invoke
    cmp rax,\status
    jne neu_multi_failed
    test rdx,rdx
    jne neu_multi_failed
.endm
.macro ipc_wait_delay
    rdtsc
    shl rdx,32
    or rax,rdx
    mov r8,rax
.Lipc_wait_delay_tick:
    rdtsc
    shl rdx,32
    or rax,rdx
    sub rax,r8
    cmp rax,500000000
    jb .Lipc_wait_delay_tick
.endm
.macro ipc_exit status
    mov edi,\status
    mov eax,2
    call neu_ipc_invoke
    ud2
.endm
.macro ipc_close handle
    mov eax,0x11
    mov rdi,\handle
    call neu_ipc_invoke
    test rax,rax
    jne neu_multi_failed
    test rdx,rdx
    jne neu_multi_failed
.endm
.macro neu_ipc_user_body
neu_ipc_user_steps:
    push rax
    push rbx
    push rcx
    push rdx
    push rsi
    push rdi
    push rbp
    push r8
    push r9
    push r10
    push r11
    push r12
    push r13
    push r14
    push r15
    lea r15,[rsp+128]
    mov r14,[r15+24]
    mov r12,[0x402108] # Common-boundary test scheduling pulse, no message data.
    lea r13,[r14*8+0x402180]
    cmp [r13],r12
    je .Lipc_done
    mov [r13],r12
    cmp qword ptr [r15+56],0
    .if {ipc_wait}
    jne .Lipc_ready
    .else
    jne .Lipc_dispatch
    .endif
    mov eax,1
    call neu_ipc_invoke
    test rax,rax
    jne neu_multi_failed
    .if {ipc_wait}
    mov rcx,0x40000007f
    .else
    mov rcx,0x30000003f
    .endif
    cmp rdx,rcx
    jne neu_multi_failed
    mov r12d,4
    mov r13d,0x402110
.Lipc_discover:
    mov eax,0x10
    mov rdi,r12
    call neu_ipc_invoke
    test rax,rax
    jne neu_multi_failed
    test rdx,rdx
    je neu_multi_failed
    mov [r13],rdx
    add r13,8
    inc r12
    cmp r12,8
    jne .Lipc_discover
    mov qword ptr [r15+56],1
    .if {ipc_wait}
    jmp .Lipc_wait
    .endif
    .if {ipc_stress}
    cmp r14,2
    je .Lipc_done
    mov qword ptr [r15+56],10
    mov qword ptr [0x402168],0
    jmp .Lipc_stress
    .endif
    .if {ipc_wait}
.Lipc_wait:
    lea rbx,[r14*8+0x402200]
    mov rax,[rbx]
    .if {wait_mode} == 0
    # A: B1 blocks in receive, then A1's blocking send commits 0x1111.
    cmp r14,3
    je .Lipc_wait_a_b
    cmp r14,1
    je .Lipc_wait_a_a1
    ipc_exit 0x22
.Lipc_wait_a_a1:
    test rax,rax
    jnz .Lipc_wait_a_send
    mov qword ptr [rbx],1
    jmp .Lipc_done
.Lipc_wait_a_send:
    ipc_wait_call 0x15,"qword ptr [0x402110]",0x1111,0,0
    mov qword ptr [rbx],2
    ipc_exit 0x21
.Lipc_wait_a_b:
    test rax,rax
    jnz .Lipc_wait_a_done
    mov qword ptr [rbx],1
    ipc_wait_call 0x16,"qword ptr [0x402110]",0,0,0x1111
    mov qword ptr [rbx],2
    ipc_exit 0x31
.Lipc_wait_a_done:
    ipc_exit 0x32
    .endif
    .if {wait_mode} == 1
    # B: A1 fills B's inbound FIFO, blocks on the third send, then commits
    # exactly once after B1 frees one slot.
    cmp r14,3
    je .Lipc_wait_b_b1
    cmp r14,1
    je .Lipc_wait_b_a1
    cmp r14,2
    je .Lipc_wait_b_a2
    ipc_exit 0x22
.Lipc_wait_b_a1:
    test rax,rax
    jnz .Lipc_wait_b_done
    # Wait until A2 has started the blocked receive. A1/A2 share Node A data;
    # B1 registers its receive wait before A2 lets A1 fill the peer queue.
    cmp qword ptr [0x402160],1
    jne .Lipc_wait_b_a1
    ipc_wait_call 0x15,"qword ptr [0x402110]",0xb1,0,0
    ipc_wait_call 0x15,"qword ptr [0x402110]",0xb2,0,0
    mov qword ptr [rbx],1
    ipc_wait_call 0x15,"qword ptr [0x402110]",0xb3,0,0
    mov qword ptr [rbx],2
    ipc_exit 0x23
.Lipc_wait_b_done:
    ipc_exit 0x24
.Lipc_wait_b_b1:
    # Register an empty-queue receive first, then drain the three scalars. A1
    # cannot observe capacity until this Thread actually suspends and consumes.
    ipc_wait_call 0x16,"qword ptr [0x402110]",0,0,0xb1
    ipc_wait_call 0x16,"qword ptr [0x402110]",0,0,0xb2
    ipc_wait_call 0x16,"qword ptr [0x402110]",0,0,0xb3
    mov qword ptr [rbx],2
    ipc_exit 0x34
.Lipc_wait_b_a2:
    mov qword ptr [0x402160],1
    ipc_exit 0x22
    .endif
    .if {wait_mode} == 2
    # C: A1 and A2 both block on Node A's inbound queue. B1 sends after both
    # registrations exist; wake-one FIFO delivers one scalar to each.
    cmp r14,3
    je .Lipc_wait_c_b1
    cmp r14,1
    je .Lipc_wait_c_a1
    # A2
    test rax,rax
    jnz .Lipc_wait_c_a2_done
    mov qword ptr [rbx],1
    ipc_wait_call 0x16,"qword ptr [0x402110]",0,0,0xc2
    mov qword ptr [rbx],2
    ipc_exit 0x42
.Lipc_wait_c_a2_done:
    ipc_exit 0x43
.Lipc_wait_c_a1:
    test rax,rax
    jnz .Lipc_wait_c_a1_done
    mov qword ptr [rbx],1
    ipc_wait_call 0x16,"qword ptr [0x402110]",0,0,0xc1
    mov qword ptr [rbx],2
    ipc_exit 0x41
.Lipc_wait_c_a1_done:
    ipc_exit 0x44
.Lipc_wait_c_b1:
    test rax,rax
    jnz .Lipc_wait_c_send
    mov qword ptr [rbx],1
    # Span at least one 10 ms quantum so A2 (next in FIFO) registers its receive
    # wait before B1 observes the queue or commits.
    ipc_wait_delay
    jmp .Lipc_wait_c_send
.Lipc_wait_c_send:
    ipc_wait_call 0x15,"qword ptr [0x402110]",0xc1,0,0
    ipc_wait_call 0x15,"qword ptr [0x402110]",0xc2,0,0
    mov qword ptr [rbx],2
    ipc_exit 0x45
    .endif
    .if {wait_mode} == 3
    # D: B1 waits in receive while Node A closes its last endpoint reference.
    cmp r14,3
    je .Lipc_wait_d_b1
    cmp r14,1
    je .Lipc_wait_d_a1
    ipc_exit 0x22
.Lipc_wait_d_a1:
    test rax,rax
    jnz .Lipc_wait_d_close
    mov qword ptr [rbx],1
    jmp .Lipc_done
.Lipc_wait_d_close:
    ipc_close "qword ptr [0x402110]"
    ipc_close "qword ptr [0x402118]"
    ipc_close "qword ptr [0x402120]"
    ipc_close "qword ptr [0x402128]"
    mov qword ptr [rbx],2
    ipc_exit 0x25
.Lipc_wait_d_b1:
    test rax,rax
    jnz .Lipc_wait_d_done
    mov qword ptr [rbx],1
    ipc_wait_error 0x16,"qword ptr [0x402110]",0,-34
    mov qword ptr [rbx],2
    ipc_exit 0x35
.Lipc_wait_d_done:
    ipc_exit 0x36
    .endif
    .if {wait_mode} == 4
    # E: A1 blocks on a full peer queue; B1 closes its endpoint. The pending
    # scalar must not be delivered and A1 returns PeerClosed.
    cmp r14,3
    je .Lipc_wait_e_b1
    cmp r14,1
    je .Lipc_wait_e_a1
    ipc_exit 0x22
.Lipc_wait_e_a1:
    test rax,rax
    jnz .Lipc_wait_e_done
    ipc_wait_call 0x15,"qword ptr [0x402110]",0xe1,0,0
    ipc_wait_call 0x15,"qword ptr [0x402110]",0xe2,0,0
    mov qword ptr [rbx],1
    ipc_wait_error 0x15,"qword ptr [0x402110]",0xe3,-34
    mov qword ptr [rbx],2
    ipc_exit 0x26
.Lipc_wait_e_done:
    ipc_exit 0x27
.Lipc_wait_e_b1:
    ipc_close "qword ptr [0x402110]"
    ipc_close "qword ptr [0x402118]"
    ipc_close "qword ptr [0x402120]"
    ipc_close "qword ptr [0x402128]"
    ipc_exit 0x37
    .endif
    .if {wait_mode} == 5
    # F: A1 blocks in receive, A2 faults. Node A cancels A1; B1 observes peer
    # closure and returns PeerClosed. A2 waits for the registration flag so the
    # fault cannot outrun the wait it is meant to cancel.
    cmp r14,3
    je .Lipc_wait_f_b1
    cmp r14,2
    je .Lipc_wait_f_fault
    mov qword ptr [rbx],1
    mov qword ptr [0x402160],1
    ipc_wait_call 0x16,"qword ptr [0x402110]",0,0,0
    ipc_exit 0x28
.Lipc_wait_f_fault:
    cmp qword ptr [0x402160],1
    jne .Lipc_wait_f_fault
    jmp neu_multi_ud_site
.Lipc_wait_f_b1:
    mov qword ptr [rbx],1
    ipc_wait_error 0x16,"qword ptr [0x402110]",0,-34
    ipc_exit 0x38
    .endif
    .if {wait_mode} == 6
    # G: all three Threads block with no possible producer. The controller must
    # classify bounded quiescence rather than hang or fabricate a wakeup.
    mov qword ptr [rbx],1
    ipc_wait_call 0x16,"qword ptr [0x402110]",0,0,0
    ipc_exit 0x39
    .endif
    jmp .Lipc_done
    .endif
.Lipc_ready:
    .if {ipc_wait}
    jmp .Lipc_wait
    .endif
.Lipc_initial_a:
    cmp r14,1
    jne .Lipc_dispatch
    ipc_call 0x10,8,0,-16,0
    mov eax,0x10
    xor edi,edi
    call neu_ipc_invoke
    test rax,rax
    jne neu_multi_failed
    mov [0x402140],rdx
    ipc_call 0x13,"qword ptr [0x402140]",1,-19,0
    ipc_call 0x13,"qword ptr [0x402128]",1,-18,0
    ipc_call 0x14,"qword ptr [0x402120]",0,-18,0
    ipc_call 0x13,0,1,-16,0
    ipc_call 0x13,0x100000009,1,-16,0
    ipc_call 0x13,0x102,1,-16,0
    ipc_call 0x13,0x200000005,1,-17,0
    ipc_call 0x14,"qword ptr [0x402128]",0,-32,0
    .if {ipc_legacy}
    # ABI 0.3 images must reject the blocking extension without side effects.
    ipc_call 0x15,"qword ptr [0x402110]",0,-1,0
    ipc_call 0x16,"qword ptr [0x402110]",0,-1,0
    .endif
    ipc_call 0x13,"qword ptr [0x402110]",0,0,0
    ipc_call 0x13,0x100000005,0xfedcba9876543210,0,0 # Guess resolves only locally.
    ipc_call 0x13,"qword ptr [0x402110]",999,-33,0
    mov qword ptr [0x402100],1
    jmp .Lipc_done
.Lipc_dispatch:
    .if {ipc_stress}
    cmp r14,2
    jne .Lipc_stress_check
    cmp qword ptr [0x402160],1
    jne .Lipc_done
.Lipc_stress_check:
    cmp qword ptr [r15+56],10
    jae .Lipc_stress
    .endif
    cmp r14,3
    je .Lipc_b
    cmp r14,2
    je .Lipc_a2
    cmp qword ptr [r15+56],1
    jne .Lipc_done
    cmp qword ptr [0x402100],2
    jne .Lipc_done
    ipc_call 0x13,"qword ptr [0x402120]",44,0,0
    ipc_call 0x11,"qword ptr [0x402110]",0,0,0
    ipc_call 0x11,"qword ptr [0x402110]",0,-17,0
    ipc_call 0x10,4,0,-17,0
    mov qword ptr [0x402100],3
    mov qword ptr [r15+56],99
    jmp .Lipc_done
.Lipc_a2:
    cmp qword ptr [r15+56],1
    jne .Lipc_a2_wait_exit
    test qword ptr [0x402150],2
    jz .Lipc_done
    ipc_receive "qword ptr [0x402128]",0x1020304050607080
    ipc_call 0x14,"qword ptr [0x402118]",0,0,99
    ipc_call 0x13,"qword ptr [0x402120]",33,0,0
    mov qword ptr [0x402100],2
    mov qword ptr [r15+56],2
    jmp .Lipc_done
.Lipc_a2_wait_exit:
    cmp qword ptr [r15+56],2
    jne .Lipc_a2_ack
    cmp qword ptr [0x402020],1
    jne .Lipc_done
    ipc_call 0x14,"qword ptr [0x402110]",0,-17,0
    mov qword ptr [r15+56],3
.Lipc_a2_ack:
    cmp qword ptr [r15+56],3
    jne .Lipc_a2_peer
    ipc_receive "qword ptr [0x402118]",55
    mov qword ptr [r15+56],4
.Lipc_a2_peer:
    mov eax,0x14
    mov rdi,[0x402118]
    call neu_ipc_invoke
    cmp rax,-32
    je .Lipc_done
    cmp rax,-34
    jne neu_multi_failed
    test rdx,rdx
    jne neu_multi_failed
    ipc_call 0x13,"qword ptr [0x402120]",66,-34,0
    ipc_call 0x11,"qword ptr [0x402120]",0,0,0
    ipc_call 0x11,"qword ptr [0x402128]",0,0,0
    ipc_call 0x11,"qword ptr [0x402118]",0,0,0
    mov qword ptr [r15+56],99
    jmp .Lipc_done
.Lipc_b:
    cmp qword ptr [r15+56],1
    jne .Lipc_b_third
    test qword ptr [0x402150],1
    jz .Lipc_done
    ipc_receive "qword ptr [0x402128]",0
    ipc_call 0x14,"qword ptr [0x402110]",0,0,0xfedcba9876543210
    ipc_call 0x13,"qword ptr [0x402120]",0x1020304050607080,0,0
    ipc_call 0x13,"qword ptr [0x402110]",99,0,0
    ipc_call 0x13,"qword ptr [0x402120]",777,-33,0
    mov qword ptr [r15+56],2
    jmp .Lipc_done
.Lipc_b_third:
    cmp qword ptr [r15+56],2
    jne .Lipc_b_fourth
    ipc_receive "qword ptr [0x402128]",33
    mov qword ptr [r15+56],3
.Lipc_b_fourth:
    cmp qword ptr [r15+56],3
    jne .Lipc_b_final
    ipc_receive "qword ptr [0x402118]",44
    .if {ipc_fault_a} || {ipc_fault_b}
    ipc_call 0x13,"qword ptr [0x402120]",55,0,0
    .endif
    .if {ipc_fault_b}
    mov qword ptr [r15+56],99
    jmp .Lipc_done
    .else
    mov qword ptr [r15+56],4
    .endif
.Lipc_b_final:
    test qword ptr [0x402150],4
    jz .Lipc_done
    ipc_call 0x11,"qword ptr [0x402110]",0,0,0
    .if {ipc_fault_a}
    ipc_call 0x14,"qword ptr [0x402118]",0,-34,0
    ipc_call 0x13,"qword ptr [0x402118]",77,-34,0
    .else
    ipc_call 0x13,"qword ptr [0x402118]",55,0,0
    .endif
    ipc_call 0x11,"qword ptr [0x402120]",0,0,0
    ipc_call 0x11,"qword ptr [0x402128]",0,0,0
    ipc_call 0x11,"qword ptr [0x402118]",0,0,0
    mov qword ptr [r15+56],99
.Lipc_done:
    pop r15
    pop r14
    pop r13
    pop r12
    pop r11
    pop r10
    pop r9
    pop r8
    pop rbp
    pop rdi
    pop rsi
    pop rdx
    pop rcx
    pop rbx
    pop rax
    ret
    .if {ipc_stress}
.Lipc_stress:
    cmp r14,3
    je .Lipc_stress_b
    cmp qword ptr [r15+56],10
    jne .Lipc_stress_a_ack
    mov rbx,[0x402168]
    add rbx,0x6000
    ipc_stress_send 0
    add rbx,0x100
    ipc_stress_send 0
    add rbx,0xf00
    ipc_stress_send -33
    mov qword ptr [r15+56],11
    jmp .Lipc_done
.Lipc_stress_a_ack:
    mov rbx,[0x402168]
    add rbx,0x6200
    ipc_receive "qword ptr [0x402118]",rbx
    inc qword ptr [0x402168]
    cmp qword ptr [0x402168],3
    je .Lipc_stress_a_complete
    mov qword ptr [r15+56],10
    jmp .Lipc_done
.Lipc_stress_a_complete:
    mov qword ptr [0x402160],1
    mov qword ptr [r15+56],1
    jmp .Lipc_initial_a
.Lipc_stress_b:
    mov rcx,[0x402168]
    add rcx,4
    bt qword ptr [0x402150],rcx
    jnc .Lipc_done
    cmp qword ptr [r15+56],10
    jne .Lipc_stress_b_second
    mov rbx,[0x402168]
    add rbx,0x6000
    ipc_receive "qword ptr [0x402118]",rbx
    mov qword ptr [r15+56],11
.Lipc_stress_b_second:
    mov rbx,[0x402168]
    add rbx,0x6100
    ipc_receive "qword ptr [0x402118]",rbx
    mov rbx,[0x402168]
    add rbx,0x6200
    ipc_stress_send 0
    inc qword ptr [0x402168]
    cmp qword ptr [0x402168],3
    je .Lipc_stress_b_complete
    mov qword ptr [r15+56],10
    jmp .Lipc_done
.Lipc_stress_b_complete:
    mov qword ptr [r15+56],1
    jmp .Lipc_done
    .endif
neu_ipc_invoke:
.global neu_ipc_syscall_site,neu_ipc_syscall_resume
neu_ipc_syscall_site:
    syscall
neu_ipc_syscall_resume:
    ret
.endm
.if {channels}
.section .text.ipc_probe,"ax"
.global neu_ipc_request_probe,neu_ipc_request_wait,neu_ipc_request_wait_end
neu_ipc_request_probe:
    push rcx
    mov ecx,0x100000
neu_ipc_request_wait:
    pause
    dec ecx
    jne neu_ipc_request_wait
neu_ipc_request_wait_end:
    pop rcx
    ret
.endif
