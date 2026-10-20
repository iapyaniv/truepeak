# Design

`truepeak` combines a true-peak meter, a lookahead limiter, and their shared resampler. The offline limiter verifies the measured output peak after each pass. The measurement tolerances and recorded results are in [validation results](validation.md).

## Inputs and execution

Inputs are planar `f32` slices: 1–8 equal-length channels at 44.1, 48, 88.2, 96, 176.4, or 192 kHz. Empty channels represent silence. Invalid layouts, unsupported rates, and non-finite samples return `Error::InvalidInput`; invalid settings return `Error::InvalidConfig`.

The library uses safe Rust and owns no files, threads, subprocesses, clocks, or global state. Runtime dependencies are `thiserror` and optional `serde`. The crate compiles for `wasm32-unknown-unknown`.

Whole-file operations check an `AtomicBool` cancellation flag and report progress at block boundaries. Cancellation returns `Error::Cancelled` without partial results. Streaming callers choose their blocks and check cancellation between calls. Invalid streaming blocks leave state and output unchanged.

For a given build and input, results are repeatable and independent of caller block partitions. Fixed accumulation order and internal smoothing boundaries preserve bit identity.

## Resampler

Upsampling and downsampling use one Kaiser-windowed sinc prototype with integer factor `L` of 4, 8, or 16. Filter specifications are relative to the base-rate Nyquist frequency `fN`:

| Property | Requirement |
|---|---|
| Passband | 0–0.907 `fN`, ripple at most 0.01 dB |
| Stopband | From 1.093 `fN`, attenuation at least 100 dB |
| Phase | Linear; symmetric coefficients and odd tap count |
| Delay | 38 base-rate samples in each direction; 76 for a round trip |
| Gain | Unity round-trip gain within 0.01 dB at 1 kHz |

The Kaiser attenuation parameter is 100 dB, giving `beta = 0.1102 * (100 - 8.7)`, about 10.06. The estimated filter order includes a 10% margin to meet attenuation at the transition edge and is rounded up to a multiple of `2L`. The resulting prototype has `76L + 1` taps. At 44.1 kHz, 20 kHz lies approximately at the 0.907 `fN` passband edge.

Coefficients and accumulation use `f64`; public samples use `f32`. A double-length history buffer stores each sample twice so the FIR reads one contiguous slice. Fixed pairwise reductions shorten dependency chains while retaining the same summation order across blocks.

`process` appends output, retaining causal delay. The upsampler emits `input.len() * L` samples; `flush` appends `76L` tail samples. The downsampler retains filtered indices 0, L, 2L, and so on, carrying its phase between blocks; flush drains `76L` zero input samples through that phase. Each direction reports delay in its own output samples.

Both streams are zero-padded outside the input. Flush ends the stream and is idempotent; reset is required before further processing. Empty streams emit no tail. When chaining processors, pass each upstream tail through the downstream processor before flushing downstream.

## Meter

The measurement API is available in `meter` and re-exported at the crate root:

| Functions | Result |
|---|---|
| `true_peak`, `true_peak_with_control` | Overall true peak |
| `true_peak_per_channel`, `true_peak_per_channel_with_control` | True peaks in channel order |
| `true_peak_in_range`, `true_peak_in_range_with_control` | Overall true peak in a base-rate frame range |
| `true_peak_per_channel_in_range`, `true_peak_per_channel_in_range_with_control` | True peaks in a range, in channel order |
| `sample_peak`, `sample_peak_with_control` | Exact overall and per-channel sample maxima |

The meter upsamples one channel at a time in bounded blocks and scans the interpolated output for peaks. At each local maximum of absolute amplitude it fits a parabola through the three signed neighboring samples. For samples `left`, `center`, and `right`, the refined value is:

```text
curvature = right - 2 * center + left
value = center - (right - left)^2 / (8 * curvature)
offset = (left - right) / (2 * curvature)
```

Zero curvature or an offset outside half a grid step falls back to the center sample. The scan also retains grid maxima, so refinement cannot reduce the reported peak. Only two neighboring samples are retained between blocks.

A sampled grid can miss a tone's continuous peak by `20 * log10(cos(pi * f / f_grid))` dB: about 0.14 dB at 8x for 20 kHz at 44.1 kHz. Refinement reduces this error; phase-sweep tests require readings within 0.02 dB of known tone peaks at all three factors. The recommended factor is 8x. These tests characterize the estimator rather than proving a bound for arbitrary waveforms.

Peak positions compensate interpolation delay. Zero padding can produce a boundary peak outside the input duration. Ties retain the first channel and earliest grid peak. Silence reads negative infinity dBTP with position zero. `sample_peak` instead measures exact absolute input-sample maxima, with the same validation and tie rules.

Range functions take `range: std::ops::Range<usize>` after the oversampling factor; controlled variants then take the cancellation flag and progress sink. Frames must satisfy `start <= end <= len`, otherwise the call returns `Error::InvalidInput`. Empty ranges read silence with position zero. For a nonempty range the upsampler processes `[start.saturating_sub(margin), min(len, end.saturating_add(margin)))`, where `margin = ceil(delay / L) + 1`: the filter half length in base-rate frames plus one frame for refinement, currently 39 frames. Only local peaks whose refined, delay-compensated frame position `p` satisfies `start <= p < end` are selected. Positions remain relative to the whole input. Full-input ranges `0..len` preserve the whole-file scan, including padding peaks outside the input duration, and match existing functions bit for bit. Progress counts processed base-rate samples across channels, including context; cancellation returns no partial result. Non-finite samples in the processed context are rejected.

