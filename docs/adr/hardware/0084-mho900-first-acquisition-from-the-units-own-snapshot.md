# ADR 0084: the MHO900's first acquisition starts its board from the unit's own snapshot of the stock system

- Status: proposed
- Date: 2026-11-22
- Category: hardware

## Context

ADR 0028 ported the MHO900's platform without its acquisition. Since then the bring-up has run on two units (an MHO98
and an MHO934, mho-dev-6, GitHub issues #4 and #248, verified on the unit): the clock chip takes the unit's table, the
FPGA its image, PCIe comes up and both ADCs answer behind the FPGA's bridge. What is still missing for a first
acquisition is the start of the two ADCs (JESD204), of the receiver in the FPGA and of the front ends. Their registers
are known only in part. The front ends' words differ from unit to unit (their calibration, assumed,
docs/protocols/mho-acquisition.md section 8), and the ADCs' register set and masks are not known.

Working these out one register at a time takes a round trip to an owner for every step, and the testers asked for
bigger steps (issue #4, 2026-11-22). The read-out side is already the DHO800/DHO900's: 169 of the 235 offsets OOS uses
there are used by the stock MHO900 system at the same offset for the same function, and the sample format is the same.

## Decision

The first acquisition is started from a snapshot of one setting of the stock system on the same unit
(`tools/mho-check.py --snapshot`): its front-end words, the FPGA's configuration registers and the ADCs' registers are
written back by `oos-fpgad mho-start`, by hand. Then one acquisition is armed and read with the sequences of the
DHO800/DHO900. The values come from the unit's own file on the unit; OOS ships none (ADR 0013).

## Alternatives

- Working out the ADC, JESD204 and front-end settings register by register: needed for a scope that sets every
  range by itself, but many round trips before anything is measured. It follows the first acquisition, which it can
  then check against.
- A warm hand-over (starting OOS from the running stock system without a power cycle): it worked on the DHO800/DHO900
  because its FPGA keeps running; the MHO900's supplies and FPGA image depend on GPIO lines that a reset of the
  processor leaves undriven (assumed), so the board would likely not survive it.

## Consequences

- The first acquisition measures only at the snapshot's setting (one channel, one scale, one time base). Other
  settings need either more snapshots or the meaning of the registers.
- The steps written back include registers whose meaning is not known; a wrong order or a register that must not be
  written back shows only on a unit. `mho-start` reads everything back and reports it, so one run says where it
  stopped.
- Nothing starts by itself (ADR 0010): `oos-fpgad mho-start` and `oos-mho-live` are run by an owner over SSH, and a
  power cycle undoes all of it.
- Worth a second look once the ADCs' and front ends' registers are understood: then the board can be set from the
  model, and the snapshot becomes a check.

## References

- docs/protocols/mho-acquisition.md, sections 7 and 10; docs/mho.md, "First acquisition"
- `hal/src/mho/snapshot.rs`, `fpgad/src/mho/start.rs`, `ui/src/board/mho/live.rs`, `rootfs/board/mho/bin/oos-mho-live`
- ADR 0028 (the platform port), ADR 0013 (no vendor data), ADR 0010 (nothing unverified by default)
