# Dedicated ABI 0.5 user image. Only test pacing words are shared with the
# controller; every payload and capability crosses the actual Channel FIFO.
.macro neu_move_user_body
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
    mov r12,[0x402108]
    lea r13,[r14*8+0x402180]
    cmp [r13],r12
    je .Lmove_done
    mov [r13],r12
    cmp qword ptr [r15+56],0
    jne .Lmove_dispatch
    ipc_call 1,0,0,0,0x5000000ff
    xor r12d,r12d
.Lmove_discover:
    mov eax,0x10
    mov rdi,r12
    call neu_ipc_invoke
    test rax,rax
    jne neu_multi_failed
    mov [r12*8+0x402110],rdx
    inc r12
    cmp r12,8
    jne .Lmove_discover
    mov qword ptr [r15+56],1
.Lmove_dispatch:
    # Three computation slices give QMP independent before-transfer user-state
    # observations for every member, including a sender that later exits early.
    cmp qword ptr [r14*8+0x4021a0],3
    jae .Lmove_start
    inc qword ptr [r14*8+0x4021a0]
    jmp .Lmove_done
.Lmove_start:
    cmp r14,3
    je .Lmove_b
    cmp r14,2
    je .Lmove_a2
    cmp qword ptr [r15+56],1
    jne .Lmove_a_retry
    ipc_call 0x17,"qword ptr [0x402130]",0,-16,0
    ipc_call 0x17,"qword ptr [0x402138]","qword ptr [0x402128]",-18,0
    ipc_call 0x17,"qword ptr [0x402130]","qword ptr [0x402110]",-19,0
    ipc_call 0x13,"qword ptr [0x402130]",11,0,0
    ipc_call 0x13,"qword ptr [0x402130]",22,0,0
    ipc_call 0x17,"qword ptr [0x402130]","qword ptr [0x402128]",-33,0
    ipc_call 0xfffffffffffffc00,"qword ptr [0x402128]",0,0,1
    mov qword ptr [r15+56],2
    jmp .Lmove_done
.Lmove_a_retry:
    cmp qword ptr [r15+56],2
    jne .Lmove_a_scalar
    ipc_receive "qword ptr [0x402130]",100
    ipc_call 0x17,"qword ptr [0x402130]","qword ptr [0x402128]",0,0
    ipc_call 0xfffffffffffffc00,"qword ptr [0x402128]",0,-17,0
    ipc_call 0x11,"qword ptr [0x402128]",0,-17,0
    ipc_call 0x10,3,0,-17,0
    mov qword ptr [r15+56],3
    .if {move_discard}
    mov qword ptr [0x402100],3
    .endif
.Lmove_a_scalar:
    cmp qword ptr [r15+56],3
    jne .Lmove_a_ack
    mov eax,0x13
    mov rdi,[0x402130]
    mov esi,33
    call neu_ipc_invoke
    cmp rax,-33
    je .Lmove_done
    test rax,rax
    jne neu_multi_failed
    mov qword ptr [r15+56],4
.Lmove_a_ack:
    .if {move_discard}
    mov eax,0x14
    mov rdi,[0x402130]
    call neu_ipc_invoke
    cmp rax,-32
    je .Lmove_done
    cmp rax,-34
    jne neu_multi_failed
    .else
    ipc_receive "qword ptr [0x402130]",200
    .endif
    mov qword ptr [0x402100],3
    mov qword ptr [r15+56],99
    jmp .Lmove_done
.Lmove_a2:
    cmp qword ptr [0x402020],1
    jne .Lmove_done
    ipc_call 0xfffffffffffffc00,"qword ptr [0x402128]",0,-17,0
    mov qword ptr [r15+56],99
    jmp .Lmove_done
.Lmove_b:
    cmp qword ptr [r15+56],1
    jne .Lmove_b_cap
    test qword ptr [0x402150],1
    jz .Lmove_done
    ipc_call 0x18,"qword ptr [0x402130]",0,-36,0
    ipc_call 0x14,"qword ptr [0x402130]",0,0,11
    ipc_call 0x13,"qword ptr [0x402130]",100,0,0
    mov qword ptr [r15+56],2
    jmp .Lmove_done
.Lmove_b_cap:
    cmp qword ptr [r15+56],2
    jne .Lmove_b_scalar
    test qword ptr [0x402150],0x4000000
    jz .Lmove_done
    ipc_call 0x14,"qword ptr [0x402130]",0,0,22
    ipc_call 0x14,"qword ptr [0x402130]",0,-36,0
    ipc_call 0x16,"qword ptr [0x402130]",0,-36,0
    ipc_call 0x18,"qword ptr [0x402130]",0,-22,0
    .if {move_discard}
    mov qword ptr [r15+56],99
    jmp .Lmove_done
    .else
    ipc_call 0x11,"qword ptr [0x402120]",0,0,0
    mov eax,0x18
    mov rdi,[0x402130]
    call neu_ipc_invoke
    test rax,rax
    jne neu_multi_failed
    test rdx,rdx
    je neu_multi_failed
    cmp rdx,[0x402128]
    je neu_multi_failed
    mov [0x402158],rdx
    ipc_call 0xfffffffffffffc00,"qword ptr [0x402158]",0,0,1
    ipc_call 0x11,"qword ptr [0x402158]",0,0,0
    ipc_call 0xfffffffffffffc00,"qword ptr [0x402128]",0,0,1
    mov qword ptr [r15+56],3
    .endif
.Lmove_b_scalar:
    ipc_receive "qword ptr [0x402130]",33
    ipc_call 0x13,"qword ptr [0x402130]",200,0,0
    ipc_call 0x11,"qword ptr [0x402130]",0,0,0
    mov qword ptr [r15+56],99
.Lipc_done:
.Lmove_done:
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
neu_ipc_invoke:
.global neu_ipc_syscall_site,neu_ipc_syscall_resume
neu_ipc_syscall_site:
    syscall
neu_ipc_syscall_resume:
    ret
.endm
