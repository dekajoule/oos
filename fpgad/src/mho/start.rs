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
use oos_hal::mho::recipe::{self, Item};
use oos_hal::mho::snapshot::{self as snap, Levels, Snapshot, Step};
use oos_hal::mho::{acq, bringup as b};
use std::time::{Duration, Instant};

/// Where the snapshot is looked for when `--snapshot` is not given.
const DEFAULT_FILE: &str = "/oos/mho-snapshot.txt";
/// Where the front ends' set-up of the stock program's start (`mho-check.py --afe-init`) is looked for when `--init`
/// is not given; without the file the start goes on without it.
const DEFAULT_INIT: &str = "/oos/mho-afe-init.txt";
/// The capture of the stock system's cold start (`mho-check.py --capture-all`) and a recipe, each used when it is
/// there (`--capture`, `--recipe` name other files).
const DEFAULT_CAPTURE: &str = "/oos/mho-capture.txt";
const DEFAULT_RECIPE: &str = "/oos/mho-recipe.txt";
/// Where the frame goes, one line per point: index, word, volts.
const CSV: &str = "/tmp/mho-frame.csv";
/// The revision of this command, the first line of its report: says which `oos-fpgad` made a report (the mho-dev
/// release it came with).
const REVISION: &str = "mho-dev-21";
/// Acquisitions tried, and how long each may take to complete.
const TRIES: u32 = 3;
const DONE_MS: u64 = 2000;
/// How long the read-out may take before the run gives up on it.
const READ_S: u64 = 10;

fn opt<'a>(args: &'a [String], k: &str) -> Option<&'a str> {
    args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).map(|s| s.as_str())
}

pub(super) fn run(args: &[String]) -> i32 {
    println!(
        "mho-start: {} ({})",
        REVISION,
        std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default()
    );
    let file = opt(args, "--snapshot").unwrap_or(DEFAULT_FILE);
    let s =
        match std::fs::read_to_string(file).map_err(|e| format!("{}: {}", file, e)).and_then(|t| Snapshot::parse(&t)) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("oos-fpgad mho-start: {}", e);
                return 2;
            }
        };
    let init_file = opt(args, "--init").unwrap_or(DEFAULT_INIT);
    let init = match std::fs::read_to_string(init_file) {
        Err(_) if opt(args, "--init").is_none() => {
            println!(
                "mho-start: no {} (mho-check.py --afe-init): the front ends get the snapshot's words only",
                init_file
            );
            Vec::new()
        }
        r => match r.map_err(|e| e.to_string()).and_then(|t| snap::parse_afe_init(&t)) {
            Ok(i) => {
                println!("mho-start: the front ends' set-up from {}: {} steps", init_file, i.len());
                i
            }
            Err(e) => {
                eprintln!("oos-fpgad mho-start: {}: {}", init_file, e);
                return 2;
            }
        },
    };
    let plan = match s.plan(&init) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("oos-fpgad mho-start: {}: {}", file, e);
            return 2;
        }
    };
    let capture = match read_optional(args, "--capture", DEFAULT_CAPTURE, recipe::parse_capture) {
        Ok(c) => c.unwrap_or_default(),
        Err(e) => {
            eprintln!("oos-fpgad mho-start: {}", e);
            return 2;
        }
    };
    let items = match read_optional(args, "--recipe", DEFAULT_RECIPE, recipe::parse) {
        Ok(Some(r)) => Some(r),
        Ok(None) if !capture.is_empty() => recipe::parse(recipe::WITH_CAPTURE).ok(),
        Ok(None) => None,
        Err(e) => {
            eprintln!("oos-fpgad mho-start: {}", e);
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
    println!(
        "mho-start: capture: {} steps; recipe: {}",
        capture.len(),
        items.as_ref().map_or("none".into(), |r| { format!("{} lines", r.len()) })
    );
    if args.iter().any(|a| a == "--dry") {
        for (name, steps) in &plan {
            for st in steps {
                println!("{} {}", name, recipe::step_line(st));
            }
        }
        for st in &capture {
            println!("capture {}", recipe::step_line(st));
        }
        for it in items.iter().flatten() {
            println!("recipe {:?}", it);
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
    if let Err(code) = bring_up(args, table, image) {
        return code;
    }
    let mut bar = match Bar1::open_len(acq::WINDOW_USED) {
        Ok(b) => b,
        Err(e) => {
            println!("RESULT: /dev/xdma0_bypass: {}", e);
            return 1;
        }
    };
    println!("mho-start: FPGA version {:#010x}", bar.rd(0x4));
    let afe = match Afe::open() {
        Ok(a) => a,
        Err(e) => {
            println!("RESULT: the front ends' bus: {}", e);
            return 1;
        }
    };
    let (first, from_recipe) = match &items {
        Some(items) => (1, run_recipe(&mut bar, &afe, &Ctx { s: &s, plan: &plan, capture: &capture, n }, items)),
        None => {
            for (name, steps) in &plan {
                run_steps(&mut bar, &afe, name, steps);
            }
            println!("---- read back");
            read_back(&mut bar, &s);
            println!("---- acquisition");
            (acquire(&mut bar, &s, n), None)
        }
    };
    if items.is_some() {
        println!("---- read back");
        read_back(&mut bar, &s);
    }
    println!("---- diagnostics");
    let found = diagnose(&mut bar, &afe, &s, &plan, n);
    verdict(first, from_recipe.or(found), s.channel)
}

/// The bring-up of `bringup` (supplies, clock, FPGA, PCIe), or with `--no-bringup` only PCIe; the exit code when it
/// stops.
fn bring_up(args: &[String], table: &str, image: &str) -> Result<(), i32> {
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
                return Err(1);
            }
        }
    } else if let Err(e) = crate::pcie::pcie_up() {
        println!("RESULT: PCIe is not up ({}); run without --no-bringup", e);
        return Err(1);
    }
    Ok(())
}

