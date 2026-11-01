# Phase 9B cross-CPU work support. The work IPI and spurious stubs save the
# same frame shape as the exception path, call Rust on the AP's IST1 stack, and
# force IF=0 before returning so the AP re-enters its documented idle window.

.section .text.smp_work,"ax"

.global neu_smp_work_ipi
neu_smp_work_ipi:
    cli
    cld
    push 0
    push 0xf0
    jmp neu_smp_work_ipi_entry

.global neu_smp_work_ipi_spurious
neu_smp_work_ipi_spurious:
    cli
    cld
    push 0
    push 0xff

neu_smp_work_ipi_entry:
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
    mov r12, rsp
    and rsp, -16
    mov rdi, r12
    call neu_smp_work_ipi_handler
    # The saved RFLAGS word sits at frame offset 152. Never return with IF=1.
    and qword ptr [r12 + 152], -513
    mov rsp, r12
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
    add rsp, 16
    iretq

# One bounded assembly dwell used by the AP after publishing START and by the
# BSP after logging a deterministic checkpoint marker. QMP matches RIP inside
# the exported begin/end range; this is not timing evidence.
.global neu_smp_work_ap_dwell_begin, neu_smp_work_ap_dwell_end
.global neu_smp_work_bsp_dwell_begin, neu_smp_work_bsp_dwell_end
neu_smp_work_ap_dwell_begin:
    mov ecx, 0x10000000
1:
    dec ecx
    jnz 1b
    ret
neu_smp_work_ap_dwell_end:

neu_smp_work_bsp_dwell_begin:
    mov ecx, 0x10000000
1:
    dec ecx
    jnz 1b
    ret
neu_smp_work_bsp_dwell_end:

# AP gate wait: polls the RELEASE header word (offset 32) until it equals the
# epoch. Returns 0 on release and 1 on the bounded timeout.
.global neu_smp_work_gate_wait_begin, neu_smp_work_gate_wait_end
neu_smp_work_gate_wait_begin:
    mov rcx, 0x400000000
1:
    mov rax, [rdi + 32]
    cmp rax, rsi
    je 2f
    pause
    dec rcx
    jnz 1b
    mov eax, 1
    ret
2:
    xor eax, eax
    ret
neu_smp_work_gate_wait_end:

# The only AP interrupt window: an interrupt taken during HLT resumes at
# `neu_smp_work_idle_after`, which the handler verifies before EOI.
.global neu_smp_work_idle_once, neu_smp_work_idle_after, neu_smp_work_idle_end
neu_smp_work_idle_once:
    sti
    hlt
neu_smp_work_idle_after:
    cli
    ret
neu_smp_work_idle_end:
