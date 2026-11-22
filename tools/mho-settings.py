#!/usr/bin/env python3
# Copyright (C) 2026 dekajoule
# SPDX-License-Identifier: GPL-2.0-or-later
"""Record what the stock MHO900 system sets for each setting, for OOS to set the same
(docs/protocols/mho-acquisition.md, section 7).

  mho-settings.py <scope-ip> [--adb-port 55555] [--scpi-port 5555] [--yes] [--quick]

Needs tools/mho-check.py next to it (its adb, SCPI, trace and register helpers) and the same set-up: network adb as
root, SCPI on port 5555, the stock system running. Nothing may be connected to the inputs.

It steps the stock system through the settings over SCPI, as a user would at the front panel, and records for each:
the settings as the scope reports them, the FPGA's configuration registers (pcie2dev, one address each, the list of
mho-check.py --snapshot), the words sent to the front ends while the setting changed (the trace event of --afe), the
select lines, and for the settings that change the sampling, the 128 registers of each ADC (read transfers through the
FPGA's ADC bridge, as --snapshot). The steps:

- the time bases from 2 ns/div to 100 ms/div with CH1 alone, and with all four channels on (the ADCs' sampling);
- every combination of channels on, at 1 us/div (the ADCs' input selection);
- every scale of each channel alone, at 200 us/div (the front ends' words and the lanes' gain and offset).

Then it sets everything back. About 15 minutes (--quick: fewer time bases, about 6). It writes
mho-settings-<model>-<date>.txt; its last part, after `---- oos settings ----`, is what OOS reads.
"""
import argparse
import datetime
import importlib.util
import itertools
import os
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
SETTINGS_MARK = "---- oos settings ----"
TIMEBASES = ("2e-9", "5e-9", "1e-8", "2e-8", "5e-8", "1e-7", "2e-7", "5e-7", "1e-6", "2e-6", "5e-6", "1e-5", "2e-5",
             "5e-5", "1e-4", "2e-4", "5e-4", "1e-3", "2e-3", "5e-3", "1e-2", "2e-2", "5e-2", "1e-1")
TIMEBASES_QUICK = ("2e-9", "1e-8", "1e-7", "1e-6", "1e-5", "1e-4", "2e-4", "1e-3", "1e-2", "1e-1")
COMBOS = [c for k in (1, 2, 3, 4) for c in itertools.combinations((1, 2, 3, 4), k)]


def load_check():
    path = os.path.join(HERE, "mho-check.py")
    if not os.path.exists(path):
        sys.exit("mho-settings.py needs mho-check.py next to it (from the same release); nothing done")
    spec = importlib.util.spec_from_file_location("mho_check", path)
    mc = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mc)
    return mc


def channels_commands(on):
    """SCPI lines that leave exactly the channels in `on` switched on (each switched on before the others go off, so
    that one channel is always on)."""
    return ([f":CHANnel{n}:DISPlay ON" for n in on]
            + [f":CHANnel{n}:DISPlay OFF" for n in (1, 2, 3, 4) if n not in on])


