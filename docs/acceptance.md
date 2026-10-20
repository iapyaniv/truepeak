# Acceptance checks

Run `just test` for formatting, feature checks, Clippy, and tests. Run `just validate` to require the FFmpeg comparisons and checksum-verified EBU vectors as well. Earlier release measurements are recorded in [validation results](validation.md).

| Release | Check | Acceptance requirement | Measured result |
|---|---|---|---|
| 0.1.1 | Range true-peak measurement | 50.15625 Hz sine at 48 kHz, amplitude 0.99: `0..76800` agrees with the continuous signal within 0.001 dB; sliced input exceeds +1 dBTP. Block partitions are invariant, cancellation returns no partial result, and refined click positions respect both range boundaries. Existing fixtures match `0..len` bit for bit. | At 8x: continuous and range −0.08728187 dBTP, standalone slice +1.05724295 dBTP. Range tests cover 4x/8x/16x, invalid and empty ranges, absolute positions, channel selection, context progress, and cancellation in both channels. Whole-file equivalence covers tone sweeps, clicks, silence, empty input, and EBU vectors. |
