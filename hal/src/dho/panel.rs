// Copyright (C) 2026 dekajoule
// SPDX-License-Identifier: GPL-2.0-or-later

//! Front-panel MCU protocol (UART0, frames `FA cmd data crc7 AF`, see docs/protocols/frontpanel.md), platform independent
//! so the host tests cover it.

pub use crate::panelframe::{crc7, frame, keys, led_frames, take, Piece, LED_HW_BIT};

/// Key code of the power key, see docs/protocols/board.md section 4.
pub const KEY_POWER: u8 = 0x1b;

/// Power commands (docs/protocols/board.md section 4).
/// "App UI loaded", sent once when the UI is up.
pub const UI_LOADED: [u8; 5] = [0xfa, 0x05, 0x00, 0x27, 0xaf];
/// Power off (stock sends it twice back to back).
pub const POWER_OFF: [u8; 5] = [0xfa, 0x05, 0x01, 0x2e, 0xaf];
/// Cancel the MCU's power-down; the stock system sends it on every power-key press.
pub const CANCEL_POWER_DOWN: [u8; 5] = [0xfa, 0x05, 0x02, 0x35, 0xaf];
/// Restart.
pub const RESTART: [u8; 5] = [0xfa, 0x05, 0x03, 0x3c, 0xaf];
/// Ask for the MCU firmware version; the reply's data byte >> 4 is the major version.
pub const KEYBOARD_VERSION: [u8; 5] = [0xfa, 0x06, 0xbb, 0x03, 0xaf];

/// Power-on mode: data 5 when `flag` is set, else 4. Which one
/// means "power on when mains is applied" is not tested yet.
pub fn power_on_mode(flag: bool) -> [u8; 5] {
    frame(0xff, if flag { 5 } else { 4 })
}

/// Power-LED flash time: data = seconds * 10, 0..25.5 s. Effect not tested.
pub fn flash_time(seconds: f32) -> [u8; 5] {
    frame(0x04, (seconds * 10.0).round().clamp(0.0, 255.0) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn power_frames_match_board_md() {
        // frames as listed in board.md section 4
        for f in [UI_LOADED, POWER_OFF, CANCEL_POWER_DOWN, RESTART, KEYBOARD_VERSION] {
            assert_eq!(frame(f[1], f[2]), f);
        }
        assert_eq!(power_on_mode(true), [0xfa, 0xff, 0x05, 0x31, 0xaf]);
        assert_eq!(power_on_mode(false), [0xfa, 0xff, 0x04, 0x38, 0xaf]);
        assert_eq!(flash_time(1.5)[2], 15);
        assert_eq!(flash_time(99.0)[2], 255);
    }

    #[test]
    fn leds_map_to_hardware_bits() {
        // CH1-CH4 + RUN (LEDs 0-4) -> hardware bits 10-13 and 16, confirmed by eye on 2026-11-03
        let f = led_frames(0b1_1111);
        assert_eq!(f[0][2], 0x00);
        assert_eq!(f[1][2], 0x3c);
        assert_eq!(f[2][2], 0x01);
        // INTENSITY (LED 18) -> hardware bit 4, TOUCH (11) -> bit 0
        assert_eq!(led_frames(1 << 18)[0][2], 0x10);
        assert_eq!(led_frames(1 << 11)[0][2], 0x01);
        // G2 (10) has no LED on the DHO900
        assert_eq!(led_frames(1 << 10), led_frames(0));
    }
}
