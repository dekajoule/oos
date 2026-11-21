// Copyright (C) 2026 dekajoule
// SPDX-License-Identifier: GPL-2.0-or-later

//! OOS hardware layer.
//!
//! The crate root holds what does not depend on one board: the model and its limits (`model`), the vendor file keys
//! (`vendorkeys`), signed updates (`update`), WLAN (`wlan`) and the start-up status file (`bootstatus`). Everything of
//! the DHO800/DHO900 board (RK3399 + Zynq FPGA over PCIe/XDMA) is in `dho`; a port to another board family adds a
//! module next to it (docs/porting.md).
//!
//! The register sequences are described in `docs/protocols/`. No code of the vendor libraries is used. Functions that
//! build register sequences are pure (they return `(offset, value)` lists and update software shadows of the
//! write-only registers), so they are unit-tested on the host; `dho::bus` applies them to the hardware.

pub mod bluez;
pub mod bootstatus;
pub mod dbus;
pub mod fsutil;
#[cfg(test)]
mod hostile;
pub mod model;
pub mod procs;
pub mod target;
pub mod update;
pub mod vendorkeys;
pub mod wlan;

pub mod dho;

/// The `N` bytes of `b` from offset `o` on, for `from_le_bytes`. Panics past the end of `b` like indexing does: for
/// records whose length was checked when they were read in.
pub(crate) fn bytes_at<const N: usize>(b: &[u8], o: usize) -> [u8; N] {
    std::array::from_fn(|k| b[o + k])
}

/// CRC-32 tables for eight bytes per step ("slicing by 8"): `T[0]` is the usual byte table, `T[k][n]` the CRC of byte
/// `n` followed by `k` zero bytes.
const CRC_T: [[u32; 256]; 8] = {
    let mut t = [[0u32; 256]; 8];
    let mut n = 0;
    while n < 256 {
        let mut c = n as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        t[0][n] = c;
        n += 1;
    }
    let mut k = 1;
    while k < 8 {
        let mut n = 0;
        while n < 256 {
            let c = t[k - 1][n];
            t[k][n] = t[0][(c & 0xff) as usize] ^ (c >> 8);
            n += 1;
        }
        k += 1;
    }
    t
};

/// zlib / IEEE 802.3 / PNG CRC32, eight bytes per step.
pub fn crc32(data: &[u8]) -> u32 {
    let t = &CRC_T;
    let mut c = !0u32;
    let (blocks, rest) = data.as_chunks::<8>();
    for b in blocks {
        let lo = c ^ u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        c = t[7][(lo & 0xff) as usize]
            ^ t[6][(lo >> 8 & 0xff) as usize]
            ^ t[5][(lo >> 16 & 0xff) as usize]
            ^ t[4][(lo >> 24) as usize]
            ^ t[3][b[4] as usize]
            ^ t[2][b[5] as usize]
            ^ t[1][b[6] as usize]
            ^ t[0][b[7] as usize];
    }
    !rest.iter().fold(c, |c, &b| t[0][((c ^ b as u32) & 0xff) as usize] ^ (c >> 8))
}
