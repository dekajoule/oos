// Copyright (C) 2026 dekajoule
// SPDX-License-Identifier: GPL-2.0-or-later

//! The frames of the front-panel MCU on its UART, `FA cmd data crc7 AF` (docs/protocols/frontpanel.md): the DHO800/DHO900
//! speaks them, and so does the MHO900 (keys, knobs and LED frames verified on an MHO934, docs/mho.md). Board
//! independent, with the LED frames both use; the power commands are the DHO's (`dho::panel`).

/// CRC-7/MMC over [cmd, data] (poly 0x09, init 0), 7-bit result.
pub fn crc7(cmd: u8, data: u8) -> u8 {
    let mut c: u8 = 0;
    for b in [cmd, data] {
        c ^= b;
        for _ in 0..8 {
            c = if c & 0x80 != 0 { (c << 1) ^ 0x12 } else { c << 1 };
        }
    }
    c >> 1
}

/// One MCU frame `FA cmd data crc7 AF`.
pub fn frame(cmd: u8, data: u8) -> [u8; 5] {
    [0xfa, cmd, data, crc7(cmd, data), 0xaf]
}

/// What `take` found at the front of a buffer: a whole frame (cmd, data), or a byte that starts none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Piece {
    Frame(u8, u8),
    Skipped(u8),
}

/// Takes the complete frames from the front of `b`; a byte that starts none is skipped. Fewer than five bytes stay for
/// the next read.
pub fn take(b: &mut Vec<u8>) -> Vec<Piece> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(f) = b.get(i..i + 5) {
        if f[0] == 0xfa && f[4] == 0xaf && f[3] == crc7(f[1], f[2]) {
            out.push(Piece::Frame(f[1], f[2]));
            i += 5;
        } else {
            out.push(Piece::Skipped(f[0]));
            i += 1;
        }
    }
    b.drain(..i);
    out
}

/// The key codes of `take`: the data byte of every frame (frontpanel.md).
pub fn keys(pieces: &[Piece]) -> Vec<u8> {
    pieces.iter().filter_map(|p| if let Piece::Frame(_, d) = p { Some(*d) } else { None }).collect()
}

/// Hardware LED bit of each panel LED index (from the LED id: bit = 8 * (id >> 8) + log2(id & 0xff) - 8).
/// G2 (index 10) has no valid id on the DHO900.
pub const LED_HW_BIT: [Option<u8>; 19] = [
    Some(10),
    Some(11),
    Some(12),
    Some(13), // CH1-CH4
    Some(16),
    Some(17),
    Some(15),
    Some(14), // RUN, STOP, SINGLE red, SINGLE orange
    Some(1),
    Some(2),
    None,
    Some(0), // LA, G1, G2, TOUCH
    Some(7),
    Some(5),
    Some(9),
    Some(8), // A up/down, B left/right, SLOPE1, SLOPE2
    Some(6),
    Some(3),
    Some(4), // A left/right, B up/down, INTENSITY
];

/// The three LED frames (cmd 1/2/3 = hardware LEDs 0-7, 8-15, 16-18) for a mask in panel LED order (see `Ui::leds`).
pub fn led_frames(mask: u32) -> [[u8; 5]; 3] {
    let mut hw = 0u32;
    for (i, b) in LED_HW_BIT.iter().enumerate() {
        if let (true, Some(b)) = (mask & (1 << i) != 0, b) {
            hw |= 1 << b;
        }
    }
    let mut out = [[0u8; 5]; 3];
    for (k, f) in out.iter_mut().enumerate() {
        let cmd = k as u8 + 1;
        let data = ((hw >> (8 * k)) & 0xff) as u8;
        *f = frame(cmd, data);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc7_matches_frontpanel_py() {
        // tools/frontpanel.py selftest
        assert_eq!(crc7(0x01, 0x00), 0x0b);
        assert_eq!(crc7(0x02, 0xff), 0x6f);
        assert_eq!(crc7(0x06, 0x15), 0x0e);
    }

    #[test]
    fn frames_are_taken_and_other_bytes_skipped() {
        let mut b = vec![0x00];
        b.extend(frame(0x01, 0x0c));
        b.extend([0xfa, 0x01, 0x0c, 0x00, 0xaf]); // wrong CRC
        b.extend(frame(0x02, 0x00));
        b.extend([0xfa, 0x01]);
        let p = take(&mut b);
        assert_eq!(p.first(), Some(&Piece::Skipped(0x00)));
        assert_eq!(p[1], Piece::Frame(0x01, 0x0c));
        assert_eq!(p.iter().filter(|x| matches!(x, Piece::Skipped(_))).count(), 6);
        assert_eq!(p.last(), Some(&Piece::Frame(0x02, 0x00)));
        assert_eq!(keys(&p), vec![0x0c, 0x00]);
        assert_eq!(b, vec![0xfa, 0x01]);
    }
}
