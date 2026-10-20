# truepeak

A Rust library for true-peak measurement and lookahead limiting of planar `f32` audio.

The meter and limiter share a Kaiser-windowed sinc resampler with 4x, 8x, or 16x interpolation. The offline limiter bypasses audio below the ceiling, compensates latency, and verifies the output after each pass. Streaming processors expose their latency and retain state between blocks.

## Usage

Requires Rust 1.96 or later. The crate is not published to crates.io; use a Cargo path or Git dependency.

```rust
use std::sync::atomic::AtomicBool;
use truepeak::{Config, limit, true_peak};

fn main() -> Result<(), truepeak::Error> {
    let audio = [0.0_f32, 1.2, -1.2, 0.0];
    let channels = [audio.as_slice()];
    let peak = true_peak(&channels, 48_000, 8)?;

    let config = Config {
        ceiling_dbtp: -1.0,
        verify_bit_depth: Some(24),
        ..Config::default()
    };
    let output = limit(
        &channels,
        48_000,
        &config,
        &AtomicBool::new(false),
        &mut |_| {},
    )?;

    println!("Input: {} dBTP", peak.dbtp);
    println!("Output: {} dBTP", output.output_true_peak_dbtp);
    assert!(output.ceiling_met);
    Ok(())
}
```

Inputs contain 1–8 equal-length channels at 44.1, 48, 88.2, 96, 176.4, or 192 kHz. Non-finite samples are rejected. Silence reads negative infinity dBTP. The optional `serde` feature enables serialization of configuration and result types.

`limit` returns time-aligned float audio with the input frame count. `verify_bit_depth` measures a rounded, saturated 16- or 24-bit PCM copy; it does not quantize the returned audio. Check `ceiling_met` before accepting a render: a limited pass budget or quantization on bypass can leave the measured output above the ceiling.

`Limiter`, `Upsampler`, and `Downsampler` provide streaming processing. Meter functions ending in `_with_control` accept a cancellation flag and a progress callback. Generate the API reference with `cargo doc --open` for field ranges and streaming contracts.

## WAV example

```sh
cargo run --release --example truepeak-cli -- \
    in.wav out.wav --ceiling -1.0 --bit-depth 24 --json
```

The example reads 16/24/32-bit integer and float32 WAVs. It preserves the input format unless `--bit-depth` is given (`16`, `24`, or `32f`). Exit status is 0 when the measured ceiling is met, 2 when it is exceeded, and 1 for errors or cancellation. JSON encodes silence as `null`.

## Development

```sh
just test      # Formatting, Clippy, feature checks, and tests
just validate  # Fetch EBU vectors and require both external comparisons
just wasm     # Check wasm32-unknown-unknown
just bench    # Meter and limiter timings
```

`just test` skips external comparisons when FFmpeg or the EBU vectors are missing. `just validate` requires FFmpeg, Python 3.11 or later, and curl; it downloads about 92 MB of checksum-verified EBU test material. Audio fixtures are ignored by Git. For `just wasm`, install the target with `rustup target add --toolchain 1.96.0 wasm32-unknown-unknown`.

At 8x, the meter's worst error on the recorded phase sweeps is 0.0029 dB. A four-minute stereo fixture measured at 80x real time on an M2 Max; the verified limiter took 13.76 seconds for one pass, above its six-second target. These are fixture measurements, not bounds for arbitrary signals. See the [design](docs/design.md) and [validation results](docs/validation.md) for methods, tolerances, and timings.

## License

[MIT](LICENSE).
