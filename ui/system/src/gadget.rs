// Copyright (C) 2026 dekajoule
// SPDX-License-Identifier: GPL-2.0-or-later

//! The rear USB port as a network link to one computer (docs/usb-device.md, ADR 0079): `/bin/oos-gadget` builds the
//! USB gadget (RNDIS and ECM), gives the scope its fixed address and the computer one by DHCP. oos-ui asks the script
//! to build or take down the gadget when `usb.net` changes, shows its state, and lets the computer on the cable in
//! like loopback (`on_usb_link`, used by `netacl`).

use std::net::IpAddr;

/// The script that owns the gadget.
pub const SCRIPT: &str = "/bin/oos-gadget";
pub const LOG: &str = "/tmp/oos-gadget.log";
/// One line of state the script leaves: `on: 192.168.235.1`, `off`, `off: no USB device port`, ...
const STATE: &str = "/tmp/oos-gadget.state";
/// The USB prefix in force (`192.168.235.0/29`), only while the network over the cable is up.
const NET_FILE: &str = "/tmp/oos-gadget.net";

/// The script's state line; "not started" before it ran.
pub fn state() -> String {
    std::fs::read_to_string(STATE).map(|s| s.trim().to_string()).unwrap_or_else(|_| "not started".into())
}

/// The scope's address on the cable while the network is up (`on: <address>`), else None.
pub fn address(state: &str) -> Option<&str> {
    state.strip_prefix("on: ").map(str::trim).filter(|a| a.parse::<std::net::Ipv4Addr>().is_ok())
}

/// Whether `ip` is in the prefix of the USB link (`text`: what the script left in `NET_FILE`, None without it). Only
/// an IPv4 prefix of the script's form counts; anything else in the file lets nobody in.
pub fn in_usb_prefix(text: Option<&str>, ip: IpAddr) -> bool {
    let Some((net, bits)) = text.and_then(|t| t.trim().split_once('/')) else { return false };
    let (Ok(net), Ok(bits)) = (net.parse::<std::net::Ipv4Addr>(), bits.parse::<u32>()) else { return false };
    let ip = match ip {
        IpAddr::V4(a) => a,
        IpAddr::V6(a) => match a.to_ipv4_mapped() {
            Some(a) => a,
            None => return false,
        },
    };
    let mask = if (24..=32).contains(&bits) { u32::MAX << (32 - bits) } else { return false };
    u32::from(ip) & mask == u32::from(net) & mask
}

/// The computer on the rear USB port: an address of the USB prefix while the network over the cable is up. The
/// firewall lets it in on the gadget's interfaces only (`oos-firewall`); here it is the address alone, which other
/// interfaces cannot bring past the firewall.
pub fn on_usb_link(ip: IpAddr) -> bool {
    in_usb_prefix(std::fs::read_to_string(NET_FILE).ok().as_deref(), ip)
}

/// Builds the gadget again with the current settings or takes it down (`oos-gadget apply`), in a thread of its own:
/// the script waits for the device controller, the frame loop does not. Only on the scope, where the script is.
pub fn apply() {
    if !std::path::Path::new(SCRIPT).exists() {
        return;
    }
    let step = crate::remote::Step { prog: SCRIPT, arg: "apply", log: LOG };
    std::thread::spawn(move || {
        if let Err(e) = crate::remote::script_runner()(&step) {
            eprintln!("oos-ui: {}", e);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    // Break it catches: the computer on the cable shut out by `net.allow` (no address list of its own), or the USB
    // prefix letting in addresses beside it, or a broken file letting in everyone
    #[test]
    fn the_usb_prefix_lets_in_the_computer_on_the_cable_only() {
        let f = Some("10.9.9.8/29\n");
        for a in ["10.9.9.8", "10.9.9.9", "::ffff:10.9.9.9"] {
            assert!(in_usb_prefix(f, ip(a)), "{}", a);
        }
        for a in ["10.9.9.1", "10.0.0.9", "10.1.2.3", "fe80::1"] {
            assert!(!in_usb_prefix(f, ip(a)), "{}", a);
        }
        // the network over the cable is down, or the file is not the script's: nobody
        for t in [None, Some(""), Some("garbage"), Some("0.0.0.0/0"), Some("10.9.9.8/x"), Some("fd00::/8")] {
            assert!(!in_usb_prefix(t, ip("10.9.9.9")), "{:?}", t);
        }
    }

    #[test]
    fn the_address_of_the_state_line() {
        assert_eq!(address("on: 192.168.235.1"), Some("192.168.235.1"));
        for s in ["off", "off: no USB device port", "not started", "on: x"] {
            assert_eq!(address(s), None, "{}", s);
        }
    }
}