/// The one RESULT line of a run and its exit code: from the first acquisition (0 square, 1 not, 2 none read) and the
/// recipe's or the diagnostics' first variant that held the square wave.
fn verdict(first: i32, found: Option<String>, channel: usize) -> i32 {
    match (first, found) {
        (0, _) => {
            println!("RESULT: IT MEASURES: the probe compensation signal on CH{}", channel);
            0
        }
        (_, Some(what)) => {
            println!("RESULT: IT MEASURES in a diagnostic variant: {}", what);
            0
        }
        (1, None) => {
            println!(
                "RESULT: a frame was read, but no variant held the square wave; the diagnostics say what each saw"
            );
            1
        }
        _ => {
            println!("RESULT: no acquisition was read; the lines above say how far it got");
            1
        }
    }
}

/// A file `--KEY FILE` names, or `default` when it is there: None when neither is.
fn read_optional<T>(
    args: &[String],
    key: &str,
    default: &str,
    parse: fn(&str) -> Result<T, String>,
) -> Result<Option<T>, String> {
    let file = opt(args, key).unwrap_or(default);
    match std::fs::read_to_string(file) {
        Err(_) if opt(args, key).is_none() => Ok(None),
        r => r
            .map_err(|e| format!("{}: {}", file, e))
            .and_then(|t| parse(&t))
            .map(Some)
            .map_err(|e| format!("{}: {}", file, e)),
    }
}

/// The steps of one phase, each failure reported, the time it took.
fn run_steps(bar: &mut Bar1, afe: &Afe, name: &str, steps: &[Step]) {
    println!("---- {} ({} steps)", name, steps.len());
    let t = Instant::now();
    for st in steps {
        if let Err(e) = apply(bar, afe, st) {
            println!("mho-start: {}: {}", recipe::step_line(st), e);
        }
    }
    println!("mho-start: {} done in {} ms", name, t.elapsed().as_millis());
}

/// What a recipe draws on besides the board: the snapshot, the plan made from it, the capture and the frame size.
struct Ctx<'a> {
    s: &'a Snapshot,
    plan: &'a [(&'a str, Vec<Step>)],
    capture: &'a [Step],
    n: u32,
}

