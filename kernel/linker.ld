OUTPUT_FORMAT(elf64-x86-64)
ENTRY(_start)

PHDRS
{
    requests PT_LOAD FLAGS(6);
    text PT_LOAD FLAGS(5);
    rodata PT_LOAD FLAGS(4);
    data PT_LOAD FLAGS(6);
    stack PT_GNU_STACK FLAGS(6);
}

SECTIONS
{
    . = 0xffffffff80000000;
    .limine_requests : ALIGN(8)
    {
        KEEP(*(.limine_requests_start))
        KEEP(*(.limine_requests))
        KEEP(*(.limine_requests_end))
    } :requests

    . = ALIGN(4096);
    .text : { *(.text.entry) *(.text .text.*) } :text

    . = ALIGN(4096);
    .rodata : { *(.rodata .rodata.*) } :rodata

    . = ALIGN(4096);
    .data : { *(.data .data.*) *(.got .got.*) } :data
    .bss (NOLOAD) :
    {
        *(.bss .bss.*) *(COMMON)
        . = ALIGN(16);
        __stack_bottom = .;
        . += 64K;
        __stack_top = .;
    } :data

    /DISCARD/ : { *(.eh_frame*) *(.note .note.*) }
}

ASSERT((__stack_top & 15) == 0, "kernel stack must be 16-byte aligned")
