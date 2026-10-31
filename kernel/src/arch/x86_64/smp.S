# Phase 9A application-processor bootstrap. Limine jumps to these stubs with
# IF=0, the kernel loader page tables active, RDI = HHDM mp_info, and RSP on a
# loader-owned stack. Each stub immediately abandons that stack for its own
# image-reserved arena and enters Rust; it never returns and never touches the
# BSP's single-CPU kernel state.

.section .text.smp,"ax"
.global neu_smp_ap_entry1, neu_smp_ap_entry2, neu_smp_ap_entry3
.type neu_smp_ap_entry1, @function
.type neu_smp_ap_entry2, @function
.type neu_smp_ap_entry3, @function
neu_smp_ap_entry1:
    cli
    cld
    mov rdx, rsp
    lea rsp, [rip + neu_smp_ap1_stack_top]
    and rsp, -16
    xor ebp, ebp
    mov esi, 1
    call neu_smp_ap_main
    ud2
.size neu_smp_ap_entry1, . - neu_smp_ap_entry1

neu_smp_ap_entry2:
    cli
    cld
    mov rdx, rsp
    lea rsp, [rip + neu_smp_ap2_stack_top]
    and rsp, -16
    xor ebp, ebp
    mov esi, 2
    call neu_smp_ap_main
    ud2
.size neu_smp_ap_entry2, . - neu_smp_ap_entry2

neu_smp_ap_entry3:
    cli
    cld
    mov rdx, rsp
    lea rsp, [rip + neu_smp_ap3_stack_top]
    and rsp, -16
    xor ebp, ebp
    mov esi, 3
    call neu_smp_ap_main
    ud2
.size neu_smp_ap_entry3, . - neu_smp_ap_entry3

.global neu_smp_park_begin, neu_smp_park_end
.type neu_smp_park_begin, @function
neu_smp_park_begin:
    cli
1:
    hlt
    jmp 1b
neu_smp_park_end:
.size neu_smp_park_begin, . - neu_smp_park_begin

# One arena per possible AP logical index (1..=3, MAX_CPUS = 4). Stacks and
# IST stacks are page-bounded; the three guard pages per arena are skipped by
# the owned-root image mapping and remain unmapped after CR3 activation.
.section .bss.smp,"aw",@nobits

.balign 4096
.global neu_smp_ap1_guard_low
neu_smp_ap1_guard_low:
    .skip 4096
.global neu_smp_ap1_stack_bottom, neu_smp_ap1_stack_top
neu_smp_ap1_stack_bottom:
    .skip 16384
neu_smp_ap1_stack_top:
.global neu_smp_ap1_guard_mid
neu_smp_ap1_guard_mid:
    .skip 4096
.global neu_smp_ap1_ist_bottom, neu_smp_ap1_ist_top
neu_smp_ap1_ist_bottom:
    .skip 8192
neu_smp_ap1_ist_top:
.global neu_smp_ap1_guard_ist
neu_smp_ap1_guard_ist:
    .skip 4096
.balign 16
.global neu_smp_ap1_gdt
neu_smp_ap1_gdt:
    .skip 64
.balign 16
.global neu_smp_ap1_tss
neu_smp_ap1_tss:
    .skip 104
.balign 4096
.global neu_smp_ap1_idt
neu_smp_ap1_idt:
    .skip 4096

.balign 4096
.global neu_smp_ap2_guard_low
neu_smp_ap2_guard_low:
    .skip 4096
.global neu_smp_ap2_stack_bottom, neu_smp_ap2_stack_top
neu_smp_ap2_stack_bottom:
    .skip 16384
neu_smp_ap2_stack_top:
.global neu_smp_ap2_guard_mid
neu_smp_ap2_guard_mid:
    .skip 4096
.global neu_smp_ap2_ist_bottom, neu_smp_ap2_ist_top
neu_smp_ap2_ist_bottom:
    .skip 8192
neu_smp_ap2_ist_top:
.global neu_smp_ap2_guard_ist
neu_smp_ap2_guard_ist:
    .skip 4096
.balign 16
.global neu_smp_ap2_gdt
neu_smp_ap2_gdt:
    .skip 64
.balign 16
.global neu_smp_ap2_tss
neu_smp_ap2_tss:
    .skip 104
.balign 4096
.global neu_smp_ap2_idt
neu_smp_ap2_idt:
    .skip 4096

.balign 4096
.global neu_smp_ap3_guard_low
neu_smp_ap3_guard_low:
    .skip 4096
.global neu_smp_ap3_stack_bottom, neu_smp_ap3_stack_top
neu_smp_ap3_stack_bottom:
    .skip 16384
neu_smp_ap3_stack_top:
.global neu_smp_ap3_guard_mid
neu_smp_ap3_guard_mid:
    .skip 4096
.global neu_smp_ap3_ist_bottom, neu_smp_ap3_ist_top
neu_smp_ap3_ist_bottom:
    .skip 8192
neu_smp_ap3_ist_top:
.global neu_smp_ap3_guard_ist
neu_smp_ap3_guard_ist:
    .skip 4096
.balign 16
.global neu_smp_ap3_gdt
neu_smp_ap3_gdt:
    .skip 64
.balign 16
.global neu_smp_ap3_tss
neu_smp_ap3_tss:
    .skip 104
.balign 4096
.global neu_smp_ap3_idt
neu_smp_ap3_idt:
    .skip 4096
