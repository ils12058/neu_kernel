# The macro expands inside the sealed Node RX page, never into a public runtime.
.macro neu_cap_user_body
neu_cap_user_steps:
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
    mov rax,[r15+24]
    cmp rax,3
    je .Lcap_b
    cmp rax,2
    je .Lcap_a2
    cmp qword ptr [r15+56],0
    jne .Lcap_a1_later
    cap_call 0xfffffffffffffd00,0x100000001,0,0,10
    cap_call 0xfffffffffffffd01,0x100000002,999,-18,0
    cap_call 0xfffffffffffffd00,0x100000003,0,-18,0
    cap_call 0xfffffffffffffd03,0x100000002,999,-18,0
    cap_call 0xfffffffffffffd00,0x100000004,0,-19,0
    cap_call 0xfffffffffffffd04,0x100000004,0,0,1
    cap_call 0xfffffffffffffd00,0,0,-16,0
    cap_call 0xfffffffffffffd00,0x100000009,0,-16,0
    cap_call 0xfffffffffffffd00,1,0,-16,0
    cap_call 0xfffffffffffffd00,0xdeadbeefcafebabe,0,-16,0
    .if {cap_rollback}
    cap_call 0xfffffffffffffd00,0x100000008,0,-17,0
    .else
    cap_call 0xfffffffffffffd00,0x100000008,0,-16,0
    .endif
    cap_call 0xfffffffffffffd00,0x10000000101,0,-16,0
    cap_call 0xfffffffffffffd01,0x100000001,20,0,0
    cap_call 0xfffffffffffffd05,0x100000005,0,0,9
    cap_call 0xfffffffffffffd02,0x100000005,0,-17,0
    mov qword ptr [r15+56],1
    mov qword ptr [0x402100],1
    jmp .Lcap_done
.Lcap_a1_later:
    cmp qword ptr [r15+56],1
    jne .Lcap_done
    cmp qword ptr [0x402100],2
    jne .Lcap_done
    cap_call 0xfffffffffffffd02,0x100000001,0,0,0
    cap_call 0xfffffffffffffd00,0x100000002,0,0,40
    mov qword ptr [r15+56],2
    mov qword ptr [0x402100],3
    jmp .Lcap_done
.Lcap_a2:
    cmp qword ptr [r15+56],0
    jne .Lcap_a2_after
    cmp qword ptr [0x402100],1
    jne .Lcap_done
    cap_call 0xfffffffffffffd00,0x100000001,0,0,20
    cap_call 0xfffffffffffffd03,0x100000001,40,0,20
    cap_call 0xfffffffffffffd00,0x100000002,0,0,40
    mov qword ptr [r15+56],1
    mov qword ptr [0x402100],2
    jmp .Lcap_done
.Lcap_a2_after:
    cmp qword ptr [r15+56],1
    jne .Lcap_a2_reused
    cmp qword ptr [0x402100],3
    jne .Lcap_done
    cap_call 0xfffffffffffffd00,0x100000001,0,-17,0
    cap_call 0xfffffffffffffd00,0x100000002,0,0,40
    cap_call 0xfffffffffffffd01,0x100000003,50,0,0
    cap_call 0xfffffffffffffd00,0x100000002,0,0,50
    mov qword ptr [r15+56],2
    mov qword ptr [0x402100],4
    jmp .Lcap_done
.Lcap_a2_reused:
    cmp qword ptr [r15+56],2
    jne .Lcap_done
    cmp qword ptr [0x402100],5
    jne .Lcap_done
    cap_call 0xfffffffffffffd00,0x100000001,0,-17,0
    cap_call 0xfffffffffffffd00,"qword ptr [0x402110]",0,0,77
    cap_call 0xfffffffffffffd01,"qword ptr [0x402110]",88,0,0
    cap_call 0xfffffffffffffd03,"qword ptr [0x402110]",99,0,88
    cap_call 0xfffffffffffffd00,"qword ptr [0x402110]",0,0,99
    cap_call 0xfffffffffffffd00,0x100000002,0,0,50
    mov qword ptr [r15+56],3
    mov qword ptr [0x402100],6
    jmp .Lcap_done
.Lcap_b:
    cmp qword ptr [r15+56],0
    jne .Lcap_done
    cap_call 0xfffffffffffffd00,0x100000001,0,0,200
    cap_call 0xfffffffffffffd01,0x100000001,300,0,0
    cap_call 0xfffffffffffffd00,0x100000002,0,-16,0
    mov qword ptr [r15+56],6
    mov qword ptr [0x402100],6
.Lcap_done:
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
neu_cap_user_final:
    cmp qword ptr [rsp+32],3 # CALL shifted the caller's label by eight bytes.
    je .Lcap_b_final
    cmp qword ptr [rsp+32],2
    jne .Lcap_final_done
    cap_call 0xfffffffffffffd00,0x100000002,0,0,50
    cap_call 0xfffffffffffffd00,"qword ptr [0x402110]",0,0,99
    cap_call 0xfffffffffffffd01,"qword ptr [0x402110]",111,0,0
    cap_call 0xfffffffffffffd00,"qword ptr [0x402110]",0,0,111
    cap_call 0xfffffffffffffd02,"qword ptr [0x402110]",0,0,0
    cap_call 0xfffffffffffffd00,"qword ptr [0x402110]",0,-17,0
    cap_call 0xfffffffffffffd00,0x100000002,0,0,50
    jmp .Lcap_final_done
.Lcap_b_final:
    cap_call 0xfffffffffffffd00,0x100000001,0,0,300
.Lcap_final_done:
    ret
neu_cap_invoke:
.global neu_cap_syscall_site,neu_cap_syscall_resume
neu_cap_syscall_site:
    syscall
neu_cap_syscall_resume:
    ret
.endm
.macro cap_call op,handle,value,status,scalar
    mov rax,\op
    mov rdi,\handle
    mov rsi,\value
    xor edx,edx
    xor r10d,r10d
    xor r8d,r8d
    mov r9,0x100000001 # Deliberately ignored user-selected authority domain.
    call neu_cap_invoke
    cmp rax,\status
    jne neu_multi_failed
    cmp rdx,\scalar
    jne neu_multi_failed
.endm

.if {capabilities}
.section .text.cap_probe,"ax"
.global neu_cap_request_probe,neu_cap_request_wait,neu_cap_request_wait_end
neu_cap_request_probe:
    push rcx
    mov ecx,0x100000
neu_cap_request_wait:
    pause
    dec ecx
    jne neu_cap_request_wait
neu_cap_request_wait_end:
    pop rcx
    ret
.endif