def plan(quick=False):
    """[(name, [SCPI commands], read the ADCs)] in order: time bases with CH1 alone and with all four, the channel
    combinations, each channel's scales."""
    tbs = TIMEBASES_QUICK if quick else TIMEBASES
    base = [":CHANnel1:DISPlay ON", ":TRIGger:MODE EDGE", ":TRIGger:EDGE:SOURce CHANnel1", ":TRIGger:EDGE:LEVel 0",
            ":RUN"] + [c for n in (1, 2, 3, 4) for c in (f":CHANnel{n}:PROBe 1", f":CHANnel{n}:IMPedance OMEG",
                                                          f":CHANnel{n}:COUPling DC", f":CHANnel{n}:BWLimit OFF",
                                                          f":CHANnel{n}:OFFSet 0")]
    steps = [("base", base, False)]
    for chans, label in (((1,), "CH1"), ((1, 2, 3, 4), "CH1-4")):
        steps.append((f"{label} on", channels_commands(chans) + [f":CHANnel{n}:SCALe 1" for n in chans], False))
        steps += [(f"{label} tdiv={tb}", [f":TIMebase:MAIN:SCALe {tb}"], True) for tb in tbs]
    steps.append(("tdiv=1e-6", [":TIMebase:MAIN:SCALe 1e-6"], False))
    for c in COMBOS:
        steps.append((f"chans={','.join(map(str, c))} tdiv=1e-6", channels_commands(c), True))
    steps.append(("tdiv=2e-4", [":TIMebase:MAIN:SCALe 2e-4"], False))
    for n in (1, 2, 3, 4):
        steps.append((f"CH{n} alone", channels_commands((n,)), False))
        steps += [(f"CH{n} scale={v}", [f":CHANnel{n}:SCALe {v}"], False) for v in MC.SCALES]
        steps.append((f"CH{n} scale=1 again", [f":CHANnel{n}:SCALe 1"], False))
    return steps


STATE_QUERIES = ([f":CHANnel{n}:DISPlay?" for n in (1, 2, 3, 4)] + [f":CHANnel{n}:SCALe?" for n in (1, 2, 3, 4)]
                 + [":TIMebase:MAIN:SCALe?", ":ACQuire:SRATe?", ":ACQuire:MDEPth?"])


def state_line(answers):
    """`state KEY=VALUE ...` of the queries' answers, the keys short: on1..on4, scale1..scale4, tdiv, srate, mdepth."""
    keys = [f"on{n}" for n in (1, 2, 3, 4)] + [f"scale{n}" for n in (1, 2, 3, 4)] + ["tdiv", "srate", "mdepth"]
    return "state " + " ".join(f"{k}={(v or '-').replace(' ', '')}" for k, v in zip(keys, answers))


