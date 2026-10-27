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
    .if {move_mode} == 2 || {move_mode} == 3 || {move_mode} == 7
    cmp qword ptr [r15+56],99
    je .Lmove_done
    .endif
    # Three computation slices give QMP independent before-transfer user-state
    # observations for every member, including a sender that later exits early.
    cmp qword ptr [r14*8+0x4021a0],3
    jae .Lmove_start
    inc qword ptr [r14*8+0x4021a0]
    jmp .Lmove_done
.Lmove_start:
    .if {move_mode} == 6
    cmp r14,3
    je .Lterminal_b
    cmp r14,2
    je .Lterminal_a2
    cmp qword ptr [r15+56],1
    jne .Lmove_done
    mov rax,1
    shl rax,37
    test qword ptr [0x402150],rax
    jz .Lmove_done
    ipc_call 0x13,"qword ptr [0x402130]",11,0,0
    ipc_call 0x17,"qword ptr [0x402130]","qword ptr [0x402128]",0,0
    # FIFO is full: retain a wait pin while the peer consumes the scalar.
    mov eax,0x15
    mov rdi,[0x402130]
    mov esi,33
    call neu_ipc_invoke
    cmp rax,-34
    jne neu_multi_failed
    mov edi,0x21
    mov eax,2
    call neu_multi_invoke
    jmp neu_multi_failed
.Lterminal_a2:
    cmp qword ptr [r15+56],1
    jne .Lterminal_a2_finish
    # Leave exactly one A endpoint handle before the wait is registered.
    ipc_call 0x11,"qword ptr [0x402138]",0,0,0
    ipc_call 0x11,"qword ptr [0x402140]",0,0,0
    ipc_call 0x11,"qword ptr [0x402148]",0,0,0
    mov qword ptr [r15+56],2
    jmp .Lmove_done
.Lterminal_a2_finish:
    mov rax,1
    shl rax,35
    test qword ptr [0x402150],rax
    jz .Lmove_done
    # A1 has been admitted already; close the last A endpoint reference.
    ipc_call 0x11,"qword ptr [0x402130]",0,0,0
    mov edi,0x22
    mov eax,2
    call neu_multi_invoke
    jmp neu_multi_failed
.Lterminal_b:
    cmp qword ptr [r15+56],1
    jne .Lterminal_b_receive
    test qword ptr [0x402150],0x2000000
    jz .Lmove_done
    ipc_call 0x14,"qword ptr [0x402130]",0,0,11
    mov qword ptr [r15+56],2
    jmp .Lmove_done
.Lterminal_b_receive:
    mov rax,1
    shl rax,36
    test qword ptr [0x402150],rax
    jz .Lmove_done
    ipc_call 0x11,"qword ptr [0x402120]",0,0,0
    mov eax,0x18
    mov rdi,[0x402130]
    call neu_ipc_invoke
    test rax,rax
    jne neu_multi_failed
    mov [0x402158],rdx
    ipc_call 0xfffffffffffffc00,"qword ptr [0x402158]",0,0,1
    ipc_call 0x11,"qword ptr [0x402158]",0,0,0
    mov edi,0x31
    mov eax,2
    call neu_multi_invoke
    jmp neu_multi_failed
    .endif
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
    .if {move_mode} == 7
    # Sender-before-commit fixture: retain the pending scalar FIFO entries but
    # never issue the later capability-send transaction.
    mov qword ptr [r15+56],99
    .endif
    jmp .Lmove_done
