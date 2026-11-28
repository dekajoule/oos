// Copyright (C) 2026 dekajoule
// SPDX-License-Identifier: GPL-2.0-or-later

//! The Rigol MHO900 (MHO934, MHO954, MHO984, MHO98), a port of the platform alone (docs/mho.md): the same RK3399 main
//! board family as the DHO800/DHO900 with another acquisition board (a Kintex-7 the SoC loads at every start, two ADCs
//! behind JESD204, other front ends). OOS does not drive that board: `target::MHO` says so (`acquisition: false`),
//! and nothing here touches it. `acq` describes the board (registers, read-out format) for the bring-up to come.

pub mod acq;
pub mod afg;
/// The register window and the read-out stream: the same XDMA devices and access as on the DHO800/DHO900
/// (`acq::BAR_DEVICE`, `acq::C2H_DEVICE`), mapped to `acq::WINDOW_USED`.
pub mod bus {
    #[cfg(target_os = "linux")]
    pub use crate::dho::bus::Bar1;
    pub use crate::dho::bus::{read_dma, Bus, DmaBuf};
}
/// The front ends' bus: the bit-banged bus of the DHO800/DHO900 on the same pins, with the stock timing
/// (protocols/mho-acquisition.md section 8); the MHO900's channel n is on chip select 4 - n.
pub mod afe_bus {
    pub use crate::dho::afe::AfeBus;
    #[cfg(target_os = "linux")]
    pub use crate::dho::afe::GpioAfe;
    /// the select lines are GPIO lines of their own, set by name
    #[cfg(target_os = "linux")]
    pub use crate::dho::gpio::Line;
}
/// The record of several channels: the DHO800/DHO900's interleave (`hz::ratio`, `seq::deinterleave`), **assumed** the
/// same on the MHO900.
pub mod record {
    pub use crate::dho::hz::ratio;
    pub use crate::dho::scu::Window;
    pub use crate::dho::seq::deinterleave;
}
/// The trigger: the TPU of the DHO800/DHO900 at the same addresses (protocols/mho-acquisition.md section 2). **Assumed**
/// from the unit's recordings: the MHO900's stock 0x201c is the DHO's word for an edge on CH1 (0x00aa4100) and its
/// level words have the DHO's layout around 0x8000 (section 11).
pub mod tpu {
    pub use crate::dho::tpu::{edge, noise_hysteresis, Shadows, Slope};
}
pub mod board;
pub mod bringup;
pub mod counter;
pub mod la;
pub mod recipe;
pub mod settings;
pub mod snapshot;
pub mod sweep;
pub mod target;
pub mod vertical;
