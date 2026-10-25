.section .text.interrupts,"ax"
.macro IRQ number
.global neu_irq_\number
neu_irq_\number:
    push 0
    push \number
    jmp neu_irq_common
.endm
IRQ 32
IRQ 33
IRQ 34
IRQ 35
IRQ 36
IRQ 37
IRQ 38
IRQ 39
IRQ 40
IRQ 41
IRQ 42
IRQ 43
IRQ 44
IRQ 45
IRQ 46
IRQ 47
IRQ 48
IRQ 49
IRQ 50
IRQ 51
IRQ 52
IRQ 53
IRQ 54
IRQ 55
IRQ 56
IRQ 57
IRQ 58
IRQ 59
IRQ 60
IRQ 61
IRQ 62
IRQ 63
IRQ 64
IRQ 65
IRQ 66
IRQ 67
IRQ 68
IRQ 69
IRQ 70
IRQ 71
IRQ 72
IRQ 73
IRQ 74
IRQ 75
IRQ 76
IRQ 77
IRQ 78
IRQ 79
IRQ 80
IRQ 81
IRQ 82
IRQ 83
IRQ 84
IRQ 85
IRQ 86
IRQ 87
IRQ 88
IRQ 89
IRQ 90
IRQ 91
IRQ 92
IRQ 93
IRQ 94
IRQ 95
IRQ 96
IRQ 97
IRQ 98
IRQ 99
IRQ 100
IRQ 101
IRQ 102
IRQ 103
IRQ 104
IRQ 105
IRQ 106
IRQ 107
IRQ 108
IRQ 109
IRQ 110
IRQ 111
IRQ 112
IRQ 113
IRQ 114
IRQ 115
IRQ 116
IRQ 117
IRQ 118
IRQ 119
IRQ 120
IRQ 121
IRQ 122
IRQ 123
IRQ 124
IRQ 125
IRQ 126
IRQ 127
IRQ 128
IRQ 129
IRQ 130
IRQ 131
IRQ 132
IRQ 133
IRQ 134
IRQ 135
IRQ 136
IRQ 137
IRQ 138
IRQ 139
IRQ 140
IRQ 141
IRQ 142
IRQ 143
IRQ 144
IRQ 145
IRQ 146
IRQ 147
IRQ 148
IRQ 149
IRQ 150
IRQ 151
IRQ 152
IRQ 153
IRQ 154
IRQ 155
IRQ 156
IRQ 157
IRQ 158
IRQ 159
IRQ 160
IRQ 161
IRQ 162
IRQ 163
IRQ 164
IRQ 165
IRQ 166
IRQ 167
IRQ 168
IRQ 169
IRQ 170
IRQ 171
IRQ 172
IRQ 173
IRQ 174
IRQ 175
IRQ 176
IRQ 177
IRQ 178
IRQ 179
IRQ 180
IRQ 181
IRQ 182
IRQ 183
IRQ 184
IRQ 185
IRQ 186
IRQ 187
IRQ 188
IRQ 189
IRQ 190
IRQ 191
IRQ 192
IRQ 193
IRQ 194
IRQ 195
IRQ 196
IRQ 197
IRQ 198
IRQ 199
IRQ 200
IRQ 201
IRQ 202
IRQ 203
IRQ 204
IRQ 205
IRQ 206
IRQ 207
IRQ 208
IRQ 209
IRQ 210
IRQ 211
IRQ 212
IRQ 213
IRQ 214
IRQ 215
IRQ 216
IRQ 217
IRQ 218
IRQ 219
IRQ 220
IRQ 221
IRQ 222
IRQ 223
IRQ 224
IRQ 225
IRQ 226
IRQ 227
IRQ 228
IRQ 229
IRQ 230
IRQ 231
IRQ 232
IRQ 233
IRQ 234
IRQ 235
IRQ 236
IRQ 237
IRQ 238
IRQ 239
IRQ 240
IRQ 241
IRQ 242
IRQ 243
IRQ 244
IRQ 245
IRQ 246
IRQ 247
IRQ 248
IRQ 249
IRQ 250
IRQ 251
IRQ 252
IRQ 253
IRQ 254
IRQ 255
neu_irq_common:
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
    mov r12, rsp
