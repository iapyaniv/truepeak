# Validation results

Measured 2026-10-15 on macOS, Apple M2 Max, Rust 1.96.0, FFmpeg 8.0, and just 1.50.0. Numerical and behavior checks pass. The meter meets its throughput target; the verified limiter exceeds its six-second target.

`just validate` verifies the EBU archive and runs formatting, Clippy with warnings denied, a build without default features, and tests with `serde` enabled. It requires both FFmpeg and the extracted EBU vectors. `just wasm` checks the library with all features for `wasm32-unknown-unknown`. CI defines macOS and Ubuntu checks plus a macOS job that requires both external comparisons.

## Behavior checks

Limiter fixtures use a −1 dBTP ceiling, 0.03 dB initial margin, and 8x interpolation unless specified. Detailed numeric output is available from `just validate`; displayed measurements below are rounded.

| Check | Requirement | Recorded result |
|---|---|---|
| Resampler round trip | Magnitude error ≤0.01 dB; residual <−90 dBFS RMS | Worst error 0.000117 dB; residual −106.884 dBFS RMS |
| Meter phase sweeps | Error ≤0.02 dB for each tone/phase | Worst error 0.014409 dB across factors; 0.002851 dB at 8x |
| FFmpeg comparison | ±0.1 dB at 997 Hz/5 kHz; −0.05…+0.25 at 12 kHz; −0.05…+0.5 above 12 kHz | All fixtures within bounds; detailed differences below |
| EBU cases 15–20 | Error −0.4…+0.2 dB | −0.089208…+0.009889 dB across factors/channels |
| 997 Hz at +1 dBTP | Output ≤−0.95 dBTP by both meters at 44.1/48 kHz | −1.0300 dBTP; FFmpeg −1.0; one pass |
| 17.2 kHz at +1 dBTP, 44.1 kHz | Same ceiling bound | −1.0300 dBTP; FFmpeg −1.0; one pass |
| Full-scale square, period 64, 48 kHz | Same ceiling bound | Input +2.1572, output −1.0515 dBTP; FFmpeg −1.1; one pass |
| Seeded program at +3 dBTP | Same ceiling bound at 44.1/48 kHz; ceiling met within four passes | −1.0300 dBTP; FFmpeg −1.0/−1.1; one pass |
| Bypass | Input below ceiling is copied unchanged with zero passes | Bit-exact, including negative zero/subnormals; zero reduction |
| 16/24-bit verification | Peak reported on quantized output; ceiling holds on that copy | −1.0299/−1.0300 dBTP; reported and separately measured peaks equal |
| Click alignment | Clicks at their input positions; output length matches input | Zero displacement; 14,700/16,000 frames at 44.1/48 kHz |
| Channel linking | Linked gain traces match; quiet unlinked channel has zero reduction | Linked traces bit-identical; unlinked right channel at 0 dB |
| Transient recovery | Reduction <0.5 dB after 200 ms and <0.1 dB after 1 s | 0.1020/0.0043 dB |
| Sustained 50 Hz over | Constant-gain residual <−30 dB after 500 ms | Gain −3.9911 dB; residual −45.314 dB |
| Repeatability | Identical runs and block partitions | Audio and per-channel gains bit-identical for both link modes |
| Cancellation | Processing or verification returns `Err(Cancelled)` without output | Both paths pass |
| Invalid configuration | Errors identify each invalid field | 42 configurations rejected; initial-margin endpoints accepted |
| Higher sample rates | Tone/program ceiling and click alignment at 88.2/96/176.4/192 kHz | Both meters within bounds; one pass; zero click displacement |

The [limiter tests](../tests/limiter.rs) also check exhausted pass budgets, retries from the original input, quantized bypass, and invalid streaming blocks without state changes. Blocks of 1, 7, 64, 4096, and the whole input exercise partition invariance. The linking fixture uses a 6 kHz tone with 0.35-radian phase to expose grid under-read.

## Resampler measurements

The [frequency-response test](../tests/resample.rs) uses a direct 524,288-point DFT of the public interpolator's impulse response, divided by L to recover the prototype. It checks all passband and stopband bins, including the high-rate Nyquist. The symmetric prototype has `76L + 1` taps and a round-trip delay of 76 base-rate samples.

