// Copyright (C) 2026 dekajoule
// SPDX-License-Identifier: GPL-2.0-or-later

//! Recipes: the start of the MHO900's acquisition board as text (docs/protocols/mho-acquisition.md, section 10). The
//! capture of the stock system's cold start (`mho-check.py --capture-all`) is written in this form, and so is a
//! recipe an owner copies to the scope to try a start without a new build. Pure: text in, items out; `oos-fpgad
//! mho-start` runs them. **Not yet verified on hardware.**
//!
//! One item per line, `#` starts a comment; numbers are hex with `0x` or decimal:
//!
//! | line | what |
//! |---|---|
//! | `wr OFFSET VALUE` | a register write |
//! | `wait MS` | a pause |
//! | `adc N REG DATA` | an ADC register write through the bridge |
//! | `adcread N REG` | an ADC register read through the bridge, reported |
//! | `afe CH REG DATA` | a frame on a channel's front end |
//! | `select NAME LEVEL` | a front-end select line (`afe_sel_c_ch2`, or the snapshot's `c131`) |
//! | `plan PHASE` | a phase of the snapshot's start: `adc`, `link`, `fpga`, `afe-init`, `afe` |
//! | `capture` | the steps of the capture, in its order |
//! | `acquire [MASK]` | one acquisition, its levels reported (the snapshot's channel mask if none) |
//! | `status` | the four front ends' status read, reported |
//! | `afe-power-cycle` | the front ends' supply off for 100 ms and on again |
//! | `label TEXT` | a heading in the report |

use super::acq;
use super::snapshot::{Step, FORMAT};

/// The line that starts the machine-readable part of `mho-check.py --capture-all`.
pub const CAPTURE_MARK: &str = "---- oos capture ----";

/// One line of a recipe.
#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Step(Step),
    Plan(String),
    Capture,
    Acquire(Option<u32>),
    Status,
    AfePowerCycle,
    Label(String),
}

/// The phases `plan` may name.
pub const PHASES: [&str; 5] = ["adc", "link", "fpga", "afe-init", "afe"];

fn num(s: &str) -> Result<u32, String> {
    let r = match s.strip_prefix("0x") {
        Some(h) => u32::from_str_radix(h, 16),
        None => s.parse(),
    };
    r.map_err(|_| format!("not a number: {:?}", s))
}

/// The device-tree name of a select line written as such or as the snapshot writes it (`c131`); None for a line that
/// is no front-end select line (the start never drives those).
fn select_line(name: &str) -> Result<Option<String>, String> {
    if name.starts_with("afe_sel_") {
        let known = acq::afe::SELECT_A.iter().chain(&acq::afe::SELECT_B).chain(&acq::afe::SELECT_C);
        return match known.filter_map(|&g| acq::afe::select_name(g)).find(|n| n == name) {
            Some(n) => Ok(Some(n)),
            None => Err(format!("no select line {:?}", name)),
        };
    }
    let g = &name[name.trim_end_matches(|c: char| c.is_ascii_digit()).len()..];
    Ok(acq::afe::select_name(num(g)?))
}

/// One step's line, the form `parse` reads.
pub fn step_line(s: &Step) -> String {
    match s {
        Step::Wr(a, v) => format!("wr {:#x} {:#010x}", a, v),
        Step::Wait(ms) => format!("wait {}", ms),
        Step::Adc(n, r, d) => format!("adc {} {:#04x} {:#06x}", n, r, d),
        Step::AdcRead(n, r) => format!("adcread {} {:#04x}", n, r),
        Step::Afe(ch, r, d) => format!("afe {} {:#04x} {:#06x}", ch, r, d),
        Step::Select(n, v) => format!("select {} {}", n, *v as u8),
    }
}

/// Reads a recipe. An error names the line.
pub fn parse(text: &str) -> Result<Vec<Item>, String> {
    let mut out = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        let f: Vec<&str> = line.split_whitespace().collect();
        let bad = |e: String| format!("recipe line {} {:?}: {}", i + 1, raw, e);
        let n = |s: &str| num(s).map_err(bad);
        let item = match f.as_slice() {
            [] => continue,
            ["wr", a, v] => {
                let a = n(a)?;
                if !a.is_multiple_of(4) || a as usize >= acq::WINDOW_USED {
                    return Err(bad("not a register of the window".into()));
                }
                Item::Step(Step::Wr(a, n(v)?))
            }
            ["wait", ms] => Item::Step(Step::Wait(n(ms)?.min(10_000))),
            ["adc", a, r, d] => {
                let (a, r, d) = (n(a)?, n(r)?, n(d)?);
                if a > 1 || r > 0x7f || d > 0xffff {
                    return Err(bad("ADC 0 or 1, register below 0x80, 16 bits of data".into()));
                }
                Item::Step(Step::Adc(a, r as u8, d as u16))
            }
            ["adcread", a, r] => {
                let (a, r) = (n(a)?, n(r)?);
                if a > 1 || r > 0x7f {
                    return Err(bad("ADC 0 or 1, register below 0x80".into()));
                }
                Item::Step(Step::AdcRead(a, r as u8))
            }
            ["afe", ch, r, d] => {
                let (ch, r, d) = (n(ch)?, n(r)?, n(d)?);
                if !(1..=4).contains(&ch) || r > 0xff || d > 0xffff {
                    return Err(bad("channel 1 to 4, register 8 bits, data 16".into()));
                }
                Item::Step(Step::Afe(ch as usize, r as u8, d as u16))
            }
            ["select", name, v] => match select_line(name).map_err(bad)? {
                Some(name) => Item::Step(Step::Select(name, *v == "1")),
                None => continue,
            },
            ["plan", p] if PHASES.contains(p) => Item::Plan(p.to_string()),
            ["capture"] => Item::Capture,
            ["acquire"] => Item::Acquire(None),
            ["acquire", m] => Item::Acquire(Some(n(m)? & 0xf)),
            ["status"] => Item::Status,
            ["afe-power-cycle"] => Item::AfePowerCycle,
            ["label", ..] => Item::Label(line["label".len()..].trim().to_string()),
            _ => return Err(bad("unknown line".into())),
        };
        out.push(item);
    }
    Ok(out)
}

