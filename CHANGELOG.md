# Changelog

## 0.1.1

- Add `true_peak_in_range` and `true_peak_per_channel_in_range`, with `_with_control` variants, in `meter` and at the crate root. Base-rate frame ranges use surrounding interpolation context and select refined peaks by absolute position, avoiding ringing introduced by slicing at a cut.
- Reject invalid ranges; return silence for empty ranges. Controlled measurements support cancellation and progress including context frames.
- Preserve existing whole-file behavior and bit-identical results for full-input ranges `0..len`.

## 0.1.0

- Initial release with per-channel and overall true-peak measurement, sample-peak measurement, and 4x/8x/16x Kaiser-windowed sinc resampling.
- Add streaming lookahead limiting and offline limiting with bypass, latency alignment, iterative ceiling verification, and optional 16/24-bit PCM verification.
- Support cancellation, progress, optional serde serialization, and a WAV command-line example.
