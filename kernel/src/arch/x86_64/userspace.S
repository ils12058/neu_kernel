# Test-only bridge. No SYSCALL/SYSRET, scheduler, or user-pointer arguments.
.section .bss.user_bridge,"aw",@nobits
.balign 8
neu_user_saved_rsp: .skip 8
neu_user_saved_root: .skip 8
neu_user_saved_cr0: .skip 8
neu_user_saved_efer: .skip 8

.section .bss.user_kernel_stack,"aw",@nobits
.balign 4096
.global __user_kernel_guard_low, __user_kernel_bottom, __user_kernel_top, __user_kernel_guard_high
__user_kernel_guard_low: .skip 4096
__user_kernel_bottom: .skip 32768
__user_kernel_top:
__user_kernel_guard_high: .skip 4096

.section .text.user_bridge,"ax"
.global neu_enter_user, neu_resume_kernel
.type neu_enter_user,@function
neu_enter_user:
    xor r10d, r10d
    jmp neu_user_bridge
.global neu_enter_native
.type neu_enter_native,@function
neu_enter_native:
    mov r10d, 1
neu_user_bridge:
    cli
    cld
    push rbp
    push rbx
    push r12
    push r13
    push r14
    push r15
    mov [rip + neu_user_saved_rsp], rsp
    mov [rip + neu_user_saved_root], rdx
    mov r8, rdi
    mov r9, rsi
    mov rax, cr0
    mov [rip + neu_user_saved_cr0], rax
    or rax, 10 # MP + TS: x87/SIMD unavailable; no prior register state exposed.
    mov cr0, rax
    mov ecx, 0xc0000080
    rdmsr
    shl rdx, 32
    or rax, rdx
    mov [rip + neu_user_saved_efer], rax
    test r10d, r10d
    jnz 8f # Native configuration owns SCE; preserve its validated setting.
    and eax, -2 # Phase 2A explicitly keeps native SYSCALL disabled.
8:
    mov edx, 0
    wrmsr
    # No legacy SYSENTER entry is configured either (CS=0 causes #GP).
    mov ecx, 0x174
    xor eax, eax
    xor edx, edx
    wrmsr
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
    mov ax, 0x33
    mov ds, ax
    mov es, ax
    push 0x33
    push r9
    push 2 # IF/DF/IOPL/TF/NT/AC clear; reserved bit 1 set.
    push 0x3b
    push r8
    xor eax, eax
    xor ebx, ebx
    xor ecx, ecx
    xor edx, edx
    xor ebp, ebp
    xor esi, esi
    xor edi, edi
    xor r8d, r8d
    xor r9d, r9d
    xor r10d, r10d
    xor r11d, r11d
    xor r12d, r12d
    xor r13d, r13d
    xor r14d, r14d
    xor r15d, r15d
    iretq

.type neu_resume_kernel,@function
neu_resume_kernel:
    cli
    cld
    mov rax, [rip + neu_user_saved_root]
    mov cr3, rax # Deactivate user translations before any reclamation.
    mov rax, [rip + neu_user_saved_cr0]
    mov cr0, rax
    mov rax, [rip + neu_user_saved_efer]
    mov rdx, rax
    shr rdx, 32
    mov ecx, 0xc0000080
    wrmsr
    mov rsp, [rip + neu_user_saved_rsp]
    mov qword ptr [rip + neu_user_saved_rsp], 0
    pop r15
    pop r14
    pop r13
    pop r12
    pop rbx
    pop rbp
    ret

# These bytes are data in the kernel ELF, then copied to a NEW zeroed frame.
# All intra-fixture branches are relative; data VAs are the narrow test layout.
.section .rodata.user_fixture,"a"
.balign 16
.global neu_user_payload, neu_user_payload_end, neu_user_wait
.global neu_user_kernel_site, neu_user_readonly_site, neu_user_privileged_site
.global neu_user_unmapped_site, neu_user_gate_site, neu_user_guard_site
.global neu_user_done, neu_user_done_resume
neu_user_payload:
    or rax, rbx
    or rax, rcx
    or rax, rdx
    or rax, rbp
    or rax, rsi
    or rax, rdi
    or rax, r8
    or rax, r9
    or rax, r10
    or rax, r11
    or rax, r12
    or rax, r13
    or rax, r14
    or rax, r15
    jne neu_user_done # Nonzero entry state cannot produce the expected result.
    movabs rdi, 0x401000
    mov rax, [rdi] # Read R/NX constants (35).
    add rax, 7
    push rax
    pop rbx # Exercise the separate RW/NX user stack.
    movabs rdi, 0x402000
    mov [rdi], rbx # Result read back through the owned frame after return.
    mov r8w, cs
    mov r9w, ss
    mov ecx, 0x10000000 # Bounded observation interval for a direct CPL=3 QMP sample.
neu_user_wait:
    dec ecx
    jne neu_user_wait
    movabs rsi, 0x401000
    mov edx, [rsi + 8]
    cmp edx, 0
    je neu_user_done
    cmp edx, 1
    je 1f
    cmp edx, 2
    je 2f
    cmp edx, 3
    je 3f
    cmp edx, 4
    je 4f
    cmp edx, 5
    je neu_user_privileged_site
    cmp edx, 6
    je neu_user_gate_site
    cmp edx, 7
    je 7f
    ud2
1:
    movabs rdi, 0xffffffff80000000
neu_user_kernel_site:
    mov rax, [rdi]
    jmp neu_user_done
2:
    movabs rdi, 0x401000
neu_user_readonly_site:
    mov byte ptr [rdi], 0
    jmp neu_user_done
3:
    movabs rdi, 0x402100
    call rdi # An NX failure returns from the RET byte and is explicitly rejected.
    jmp neu_user_done
4:
    movabs rdi, 0x406000
neu_user_unmapped_site:
    mov rax, [rdi]
    jmp neu_user_done
neu_user_privileged_site:
    hlt
    jmp neu_user_done
neu_user_gate_site:
    int 0x81 # Present kernel gate, DPL=0: expected #GP(0x40a).
    jmp neu_user_done
7:
    movabs rdi, 0x403000
neu_user_guard_site:
    mov rax, [rdi]
neu_user_done:
    int 0x80
neu_user_done_resume:
    ud2
neu_user_payload_end:
