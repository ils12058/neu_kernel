.section .text.entry,"ax"
.global _start
.type _start, @function
_start:
    cli
    cld
    # Limine has mapped the ELF's writable, zero-filled stack segment.
    # Switch before Rust runs; retain the loader's page tables. Rust installs
    # private CPU descriptor tables before continuing bootstrap.
    lea rsp, [rip + __stack_top]
    and rsp, -16
    xor ebp, ebp
    call neu_main
    jmp neu_halt_begin
.size _start, . - _start

.global neu_halt_begin
.global neu_halt_end
.type neu_halt_begin, @function
neu_halt_begin:
    cli
1:
    hlt
    jmp 1b
neu_halt_end:
.size neu_halt_begin, . - neu_halt_begin