| Factor | Taps | Ripple (dB) | Stopband attenuation (dB) | Max round-trip error (dB) | Worst residual (dBFS RMS) | 1 kHz round-trip gain (dB) |
|---|---|---|---|---|---|---|
| 4 | 305 | 0.00014805 | 101.087 | 0.00011675 | −106.914 | +0.00000853 |
| 8 | 609 | 0.00014831 | 101.159 | 0.00011715 | −106.884 | +0.00000896 |
| 16 | 1217 | 0.00014837 | 101.163 | 0.00011716 | −106.884 | +0.00000905 |

Round trips cover all six supported rates. Separate −6 dBFS tones step through 20, 50, 100, 500, 1000, 2000, 5000, 10000, 15000, 17200, 19600, and 20000 Hz with 10 ms cosine fades. Magnitude is the RMS output/input ratio after delay alignment; residual includes the fades. Seeded random Fourier components synthesized below 0.907 Nyquist provide independent band-limited noise. Tests also cover delay, flush, reset, partial decimation groups, and invalid-block atomicity.

## Meter and FFmpeg measurements

The [phase sweeps](../tests/meter.rs) cover six frequencies at 44.1 and 48 kHz, 32 phases each, and all three interpolation factors: 1,152 readings, each asserted against 0.02 dB. The worst case is 19.6 kHz at 44.1 kHz, 4x.

[FFmpeg comparisons](../tests/ffmpeg_oracle.rs) use the same 384 fixtures at 8x, float WAVs, and `ebur128=peak=true`. Differences are `truepeak − FFmpeg`, in dB. FFmpeg's 192 kHz grid can under-read a 12 kHz tone by about 0.17 dB before its summary rounds to 0.1 dB; it reports −3.2 dBTP for some −3 dBTP fixtures. This accounts for the asymmetric comparison bounds in the design.

| Rate (Hz) | Frequency (Hz) | Minimum difference (dB) | Maximum difference (dB) |
|---|---|---|---|
| 44100 | 997 | +0.000014 | +0.000014 |
| 44100 | 5000 | +0.000027 | +0.000032 |
| 44100 | 12000 | +0.000041 | +0.200052 |
| 44100 | 17200 | +0.000013 | +0.000027 |
| 44100 | 19600 | −0.002851 | +0.000352 |
| 44100 | 20000 | +0.000048 | +0.000442 |
| 48000 | 997 | +0.000014 | +0.000014 |
| 48000 | 5000 | +0.000005 | +0.000014 |
| 48000 | 12000 | +0.000014 | +0.200045 |
| 48000 | 17200 | +0.000010 | +0.000018 |
| 48000 | 19600 | −0.000027 | +0.000026 |
| 48000 | 20000 | +0.000014 | +0.000117 |

Sample-peak tests cover exact values, positions, channel selection, silence, empty channels, all rates, invalid inputs, cancellation in either channel, progress completion, and finite serde round trips.

## EBU measurements

