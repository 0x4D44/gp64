; Hayes/UART acceptance: role and COM base are supplied by the Makefile.
bits 16
org 100h
%ifndef BASE
%error BASE is required
%endif
%ifndef CALLER
%error CALLER is required
%endif
%ifndef AUTO
%define AUTO 0
%endif
%ifndef RX_START
%define RX_START 0
%endif
%ifndef HANG_DTR
%define HANG_DTR 1
%endif
start:
    cli
    cld
    mov dx, BASE+3
    mov al, 80h                 ; DLAB
    out dx, al
    mov dx, BASE
    mov al, 12                  ; 9600 baud DTE
    out dx, al
    inc dx
    xor al, al
    out dx, al
    mov dx, BASE+3
    mov al, 3                   ; 8N1
    out dx, al
    mov dx, BASE+2
    mov al, 7                   ; enable and clear UART FIFOs
    out dx, al
    mov dx, BASE+4
    mov al, 3                   ; DTR/RTS, polling without UART IRQs
    out dx, al
    sti
    mov byte [stage], 1
    mov si, init
    call send_string
    mov si, ok
    call response
    mov byte [stage], 2
%if CALLER
    mov si, dial
    call send_string
%else
%if !AUTO
    mov si, ring
    call response
    mov byte [stage], 3
    mov si, answer
    call send_string
%endif
%endif
    mov si, connect
    call response
    mov dx, BASE+6
    in al, dx
    test al, 80h                ; CONNECT must accompany DCD
    jz fail
    mov byte [stage], 4
    call settle                 ; allow DSP descrambler convergence before payload
%if CALLER
    mov al, 0d3h
    call send_byte
    xor bx, bx
    call send_payload
    mov bl, 0c3h
    call sync_byte
    mov bl, 0a0h
    call receive_payload
%else
    mov bl, 0d3h
    call sync_byte
    mov bx, RX_START
    call receive_payload
    mov al, 0c3h
    call send_byte
    mov bl, 0a0h
    call send_payload
%endif
    mov byte [stage], 5
%if CALLER
%if HANG_DTR
    mov dx, BASE+4
    mov al, 2                   ; drop DTR while retaining RTS
    out dx, al
%else
    call guard
    mov si, escape
    call send_string
    mov si, ok
    call response               ; post-guard expires in the modem DSP
    mov si, hang
    call send_string
%endif
%endif
    mov si, no_carrier
    call response
    mov dx, BASE+6
    in al, dx
    test al, 80h
    jnz fail
    mov byte [stage], 6
    mov ax, 4c00h
    int 21h
payload_fail:
    mov [received], al
    mov [expected], bl
fail:
    mov ax, 4c01h
    int 21h
send_payload:
    mov cx, 32
.loop:
    mov al, bl
    call send_byte
    inc bl
    loop .loop
    ; Wait until UART shift register is empty before advancing the protocol.
    mov dx, BASE+5
.empty:
    in al, dx
    test al, 40h
    jz .empty
    ret
receive_payload:
    mov cx, 32
.loop:
    call receive_byte
    cmp al, bl
    jne payload_fail
    inc bl
    loop .loop
    ret
send_string:
    lodsb
    test al, al
    jz .done
    call send_byte
    jmp send_string
.done:
    ret
send_byte:
    push ax
    mov dx, BASE+5
.wait:
    in al, dx
    test al, 20h
    jz .wait
    mov dx, BASE+6
    in al, dx
    test al, 10h                ; CTS
    jz .wait
    pop ax
    mov dx, BASE
    out dx, al
    ret
receive_byte:
    mov dx, BASE+5
.wait:
    in al, dx
    test al, 1
    jz .wait
    mov dx, BASE
    in al, dx
    ret
response:
    mov di, si
.scan:
    call receive_byte
    cmp al, [si]
    jne .reset
    inc si
    cmp byte [si], 0
    jne .scan
    ret                         ; expected response includes final CR/LF
.reset:
    mov si, di
    jmp .scan
sync_byte:
    call receive_byte
    cmp al, bl
    jne sync_byte
    ret
settle:
    mov ax, 40h
    mov es, ax
    mov bx, [es:6ch]
.loop:
    mov dx, BASE+5
    in al, dx
    test al, 1
    jz .time
    mov dx, BASE
    in al, dx                   ; discard idle convergence bytes, never payload
.time:
    mov ax, [es:6ch]
    sub ax, bx
%if CALLER
    cmp ax, 36                  ; caller waits longer before initiating transfer
%else
    cmp ax, 18                  ; answerer is receiving before caller transmits
%endif
    jb .loop
    ret
guard:
    mov ax, 40h
    mov es, ax
    mov bx, [es:6ch]
.wait:
    mov ax, [es:6ch]
    sub ax, bx
    cmp ax, 6                   ; >100ms S12 guard, even at BIOS tick boundary
    jb .wait
    ret
init: db 'ATE0V1X0S12=5S0=', '0'+AUTO, 13, 0
dial: db 'ATD555', 13, 0
answer: db 'ATA', 13, 0
escape: db '+++', 0
hang: db 'ATH', 13, 0
ok: db 'OK', 13, 10, 0
ring: db 'RING', 13, 10, 0
connect: db 'CONNECT', 13, 10, 0
no_carrier: db 'NO CARRIER', 13, 10, 0
stage: db 0
received: db 0
expected: db 0
