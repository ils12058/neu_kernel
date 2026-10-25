.section .text.exceptions,"ax"
.macro ISR number, has_error=0
.global neu_isr_\number
.type neu_isr_\number, @function
neu_isr_\number:
.if \has_error == 0
    push 0
.endif
    push \number
    jmp neu_exception_common
.endm

ISR 0
ISR 1
ISR 2
ISR 3
ISR 4
ISR 5
ISR 6
ISR 7
ISR 8, 1
ISR 9
ISR 10, 1
ISR 11, 1
ISR 12, 1
ISR 13, 1
ISR 14, 1
ISR 15
ISR 16
ISR 17, 1
ISR 18
ISR 19
ISR 20
ISR 21, 1
ISR 22
ISR 23
ISR 24
ISR 25
ISR 26
ISR 27
ISR 28
ISR 29, 1
ISR 30, 1
ISR 31
ISR 255

neu_exception_common:
    cli
    cld
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
    mov rdi, rsp
    mov rsi, cr2
    and rsp, -16
    call neu_exception
    jmp neu_halt_begin

.global neu_load_gdt
.type neu_load_gdt, @function
neu_load_gdt:
    lgdt [rdi]
    push 0x08
    lea rax, [rip + 1f]
    push rax
    retfq
1:
    mov ax, 0x10
    mov ds, ax
    mov es, ax
    mov ss, ax
    xor eax, eax
    mov fs, ax
    mov gs, ax
    mov ax, 0x18
    ltr ax
    ret

.section .rodata.isr_table,"a"
.global neu_isr_table
neu_isr_table:
.quad neu_isr_0, neu_isr_1, neu_isr_2, neu_isr_3, neu_isr_4, neu_isr_5, neu_isr_6, neu_isr_7
.quad neu_isr_8, neu_isr_9, neu_isr_10, neu_isr_11, neu_isr_12, neu_isr_13, neu_isr_14, neu_isr_15
.quad neu_isr_16, neu_isr_17, neu_isr_18, neu_isr_19, neu_isr_20, neu_isr_21, neu_isr_22, neu_isr_23
.quad neu_isr_24, neu_isr_25, neu_isr_26, neu_isr_27, neu_isr_28, neu_isr_29, neu_isr_30, neu_isr_31

.section .text.test_ud,"ax"
.global neu_test_ud, neu_test_ud_site
neu_test_ud:
neu_test_ud_site:
    ud2
    jmp neu_halt_begin

.section .text.test_gp,"ax"
.global neu_test_gp, neu_test_gp_site
neu_test_gp:
    mov ax, 0x28
neu_test_gp_site:
    mov ds, ax
    jmp neu_halt_begin

.section .text.test_bp,"ax"
.global neu_test_bp, neu_test_bp_resume
neu_test_bp:
    int3
neu_test_bp_resume:
    jmp neu_halt_begin

.section .text.test_pf,"ax"
.global neu_test_pf, neu_test_pf_site
neu_test_pf:
neu_test_pf_site:
    mov byte ptr [rdi], 0
    jmp neu_halt_begin
