# K11 userspace driver and clients. This payload is copied into each Node's RX
# page. Only native ABI calls touch hardware; BAR is a shared test rendezvous.
.section .rodata.smp_device_fixture,"a"
.balign 16
.global neu_smp_sched_payload, neu_smp_sched_payload_end
.global neu_smp_sched_user_loop_begin, neu_smp_sched_user_loop_end
.global neu_smp_sched_fault_site, neu_smp_sched_yield_resume
.global neu_smp_sched_query_resume, neu_smp_sched_done_resume
.macro DZERO
    xor esi, esi
    xor edx, edx
    xor r10d, r10d
    xor r8d, r8d
    xor r9d, r9d
.endm
.macro DOK
    test rax, rax
    jne neu_device_bad
.endm
.macro DERR value
    cmp rax, \value
    jne neu_device_bad
    test rdx, rdx
    jne neu_device_bad
    inc qword ptr [rbp + 104]
.endm
neu_smp_sched_payload:
    mov r12, [rsp]
    mov r13, [rsp - 8]
    mov r10, [rsp - 16]
    mov r11, [rsp - 24]
    mov r14d, 0x402000
    mov r15d, 0x417000
    mov rbp, r12
    dec rbp
    shl rbp, 7
    add rbp, r14
    mov [rbp + 40], r11
    mov [rbp + 48], r10
    mov eax, 1
    syscall
neu_smp_sched_query_resume:
    DOK
    mov [rbp + 16], rdx
    xor edi, edi
    mov eax, 0x10
    syscall
    DOK
    mov rdi, rdx
    mov esi, 0x400000
    mov eax, 0x12
    syscall
    DOK
    cmp rdx, 11
    jne neu_device_bad
    mov [rbp + 80], rdx
    mov edi, 5
    mov eax, 0x10
    syscall
    DOK
    mov [rbp + 56], rdx
    movabs r8, 0x1122334455667788
    xor ebx, ebx
neu_smp_sched_user_loop_begin:
    inc rbx
    mov [rbp], rbx
    mov [rbp + 8], r8
    cmp qword ptr [r15], 1
    jne neu_smp_sched_user_loop_begin
neu_smp_sched_user_loop_end:
    cmp r12, 1
    je neu_device_helper
    cmp r12, 2
    je neu_device_client
    # A2 is the first driver; attenuation and invalid calls are made in CPL3.
    mov rdi, [rbp + 56]
    DZERO
    mov esi, 0x314
    mov eax, 0x19
    syscall
    DOK
    mov [rbp + 64], rdx
    mov rdi, [rbp + 56]
    DZERO
    mov esi, 16
    mov eax, 0x19
    syscall
    DOK
    mov rbx, rdx
    mov rdi, rbx
    DZERO
    mov eax, 0x1b
    syscall
    DOK
    cmp edx, 0x11e81234
    jne neu_device_bad
    mov rdi, rbx
    DZERO
    mov esi, 1
    mov eax, 0x1b
    syscall
    DOK
    cmp edx, 0x100000
    jne neu_device_bad
    mov rdi, rbx
    DZERO
    mov esi, 2
    mov eax, 0x1b
    syscall
    DOK
    cmp edx, 7
    jne neu_device_bad
    mov rdi, rbx
    DZERO
    mov esi, 3
    mov eax, 0x1b
    syscall
    DOK
    cmp edx, 1
    jne neu_device_bad
    mov rdi, rbx
    DZERO
    mov esi, 4
    mov eax, 0x1b
    syscall
    DERR -2
    mov rdi, rbx
    DZERO
    mov eax, 0x1c
    syscall
    DERR -18
    mov rdi, rbx
    mov eax, 0x11
    syscall
    DOK
    mov rdi, [rbp + 64]
    DZERO
    mov eax, 0x1c
    syscall
    DOK
    mov [rbp + 72], rdx
    mov rdi, rdx
    DZERO
    mov eax, 0x1d
    syscall
    DOK
    mov [rbp + 88], rdx
    mov rdi, rdx
    DZERO
    mov esi, 1
    mov eax, 0x19
    syscall
    DOK
    mov [rbp + 96], rdx
    # Wrong type, DMA, width, truncation, reserved input and missing ACK rights.
    mov rdi, [rbp + 56]
    DZERO
    mov edx, 4
    mov eax, 0x1e
    syscall
    DERR -19
    mov rdi, [rbp + 72]
    DZERO
    mov esi, 0x80
    mov edx, 4
    mov eax, 0x1e
    syscall
    DERR -45
    mov rdi, [rbp + 72]
    DZERO
    mov esi, 4
    mov edx, 8
    mov eax, 0x1e
    syscall
    DERR -45
    mov rdi, [rbp + 72]
    DZERO
    mov esi, 4
    mov edx, 4
    movabs r10, 0x100000000
    mov eax, 0x1f
    syscall
    DERR -45
    mov rdi, [rbp + 72]
    DZERO
    mov edx, 4
    mov r9d, 1
    mov eax, 0x1e
    syscall
    DERR -2
    mov rdi, [rbp + 96]
    DZERO
    mov esi, 1
    mov eax, 0x21
    syscall
    DERR -18
