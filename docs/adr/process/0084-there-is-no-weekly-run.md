# ADR 0084: There is no weekly run

- Status: proposed
- Date: 2026-11-28
- Category: process

Amends ADR 0045 (its weekly try of the newest stable Rust, job `canary`).

## Context

`.github/workflows/weekly.yml` ran once a week (issue 68) what was too slow or too rare for every pull request:
`tools/check.sh` on macOS, every test five times next to busy processes (`tools/stress-tests.sh`), the test coverage
of each crate, the image build with the simulations after it (`tools/check.sh sim`), and `tools/check.sh` with the
newest stable Rust (`canary`, ADR 0045). When a job failed it opened the issue "Weekly run failed".

Its failures came days after the change that caused them, from nobody's pull request, and stayed open as an issue
until someone took it up (#647 on 2026-11-28, the run of 2026-11-21 failed as well). The maintainer decided to remove
it.

## Decision

The scheduled weekly run is removed, with its issue. What it ran stays available by hand: `tools/check.sh` (also on
macOS, where most of the development happens), `tools/stress-tests.sh`, and the workflow Image with `sims`, which also
runs the simulations when `image.yml` changes on a branch.

## Consequences

- No job tries the newest stable Rust before the pin moves: a release the tree does not pass shows when a pull
  request bumps `rust-toolchain.toml` (ADR 0045, [docs/fork/deps.md](../../fork/deps.md)).
- The simulations of the image (firewall, DHCP, update and the other scripts in containers and virtual machines) run
  only by hand, when `image.yml` changes, or before a release when someone asks for them; a change of a script in
  `rootfs/` is tried on the unit (ADR 0069) but not in the simulations unless they are run.
- A test that depends on how fast the computer is, or that fails only on macOS, shows when someone runs it there; the
  rule of ADR 0020 stays.
- Worth a second look when such a failure reaches a release that a scheduled run would have caught.

## References

`.github/workflows/image.yml` (`sims`), `tools/check.sh`, `tools/stress-tests.sh`, `rust-toolchain.toml`, issue 68,
issue 647.