.if {preemptive}
    test qword ptr [rsp + 144], 3
    jz 8f
    # One QMP hold per published dispatch. No-peer IRQ returns need not repeat
    # an already recorded observation delay on every subsequent clockevent.
    cmp qword ptr [rip + neu_preempt_irq_observed], 0
    jne 8f
    mov qword ptr [rip + neu_preempt_irq_observed], 1
    # Observe real hardware frame and ISR before EOI; never change saved GPRs.
    mov ecx, 0x04000000
.global neu_preempt_irq_wait, neu_preempt_irq_wait_end
neu_preempt_irq_wait:
    dec ecx
    jne neu_preempt_irq_wait
neu_preempt_irq_wait_end:
8:
.endif
    mov rdi, rsp
    and rsp, -16
    call neu_interrupt
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

# This is the only supported IF=1 Ring 0 region; Phase 4C also admits Ring 3.
# The PIT reference plus an independent instruction budget bounds missing IRQs.
.global neu_timer_wait, neu_timer_wait_begin, neu_timer_wait_end
neu_timer_wait:
    push rbx
    push rbp
    push r12
    push r13
    push r14
    push r15
    sub rsp, 8
    mov [rip + neu_timer_wait_rsp], rsp
    mov qword ptr [rip + neu_timer_budget], 10000000
    mov byte ptr [rip + neu_timer_deadline], 0
    movabs rax, 0x1122334400000001
    movabs rbx, 0x1122334400000002
    movabs rcx, 0x1122334400000003
    movabs rdx, 0x1122334400000004
    movabs rbp, 0x1122334400000005
    movabs rsi, 0x1122334400000006
    movabs rdi, 0x1122334400000007
    movabs r8, 0x1122334400000008
    movabs r9, 0x1122334400000009
    movabs r10, 0x112233440000000a
    movabs r11, 0x112233440000000b
    movabs r12, 0x112233440000000c
    movabs r13, 0x112233440000000d
    movabs r14, 0x112233440000000e
    movabs r15, 0x112233440000000f
    sti
neu_timer_wait_begin:
    push rax
    push rdx
    mov dx, 0x61
    in al, dx
    test al, 0x20
    pop rdx
    pop rax
    jnz 1f
    dec qword ptr [rip + neu_timer_budget]
    jnz neu_timer_wait_begin
    cli
    jmp 2f
1:
    cli
    mov byte ptr [rip + neu_timer_deadline], 1
2:
neu_timer_wait_end:
    cmp rax, [rip + neu_timer_canaries + 0]
    jne 3f
    cmp rbx, [rip + neu_timer_canaries + 8]
    jne 3f
    cmp rcx, [rip + neu_timer_canaries + 16]
    jne 3f
    cmp rdx, [rip + neu_timer_canaries + 24]
    jne 3f
    cmp rbp, [rip + neu_timer_canaries + 32]
    jne 3f
    cmp rsi, [rip + neu_timer_canaries + 40]
    jne 3f
    cmp rdi, [rip + neu_timer_canaries + 48]
    jne 3f
    cmp r8, [rip + neu_timer_canaries + 56]
    jne 3f
    cmp r9, [rip + neu_timer_canaries + 64]
    jne 3f
    cmp r10, [rip + neu_timer_canaries + 72]
    jne 3f
    cmp r11, [rip + neu_timer_canaries + 80]
    jne 3f
    cmp r12, [rip + neu_timer_canaries + 88]
    jne 3f
    cmp r13, [rip + neu_timer_canaries + 96]
    jne 3f
    cmp r14, [rip + neu_timer_canaries + 104]
    jne 3f
    cmp r15, [rip + neu_timer_canaries + 112]
    jne 3f
    cmp rsp, [rip + neu_timer_wait_rsp]
    jne 3f
    cmp byte ptr [rip + neu_timer_deadline], 1
    jne 3f
    xor eax, eax
    jmp 4f
3:
    mov eax, 1
4:
    cld
    add rsp, 8
    pop r15
    pop r14
    pop r13
    pop r12
    pop rbp
    pop rbx
    ret