neu_device_first_arm:
    cmp qword ptr [r15 + 128], 3
    jne neu_device_first_arm
    mov rdi, [rbp + 72]
    DZERO
    mov edx, 4
    mov eax, 0x1e
    syscall
    DOK
    cmp edx, 0x010000ed
    jne neu_device_bad
    mov [rbp + 112], rdx
    mov rdi, [rbp + 72]
    DZERO
    mov esi, 4
    mov edx, 4
    mov r10d, 0xa5a55a5a
    mov eax, 0x1f
    syscall
    DOK
    mov rdi, [rbp + 72]
    DZERO
    mov esi, 4
    mov edx, 4
    mov eax, 0x1e
    syscall
    DOK
    cmp edx, 0x5a5aa5a5
    jne neu_device_bad
    mov [rbp + 120], rdx
    mov rax, [rbp + 72]
    mov [r15 + 112], rax       # deliberately leaked scalar for foreign-table denial
    mov qword ptr [r15 + 64], 1
neu_device_conflict_wait:
    cmp qword ptr [r15 + 72], 1
    jne neu_device_conflict_wait
    mov rdi, [rbp + 96]
    DZERO
    mov eax, 0x20             # actual blocking wait before A1 raises the IRQ
    syscall
.global neu_device_first_wait_resume
neu_device_first_wait_resume:
    DOK
    cmp rdx, 1
    jne neu_device_bad
    mov [rbp + 24], rdx
    cmp qword ptr [r15 + 96], 1
    je neu_device_fault_wait
    cmp qword ptr [r15 + 96], 4
    je neu_device_retired_wait # intentionally leave the delivered IRQ unacknowledged
    mov rdi, [rbp + 88]
    DZERO
    mov esi, 99
    mov eax, 0x21
    syscall
    DERR -43
    mov rdi, [rbp + 88]
    DZERO
    mov esi, 1
    mov eax, 0x21
    syscall
    DOK
    cmp qword ptr [r15 + 96], 2
    je neu_device_cancel_wait
    cmp qword ptr [r15 + 96], 3
    je neu_device_cancel_wait
    cmp qword ptr [r15 + 96], 5
    je neu_device_budget_loop
neu_device_factorial_arm:
    cmp qword ptr [r15 + 128], 3
    jne neu_device_factorial_arm
    mov rdi, [rbp + 72]
    DZERO
    mov esi, 0x20
    mov edx, 4
    mov r10d, 0x80
    mov eax, 0x1f
    syscall
    DOK
    mov rdi, [rbp + 72]
    DZERO
    mov esi, 8
    mov edx, 4
    mov r10d, 6
    mov eax, 0x1f
    syscall
    DOK
    cmp qword ptr [r15 + 96], 6
    je neu_device_flight_fault_wait
neu_device_pending_wait:
    cmp qword ptr [r15 + 104], 1
    jne neu_device_pending_wait
    mov rdi, [rbp + 88]
    DZERO
    mov eax, 0x20             # completion already pending before this wait
    syscall
    DOK
    cmp rdx, 2
    jne neu_device_bad
    mov [rbp + 24], rdx
    mov rdi, [rbp + 72]
    DZERO
    mov esi, 8
    mov edx, 4
    mov eax, 0x1e
    syscall
    DOK
    cmp rdx, 720
    jne neu_device_bad
    mov [rbp + 112], rdx
    mov rdi, [rbp + 88]
    DZERO
    mov esi, 2
    mov eax, 0x21
    syscall
    DOK
    mov rdi, [rbp + 64]       # closing the claiming anchor releases its lease
    mov eax, 0x11
    syscall
    DOK
    jmp neu_device_retired_wait
neu_device_cancel_wait:
    mov rdi, [rbp + 96]
    DZERO
    mov eax, 0x20
    syscall
.global neu_device_cancel_resume
neu_device_cancel_resume:
    DERR -46
    jmp neu_device_retired_wait
neu_device_budget_loop:
    mov rdi, [rbp + 72]
    DZERO
    mov edx, 4
    mov eax, 0x1e
    syscall
    cmp rax, -48
    je neu_device_budget_done
    DOK
    cmp edx, 0x010000ed
    jne neu_device_bad
    jmp neu_device_budget_loop
neu_device_budget_done:
    DERR -48
neu_device_retired_wait:
    inc qword ptr [rbp]
    cmp qword ptr [r15 + 152], 1
    jne neu_device_retired_wait
    mov rdi, [rbp + 72]
    DZERO
    mov edx, 4
    mov eax, 0x1e
    syscall
    DERR -17
    jmp neu_device_all_done_wait
neu_device_flight_fault_wait:
    cmp qword ptr [r15 + 160], 2
    jb neu_device_flight_fault_wait
neu_device_fault_wait:
    cmp qword ptr [r15 + 16], 1
    jne neu_device_fault_wait
    movabs rax, 0xffffb00000001000
neu_smp_sched_fault_site:
    mov byte ptr [rax], 1
    jmp neu_device_bad

