// Copyright (C) 2026 dekajoule
// SPDX-License-Identifier: GPL-2.0-or-later

//! The commands of oos-fpgad for the MHO900 (docs/mho.md, ADR 0028). Nothing starts this board's hardware by itself;
//! the two commands below are run by hand by owners who test them.
//!
//!   oos-fpgad bringup --hw-code N [--until power|clock|fpga|pcie|adc] [--yes]
//!       the first bring-up step, started by hand by an owner who tests it (docs/mho.md, "Bring-up, step 1"): the
//!       supplies, the sample clock from the unit's own table, the FPGA from the unit's own image (volatile: lost at
//!       power-off), PCIe, and a read of the first registers. Not yet verified on hardware.
//!
//!   oos-fpgad mho-start [--snapshot FILE] [--init FILE] [--hw-code N] [--n POINTS] [--no-bringup] [--dry] [--yes]
//!       the first acquisition, by hand (docs/mho.md, "First acquisition"): the bring-up, then the ADCs, the JESD204
//!       receiver, the FPGA's configuration and one front end set from the unit's own snapshot of the stock system
//!       (`tools/mho-check.py --snapshot`, default /oos/mho-snapshot.txt), the front ends' set-up of the stock
//!       program's start before it (`--afe-init`, default /oos/mho-afe-init.txt, if there), all read back, then one
//!       acquisition of the
//!       probe compensation signal with the read-out of the DHO800/DHO900. Volatile. Not yet verified on hardware.
//!
//! Every other command but `model` (answered by the neutral part) and `help` is refused.

#[cfg(target_os = "linux")]
mod bringup;
#[cfg(target_os = "linux")]
mod start;

/// Runs `cmd`: `help` prints what this build does; `bringup` the step above; every other command is refused with exit
/// code 3.
pub(crate) fn run(cmd: &str, args: &[String]) {
    let family = oos_hal::target::ACTIVE.family_name;
    match cmd {
        "help" | "--help" | "-h" => {
            println!(
                "oos-fpgad for the {}: `model` prints the unit; `bringup --hw-code N [--until power|clock|fpga|pcie|adc] \
                 [--yes]` is the first bring-up step, `mho-start --snapshot FILE [--dry] [--yes]` the first acquisition from \
                 the unit's snapshot, both by hand only (docs/mho.md). Nothing else drives {} hardware.",
                family, family
            );
        }
        "bringup" => {
            #[cfg(target_os = "linux")]
            std::process::exit(bringup::run(args));
            #[cfg(not(target_os = "linux"))]
            {
                let _ = args;
                eprintln!("oos-fpgad: bringup runs on the scope only");
                std::process::exit(3);
            }
        }
        "mho-start" => {
            #[cfg(target_os = "linux")]
            std::process::exit(start::run(args));
            #[cfg(not(target_os = "linux"))]
            {
                let _ = args;
                eprintln!("oos-fpgad: mho-start runs on the scope only");
                std::process::exit(3);
            }
        }
        _ => {
            eprintln!("oos-fpgad: OOS drives no {} hardware (docs/mho.md)", family);
            std::process::exit(3);
        }
    }
}
