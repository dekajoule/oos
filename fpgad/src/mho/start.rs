// Copyright (C) 2026 dekajoule
// SPDX-License-Identifier: GPL-2.0-or-later

//! `oos-fpgad mho-start`: the first acquisition of the MHO900 under OOS, run by hand (docs/mho.md, "First
//! acquisition"; docs/protocols/mho-acquisition.md, section 10). It brings the board up as `bringup` does, sets the
//! ADCs, the JESD204 receiver, the FPGA's configuration and one front end from the unit's own snapshot of the stock
//! system (`tools/mho-check.py --snapshot`), reads all of it back, then arms one acquisition with the read-out of the
//! DHO800/DHO900 and says whether the probe compensation signal came back. Everything it sets is volatile: a
//! power-off undoes it. It goes on past a step that does not read back as written, so one run shows every step.
//! Not yet verified on hardware.

use oos_hal::dho::bus::{Bar1, Bus};
use oos_hal::dho::gpio::Line;
use oos_hal::dho::scu;
use oos_hal::mho::snapshot::{self as snap, Levels, Snapshot, Step};
use oos_hal::mho::{acq, bringup as b};
use std::time::{Duration, Instant};

/// Where the snapshot is looked for when `--snapshot` is not given.
const DEFAULT_FILE: &str = "/oos/mho-snapshot.txt";
/// Where the frame goes, one line per point: index, word, volts.
const CSV: &str = "/tmp/mho-frame.csv";
/// Acquisitions tried, and how long each may take to complete.
const TRIES: u32 = 3;
const DONE_MS: u64 = 2000;
/// How long the read-out may take before the run gives up on it.
const READ_S: u64 = 10;

fn opt<'a>(args: &'a [String], k: &str) -> Option<&'a str> {
    args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).map(|s| s.as_str())
}

pub(super) fn run(args: &[String]) -> i32 {
    let file = opt(args, "--snapshot").unwrap_or(DEFAULT_FILE);
    let s =
        match std::fs::read_to_string(file).map_err(|e| format!("{}: {}", file, e)).and_then(|t| Snapshot::parse(&t)) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("oos-fpgad mho-start: {}", e);
                return 2;
            }
        };
    let plan = match s.plan() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("oos-fpgad mho-start: {}: {}", file, e);
            return 2;
        }
    };
    let code = opt(args, "--hw-code").and_then(|c| c.parse::<u32>().ok()).or(s.hwcode);
    let Some((table, image)) = code.and_then(|c| Some((b::clock_table(c)?, acq::fpga_image(c)?))) else {
        eprintln!("oos-fpgad mho-start: no clock table or FPGA image for hardware code {:?} (--hw-code N)", code);
        return 2;
    };
    let n = opt(args, "--n").and_then(|v| v.parse().ok()).unwrap_or(4000u32);
    println!(
        "mho-start: snapshot {} of an {}, CH{} at {} V/div, {} s/div; hardware code {}",
        file,
        s.model,
        s.channel,
        s.scale_text,
        s.timebase,
        code.unwrap_or(0)
    );
    for (name, steps) in &plan {
        println!("mho-start: phase {}: {} steps", name, steps.len());
    }
    if args.iter().any(|a| a == "--dry") {
        for (name, steps) in &plan {
            for st in steps {
                println!("{} {:?}", name, st);
            }
        }
        return 0;
    }
    if !args.iter().any(|a| a == "--yes") {
        println!(
            "mho-start: this brings the acquisition board up, writes the ADCs, the FPGA and the CH{} front end from the \
             snapshot and arms one acquisition (volatile: all of it is gone at power-off). The probe compensation \
             signal goes to CH{}. Run again with --yes to go on (--dry lists every step).",
            s.channel, s.channel
        );
        return 0;
    }
    if !args.iter().any(|a| a == "--no-bringup") {
        let up: [super::bringup::Stage; 4] = [
            ("power", &|| super::bringup::power(&[b::line::ADC_POWER, b::line::AFE_POWER])),
            ("clock", &|| super::bringup::clock(table)),
            ("fpga", &|| super::bringup::power(&[b::line::FPGA_POWER]).and_then(|_| super::bringup::fpga(image))),
            ("pcie", &super::bringup::pcie),
        ];
        for (name, step) in up {
            println!("---- bring-up: {}", name);
            if let Err(e) = step() {
                println!("RESULT: the bring-up stopped at {}: {}", name, e);
                return 1;
            }
        }
    } else if let Err(e) = crate::pcie::pcie_up() {
        println!("RESULT: PCIe is not up ({}); run without --no-bringup", e);
        return 1;
    }
    let mut bar = match Bar1::open_len(acq::WINDOW_USED) {
        Ok(b) => b,
        Err(e) => {
            println!("RESULT: /dev/xdma0_bypass: {}", e);
            return 1;
        }
    };
    println!("mho-start: FPGA version {:#010x}", bar.rd(0x4));
    let afe = match Afe::open(s.channel) {
        Ok(a) => a,
        Err(e) => {
            println!("RESULT: the front ends' bus: {}", e);
            return 1;
        }
    };
    for (name, steps) in &plan {
        println!("---- {} ({} steps)", name, steps.len());
        let t = Instant::now();
        for st in steps {
            if let Err(e) = apply(&mut bar, &afe, st) {
                println!("mho-start: {:?}: {}", st, e);
            }
        }
        println!("mho-start: {} done in {} ms", name, t.elapsed().as_millis());
    }
    println!("---- read back");
    read_back(&mut bar, &s);
    println!("---- acquisition");
    acquire(&mut bar, &s, n)
}