.Lmove_a_retry:
    cmp qword ptr [r15+56],2
    jne .Lmove_a_scalar
    .if {move_mode} == 5
    # The two-entry scalar queue is full, so this continuation registers one
    # blocking scalar send. It resumes only after B dequeues and settlement
    # commits the payload exactly once.
    mov eax,0x15
    mov rdi,[0x402130]
    mov esi,33
    call neu_ipc_invoke
    test rax,rax
    jne neu_multi_failed
    mov qword ptr [r15+56],3
    jmp .Lmove_done
    .endif
    ipc_receive "qword ptr [0x402130]",100
    .if {move_mode} == 4
    ipc_call 0x17,"qword ptr [0x402130]","qword ptr [0x402120]",0,0
    mov qword ptr [r15+56],5
    jmp .Lmove_done
    .else
    ipc_call 0x17,"qword ptr [0x402130]","qword ptr [0x402128]",0,0
    .endif
    .if {move_mode} != 4
    ipc_call 0xfffffffffffffc00,"qword ptr [0x402128]",0,-17,0
    ipc_call 0x11,"qword ptr [0x402128]",0,-17,0
    ipc_call 0x10,3,0,-17,0
    mov qword ptr [r15+56],3
    .if {move_mode} == 2
    # The controller shortens this faulting Thread's run after it observes the
    # committed send. Its configured terminal action then captures the fault.
    mov qword ptr [r15+56],99
    jmp .Lmove_done
    .endif
    .if {move_discard}
    mov qword ptr [0x402100],3
    .endif
    .endif
.Lmove_a_scalar:
    .if {move_mode} == 5
    cmp qword ptr [r15+56],3
    je .Lmove_backpressure_a_move
    cmp qword ptr [r15+56],4
    je .Lmove_backpressure_a_finish
    .endif
    .if {move_mode} == 4
    cmp qword ptr [r15+56],5
    je .Lmove_double_a_second
    .endif
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
    .if {move_mode} == 3
    # Keep both A members resumable while B faults after delivery; the controller
    # later lets A1 finish normally through the existing bounded loop contract.
    mov qword ptr [r15+56],99
    jmp .Lmove_done
    .endif
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
.if {move_mode} == 4
.Lmove_double_a_second:
    # The second grant cannot move until B has consumed and read the first one.
    # This deliberately exercises queue backpressure without blocking ABI calls.
    test qword ptr [0x402150],0x40000000
    jz .Lmove_done
    ipc_call 0x17,"qword ptr [0x402130]","qword ptr [0x402128]",0,0
    ipc_call 0xfffffffffffffc00,"qword ptr [0x402120]",0,-17,0
    ipc_call 0x11,"qword ptr [0x402120]",0,-17,0
    ipc_call 0x10,2,0,-17,0
    ipc_call 0xfffffffffffffc00,"qword ptr [0x402128]",0,-17,0
    ipc_call 0x11,"qword ptr [0x402128]",0,-17,0
    ipc_call 0x10,3,0,-17,0
    mov qword ptr [r15+56],3
    jmp .Lmove_done
.endif
.Lmove_a2:
    .if {move_mode} == 5
    cmp qword ptr [0x402020],1
    jne .Lmove_done
    ipc_call 0x11,"qword ptr [0x402128]",0,-17,0
    mov edi,0x22
    mov eax,2
    call neu_multi_invoke
    jmp neu_multi_failed
    .endif
    cmp qword ptr [0x402020],1
    jne .Lmove_done
    ipc_call 0xfffffffffffffc00,"qword ptr [0x402128]",0,-17,0
    mov qword ptr [r15+56],99
    jmp .Lmove_done
.Lmove_b:
    .if {move_mode} == 5
    jmp .Lmove_backpressure_b
    .endif
    .if {move_mode} == 4
    cmp qword ptr [r15+56],4
    je .Lmove_double_b_second
    .endif
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
    .if {move_mode} == 7
    # Wait until the sender's controlled fault completed full Node cleanup.
    mov eax,0x80000000
    test qword ptr [0x402150],rax
    jz .Lmove_done
    ipc_receive "qword ptr [0x402130]",22
    ipc_call 0x14,"qword ptr [0x402130]",0,-34,0
    mov edi,3
    mov eax,2
    call neu_multi_invoke
    jmp neu_multi_failed
    .endif
    .if {move_mode} == 2
    # In this profile the receiver waits for trusted sender-Node reclamation,
    # not merely the sender's successful-enqueue marker.
    mov eax,0x80000000
    test qword ptr [0x402150],rax
    .else
    test qword ptr [0x402150],0x4000000
    .endif
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
    .if {move_mode} == 3
    # Leave the freshly installed handle live for ordinary Node-fault cleanup.
    mov qword ptr [r15+56],99
    jmp .Lmove_done
    .endif
    .if {move_mode} == 4
    ipc_call 0x11,"qword ptr [0x402118]",0,0,0
    mov qword ptr [r15+56],4
    jmp .Lmove_done
    .else
    ipc_call 0x11,"qword ptr [0x402158]",0,0,0
    .endif
