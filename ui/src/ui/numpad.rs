// Copyright (C) 2026 dekajoule
// SPDX-License-Identifier: GPL-2.0-or-later

//! The number pad (issue #22): a tap on the middle of a value row whose number can be typed (the scale and offset of
//! a channel, the time base and its position, the trigger level) opens a picker of keys: the digits, the point, the
//! sign, and a key for each prefix of the row's unit (`nV` ... `kV`, `ns` ... `ks`), which ends the entry and sets the
//! row. The value is taken as SCPI takes it (`:CHANnel<n>:SCALe`, `:TIMebase:SCALe`, ...): onto the nearest step of a
//! ladder, the fine scale onto its calibrated row.

use super::*;

/// The row the pad enters into, while it is open.
#[derive(Default)]
pub(super) struct NumPad {
    pub target: Option<Item>,
}

/// The keys, four in a row; a prefix key (`n`, `u`, `m`, `1`, `k`) is shown with the row's unit and ends the entry.
const KEYS: [&str; 20] =
    ["7", "8", "9", "n", "4", "5", "6", "u", "1", "2", "3", "m", "0", ".", "-", "1x", "Del", "Clear", "Cancel", "k"];

/// The factor and the shown prefix of a prefix key, from `SI_PREFIXES`.
fn prefix(key: &str) -> Option<(f64, &'static str)> {
    let shown = match key {
        "n" | "m" | "k" => key,
        "u" => "\u{b5}",
        "1x" => "",
        _ => return None,
    };
    SI_PREFIXES.iter().copied().find(|&(_, p)| p == shown)
}

/// Longest entry, in characters.
const ENTRY_MAX: usize = 12;