/// One step of the plan.
fn apply(bar: &mut Bar1, afe: &Afe, st: &Step) -> Result<(), String> {
    match st {
        Step::Wr(a, v) => bar.wr(*a, *v),
        Step::Wait(ms) => std::thread::sleep(Duration::from_millis(*ms as u64)),
        Step::Adc(n, r, d) => {
            for (off, v) in acq::adc_write(*n, *r as u32, *d).ok_or("no such ADC register")? {
                bar.wr(off, v);
                std::thread::sleep(Duration::from_micros(acq::ADC_SPI_WAIT_US));
            }
        }
        Step::Afe(r, d) => afe.write(*r, *d),
        Step::Select(name, v) => drop(Line::output(name, *v).map_err(|e| format!("{}: {}", name, e))?),
    }
    Ok(())
}

/// One ADC register through the bridge: the 16 data bits, None when the answer was not marked valid.
fn adc_read(bar: &mut Bar1, n: u32, r: u8) -> Option<u16> {
    for (off, v) in acq::adc_read(n, r as u32)? {
        bar.wr(off, v);
        std::thread::sleep(Duration::from_micros(acq::ADC_SPI_WAIT_US));
    }
    let x = bar.rd(acq::ADC_SPI_READ);
    (x & acq::ADC_SPI_VALID != 0).then_some(x as u16)
}

/// The FPGA's words and the ADCs' registers against the snapshot: how many read as the stock system had them, and
/// the first that do not. A word may read otherwise and be right (a write-only register, a status bit): this is for
/// the report, it decides nothing.
fn read_back(bar: &mut Bar1, s: &Snapshot) {
    let skip = |a: &u32| snap::NOT_REPLAYED.iter().any(|(n, _)| n == a) || *a == snap::SYNC;
    let mut differ = Vec::new();
    let words: Vec<(u32, u32)> = s.fpga.iter().filter(|(a, _)| !skip(a)).map(|(&a, &v)| (a, v)).collect();
    for &(a, v) in &words {
        let now = bar.rd(a);
        if now != v {
            differ.push(format!("{:#x} {:#010x} (stock {:#010x})", a, now, v));
        }
    }
    println!("mho-start: FPGA words: {} of {} read as in the snapshot", words.len() - differ.len(), words.len());
    for d in differ.iter().take(40) {
        println!("mho-start:   {}", d);
    }
    for n in 0..2u32 {
        let mut differ = Vec::new();
        for (&r, &v) in &s.adc[n as usize] {
            match adc_read(bar, n, r) {
                Some(x) if x == v => {}
                Some(x) => differ.push(format!("{:02x}={:04x} (stock {:04x})", r, x, v)),
                None => differ.push(format!("{:02x}=- (stock {:04x})", r, v)),
            }
        }
        let total = s.adc[n as usize].len();
        println!("mho-start: ADC {}: {} of {} registers read as in the snapshot", n, total - differ.len(), total);
        if !differ.is_empty() {
            println!("mho-start:   {}", differ.join(" "));
        }
    }
    let count = bar.rd(oos_hal::mho::bringup::ADC_CLOCK_COUNT);
    println!(
        "mho-start: ADC clock count {} (stock {})",
        count,
        s.fpga.get(&oos_hal::mho::bringup::ADC_CLOCK_COUNT).map_or("-".into(), |v| v.to_string())
    );
}