/// A recipe, line by line (hal/src/mho/recipe.rs). Returns the first acquisition that held the square wave, named by
/// the label before it and its mask.
fn run_recipe(bar: &mut Bar1, afe: &Afe, c: &Ctx, items: &[Item]) -> Option<String> {
    let (s, plan, capture, n) = (c.s, c.plan, c.capture, c.n);
    let (mut label, mut found) = (String::from("recipe"), None);
    for it in items {
        match it {
            Item::Label(l) => {
                println!("---- {}", l);
                label = l.clone();
            }
            Item::Step(st) => {
                if let Err(e) = apply(bar, afe, st) {
                    println!("mho-start: {}: {}", recipe::step_line(st), e);
                }
            }
            Item::Plan(p) => match plan.iter().find(|(name, _)| name == p) {
                Some((name, steps)) => run_steps(bar, afe, name, steps),
                None => println!("mho-start: plan {}: no such phase here (no file for it)", p),
            },
            Item::Capture if capture.is_empty() => println!("mho-start: capture: none (/oos/mho-capture.txt)"),
            Item::Capture => run_steps(bar, afe, "capture", capture),
            Item::AdcSync => adc_sync(bar, s, "recipe"),
            Item::Acquire(m) => {
                let mask = m.unwrap_or_else(|| s.fsm_mask());
                match one(bar, s, n, mask) {
                    Ok(w) => {
                        let (t, sq) = levels_line(&w, s.scale);
                        println!("recipe: acquire {:#x}: {}", mask, t);
                        if sq && found.is_none() {
                            found = Some(format!("{}, mask {:#x}", label, mask));
                        }
                    }
                    Err(e) => println!("recipe: acquire {:#x}: {}", mask, e),
                }
            }
            Item::Status => {
                let st: Vec<String> = (1..=4).map(|ch| format!("CH{} {:#06x}", ch, afe.read(ch, 0))).collect();
                println!("recipe: front ends' status (stock 0x0440): {}", st.join(", "));
            }
            Item::AfePowerCycle => {
                let r = Line::output(b::line::AFE_POWER, false).map(|l| {
                    std::thread::sleep(Duration::from_millis(100));
                    l.set(true);
                });
                println!(
                    "recipe: front ends' supply off and on: {}",
                    r.map_or_else(|e| e.to_string(), |_| "done".into())
                );
            }
        }
    }
    found
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
        Step::AdcRead(n, r) => {
            let v = adc_read(bar, *n, *r);
            println!(
                "mho-start: ADC {} reg {:#04x} reads {}",
                n,
                r,
                v.map_or("- (not valid)".into(), |x| format!("{:#06x}", x))
            );
        }
        Step::Afe(ch, r, d) => afe.write(*ch, *r, *d),
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

/// Arms up to `TRIES` acquisitions with the DHO800/DHO900's sequence and reads the first that completes: 0 when it
/// holds the probe compensation signal, 1 when it does not, 2 when none was read.
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
    println!("mho-start: first acquisition: none was read");
    2
}

/// One acquisition with FSM channel mask `mask` (the snapshot's shadows otherwise), read whole.
fn one(bar: &mut Bar1, s: &Snapshot, n: u32, mask: u32) -> Result<Vec<u16>, String> {
    let mut sh = s.shadows();
    sh.s4008 = (sh.s4008 & !0xf) | mask;
    let mut win = s.window(n);
    win.fsm = mask;
    snap::acquire(bar, &mut sh, &win, DONE_MS, &mut |ms| std::thread::sleep(Duration::from_millis(ms)))
        .map_err(|a| format!("not complete after {} ms, status {:#010x}", a.ms, a.status))?;
    read_frame(2 * win.n as usize)
}

/// One line of what a frame holds: its extremes and mean in codes and volts, and whether it is the square wave.
fn levels_line(words: &[u16], scale: f64) -> (String, bool) {
    let Some(l) = Levels::of(words, scale) else {
        return ("empty".into(), false);
    };
    let (lo, hi) = (words.iter().min().copied().unwrap_or(0), words.iter().max().copied().unwrap_or(0));
    let sq = l.is_square(scale);
    (
        format!(
            "codes {:04x}..{:04x}, {:.3} V..{:.3} V, mean {:.3} V, levels {:.3}/{:.3} V{}",
            lo,
            hi,
            l.min,
            l.max,
            l.mean,
            l.low,
            l.high,
            if sq { "  SQUARE WAVE" } else { "" }
        ),
        sq,
    )
}