impl Ui {
    /// The unit of a row whose number can be typed, or none.
    pub(super) fn numpad_unit(&self, it: Item) -> Option<&'static str> {
        let m = &self.m;
        match it {
            Item::ChScale(c) | Item::ChOffset(c) if c < NCH => Some(m.unit(c)),
            Item::HScale | Item::HDelay => Some("s"),
            Item::TrigLevel if m.trig_src < NCH => Some(m.unit(m.trig_src)),
            // any amplitude of the sweep, not only the knob's steps, in the unit of the menu (#52)
            Item::BodeAmp if m.bode_rms => Some("Vrms"),
            Item::BodeAmp => Some("Vpp"),
            _ => None,
        }
    }

    /// Opens the pad for row `it`, with an empty entry.
    pub(super) fn open_numpad(&mut self, it: Item) {
        self.numpad.target = Some(it);
        self.keypad.clear();
        self.open_picker(Item::NumEntry);
    }

    /// The keys of the pad, the prefix keys with the unit of its row.
    pub(super) fn numpad_keys(&self) -> Vec<String> {
        let unit = self.numpad.target.and_then(|it| self.numpad_unit(it)).unwrap_or("");
        KEYS.iter()
            .map(|&k| match prefix(k) {
                Some((_, p)) => format!("{}{}", p, unit),
                None => k.to_string(),
            })
            .collect()
    }

    /// An entry pad that is open (the number pad, the generator's frequency, a hex value of a bus trigger): its title
    /// and the entry so far, which shows large above the keys. The pads open centred and larger (ADR 0077).
    pub(super) fn pad_entry(&self) -> Option<(String, String)> {
        let p = self.pick.filter(|_| self.menu_open && self.menu == MenuId::Pick)?;
        if Self::hex_item(p.it) {
            return Some((self.item_label(p.it).to_string(), format!("0x{}_", self.keypad)));
        }
        let title = match p.it {
            Item::NumEntry => self.numpad.target.map_or(String::new(), |it| self.item_label(it).to_string()),
            Item::AfgFreqEntry => format!("Frequency (max {})", si_short(self.m.gen_max_hz(), "Hz")),
            _ => return None,
        };
        Some((title, format!("{}_", self.keypad)))
    }

    /// The value the row of the number pad (or the generator's frequency) has now, shown dim in its field until
    /// something is typed.
    pub(super) fn pad_now(&self) -> Option<String> {
        if !self.keypad.is_empty() || self.pad_entry().is_none() {
            return None;
        }
        let it = match self.pick?.it {
            Item::NumEntry => self.numpad.target?,
            Item::AfgFreqEntry => Item::AfgFreq,
            _ => return None,
        };
        Some(self.value(it))
    }

    /// Key `k` of the pad was tapped.
    pub(super) fn numpad_key(&mut self, k: usize) {
        let Some(&key) = KEYS.get(k) else { return };
        if let Some((factor, _)) = prefix(key) {
            let typed = crate::words::num64(&self.keypad);
            let target = self.numpad.target.take();
            self.close_picker();
            if let (Some(v), Some(it)) = (typed, target) {
                self.numpad_set(it, v * factor);
            }
            return;
        }
        match key {
            "Cancel" => {
                self.numpad.target = None;
                self.close_picker();
            }
            "Clear" => self.keypad.clear(),
            "Del" => {
                self.keypad.pop();
            }
            "." if self.keypad.contains('.') => {}
            "-" if !self.keypad.is_empty() => {}
            d if self.keypad.chars().count() < ENTRY_MAX => self.keypad.push_str(d),
            _ => {}
        }
    }

    /// Sets row `it` to `v` (in its unit), as SCPI would.
    fn numpad_set(&mut self, it: Item, v: f64) {
        // the amplitude is kept as f64 in Vpp, not rounded to f32 as the rows below, within the generator's range
        if it == Item::BodeAmp {
            use crate::settings::bode::{AMP_RANGE, VPP_PER_VRMS};
            let (k, unit) = if self.m.bode_rms { (VPP_PER_VRMS, "Vrms") } else { (1.0, "Vpp") };
            // within the range; the clamp below only takes up the rounding of Vrms to Vpp at its ends
            let vpp = v * k;
            if !(AMP_RANGE.0 * (1.0 - 1e-9)..=AMP_RANGE.1 * (1.0 + 1e-9)).contains(&vpp) {
                let (lo, hi) = (si_short(AMP_RANGE.0 / k, unit), si_short(AMP_RANGE.1 / k, unit));
                return self.toast(&format!("The amplitude must be {} to {}", lo, hi));
            }
            // as - and + of the Bode rows: no settings_changed (setting_items.rs)
            self.m.bode_amp = vpp.clamp(AMP_RANGE.0, AMP_RANGE.1);
            return;
        }
        let m = &mut self.m;
        let v = v as f32;
        match it {
            Item::ChScale(c) => {
                if v <= 0.0 {
                    return self.toast("The scale must be above 0");
                }
                let old = m.ch[c].volts_per_div();
                let want = v / m.ch[c].probe;
                (m.ch[c].vscale, m.ch[c].vfine) = Self::vscale_for(want, m.fine_v);
                // around ground: the zero line keeps its place on the screen
                if m.v_expand_gnd && old > 0.0 {
                    m.ch[c].offset *= m.ch[c].volts_per_div() / old;
                }
            }
            Item::ChOffset(c) => m.ch[c].offset = v,
            Item::HScale => {
                if v <= 0.0 {
                    return self.toast("The scale must be above 0");
                }
                let old = m.time_per_div();
                (m.tscale, m.tfine) = (nearest_step(&tscales(), v), 1.0);
                m.keep_expand_point(old);
            }
            Item::HDelay => m.delay = v,
            Item::TrigLevel => m.trig_level = v,
            _ => return,
        }
        self.settings_changed();
    }

    /// The `vscale` step and its fine factor for `want` V/div at the BNC: the nearest step of the ladder, or with the
    /// fine scale the first row of the calibrated table at or above it, so the range never cuts the signal off (SCPI's
    /// `:CHANnel<n>:SCALe` and the pad).
    pub(crate) fn vscale_for(want: f32, fine: bool) -> (usize, f32) {
        if !fine {
            return (nearest_step(&VSCALES, want), 1.0);
        }
        let rows = board().vscale_steps_nv();
        let nv = want as f64 * 1e9 * (1.0 - 1e-6);
        let row = rows.iter().copied().find(|&k| k as f64 >= nv).unwrap_or(rows[rows.len() - 1]);
        let row = (row as f64 * 1e-9) as f32;
        let i = VSCALES.iter().rposition(|&s| s <= row * 1.000001).unwrap_or(0);
        let f = row / VSCALES[i];
        (i, if (f - 1.0).abs() < 1e-6 { 1.0 } else { f })
    }

    pub(super) fn numpad_kind(it: Item) -> Option<Kind> {
        (it == Item::NumEntry).then_some(Kind::Choice)
    }

    pub(super) fn numpad_label(it: Item) -> Option<&'static str> {
        (it == Item::NumEntry).then_some("Enter a value")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(u: &mut Ui, keys: &[&str]) {
        let shown = u.numpad_keys();
        for k in keys {
            let i = shown.iter().position(|s| s == k).unwrap_or_else(|| panic!("no key {:?} in {:?}", k, shown));
            u.activate(Item::PickOpt(i));
        }
    }

    fn opened(it: Item) -> Ui {
        let mut u = crate::ui::testkit::demo_ui();
        u.open_menu(MenuId::Channel(0));
        u.open_numpad(it);
        assert!(u.menu == MenuId::Pick && u.pick.is_some_and(|p| p.it == Item::NumEntry));
        u
    }

    #[test]
    fn a_typed_scale_offset_time_and_level_are_set() {
        let mut u = opened(Item::ChScale(0));
        assert_eq!(u.numpad_keys()[11], "mV");
        press(&mut u, &["2", "0", "0", "mV"]);
        assert!((u.m.ch[0].volts_per_div() - 0.2).abs() < 1e-6, "{}", u.m.ch[0].volts_per_div());
        assert!(u.menu != MenuId::Pick && u.numpad.target.is_none(), "the pad closes");

        let mut u = opened(Item::ChOffset(0));
        press(&mut u, &["-", "1", ".", "5", "V"]);
        assert_eq!(u.m.ch[0].offset, -1.5);

        let mut u = opened(Item::HScale);
        press(&mut u, &["5", "\u{b5}s"]);
        assert!((u.m.time_per_div() - 5e-6).abs() < 1e-12, "{}", u.m.time_per_div());

        let mut u = opened(Item::HDelay);
        press(&mut u, &["1", "2", "ns"]);
        assert!((u.m.delay - 12e-9).abs() < 1e-15);

        let mut u = opened(Item::TrigLevel);
        press(&mut u, &["3", "0", "0", "mV"]);
        assert!((u.m.trig_level - 0.3).abs() < 1e-6);
    }

    // Break it catches (ADR 0077): the pad opened small beside its menu, its entry in the title line
    #[test]
    fn the_pad_opens_centred_with_large_keys_and_the_entry_above_them() {
        let u = opened(Item::ChScale(0));
        let d = u.dlg_layout().expect("the pad is a dialog");
        let centre = d.r.x + d.r.w / 2;
        assert!((centre - u.scr.w / 2).abs() <= 1, "centred: {:?} on {}", d.r, u.scr.w);
        assert!(d.rows.iter().all(|r| r.ctrl.h > DLG_CTRL), "larger keys");
        assert!(d.rows.iter().all(|r| r.ctrl.y >= d.r.y + DLG_HEAD + PAD_ENTRY_H), "the keys under the entry field");
        assert_eq!(u.menu_title(MenuId::Pick), u.item_label(Item::ChScale(0)), "the title is the row");
        assert_eq!(u.pad_now(), Some(u.value(Item::ChScale(0))), "before typing, the field shows the value now");
    }

    #[test]
    fn the_entry_keys_and_cancel() {
        let mut u = opened(Item::ChOffset(0));
        let before = u.m.ch[0].offset;
        press(&mut u, &["1", ".", ".", "2", "-", "Del", "3"]);
        assert_eq!(u.keypad, "1.3", "one point, the sign only first, Del takes the last");
        assert_eq!(u.pad_entry().map(|e| e.1), Some("1.3_".to_string()), "the entry shows above the keys");
        assert_eq!(u.pad_now(), None, "the value now shows only while nothing is typed");
        press(&mut u, &["Clear", "Cancel"]);
        assert_eq!(u.m.ch[0].offset, before);
        assert!(u.menu != MenuId::Pick && u.numpad.target.is_none());
        // nothing typed: the unit key closes and sets nothing; a scale of 0 is refused
        let mut u = opened(Item::ChScale(0));
        let scale = u.m.ch[0].volts_per_div();
        press(&mut u, &["V"]);
        assert_eq!(u.m.ch[0].volts_per_div(), scale);
        let mut u = opened(Item::ChScale(0));
        press(&mut u, &["0", "V"]);
        assert_eq!(u.m.ch[0].volts_per_div(), scale);
        assert_eq!(u.toast.as_ref().unwrap().0, "The scale must be above 0");
    }

    /// A tap on the middle of a value row of a number opens the pad; of another value row it does not.
    #[test]
    fn a_tap_on_the_value_opens_the_pad() {
        let mut u = crate::ui::testkit::demo_ui();
        u.open_menu(MenuId::Channel(0));
        let row = |u: &Ui, it: Item| u.dlg_layout().unwrap().rows.into_iter().find(|r| r.it == it).unwrap().ctrl;
        let r = row(&u, Item::ChScale(0));
        u.tap(r.x + r.w / 2, r.y + r.h / 2);
        assert_eq!(u.numpad.target, Some(Item::ChScale(0)));
        assert!(u.pick.is_some_and(|p| p.it == Item::NumEntry));
    }
}