Stated readings and tolerances come from [EBU Tech 3341, Table 1, cases 15–20](https://tech.ebu.ch/docs/tech/tech3341.pdf). The [EBU tests](../tests/ebu_oracle.rs) assert both channels independently at 4x, 8x, and 16x. Recommended 8x readings are:

| Sequence | Stated dBTP | Measured dBTP | Error (dB) |
|---|---|---|---|
| seq-3341-15 | −6.0 | −6.00025 | −0.00025 |
| seq-3341-16 | −6.0 | −6.00009 | −0.00009 |
| seq-3341-17 | −6.0 | −6.00011 | −0.00011 |
| seq-3341-18 | −6.0 | −6.00012 | −0.00012 |
| seq-3341-19 | +3.0 | +3.00989 | +0.00989 |
| seq-3341-20 | 0.0 | −0.07862 | −0.07862 |

The fetch script tries the EBU origin and then Gentoo's mirror. Either download must match the pinned 91,631,421-byte size and SHA-512 from [Gentoo's manifest](https://github.com/gentoo/gentoo/blob/master/media-libs/libebur128/Manifest):

```text
60d022fdac47ad0be2688411be9daecbff85da994d6fa4921bba6cffab841b081d8b15d9ce284ad2253efb686463450a84a0d19cb0bad7a934546cc52dd73771
```

The recorded run used the mirror after the EBU origin returned HTTP 403. Audio and archives are ignored by Git.

## Initial margin

Separate local measurements on 17 real 44.1 kHz, 16-bit masters used a −4 dBTP ceiling, one pass, 24-bit verification, and the earlier 0.005 dB initial margin. Ceiling excess ranged from −0.0034 to +0.0198 dB, or +0.0016 to +0.0248 dB above the internal target; 9 of 17 needed a second pass. These measurements motivate the 0.03 dB default. The recordings are private and are not part of the reproducible synthetic suite.

All default-ceiling fixtures in the suite finish in one pass; bypass uses zero. The largest rise above the −1.03 dBTP target in the tone/program/export checks is +0.00009969 dB, including 16-bit rounding. Without quantization it is +0.00005349 dB. The four-minute benchmark rises by +0.00270635 dB above its target, also finishing in one pass below the requested ceiling. Decimation, meter interpolation, refinement error, and export rounding contribute to the remaining target excess.

### Retry and quantized bypass

A separate retry case uses a 6 kHz tone at −37 dBFS, 44.1 kHz, a −40 dBTP ceiling, 0.01 dB initial margin, and 16-bit verification. With one allowed pass it reads −39.996803 dBTP and reports `ceiling_met=false`. With four allowed passes it finishes in two at −40.007243 dBTP.

For ceiling `C`, initial margin `m`, and first-pass excess `e`, the second target is `(C - m) - (e + 0.005)`. A fresh one-pass call with ceiling `second_target + m` must produce exactly the same audio and gain series as the retry. Using different initial and retry margins checks that they are applied separately.

Quantized bypass cases preserve float input while reporting a failed ceiling: a 16-bit case with ceiling −94.050732 reads −88.973801 dBTP; a 24-bit case with ceiling −142.215531 reads −137.138601 dBTP. Both return zero passes and reduction. Callers must inspect `ceiling_met` even on bypass.

## CLI checks

[CLI tests](../examples/truepeak-cli/main.rs) exercise 16/24/32-bit integer and float32 input, format preservation, and each output override. They check channel/frame counts, measure exported audio against the JSON summary, and verify that invalid configuration preserves an existing destination.

The CLI writes to a temporary file beside the destination and renames it after completion. Recorded release-binary smoke checks confirmed that SIGINT returns status 1 with `operation cancelled` and preserves an existing destination, and that silence produces JSON null dBTP with zero passes.

## Performance

`just bench` runs the public APIs on a generated 240-second, 48 kHz stereo fixture: a 997 Hz carrier with a 0.7 Hz amplitude envelope and peak +2.9226 dBTP. Timings exclude generation, compilation, and WAV I/O. The release profile uses thin LTO and one codegen unit; DSP runs on one thread.

| Measurement | Recorded result | Target |
|---|---|---|
| Four-minute stereo meter at 8x | 2.991 s; 80.23x real time | At least 40x: met |
| Ten-second stereo meter at 8x | 0.114 s; about 87.8x real time | At least 40x: met |
| Four-minute streaming pass | 7.442 s | Under 6 s: exceeded |
| Full `limit`, one verified pass | 13.763 s, including input measurement | Under 6 s/pass: exceeded |
| Full `limit`, excluding input measurement | 10.724 s/pass | Under 6 s/pass: exceeded |
| Output | −1.0273 dBTP; one pass; `ceiling_met=true` | Measured ceiling met |

Progress timestamps divide the verified pass into 7.501 s processing/telemetry and 3.218 s flush/alignment/verification. Each pass runs an upsampler, a decimator, and the meter's interpolator, approximately 42.1 billion multiply-accumulate operations across the stereo fixture. Timings include allocation and gain-buffer costs; they are not an instruction-level profile.

Coefficients and accumulation use `f64`. The FIR reads contiguous history with fixed pairwise reductions; smoothing rebases at fixed internal boundaries. Streaming state and scratch storage are bounded by filters, lookahead, channels, and block size. Offline processing owns base-rate output/gain buffers and quantizes block copies for verification.
