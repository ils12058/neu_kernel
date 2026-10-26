# Single-CPU native entry. Every early memory operand is RIP-relative kernel
# storage. No push/call or user-address dereference precedes the trusted RSP.
.section .bss.native_entry,"aw",@nobits
.balign 8
.global neu_native_busy, neu_native_observed, neu_native_saved_user_rsp
neu_native_busy: .skip 8
neu_native_saved_user_rsp: .skip 8
neu_native_observed: .skip 8
.global neu_native_selected_top
neu_native_selected_top: .skip 8

.section .bss.native_emergency,"aw",@nobits
.balign 4096
.global __native_guard_low, __native_emergency_bottom, __native_emergency_top, __native_guard_high
__native_guard_low: .skip 4096
__native_emergency_bottom: .skip 16384
__native_emergency_top:
__native_guard_high: .skip 4096

.section .text.native_entry,"ax"
.global neu_native_entry, neu_native_stack_ready, neu_native_iret
.type neu_native_entry,@function
neu_native_entry:
    cmp qword ptr [rip + neu_native_busy], 0
    jne neu_native_nested_entry
    mov qword ptr [rip + neu_native_busy], 1
    mov [rip + neu_native_saved_user_rsp], rsp
.if {cooperative}
    mov rsp, [rip + neu_native_selected_top]
.else
    lea rsp, [rip + __user_kernel_top]
.endif
neu_native_stack_ready:
    push {user_ss}
    push qword ptr [rip + neu_native_saved_user_rsp]
    push r11
    push {user_cs}
    push rcx
    push rax
    push rbx
    push rcx
    push rdx
    push rbp
    push rsi
    push rdi
    push r8
    push r9
    push r10
    push r11
    push r12
    push r13
    push r14
    push r15
    # User registers are saved. No GS-relative state or SWAPGS is used.
    mov ax, {kernel_ss}
    mov ds, ax
    mov es, ax
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
    cld
    cmp qword ptr [rip + neu_native_observed], 0
    jne 9f
    mov qword ptr [rip + neu_native_observed], 1
    mov ecx, {kernel_wait}
.global neu_native_kernel_wait, neu_native_kernel_wait_end
neu_native_kernel_wait:
    dec ecx
    jne neu_native_kernel_wait
neu_native_kernel_wait_end:
9:
    mov rdi, rsp
    call neu_syscall
    # Rust returns only after validating/rechecking this complete return frame.
    mov ax, {user_ss}
    mov ds, ax
    mov es, ax
    mov qword ptr [rip + neu_native_busy], 0
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
neu_native_iret:
    iretq
neu_native_nested_entry:
    lea rsp, [rip + __native_emergency_top]
    call neu_native_nested
    jmp neu_halt_begin

# Constants originate in the Rust ABI/test definition; absolute ELF symbols let
# the host check the emitted contract independently without copying definitions.
.global neu_abi_query, neu_abi_exit, neu_abi_info, neu_abi_unknown, neu_abi_invalid
.set neu_abi_query, {query}
.set neu_abi_exit, {exit}
.set neu_abi_info, {info}
.set neu_abi_unknown, {unsupported}
.set neu_abi_invalid, {invalid}
.global neu_abi_star, neu_abi_fmask
.set neu_abi_star, {star}
.set neu_abi_fmask, {fmask}
.global neu_native_canary_bx, neu_native_canary_bp, neu_native_canary_12, neu_native_canary_13, neu_native_canary_14, neu_native_canary_15
.set neu_native_canary_bx, {canary_bx}
.set neu_native_canary_bp, {canary_bp}
.set neu_native_canary_12, {canary_12}
.set neu_native_canary_13, {canary_13}
.set neu_native_canary_14, {canary_14}
.set neu_native_canary_15, {canary_15}

.section .rodata.native_fixture,"a"
.balign 16
.global neu_native_payload, neu_native_payload_end
.global neu_native_before_wait, neu_native_before_wait_end
.global neu_native_after_wait, neu_native_after_wait_end
.global neu_native_syscall_site, neu_native_resume, neu_native_bad_rsp_site, neu_native_bad_rsp_resume
neu_native_payload:
    movabs rbx, {canary_bx}
    movabs rbp, {canary_bp}
    movabs r12, {canary_12}
    movabs r13, {canary_13}
    movabs r14, {canary_14}
    movabs r15, {canary_15}
    mov ecx, 0x10000000
neu_native_before_wait:
    dec ecx
    jne neu_native_before_wait
