.macro as_call op,arg,value,status,result
    mov rax,\op
    mov rdi,\arg
    mov rsi,\value
    xor edx,edx
    xor r10d,r10d
    xor r8d,r8d
    mov r9,0x100000001 # Ignored; cannot select a Node or address space.
    call neu_as_invoke
    cmp rax,\status
    jne neu_multi_failed
    mov rcx,\result
    cmp rdx,rcx
    jne neu_multi_failed
.endm
.macro as_get index,offset
    mov eax,0x10
    mov edi,\index
    call neu_as_invoke
    test rax,rax
    jne neu_multi_failed
    test rdx,rdx
    je neu_multi_failed
    mov [0x402100+\offset],rdx
.endm
.macro neu_as_user_body
neu_as_user_steps:
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
    cmp qword ptr [r15+24],3
    je .Las_b
    cmp qword ptr [r15+24],2
    je .Las_a2
    cmp qword ptr [r15+56],0
    jne .Las_a1_after
    as_call 1,0,0,0,0x20000001f
    .if {channel_disabled}
    as_call 0x13,0,0,-1,0
    as_call 0x14,0,0,-1,0
    .endif
    as_get 0,16
    as_get 1,24
    as_get 2,32
    as_get 3,40
    as_call 0x10,4,0,-16,0
    as_call 0x10,0xffffffffffffffff,0,-16,0
    as_call 0x12,"qword ptr [0x402110]",0x400000,0,11
    as_call 0x12,"qword ptr [0x402110]",0x402000,0,7
    as_call 0x12,"qword ptr [0x402110]",0x401000,0,3
    as_call 0x12,"qword ptr [0x402110]",0x406000,0,0
    as_call 0x12,"qword ptr [0x402120]",0x400000,-18,0
    as_call 0x12,"qword ptr [0x402128]",0x400000,-19,0
    as_call 0x12,0,0x400000,-16,0
    as_call 0x12,0xdeadbeefcafebabe,0x400000,-16,0
    as_call 0x12,0x100000009,0x400000,-16,0
    as_call 0x12,1,0x400000,-16,0
    as_call 0x12,0x200000001,0x400000,-17,0
    as_call 0x12,"qword ptr [0x402110]",0x400001,-2,0
    as_call 0x12,"qword ptr [0x402110]",0x800000000000,-2,0
    as_call 0x12,"qword ptr [0x402110]",0xffff800000000000,-2,0
    as_call 0x12,"qword ptr [0x402110]",0x800000,-2,0
    as_call 0xfffffffffffffd00,0,0,-1,0
    mov qword ptr [r15+56],1
    mov qword ptr [0x402100],1
    jmp .Las_done
.Las_a1_after:
    cmp qword ptr [r15+56],1
    jne .Las_done
    cmp qword ptr [0x402100],2
    jne .Las_done
    as_call 0x11,"qword ptr [0x402110]",0,0,0
    as_call 0x11,"qword ptr [0x402110]",0,-17,0
    as_call 0x10,0,0,-17,0
    as_call 0x12,"qword ptr [0x402118]",0x402000,0,7
    mov qword ptr [r15+56],2
    mov qword ptr [0x402100],3
    jmp .Las_done
.Las_a2:
    cmp qword ptr [r15+56],0
    jne .Las_a2_after
    cmp qword ptr [0x402100],1
    jne .Las_done
    as_call 0x10,0,0,0,"qword ptr [0x402110]"
    as_call 0x12,"qword ptr [0x402110]",0x404000,0,7
    as_call 0x12,"qword ptr [0x402110]",0x406000,0,0
    mov qword ptr [r15+56],1
    mov qword ptr [0x402100],2
    jmp .Las_done
.Las_a2_after:
    cmp qword ptr [r15+56],1
    jne .Las_done
    cmp qword ptr [0x402100],3
    jne .Las_done
    as_call 0x12,"qword ptr [0x402110]",0x400000,-17,0
    as_call 0x10,0,0,-17,0
    as_call 0x12,"qword ptr [0x402118]",0x402000,0,7
    mov qword ptr [r15+56],2
    jmp .Las_done
.Las_b:
    cmp qword ptr [r15+56],0
    jne .Las_done
    as_get 0,16
    as_get 1,24
    as_get 2,32
    as_get 3,40
    as_call 0x12,"qword ptr [0x402110]",0x406000,0,3
    as_call 0x12,"qword ptr [0x402110]",0x410000,0,0
    as_call 0x12,"qword ptr [0x402118]",0x402000,0,7
    mov qword ptr [r15+56],2
.Las_done:
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
neu_as_user_final:
    cmp qword ptr [rsp+32],3
    je .Las_b_final
    cmp qword ptr [rsp+32],2
    jne .Las_final_done
    as_call 0x12,"qword ptr [0x402118]",0x404000,0,0
    as_call 0x12,"qword ptr [0x402118]",0x402000,0,7
    jmp .Las_final_done
.Las_b_final:
    as_call 0x12,"qword ptr [0x402110]",0x406000,0,3
    as_call 0x12,"qword ptr [0x402118]",0x402000,0,7
.Las_final_done:
    ret
neu_as_invoke:
.global neu_as_syscall_site,neu_as_syscall_resume
neu_as_syscall_site:
    syscall
neu_as_syscall_resume:
    ret
.endm
.if {abi02} || {channels}
.section .text.as_probe,"ax"
.global neu_as_request_probe,neu_as_request_wait,neu_as_request_wait_end
neu_as_request_probe:
    push rcx
    mov ecx,0x100000
neu_as_request_wait:
    pause
    dec ecx
    jne neu_as_request_wait
neu_as_request_wait_end:
    pop rcx
    ret
.endif