/// The steps of a capture (`mho-check.py --capture-all`): its part after `CAPTURE_MARK`, which holds steps only.
pub fn parse_capture(text: &str) -> Result<Vec<Step>, String> {
    let mut lines = text.lines().skip_while(|l| l.trim() != CAPTURE_MARK);
    if lines.next().is_none() {
        return Err(format!("no line {:?}: not a file of mho-check.py --capture-all", CAPTURE_MARK));
    }
    let mut body = String::new();
    let (mut format, mut end) = (None, false);
    for line in lines {
        match line.split_whitespace().collect::<Vec<_>>().as_slice() {
            ["format", v] => format = num(v).ok(),
            ["end"] => {
                end = true;
                break;
            }
            _ => {
                body.push_str(line);
                body.push('\n');
            }
        }
    }
    if format != Some(FORMAT) {
        return Err(format!("capture format {:?}, this build reads {}", format, FORMAT));
    }
    if !end {
        return Err("the capture ends before its line `end` (cut off?)".into());
    }
    parse(&body)?
        .into_iter()
        .map(|i| match i {
            Item::Step(s) => Ok(s),
            other => Err(format!("a capture holds steps only, not {:?}", other)),
        })
        .collect()
}

/// The recipe `mho-start` runs when the unit's capture is there and no recipe is given: the stock program's cold
/// start in its order, then the snapshot's link, configuration and channel, one acquisition and the status read.
pub const WITH_CAPTURE: &str = "label the stock program's cold start (capture), then the snapshot
capture
plan link
plan fpga
plan afe
acquire
status
";

#[cfg(test)]
mod tests {
    use super::*;

    // Break it catches: a line the executor would take for another one, a register outside the window, a PMIC or
    // panel line driven, a recipe that names an unknown phase.
    #[test]
    fn every_kind_of_line_is_read_and_wrong_ones_are_named() {
        let r = parse(
            "# a comment\nwr 0x2003c 1\nwait 5\nadc 0 0x0a 0x8078\nadcread 1 9\nafe 2 0x07 0xc440 # dc\n\
             select c131 1\nselect afe_sel_a_ch1 0\nselect vsel46 1\nplan fpga\ncapture\nacquire\nacquire 0xf\n\
             status\nafe-power-cycle\nlabel try two\n",
        )
        .unwrap();
        assert_eq!(
            r,
            [
                Item::Step(Step::Wr(0x2003c, 1)),
                Item::Step(Step::Wait(5)),
                Item::Step(Step::Adc(0, 0x0a, 0x8078)),
                Item::Step(Step::AdcRead(1, 9)),
                Item::Step(Step::Afe(2, 7, 0xc440)),
                Item::Step(Step::Select("afe_sel_c_ch2".into(), true)),
                Item::Step(Step::Select("afe_sel_a_ch1".into(), false)),
                Item::Plan("fpga".into()),
                Item::Capture,
                Item::Acquire(None),
                Item::Acquire(Some(0xf)),
                Item::Status,
                Item::AfePowerCycle,
                Item::Label("try two".into()),
            ]
        );
        for bad in ["wr 0x2003e 1", "wr 0x40000 1", "adc 2 0 0", "afe 5 0 0", "plan other", "frobnicate", "select x 1"]
        {
            assert!(parse(bad).is_err(), "{}", bad);
        }
        assert!(parse(WITH_CAPTURE).is_ok());
    }

    #[test]
    fn a_step_reads_back_as_itself() {
        for s in [
            Step::Wr(0x10120, 1),
            Step::Wait(50),
            Step::Adc(1, 0x7f, 0xffff),
            Step::AdcRead(0, 0x16),
            Step::Afe(4, 0x36, 0x1234),
            Step::Select("afe_sel_b_ch3".into(), true),
        ] {
            assert_eq!(parse(&step_line(&s)).unwrap(), [Item::Step(s.clone())]);
        }
    }

    #[test]
    fn a_capture_holds_steps_in_its_order() {
        let text = "for people\n---- oos capture ----\nformat 1\nadc 0 0x0a 0x8078\nwait 10\nafe 2 0x07 0xc440\n\
                    select reset52 1\nwr 0x2003c 0x00000001\nend\n";
        assert_eq!(
            parse_capture(text).unwrap(),
            [Step::Adc(0, 0x0a, 0x8078), Step::Wait(10), Step::Afe(2, 7, 0xc440), Step::Wr(0x2003c, 1)]
        );
        assert!(parse_capture(&text.replace("end\n", "")).is_err());
        assert!(parse_capture(&text.replace("format 1", "format 2")).is_err());
        assert!(parse_capture(&text.replace("wait 10", "acquire")).is_err());
    }
}
