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
    jne .Lipc_dispatch
    mov eax,1
    call neu_ipc_invoke
    test rax,rax
    jne neu_multi_failed
    mov rcx,0x30000003f
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
    ipc_call 0x13,"qword ptr [0x402110]",0,0,0
    ipc_call 0x13,0x100000005,0xfedcba9876543210,0,0 # Guess resolves only locally.
    ipc_call 0x13,"qword ptr [0x402110]",999,-33,0
    mov qword ptr [0x402100],1
    jmp .Lipc_done
.Lipc_dispatch:
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