.section .rodata.interrupts,"a"
.global neu_irq_table, neu_timer_canaries
neu_irq_table:
.quad neu_irq_32
.quad neu_irq_33
.quad neu_irq_34
.quad neu_irq_35
.quad neu_irq_36
.quad neu_irq_37
.quad neu_irq_38
.quad neu_irq_39
.quad neu_irq_40
.quad neu_irq_41
.quad neu_irq_42
.quad neu_irq_43
.quad neu_irq_44
.quad neu_irq_45
.quad neu_irq_46
.quad neu_irq_47
.quad neu_irq_48
.quad neu_irq_49
.quad neu_irq_50
.quad neu_irq_51
.quad neu_irq_52
.quad neu_irq_53
.quad neu_irq_54
.quad neu_irq_55
.quad neu_irq_56
.quad neu_irq_57
.quad neu_irq_58
.quad neu_irq_59
.quad neu_irq_60
.quad neu_irq_61
.quad neu_irq_62
.quad neu_irq_63
.quad neu_irq_64
.quad neu_irq_65
.quad neu_irq_66
.quad neu_irq_67
.quad neu_irq_68
.quad neu_irq_69
.quad neu_irq_70
.quad neu_irq_71
.quad neu_irq_72
.quad neu_irq_73
.quad neu_irq_74
.quad neu_irq_75
.quad neu_irq_76
.quad neu_irq_77
.quad neu_irq_78
.quad neu_irq_79
.quad neu_irq_80
.quad neu_irq_81
.quad neu_irq_82
.quad neu_irq_83
.quad neu_irq_84
.quad neu_irq_85
.quad neu_irq_86
.quad neu_irq_87
.quad neu_irq_88
.quad neu_irq_89
.quad neu_irq_90
.quad neu_irq_91
.quad neu_irq_92
.quad neu_irq_93
.quad neu_irq_94
.quad neu_irq_95
.quad neu_irq_96
.quad neu_irq_97
.quad neu_irq_98
.quad neu_irq_99
.quad neu_irq_100
.quad neu_irq_101
.quad neu_irq_102
.quad neu_irq_103
.quad neu_irq_104
.quad neu_irq_105
.quad neu_irq_106
.quad neu_irq_107
.quad neu_irq_108
.quad neu_irq_109
.quad neu_irq_110
.quad neu_irq_111
.quad neu_irq_112
.quad neu_irq_113
.quad neu_irq_114
.quad neu_irq_115
.quad neu_irq_116
.quad neu_irq_117
.quad neu_irq_118
.quad neu_irq_119
.quad neu_irq_120
.quad neu_irq_121
.quad neu_irq_122
.quad neu_irq_123
.quad neu_irq_124
.quad neu_irq_125
.quad neu_irq_126
.quad neu_irq_127
.quad neu_irq_128
.quad neu_irq_129
.quad neu_irq_130
.quad neu_irq_131
.quad neu_irq_132
.quad neu_irq_133
.quad neu_irq_134
.quad neu_irq_135
.quad neu_irq_136
.quad neu_irq_137
.quad neu_irq_138
.quad neu_irq_139
.quad neu_irq_140
.quad neu_irq_141
.quad neu_irq_142
.quad neu_irq_143
.quad neu_irq_144
.quad neu_irq_145
.quad neu_irq_146
.quad neu_irq_147
.quad neu_irq_148
.quad neu_irq_149
.quad neu_irq_150
.quad neu_irq_151
.quad neu_irq_152
.quad neu_irq_153
.quad neu_irq_154
.quad neu_irq_155
.quad neu_irq_156
.quad neu_irq_157
.quad neu_irq_158
.quad neu_irq_159
.quad neu_irq_160
.quad neu_irq_161
.quad neu_irq_162
.quad neu_irq_163
.quad neu_irq_164
.quad neu_irq_165
.quad neu_irq_166
.quad neu_irq_167
.quad neu_irq_168
.quad neu_irq_169
.quad neu_irq_170
.quad neu_irq_171
.quad neu_irq_172
.quad neu_irq_173
.quad neu_irq_174
.quad neu_irq_175
.quad neu_irq_176
.quad neu_irq_177
.quad neu_irq_178
.quad neu_irq_179
.quad neu_irq_180
.quad neu_irq_181
.quad neu_irq_182
.quad neu_irq_183
.quad neu_irq_184
.quad neu_irq_185
.quad neu_irq_186
.quad neu_irq_187
.quad neu_irq_188
.quad neu_irq_189
.quad neu_irq_190
.quad neu_irq_191
.quad neu_irq_192
.quad neu_irq_193
.quad neu_irq_194
.quad neu_irq_195
.quad neu_irq_196
.quad neu_irq_197
.quad neu_irq_198
.quad neu_irq_199
.quad neu_irq_200
.quad neu_irq_201
.quad neu_irq_202
.quad neu_irq_203
.quad neu_irq_204
.quad neu_irq_205
.quad neu_irq_206
.quad neu_irq_207
.quad neu_irq_208
.quad neu_irq_209
.quad neu_irq_210
.quad neu_irq_211
.quad neu_irq_212
.quad neu_irq_213
.quad neu_irq_214
.quad neu_irq_215
.quad neu_irq_216
.quad neu_irq_217
.quad neu_irq_218
.quad neu_irq_219
.quad neu_irq_220
.quad neu_irq_221
.quad neu_irq_222
.quad neu_irq_223
.quad neu_irq_224
.quad neu_irq_225
.quad neu_irq_226
.quad neu_irq_227
.quad neu_irq_228
.quad neu_irq_229
.quad neu_irq_230
.quad neu_irq_231
.quad neu_irq_232
.quad neu_irq_233
.quad neu_irq_234
.quad neu_irq_235
.quad neu_irq_236
.quad neu_irq_237
.quad neu_irq_238
.quad neu_irq_239
.quad neu_irq_240
.quad neu_irq_241
.quad neu_irq_242
.quad neu_irq_243
.quad neu_irq_244
.quad neu_irq_245
.quad neu_irq_246
.quad neu_irq_247
.quad neu_irq_248
.quad neu_irq_249
.quad neu_irq_250
.quad neu_irq_251
.quad neu_irq_252
.quad neu_irq_253
.quad neu_irq_254
.quad neu_irq_255
neu_timer_canaries:
.quad 0x1122334400000001
.quad 0x1122334400000002
.quad 0x1122334400000003
.quad 0x1122334400000004
.quad 0x1122334400000005
.quad 0x1122334400000006
.quad 0x1122334400000007
.quad 0x1122334400000008
.quad 0x1122334400000009
.quad 0x112233440000000a
.quad 0x112233440000000b
.quad 0x112233440000000c
.quad 0x112233440000000d
.quad 0x112233440000000e
.quad 0x112233440000000f
.section .bss.interrupts,"aw",@nobits
.balign 8
.global neu_timer_wait_rsp
neu_timer_wait_rsp: .skip 8
neu_timer_budget: .skip 8
neu_timer_deadline: .skip 1