.if {move_mode} == 4
.Lmove_double_b_second:
    test qword ptr [0x402150],0x10000000
    jz .Lmove_done
    mov eax,0x18
    mov rdi,[0x402130]
    call neu_ipc_invoke
    test rax,rax
    jne neu_multi_failed
    test rdx,rdx
    je neu_multi_failed
    mov [0x402160],rdx
    ipc_call 0xfffffffffffffc00,"qword ptr [0x402160]",0,0,1
    mov qword ptr [r15+56],3
    jmp .Lmove_done
.endif
    ipc_call 0xfffffffffffffc00,"qword ptr [0x402128]",0,0,1
    mov qword ptr [r15+56],3
    .if {move_mode} == 2
    # Delivery and actual use happen only after the sender's fault teardown.
    # Exit normally through the existing native syscall rather than polling
    # until an arbitrary loop limit.
    mov edi,3
    mov eax,2
    call neu_multi_invoke
    jmp neu_multi_failed
    .endif
    .endif
.Lmove_b_scalar:
    ipc_receive "qword ptr [0x402130]",33
    ipc_call 0x13,"qword ptr [0x402130]",200,0,0
    ipc_call 0x11,"qword ptr [0x402130]",0,0,0
    mov qword ptr [r15+56],99
    jmp .Lmove_done
.if {move_mode} == 5
.Lmove_backpressure_a_move:
    mov eax,0x17
    mov rdi,[0x402130]
    mov rsi,[0x402128]
    call neu_ipc_invoke
    cmp rax,-33
    je .Lmove_done
    test rax,rax
    jne neu_multi_failed
    mov qword ptr [r15+56],4
    jmp .Lmove_done
.Lmove_backpressure_a_finish:
    mov eax,1
    shl rax,30
    test qword ptr [0x402150],rax
    jz .Lmove_done
    mov edi,0x21
    mov eax,2
    call neu_multi_invoke
    jmp neu_multi_failed
.Lmove_backpressure_b:
    cmp qword ptr [r15+56],1
    je .Lmove_backpressure_b_start
    cmp qword ptr [r15+56],2
    je .Lmove_backpressure_b_after_first
    cmp qword ptr [r15+56],3
    je .Lmove_backpressure_b_after_second
    jmp .Lmove_done
.Lmove_backpressure_b_start:
    test qword ptr [0x402150],0x2000000
    jz .Lmove_done
    ipc_call 0x18,"qword ptr [0x402130]",0,-36,0
    ipc_receive "qword ptr [0x402130]",11
    mov qword ptr [r15+56],2
    jmp .Lmove_done
.Lmove_backpressure_b_after_first:
    mov eax,1
    shl rax,34
    test qword ptr [0x402150],rax
    jz .Lmove_done
    ipc_receive "qword ptr [0x402130]",22
    mov qword ptr [r15+56],3
    jmp .Lmove_done
.Lmove_backpressure_b_after_second:
    test qword ptr [0x402150],0x4000000
    jz .Lmove_done
    ipc_receive "qword ptr [0x402130]",33
    # Free one receiver-table slot before installing the moved grant.
    ipc_call 0x11,"qword ptr [0x402110]",0,0,0
    mov eax,0x18
    mov rdi,[0x402130]
    call neu_ipc_invoke
    cmp rax,-32
    je .Lipc_done
    test rax,rax
    jne neu_multi_failed
    test rdx,rdx
    je neu_multi_failed
    mov [0x402168],rdx
    ipc_call 0xfffffffffffffc00,"qword ptr [0x402168]",0,0,1
    ipc_call 0x11,"qword ptr [0x402168]",0,0,0
    mov edi,0x31
    mov eax,2
    call neu_multi_invoke
    jmp neu_multi_failed
.endif
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