/// Variants after the first acquisition, each with one acquisition read, so that one run says where the signal is
/// lost: the front ends' status read, every FSM channel mask (and all four, split by word), 0x1004 written again,
/// and the ADC registers that did not read back written once more. Returns the first variant that held the square
/// wave.
fn diagnose(bar: &mut Bar1, afe: &Afe, s: &Snapshot, plan: &[(&str, Vec<Step>)], n: u32) -> Option<String> {
    let mut found = None;
    let mut note = |what: String, sq: bool| {
        if sq && found.is_none() {
            found = Some(what);
        }
    };
    for ch in 1..=4 {
        println!("diag: front end CH{} status read: {:#06x} (stock reads 0x0440 at 1 MOhm)", ch, afe.read(ch, 0));
    }
    // the snapshot channel's registers as last written, read back with the status frame's read bit (assumed: the
    // read of register r is 0x80 | r, as the status read is register 0)
    let mut last = std::collections::BTreeMap::new();
    for st in plan.iter().flat_map(|(_, steps)| steps) {
        if let Step::Afe(ch, r, d) = st {
            if *ch == s.channel {
                last.insert(*r, *d);
            }
        }
    }
    let reads: Vec<String> =
        last.iter().map(|(&r, &d)| format!("{:02x}:{:04x}(w {:04x})", r, afe.read(s.channel, r), d)).collect();
    println!("diag: front end CH{} registers read back: {}", s.channel, reads.join(" "));
    for mask in [1u32, 2, 4, 8, 0xf] {
        match one(bar, s, n, mask) {
            Err(e) => println!("diag: mask {:#x}: {}", mask, e),
            Ok(w) if mask == 0xf => {
                for k in 0..4 {
                    let lane: Vec<u16> = w.iter().skip(k).step_by(4).copied().collect();
                    let (t, sq) = levels_line(&lane, s.scale);
                    println!("diag: mask 0xf, word 4i+{}: {}", k, t);
                    note(format!("mask 0xf, word 4i+{}", k), sq);
                }
            }
            Ok(w) => {
                let (t, sq) = levels_line(&w, s.scale);
                println!("diag: mask {:#x}: {}", mask, t);
                note(format!("mask {:#x}", mask), sq);
            }
        }
    }
    if let Some(&v) = s.fpga.get(&0x1004) {
        bar.wr(0x1004, v);
        println!("diag: 0x1004 written {:#010x} again, reads {:#010x}", v, bar.rd(0x1004));
        if let Ok(w) = one(bar, s, n, s.fsm_mask()) {
            let (t, sq) = levels_line(&w, s.scale);
            println!("diag: after 0x1004: {}", t);
            note("0x1004 written again".into(), sq);
        }
    }
    adc_sync(bar, s, "diag");
    if let Ok(w) = one(bar, s, n, s.fsm_mask()) {
        let (t, sq) = levels_line(&w, s.scale);
        println!("diag: after the ADC registers again: {}", t);
        note("ADC registers written again".into(), sq);
    }
    found
}

/// Every ADC register that reads otherwise than in the snapshot written with the snapshot's value, then read at
/// once; reported under `tag`. After the stock program's cold start this brings the ADCs' input selection (0x54,
/// 0x55) and the lanes' trims to the snapshot's setting (hp197's MHO98, mho-dev-15).
fn adc_sync(bar: &mut Bar1, s: &Snapshot, tag: &str) {
    for a in 0..2u32 {
        let mut out = Vec::new();
        for (&r, &v) in &s.adc[a as usize] {
            if adc_read(bar, a, r) == Some(v) {
                continue;
            }
            for (off, w) in acq::adc_write(a, r as u32, v).into_iter().flatten() {
                bar.wr(off, w);
                std::thread::sleep(Duration::from_micros(acq::ADC_SPI_WAIT_US));
            }
            let now = adc_read(bar, a, r);
            out.push(format!("{:02x}:{}", r, now.map_or("-".into(), |x| format!("{:04x}", x))));
        }
        println!("{}: ADC {} written again where it differed, then read: {}", tag, a, out.join(" "));
    }
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
        println!("mho-start: first acquisition: the read-out was empty");
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
        println!("mho-start: first acquisition: the square wave, {:.2} V to {:.2} V", l.low, l.high);
        0
    } else {
        println!("mho-start: first acquisition: not the square wave (a line, noise or clipped)");
        1
    }
}

/// The front ends' bus: the bit-banged bus of the DHO800/DHO900 (`dho::afe::GpioAfe`: SPI mode 0, the clock idle
/// low, a byte at a time, 5 us half period), which is how the stock MHO900 driver drives it too (its raw trace,
/// mho-acquisition.md section 8: the clock low between bytes, about 7 us per half period). The MHO900 numbers the chip
/// selects the other way round: CH1 is cs3 (`acq::afe::CS_NAMES`). A read is the address with bit 7, then 16 bits;
/// the stock system's status read gives 0x0440 (then 0xff) at 1 MOhm.
struct Afe {
    bus: std::cell::RefCell<oos_hal::dho::afe::GpioAfe>,
}

impl Afe {
    fn open() -> Result<Afe, String> {
        let bus = oos_hal::dho::afe::GpioAfe::open().map_err(|e| e.to_string())?;
        Ok(Afe { bus: std::cell::RefCell::new(bus) })
    }

    /// The bus's chip-select index of channel `ch` (1 to 4): CH1 on cs3 ... CH4 on cs0.
    fn cs(ch: usize) -> usize {
        4 - ch
    }

    fn write(&self, ch: usize, reg: u8, data: u16) {
        use oos_hal::dho::afe::AfeBus;
        self.bus.borrow_mut().write(Self::cs(ch), reg, data);
    }

    fn read(&self, ch: usize, reg: u8) -> u32 {
        use oos_hal::dho::afe::AfeBus;
        self.bus.borrow_mut().read(Self::cs(ch), reg) as u32
    }
}
