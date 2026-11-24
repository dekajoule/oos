# ADR 0082: the MHO900 measures as a preview, from the unit's own recordings of the stock system, and comes to main in one pull request

- Status: proposed
- Date: 2026-11-24
- Category: hardware (touches ADR 0010, ADR 0015, ADR 0028, ADR 0067, ADR 0081; process: the size of a pull request)

## Context

ADR 0028 brought the MHO900's platform to OOS without its acquisition. Since then the work on the branch `oos-mho`, in
test builds `mho-dev-1` to `mho-dev-50` and with three owners' units (two MHO98, one MHO934; GitHub issues #4, #248,
#491), has made the acquisition board measure (docs/mho.md, docs/protocols/mho-acquisition.md):

- **verified on units:** the start of the board from the unit's own recordings of the stock system (ADR 0081), the
  channels alone and together, their scales, time bases up to 4 GSa/s, offset, AC/GND coupling, the bandwidth limit,
  the edge trigger, 50 and 100 ms/div, roll, peak detect, the counter, both generators with every shape, Bode with the
  built-in generator, the fan;
- **not yet verified, or not right yet:** the logic analyser, 50 ohm, a fixed memory depth, the two ADCs' alignment at
  4 GSa/s (12 to 14 ns apart under OOS, so fast edges are wrong there and can trigger on the other slope), the gains of
  an MHO934 started without a capture of the stock system's cold start.

The board needs per unit what the stock system sets: the scope owner records it once under the stock system
(`tools/mho-setup.py record`, about 45 minutes) and copies it to the scope (`install`). Without those recordings the
build shows the demo signal, as under ADR 0028.

The work is 143 files and about 11,400 lines on top of main. Split into the usual pull requests of about 400 lines it
would be 20 to 25, each touching the same few files of `hal/src/mho/` and `ui/src/board/mho/`, and none of them could
carry the label `unit-tested`: there is no MHO900 at the bench, only the owners' reports.

## Decision

The MHO900's acquisition is part of OOS as a **preview**: it runs only on a unit whose owner put its recordings on the
scope, and the family's state in docs/porting.md stays "in development" until the criteria of docs/mho.md ("Setting a
unit up (towards a preview)") are met. The work comes to main in **one pull request**, held for the maintainer's
review and merge; the owners' reports, named in it, stand in for the run on a unit. This supersedes ADR 0028.

## Alternatives

- **A series of pull requests by topic.** Easier to review one by one, but 20 to 25 of them on the same files, each
  with the same missing run on a unit, would take weeks and gain little: the parts only make sense together, and the
  DHO800/DHO900 is proved unchanged as a whole by the comparison.
- **Keep the MHO900 on its branch.** Every change of main would have to be merged into it by hand (the last merge
  took the board macro, the model crate and per-family tests), and contributors would keep building against a branch.

## Consequences

- The DHO800/DHO900 is unchanged: `tools/compare-base.sh` against main finds the pictures, the register sequences and
  the live run equal; tests that check its hardware or its platform are built for the `dho` family only.
- Functions not verified on a unit are reached only from the screen of a unit that runs the MHO900 build with its
  recordings; nothing of it runs on a DHO800/DHO900 (ADR 0010).
- Later MHO900 work comes in the usual pull requests on main; `oos-mho` is not needed any more once this is merged.
- Worth a second look when an MHO900 is at a bench, or when the board can start without the stock recordings.

## References

docs/mho.md, docs/protocols/mho-acquisition.md, ADR 0028, ADR 0067, ADR 0081; the branch `oos-mho` and the test
builds `mho-dev-N`; GitHub issues #4, #248, #491.