neu_device_helper:
    cmp qword ptr [r15 + 88], 1
    jb neu_device_helper
    mov rdi, [r14 + 328]      # A2's MmioRegion handle in the same Node table
    DZERO
    mov esi, 0x60
    mov edx, 4
    mov r10d, 2
    mov eax, 0x1f
    syscall
    DOK
    mov qword ptr [rbp + 24], 2
    mov qword ptr [r15 + 16], 1
    cmp qword ptr [r15 + 96], 2
    je neu_device_helper_cancel
    cmp qword ptr [r15 + 96], 3
    jne neu_device_all_done_wait
neu_device_helper_cancel:
    cmp qword ptr [r15 + 88], 2
    jb neu_device_helper_cancel
    cmp qword ptr [r15 + 96], 3
    je neu_device_helper_revoke
    mov rdi, [r14 + 352]      # close the actual wait capability
    mov eax, 0x11
    syscall
    DOK
    jmp neu_device_all_done_wait
neu_device_helper_revoke:
    mov rdi, [r14 + 344]      # strict descendant revoke selects A2's wait grant
    DZERO
    mov eax, 0x1a
    syscall
    DOK
    cmp rdx, 1
    jne neu_device_bad
    mov [rbp + 112], rdx
    jmp neu_device_all_done_wait

neu_device_client:
    cmp qword ptr [r15 + 64], 1
    jne neu_device_client
    mov rdi, [rbp + 56]
    DZERO
    mov eax, 0x1c
    syscall
    DERR -40                 # distinct Node cannot claim an owned resource
    mov rdi, [r15 + 112]
    DZERO
    mov edx, 4
    mov eax, 0x1e
    syscall
    DERR -17                 # leaked scalar is not authority in B's table
    mov qword ptr [r15 + 72], 1
neu_device_reassign_wait:
    inc qword ptr [rbp]
    cmp qword ptr [r15 + 152], 1
    jne neu_device_reassign_wait
    mov rdi, [rbp + 56]
    DZERO
    mov eax, 0x1c
    syscall
    DOK
    mov [rbp + 72], rdx
    mov rdi, rdx
    DZERO
    mov eax, 0x1d
    syscall
    DOK
    mov [rbp + 88], rdx
neu_device_second_arm:
    cmp qword ptr [r15 + 128], 3
    jne neu_device_second_arm
    mov rdi, [rbp + 72]
    DZERO
    mov esi, 4
    mov edx, 4
    mov r10d, 0x12345678
    mov eax, 0x1f
    syscall
    DOK
    mov rdi, [rbp + 72]
    DZERO
    mov esi, 4
    mov edx, 4
    mov eax, 0x1e
    syscall
    DOK
    cmp edx, 0xedcba987
    jne neu_device_bad
    mov [rbp + 120], rdx
    mov rdi, [rbp + 72]
    DZERO
    mov esi, 0x60
    mov edx, 4
    mov r10d, 2
    mov eax, 0x1f
    syscall
    DOK
    mov rdi, [rbp + 88]
    DZERO
    mov eax, 0x20
    syscall
    DOK
    mov [rbp + 24], rdx
    mov rsi, rdx
    mov rdi, [rbp + 88]
    xor edx, edx
    xor r10d, r10d
    xor r8d, r8d
    xor r9d, r9d
    mov eax, 0x21
    syscall
    DOK
neu_device_successor_factorial_arm:
    cmp qword ptr [r15 + 128], 3
    jne neu_device_successor_factorial_arm
    mov rdi, [rbp + 72]
    DZERO
    mov esi, 0x20
    mov edx, 4
    mov r10d, 0x80
    mov eax, 0x1f
    syscall
    DOK
    mov rdi, [rbp + 72]
    DZERO
    mov esi, 8
    mov edx, 4
    mov r10d, 5
    mov eax, 0x1f
    syscall
    DOK
    mov rdi, [rbp + 88]
    DZERO
    mov eax, 0x20
    syscall
    DOK
    mov rbx, [rbp + 24]
    inc rbx
    cmp rdx, rbx
    jne neu_device_bad
    mov [rbp + 24], rdx
    mov rdi, [rbp + 72]
    DZERO
    mov esi, 8
    mov edx, 4
    mov eax, 0x1e
    syscall
    DOK
    cmp rdx, 120
    jne neu_device_bad
    mov [rbp + 112], rdx
    mov rdi, [rbp + 88]
    DZERO
    mov rsi, [rbp + 24]
    mov eax, 0x21
    syscall
    DOK
    mov rdi, [rbp + 72]
    mov eax, 0x11
    syscall
    DOK
neu_device_second_release:
    cmp qword ptr [r15 + 128], 1
    jne neu_device_second_release
    mov qword ptr [r15 + 144], 1
neu_device_all_done_wait:
    inc qword ptr [rbp]
    cmp qword ptr [r15 + 144], 1
    jne neu_device_all_done_wait
    mov rax, -256
    syscall
neu_smp_sched_yield_resume:
    DOK
    mov [rbp + 32], r12
    mov eax, 2
    mov edi, 128
    syscall
neu_smp_sched_done_resume:
    ud2
neu_device_bad:
    mov eax, 2
    mov edi, 255
    syscall
    ud2
neu_smp_sched_payload_end:
