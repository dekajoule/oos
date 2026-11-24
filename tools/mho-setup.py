#!/usr/bin/env python3
# Copyright (C) 2026 dekajoule
# SPDX-License-Identifier: GPL-2.0-or-later
"""Sets an MHO900 up for OOS in three steps from a computer (docs/mho.md, "Setting a unit up"):

  mho-setup.py HOST record    under the stock system: the board's cold start and the snapshot (mho-check.py
                              --capture-all, the probe compensation signal on the channel it names, about 5 minutes),
                              then every setting OOS needs (mho-settings.py --extended, nothing connected, about 40
                              minutes). The files go to a new folder mho-setup-<date>.
  mho-setup.py HOST install [DIR]
                              under OOS: copies the newest recordings of DIR (default: the newest mho-setup-* folder)
                              to the scope's /oos under the names OOS reads, then starts the acquisition
                              (oos-mho-live). From then on the scope starts it at every boot.
  mho-setup.py HOST report    under OOS: runs oos-mho-report on the scope and fetches its file, for the GitHub issue.

record changes settings of the stock system over SCPI and sets them back, and restarts the stock program once (as
mho-check.py --capture-all does); it writes nothing to the scope's partitions. install and report use ssh and scp as
root, as the docs describe. Needs mho-check.py, mho-settings.py and mho-poll next to this script (all in the release).
"""

import argparse
import datetime
import glob
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
# what OOS reads in /oos, from the files the recorders write
FILES = [
    ("mho-capture-", "mho-capture.txt", True),
    ("mho-snapshot-", "mho-snapshot.txt", True),
    ("mho-settings-", "mho-settings.txt", True),
    ("mho-afe-init-", "mho-afe-init.txt", False),
]
SSH = ["-o", "ConnectTimeout=10"]


def newest(folder, prefix):
    """The newest file of `folder` whose name starts with `prefix` and ends in .txt, or None."""
    found = sorted(glob.glob(os.path.join(folder, prefix + "*.txt")), key=os.path.getmtime)
    return found[-1] if found else None


def plan_install(folder):
    """[(local file, name in /oos)] and the required ones missing."""
    out, missing = [], []
    for prefix, name, required in FILES:
        f = newest(folder, prefix)
        if f:
            out.append((f, name))
        elif required:
            missing.append(prefix + "*.txt")
    return out, missing


def run(cmd, **kw):
    print("+ " + " ".join(cmd), flush=True)
    return subprocess.run(cmd, **kw).returncode


def record(a):
    folder = os.path.abspath(f"mho-setup-{datetime.datetime.now():%Y%m%d-%H%M%S}")
    os.makedirs(folder)
    ports = ["--adb-port", str(a.adb_port), "--scpi-port", str(a.scpi_port)]
    print(f"mho-setup: recordings go to {folder}")
    print("Step 1 of 2: the cold start and the snapshot. mho-check.py names the channel for the probe compensation "
          "signal.")
    rc = run([sys.executable, os.path.join(HERE, "mho-check.py"), a.host, "--capture-all",
              "--poll", os.path.join(HERE, "mho-poll")] + ports, cwd=folder)
    if rc != 0:
        return rc
    input("Step 2 of 2: disconnect everything from the inputs, then press Enter (about 40 minutes follow) ")
    rc = run([sys.executable, os.path.join(HERE, "mho-settings.py"), a.host, "--extended", "--yes"] + ports,
             cwd=folder)
    if rc == 0:
        print(f"mho-setup: done. Install OOS, start it, then: {sys.argv[0]} {a.host} install {folder}")
    return rc


def install(a):
    folder = a.dir or (sorted(glob.glob("mho-setup-*"), key=os.path.getmtime) or [None])[-1]
    if not folder or not os.path.isdir(folder):
        print("mho-setup: no mho-setup-* folder here; name the one with the recordings", file=sys.stderr)
        return 2
    files, missing = plan_install(folder)
    if missing:
        print(f"mho-setup: {folder} lacks {', '.join(missing)} (mho-setup.py HOST record)", file=sys.stderr)
        return 2
    for local, name in files:
        if run(["scp"] + SSH + [local, f"root@{a.host}:/oos/{name}"]) != 0:
            return 1
    return run(["ssh"] + SSH + [f"root@{a.host}", "oos-mho-live"])


def report(a):
    r = subprocess.run(["ssh"] + SSH + [f"root@{a.host}", "oos-mho-report"], capture_output=True, text=True)
    path = r.stdout.strip().splitlines()[-1] if r.stdout.strip() else ""
    if r.returncode != 0 or not path.startswith("/oos/"):
        print(f"mho-setup: oos-mho-report failed: {r.stderr.strip() or r.stdout.strip()}", file=sys.stderr)
        return 1
    rc = run(["scp"] + SSH + [f"root@{a.host}:{path}", "."])
    if rc == 0:
        print(f"mho-setup: {os.path.basename(path)} is here; attach it to your GitHub issue")
    return rc


def main():
    p = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    p.add_argument("host")
    p.add_argument("step", choices=["record", "install", "report"])
    p.add_argument("dir", nargs="?", help="install: the folder with the recordings")
    p.add_argument("--adb-port", type=int, default=55555)
    p.add_argument("--scpi-port", type=int, default=5555)
    a = p.parse_args()
    return {"record": record, "install": install, "report": report}[a.step](a)


if __name__ == "__main__":
    sys.exit(main())