def setting_lines(name, commands, state, regs, adc, frames, selects):
    """One setting of the part for OOS: `setting NAME`, its commands, its state, `fpga`, `adc`, `afe` (the frames sent
    while it changed, in order) and `select` lines, `endsetting`."""
    out = [f"setting {name.replace(' ', '_')}"] + [f"scpi {c}" for c in commands] + [state]
    out += [f"fpga 0x{a:x} 0x{v:08x}" for a, v in sorted(regs.items())]
    out += [f"adc {n} 0x{r:02x} 0x{d:04x}" for n, vals in adc for r, d in sorted(vals.items())]
    out += [f"afe {ch[2:]} 0x{r:02x} 0x{d:04x}" for ch, r, d in frames]
    out += [f"select {name_} {v}" for name_, v in selects]
    return out + ["endsetting"]


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("host")
    ap.add_argument("--adb-port", type=int, default=55555)
    ap.add_argument("--scpi-port", type=int, default=5555)
    ap.add_argument("--yes", action="store_true")
    ap.add_argument("--quick", action="store_true", help="fewer time bases (about 6 minutes)")
    a = ap.parse_args()
    adb = MC.Adb(f"{a.host}:{a.adb_port}")
    adb.connect()
    if "sparrow3_" not in adb.sh("ls /rigol/FPGA"):
        sys.exit("this is not an MHO900 under its stock system. Under OOS? Power the scope off and on: OOS starts "
                 "once after an install, the next start is the stock system. Nothing done")
    if not adb.is_root():
        sys.exit("mho-settings.py needs adb root; nothing done")
    if "pcie2dev" not in adb.sh(f"ls {MC.PCIE2DEV}"):
        sys.exit(f"{MC.PCIE2DEV} is missing; nothing done")
    tracer = MC.Tracer(adb)
    why = tracer.available()
    if why:
        sys.exit(f"mho-settings.py: {why}; nothing done")
    steps = plan(a.quick)
    if not a.yes:
        ans = input(f"mho-settings.py steps the stock system through {len(steps)} settings over SCPI (time bases, "
                    "channels, scales) and records what it sets in the FPGA, the ADCs and the front ends, then sets "
                    f"everything back (about {6 if a.quick else 15} minutes). Nothing may be connected to the inputs. "
                    "Go on? [y/N] ")
        if ans.strip().lower() not in ("y", "yes", "j", "ja"):
            sys.exit("declined; nothing done")
    scpi = MC.Scpi(a.host, a.scpi_port)
    idn = MC.idn_without_serial(scpi.query("*IDN?"))
    model = (idn.split(",")[1].strip() if idn.count(",") >= 3 else "MHO900").replace(" ", "")
    code = adb.sh("cat /sys/class/hdcode/hdcode_dev/hdcode_value").strip()
    cmds = MC.snapshot_restore_list()
    keep = {c + "?": scpi.query(c + "?") for c in MC.query_list(cmds) + [":TRIGger:STATus"]}
    levels = {}
    for g, line in MC.gpio_lines(adb.sh("cat /sys/kernel/debug/gpio")).items():
        if g in MC.traced_lines():
            levels[g] = 1 if line.split()[-1] == "hi" else 0
    now = f"{datetime.datetime.now():%Y-%m-%d %H:%M}"
    out = [f"mho-settings (OOS tools/mho-settings.py, mho-check {MC.VERSION}), {now}",
           f"instrument: {idn}", f"hardware code: {code}", f"settings before: {len(keep)} kept"]
    part = [SETTINGS_MARK, "format 1", f"model {model}", f"hwcode {code}"]
    rep = MC.Report()
    t_start = time.time()
    try:
        tracer.start(buffer_kb=8192)
        try:
            for i, (name, commands, read_adc) in enumerate(steps):
                print(f"  {i + 1:3}/{len(steps)} {name}")

                def act():
                    for c in commands:
                        scpi.write(c)
                ev = MC.parse_trace(tracer.record(act, seconds=1.0))
                err = scpi.errors()
                frames = [reg for t, cs, n, rise, fall, miso in MC.decode_bus(ev, MC.AFE_BUS, levels)
                          for reg in [MC.afe_register(cs, n, rise)] if reg]
                for _, g, op, v in ev:
                    if op == "set":
                        levels[g] = v
                state = state_line([scpi.query(q) for q in STATE_QUERIES])
                regs = {}
                for k in range(0, len(MC.SNAPSHOT_REGS), MC.SHELL_CHUNK * 4):
                    regs.update(adb.regs(MC.SNAPSHOT_REGS[k:k + MC.SHELL_CHUNK * 4]))
                adc = []
                if read_adc:
                    for n in (0, 1):
                        first = MC.adc_pass(adb, n)
                        good = {r: x & 0xFFFF for r, x in enumerate(first) if x & (1 << 16)}
                        adc.append((n, good))
                selects = [(MC.SELECT_LINES[g].replace(" ", ""), levels.get(g, 0)) for g in sorted(MC.SELECT_LINES)]
                out.append(f"{name}: {len(frames)} front-end frames, {len(regs)} FPGA words, "
                           f"{sum(len(v) for _, v in adc)} ADC registers"
                           + (f"; SCPI: {'; '.join(err)}" if err else ""))
                part += setting_lines(name, commands, state, regs, adc, frames, selects)
        finally:
            left = tracer.stop()
            out.append("trace settings: " + ("as before" if not left else "NOT ALL SET BACK: " + "; ".join(left)))
    finally:
        MC.restore(scpi, rep, keep, cmds)
        out += [f"{s} {k} {t}: {d}" for k, t, s, d in rep.results]
    out.append(f"took {time.time() - t_start:.0f} s")
    out += ["", "The part below is for OOS (copy the file to the scope as /oos/mho-settings.txt); please leave it "
            "as it is.", *part, "end"]
    fname = f"mho-settings-{model}-{datetime.datetime.now():%Y%m%d-%H%M%S}.txt"
    with open(fname, "w") as f:
        f.write("\n".join(out) + "\n")
    print(f"written: {fname} - please attach it to the GitHub issue")


MC = load_check()

if __name__ == "__main__":
    main()
