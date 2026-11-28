// Copyright (C) 2026 dekajoule
// SPDX-License-Identifier: GPL-2.0-or-later

//! Storage: the crate oos-store (`ui/store/`, ADR 0064), setup files and waveform files, brought in here so that
//! callers say `store::<item>` as before. What its readers ask of oos-ui (the registries of the measurements and the
//! decoders, the record of the screen) is in `modules.rs`, which passes it in: `store::set`,
//! `store::parse_setup_lenient` and `store::parse_ff` there take the place of the crate's. PNG screenshots and PPM
//! pictures come from `gfx::png` (base), re-exported here too. The tests that run the readers with oos-ui's
//! registries, the screen or the SCPI interpreter are here; those of the importers of other instruments' files are the
//! crate's.

#[cfg(test)]
pub(crate) mod hostile_tests;
mod modules;
#[cfg(test)]
mod settings_snapshot;
#[cfg(test)]
mod tests;
// the picture files (PNG, PPM) are base (gfx::png); a setup or a screenshot saved by store writes them
pub use crate::gfx::png::*;
// in place of the readers of oos-store that take what modules.rs passes in
pub use modules::{parse_ff, parse_setup_lenient, set};
#[cfg(test)]
pub use modules::{parse_setup, parse_wave_bin};
pub use oos_store::*;

/// `write` for the files a slot keeps on the card or a stick (setups, masks): a temporary file, fsync, rename, the
/// directory synced (`oos_hal::fsutil::write_atomic`), so a power cut leaves the old file or the new one, never a part.
/// A directory that cannot be synced is an error: the caller says the file is saved only when it is.
pub fn write_durable(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    check_target(path)?;
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let opts = oos_hal::fsutil::Opts { dir_sync: oos_hal::fsutil::DirSync::Required, ..Default::default() };
    oos_hal::fsutil::write_atomic(path, data, &opts)
}

/// The settings' copy on OOS's data partition (`card`, None without one) for the save every 2 s: replaced in one step
/// with the directory synced, as `write_durable`. Whether the text counts as saved: not after a failed write, so the
/// next pass tries again instead of leaving older settings for the next start.
pub fn setup_on_card(card: Option<&std::path::Path>, text: &str) -> bool {
    let opts = oos_hal::fsutil::Opts { dir_sync: oos_hal::fsutil::DirSync::Required, ..Default::default() };
    match card {
        Some(p) => oos_hal::fsutil::write_atomic(p, text.as_bytes(), &opts).is_ok(),
        None => true,
    }
}