Slicing a file before measuring invents a zero-padded boundary at the cut, whose ringing can dominate the reading. For a 48 kHz, 50.15625 Hz sine at amplitude 0.99, cutting at 1.6 seconds ends near a sine peak: the standalone slice reads +1.05724295 dBTP at 8x, while the continuous signal and `true_peak_in_range(..., 0..76800)` both read −0.08728187 dBTP. Surrounding context preserves the continuous interpolation; selecting refined positions excludes peaks outside the requested range, though their sinc sidelobes can still contribute peaks inside it.

FFmpeg's `ebur128` uses a fixed 192 kHz true-peak grid and rounds its summary to 0.1 dB. At 12 kHz, that grid can under-read by about 0.17 dB, so the comparison permits `truepeak - FFmpeg` from −0.05 to +0.25 dB. The bounds are ±0.1 dB at 997 Hz and 5 kHz, and −0.05 to +0.5 dB at higher test frequencies. Independent phase sweeps and EBU vectors check accuracy separately.

## Limiter

### Streaming pass

Each channel is upsampled, multiplied by a gain envelope on the oversampled grid, and downsampled. The detector shares the meter's signed three-point refinement. It decides on sample `n` when `n+1` arrives, retaining two neighbors across blocks. Linked mode takes the largest refined peak across channels and applies one envelope; unlinked mode uses one envelope per channel.

For target amplitude `ceiling`, the required gain is `min(1, ceiling / peak)`. Release has slow and fast stages in linear gain:

```text
s[n] = s[n-1] + (required[n] - s[n-1]) * k
f[n] = min(required[n], s[n] - (s[n] - f[n-1]) * a_fast)
e[n] = f[n]
```

The slow coefficient `k` is `1 - exp(-1 / (tau * f_os))`, using the slow attack time when gain falls and slow release time when it rises. `a_fast = exp(-1 / (tau_fast * f_os))`. Both stages start at unity. Defaults are 50 ms fast release, 500 ms slow release, and 50 ms slow attack. The slow stage holds sustained reduction; the slow attack keeps a short transient from holding that stage down.

Lookahead is rounded to an integer number of base-rate samples:

```text
lookahead_base = round(lookahead_ms * sample_rate_hz / 1000)
W = lookahead_base * L
m[n] = min(e[n-1], ..., e[n+W-1])
```

A monotonic deque computes this minimum in amortized O(1) time per sample. The W+1 entries accommodate the detector's one-sample delay while audio is delayed by W samples.

Three cascaded box filters, each of length `floor(W/3)`, smooth the minimum into a quadratic B-spline attack ramp. Their combined support is `3 * (floor(W/3) - 1) + 1 <= W`. Each smoothing term includes `e[n]` in its minimum, so non-negative, normalized weights give `G[n] <= e[n] <= required[n]` in exact arithmetic. Running sums rebase at fixed `1024L`-sample boundaries to bound drift without depending on caller partitions. The per-sample work stays O(1).

The gain is applied before decimation, without hard clipping. Decimation and subsequent meter interpolation can change peak shape; a single streaming pass does not guarantee the requested measured output ceiling.

Streaming latency is `76 + lookahead_base` base-rate samples: 172 at 48 kHz and 164 at 44.1 kHz with the default 2 ms lookahead. `process` emits one causal frame per input frame. Flush drains the delayed audio and FIR tail once. Output retains its leading latency; reset starts a new stream. Optional gain telemetry contains per-channel linear gain at base rate, aligned with the causal audio and taking the minimum within each oversampled frame.

### Offline verification

`limit` first measures the float input. Input at or below the requested ceiling bypasses processing and is copied bit for bit, with zero passes and zero gain reduction. With quantized verification, bypass also measures the exported PCM representation. Rounding can cross the ceiling; bypass still preserves the float input and reports the measured result.

Pass 1 targets `ceiling_dbtp - initial_margin_db`, with a default margin of 0.03 dB. This margin applies only to offline processing, after bypass. Its measurement basis is recorded in [validation results](validation.md#initial-margin).

After each pass, verification measures the base-rate output. With `verify_bit_depth` set to 16 or 24, it quantizes bounded block copies to signed PCM: nearest integer, ties away from zero, saturation to the representable range, and no dither. Returned audio remains float. `None` measures float output.

If the measured peak exceeds the requested ceiling, the next target is lowered by the excess plus 0.005 dB. Each retry starts from the original input to avoid accumulating resampler round trips. Processing stops when the measured ceiling is met or `max_passes` is exhausted. `ceiling_met` reports whether the final measurement is at or below `ceiling_dbtp + 0.001`; callers must inspect it before accepting the result.

Offline output drops leading latency and retains exactly the input frame count. The returned gain series takes the minimum gain per configured hop, across channels, from the pass that produced the audio. Values are in dB and at most zero.

Progress uses pass 0 for initial measurement, then passes 1 through `max_passes`. Counters count base-rate samples across channels. A limiting pass includes both processing and verification, for a total of `2 * frames * channels`. A quantized bypass extends pass 0's total to include its verification scan. Standalone meters use pass 1 and a total of `frames * channels`; range meters count the processed context frames.

## Memory and performance

Streaming state depends on filter length, lookahead, channel count, and the internal 1024-frame block size. Scratch buffers are reused between calls. Offline processing retains base-rate audio and gain buffers; it does not retain an oversampled copy of the whole file.

Performance targets are at least 40x real time for an 8x stereo meter and under six seconds per pass for a four-minute, 48 kHz stereo limit on an Apple M-series machine, single-threaded in release mode. The recorded meter reaches 80x real time; the verified limiter takes 13.76 seconds for one pass and exceeds its target. [Validation results](validation.md#performance) include stage timings and benchmark conditions.
