/// Entirely synthetic COM: mode 13h, changing fills, one AdLib tone, DOS exit.
pub fn program() -> Vec<u8> {
    let mut code = vec![
        0xb8, 0x13, 0x00, 0xcd, 0x10, // mov ax,13h; int 10h
        0xb8, 0x00, 0xa0, 0x8e, 0xc0, // mov ax,a000h; mov es,ax
        0xb4, 0x00, 0xcd, 0x1a, 0x89, 0xd5, // BIOS ticks; mov bp,dx
    ];
    for (register, value) in [
        (0x20, 0x21),
        (0x23, 0x21), // multiplier 1, sustain enabled
        (0x40, 0x3f),
        (0x43, 0x00), // quiet modulator, loud carrier
        (0x60, 0xf0),
        (0x63, 0xf0), // fast attack
        (0x80, 0x0f),
        (0x83, 0x0f), // fast release
        (0xa0, 0x98),
        (0xb0, 0x11), // frequency, key off
    ] {
        opl(&mut code, register, value);
    }
    let again = code.len();
    code.extend([
        0xb4, 0x00, 0xcd, 0x1a, // mov ah,0; int 1ah (ticks)
        0x29, 0xea, // sub dx,bp
        0x83, 0xfa, 0x37, // cmp dx,55 (about 3.02 seconds)
        0x72, 0x05, // jb continue
        0xb8, 0x00, 0x4c, 0xcd, 0x21, // mov ax,4c00h; int 21h
        0x89, 0xd3, // mov bx,dx
        0x89, 0xd0, 0xfe, 0xc0, // mov ax,dx; inc al (changing colour)
        0x31, 0xff, 0xb9, 0x00, 0xfa, 0xfc, 0xf3,
        0xaa, // xor di,di; mov cx,64000; cld; rep stosb
        0x83, 0xfb, 0x09, // cmp bx,9 (~0.5 s)
        0x72, 0x0a, // jb off
        0x83, 0xfb, 0x24, // cmp bx,36 (~2 s)
        0x73, 0x05, // jae off
        0xb0, 0x31, // mov al,31h (key on)
        0xeb, 0x03, // jmp write
        0x90, // alignment nop
        0xb0, 0x11, // off: mov al,11h
        0x50, // write: push ax
        0xba, 0x88, 0x03, 0xb0, 0xb0, 0xee, // mov dx,388h; mov al,b0h; out dx,al
        0x42, 0x58, 0xee, // inc dx; pop ax; out dx,al
    ]);
    // jmp near again
    let displacement = (again as isize - (code.len() + 3) as isize) as i16;
    code.push(0xe9);
    code.extend(displacement.to_le_bytes());
    code
}
fn opl(code: &mut Vec<u8>, register: u8, value: u8) {
    code.extend([
        0xba, 0x88, 0x03, 0xb0, register, 0xee, // mov dx,388h; mov al,reg; out dx,al
        0x42, 0xb0, value, 0xee, // inc dx; mov al,value; out dx,al
    ]);
}
