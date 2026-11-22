# Fixtures (copied from the owner's x17 R2 hardware report; tests never read the originals)

| Id | File | Source | Captured | sha256 of the source |
|---|---|---|---|---|
| F1 | `f1-awcc-probe-2026-10-07.txt` | `hardware-report/raw/04_awcc_thermal_information.txt` (elevated, read-only `awcc_read.ps1`) | 2026-10-07 21:58 +06:00, Alienware x17 R2, BIOS 1.32.0 | `a49917eab98899980bc61796c685f32b9b083805f169f02f146b0af336cbbf94` |
| F2 | `f2-fan-fault-2026-10-07.csv` | `hardware-report/raw/awcc_fans.csv` (`awcc_poll.ps1`, every ~2 s, G-Mode) | 2026-10-07 22:38-22:56 +06:00 | `64542447f33fb3d3a5eda9b48bead890e32d5a3f25cdfcc0e5c06db28979a3e3` |

F2 is the fault trace: fan 0x3B reads 0 RPM in all 535 samples while CPU sensor 0x01 sits at 98-100 C.
F3s (a healthy trace) is synthesised in the tests from F2 by giving 0x3B its partner 0x32's speed; replace it
with a real capture (F3r) once the fan has been checked or serviced.