neu_native_before_wait_end:
    mov edi, 11
    mov esi, 22
    mov edx, 33
    mov r10d, 44
    mov r8d, 55
    mov r9d, 66
    mov eax, {query}
    movabs rcx, 0x401000
    cmp qword ptr [rcx + 8], 4
    je 4f
    cmp qword ptr [rcx + 8], 5
    jne 1f
    std # Hardware FMASK must clear DF before Rust; return validation rejects it.
1:
    call neu_native_invoke
    test rax, rax
    jne neu_native_failed
    movabs rcx, {info}
    cmp rdx, rcx
    jne neu_native_failed
    cmp rdi, 11
    jne neu_native_failed
    call neu_native_check
    movabs rcx, 0x401000
    cmp qword ptr [rcx + 8], 8
    jb 6f
    mov rax, [rcx]
    add rax, 7 # Independent Node seed (35 or 77) produces 42 or 84.
    movabs rcx, 0x402000
    mov [rcx], rax
    xor eax, eax
    jmp 7f
6:
    movabs rcx, 0x402000
    mov qword ptr [rcx], 42
7:
    mov ecx, 0x10000000
neu_native_after_wait:
    dec ecx
    jne neu_native_after_wait
neu_native_after_wait_end:
    movabs rcx, 0x401000
    cmp qword ptr [rcx + 8], 8
    jae neu_node_action
    movabs rcx, 0x401000
    cmp qword ptr [rcx + 8], 1
    je 2f
    cmp qword ptr [rcx + 8], 2
    je 3f
    jmp 5f
2:
    .if {abi03_disabled}
    mov eax,0x13
    .elseif {abi02_disabled}
    mov eax,0x12
    .elseif {cap_disabled}
    mov rax,0xfffffffffffffd00
    .else
    mov rax,-1
    .endif
    call neu_native_invoke
    cmp rax, {unsupported}
    jne neu_native_failed
    test rdx, rdx
    jne neu_native_failed
    cmp rdi, 11
    jne neu_native_failed
    call neu_native_check
    jmp 5f
3:
    mov eax, {exit}
    mov edi, 256
    call neu_native_invoke
    cmp rax, {invalid}
    jne neu_native_failed
    test rdx, rdx
    jne neu_native_failed
    cmp rdi, 256
    jne neu_native_failed
    call neu_native_check
5:
    mov edi, 11
    mov edx, 33
    mov eax, {query}
    call neu_native_invoke
    test rax, rax
    jne neu_native_failed
    movabs rcx, {info}
    cmp rdx, rcx
    jne neu_native_failed
    cmp rdi, 11
    jne neu_native_failed
    call neu_native_check
    mov eax, {exit}
    xor edi, edi
    call neu_native_invoke
    ud2 # A successful thread_exit MUST NOT return here.
4:
    movabs rsp, 0x800000000000
neu_native_bad_rsp_site:
    syscall
neu_native_bad_rsp_resume:
    ud2
neu_native_invoke:
neu_native_syscall_site:
    syscall
neu_native_resume:
    ret
neu_native_check:
    movabs rcx, {canary_bx}
    cmp rbx, rcx
    jne neu_native_failed
    movabs rcx, {canary_bp}
    cmp rbp, rcx
    jne neu_native_failed
    movabs rcx, {canary_12}
    cmp r12, rcx
    jne neu_native_failed
    movabs rcx, {canary_13}
    cmp r13, rcx
    jne neu_native_failed
    movabs rcx, {canary_14}
    cmp r14, rcx
    jne neu_native_failed
    movabs rcx, {canary_15}
    cmp r15, rcx
    jne neu_native_failed
    cmp rsi, 22
    jne neu_native_failed
    cmp r10, 44
    jne neu_native_failed
    cmp r8, 55
    jne neu_native_failed
    cmp r9, 66
    jne neu_native_failed
    movabs rcx, 0x404ff0 # CALL to this checker consumes one user-stack word.
    cmp rsp, rcx
    jne neu_native_failed
    ret
# Node actions occur only after an actual query return and QMP user sample.
.global neu_node_cross_site, neu_node_ud_site
neu_node_action:
    cmp qword ptr [rcx + 8], 10
    je neu_node_cross
    cmp qword ptr [rcx + 8], 11
    je neu_node_ud_site
    cmp qword ptr [rcx + 8], 9
    jne 5b
    movabs rcx, 0x406000
    mov rax, [rcx] # B can actually read its own private-only page.
    movabs rcx, {private_value}
    cmp rax, rcx
    jne neu_native_failed
    jmp 5b
neu_node_cross:
    movabs rcx, 0x406000
neu_node_cross_site:
    mov rax, [rcx] # Absent in A; B's mapping must confer no authority here.
    jmp neu_native_failed
neu_node_ud_site:
    ud2
neu_native_failed:
    ud2
neu_native_payload_end:
