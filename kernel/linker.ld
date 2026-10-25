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
    __kernel_start = .;
    __requests_start = .;
    .limine_requests : ALIGN(8)
    {
        KEEP(*(.limine_requests_start))
        KEEP(*(.limine_requests))
        KEEP(*(.limine_requests_end))
        . = ALIGN(4096);
    } :requests

    . = ALIGN(4096);
    __requests_end = .;
    __text_start = .;
    .text : { *(.text.entry) *(.text .text.*) . = ALIGN(4096); } :text
    __text_end = .;

    . = ALIGN(4096);
    __rodata_start = .;
    .rodata : { *(.rodata .rodata.*) . = ALIGN(4096); } :rodata
    __rodata_end = .;

    . = ALIGN(4096);
    __data_start = .;
    .data : { *(.data .data.*) *(.got .got.*) } :data
    .bss (NOLOAD) :
    {
        *(.bss .bss.*) *(COMMON)
        . = ALIGN(4096);
        __stack_guard_low = .;
        . += 4K;
        __stack_bottom = .;
        . += 64K;
        __stack_top = .;
        __stack_guard_high = .;
        . += 4K;
        __df_guard_low = .;
        . += 4K;
        __df_stack_bottom = .;
        . += 32K;
        __df_stack_top = .;
        __df_guard_high = .;
        . += 4K;
        . = ALIGN(4096);
        __kernel_end = .;
    } :data

    /DISCARD/ : { *(.eh_frame*) *(.note .note.*) }
}

ASSERT((__stack_top & 15) == 0, "kernel stack must be 16-byte aligned")