.global neu_timer_canary_rax
.set neu_timer_canary_rax, 0x1122334400000001
.global neu_timer_canary_rbx
.set neu_timer_canary_rbx, 0x1122334400000002
.global neu_timer_canary_rcx
.set neu_timer_canary_rcx, 0x1122334400000003
.global neu_timer_canary_rdx
.set neu_timer_canary_rdx, 0x1122334400000004
.global neu_timer_canary_rbp
.set neu_timer_canary_rbp, 0x1122334400000005
.global neu_timer_canary_rsi
.set neu_timer_canary_rsi, 0x1122334400000006
.global neu_timer_canary_rdi
.set neu_timer_canary_rdi, 0x1122334400000007
.global neu_timer_canary_r8
.set neu_timer_canary_r8, 0x1122334400000008
.global neu_timer_canary_r9
.set neu_timer_canary_r9, 0x1122334400000009
.global neu_timer_canary_r10
.set neu_timer_canary_r10, 0x112233440000000a
.global neu_timer_canary_r11
.set neu_timer_canary_r11, 0x112233440000000b
.global neu_timer_canary_r12
.set neu_timer_canary_r12, 0x112233440000000c
.global neu_timer_canary_r13
.set neu_timer_canary_r13, 0x112233440000000d
.global neu_timer_canary_r14
.set neu_timer_canary_r14, 0x112233440000000e
.global neu_timer_canary_r15
.set neu_timer_canary_r15, 0x112233440000000f