/// Arms up to `TRIES` acquisitions with the DHO800/DHO900's sequence and reads the first that completes; says whether
/// it holds the probe compensation signal.
fn acquire(bar: &mut Bar1, s: &Snapshot, n: u32) -> i32 {
    let mut sh = s.shadows();
    let win = s.window(n);
    println!("mho-start: window {} points from {}, FSM mask {:#x}", win.n, win.start, win.fsm);
    for i in 1..=TRIES {
        let r = snap::acquire(bar, &mut sh, &win, DONE_MS, &mut |ms| std::thread::sleep(Duration::from_millis(ms)));
        let a = r.unwrap_or_else(|a| a);
        println!(
            "mho-start: arm {}: {} after {} ms, 0x4080 {:#010x}, status {:#010x} ({:?}), stop reason {:#010x}",
            i,
            if r.is_ok() { "complete" } else { "NOT complete" },
            a.ms,
            a.playinfo,
            a.status,
            scu::fired(a.status),
            a.stop_reason
        );
        if r.is_err() {
            continue;
        }
        match read_frame(2 * win.n as usize) {
            Ok(words) => return judge(s, &words),
            Err(e) => println!("mho-start: read-out of {} bytes: {}", 2 * win.n, e),
        }
    }
    println!("RESULT: no acquisition was read; the lines above say how far it got");
    1
}

/// One read of the read-out stream, given up after `READ_S` (the driver's own timeout may be longer).
fn read_frame(bytes: usize) -> Result<Vec<u16>, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let r = std::fs::File::open(acq::C2H_DEVICE)
            .and_then(|mut f| oos_hal::dho::bus::read_dma(&mut f, bytes, |_, _| {}))
            .map_err(|e| e.to_string());
        let _ = tx.send(r);
    });
    let b = rx.recv_timeout(Duration::from_secs(READ_S)).map_err(|_| format!("nothing within {} s", READ_S))??;
    Ok(b.as_chunks::<2>().0.iter().map(|w| u16::from_le_bytes(*w)).collect())
}

/// The frame's levels, the file with its points, and the verdict.
fn judge(s: &Snapshot, words: &[u16]) -> i32 {
    let csv: String =
        words.iter().enumerate().map(|(i, &w)| format!("{},{},{:.5}\n", i, w, acq::volts(w, s.scale, 0.0))).collect();
    match std::fs::write(CSV, csv) {
        Ok(()) => println!("mho-start: {} points written to {}", words.len(), CSV),
        Err(e) => println!("mho-start: {}: {}", CSV, e),
    }
    let step = (words.len() / 40).max(1);
    println!(
        "mho-start: words (every {}th): {}",
        step,
        words.iter().step_by(step).map(|w| format!("{:04x}", w)).collect::<Vec<_>>().join(" ")
    );
    let Some(l) = Levels::of(words, s.scale) else {
        println!("RESULT: the read-out was empty");
        return 1;
    };
    println!(
        "mho-start: min {:.3} V, max {:.3} V, mean {:.3} V; levels {:.3} V and {:.3} V, {:.0} % of the points high",
        l.min,
        l.max,
        l.mean,
        l.low,
        l.high,
        100.0 * l.high_share
    );
    if l.is_square(s.scale) {
        println!(
            "RESULT: IT MEASURES: a square wave between {:.2} V and {:.2} V on CH{} (the probe compensation signal)",
            l.low, l.high, s.channel
        );
        0
    } else {
        println!(
            "RESULT: a frame was read, but it is not the square wave (a line, noise or clipped): see the words above"
        );
        1
    }
}

/// The front ends' bus, driven bit by bit as the stock driver does (mho-acquisition.md, section 8): the data line set,
/// then the clock low and high; 24 bits, MSB first, while the channel's chip select is low.
struct Afe {
    sclk: Line,
    mosi: Line,
    cs: Line,
}

impl Afe {
    fn open(ch: usize) -> Result<Afe, String> {
        let e = |n: &str, e: std::io::Error| format!("{}: {}", n, e);
        let cs = acq::afe::CS_NAMES[ch - 1];
        Ok(Afe {
            sclk: Line::output(acq::afe::SCLK_NAME, true).map_err(|x| e(acq::afe::SCLK_NAME, x))?,
            mosi: Line::output(acq::afe::MOSI_NAME, false).map_err(|x| e(acq::afe::MOSI_NAME, x))?,
            cs: Line::output(cs, true).map_err(|x| e(cs, x))?,
        })
    }

    fn write(&self, reg: u8, data: u16) {
        let w = acq::afe::frame(reg, data);
        self.cs.set(false);
        for i in (0..acq::afe::FRAME_BITS).rev() {
            self.mosi.set(w >> i & 1 != 0);
            self.sclk.set(false);
            self.sclk.set(true);
        }
        self.cs.set(true);
    }
}
