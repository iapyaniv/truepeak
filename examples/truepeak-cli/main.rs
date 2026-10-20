use clap::{Parser, ValueEnum};
use std::{
    fs::{self, OpenOptions},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};
use truepeak::{Config, limit};

type Audio = Vec<Vec<f32>>;

static CANCEL: AtomicBool = AtomicBool::new(false);

#[cfg(unix)]
extern "C" fn interrupt(_: i32) {
    CANCEL.store(true, Ordering::Relaxed);
}

fn install_interrupt() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(unix)]
    {
        unsafe extern "C" {
            fn signal(signal: i32, handler: extern "C" fn(i32)) -> usize;
        }
        // The standard library has no safe signal-registration API. The handler
        // performs only a lock-free atomic store; it allocates and locks nothing.
        if unsafe { signal(2, interrupt) } == usize::MAX {
            return Err("could not install SIGINT handler".into());
        }
    }
    Ok(())
}

#[derive(Clone, Copy, ValueEnum)]
enum BitDepth {
    #[value(name = "16")]
    Pcm16,
    #[value(name = "24")]
    Pcm24,
    #[value(name = "32f")]
    Float32,
}

#[derive(Parser)]
#[command(about = "Limit WAV audio to a verified true-peak ceiling")]
struct Args {
    input: PathBuf,
    output: PathBuf,
    #[arg(long, default_value_t = -1.0, allow_hyphen_values = true)]
    ceiling: f64,
    #[arg(long, default_value_t = 0.03)]
    initial_margin_db: f64,
    #[arg(long, default_value_t = 2.0)]
    lookahead_ms: f64,
    #[arg(long, default_value_t = 50.0)]
    release_fast_ms: f64,
    #[arg(long, default_value_t = 500.0)]
    release_slow_ms: f64,
    #[arg(long, default_value_t = 8)]
    oversampling: u32,
    #[arg(long, value_enum)]
    bit_depth: Option<BitDepth>,
    #[arg(long)]
    json: bool,
}

struct PendingFile(PathBuf);
impl Drop for PendingFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn read_wav(path: &Path) -> Result<(hound::WavSpec, Audio), Box<dyn std::error::Error>> {
    let mut reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    if !matches!(
        (spec.sample_format, spec.bits_per_sample),
        (hound::SampleFormat::Float, 32) | (hound::SampleFormat::Int, 16 | 24 | 32)
    ) {
        return Err("supported WAV formats are signed PCM 16/24/32 and float32".into());
    }
    if !(1..=8).contains(&spec.channels) {
        return Err("channel count must be 1 to 8".into());
    }
    let mut channels = vec![Vec::new(); spec.channels as usize];
    let mut index = 0;
    match spec.sample_format {
        hound::SampleFormat::Float => {
            for sample in reader.samples::<f32>() {
                if index % 8192 == 0 && CANCEL.load(Ordering::Relaxed) {
                    return Err(truepeak::Error::Cancelled.into());
                }
                channels[index % spec.channels as usize].push(sample?);
                index += 1;
            }
        }
        hound::SampleFormat::Int => {
            let scale = 2.0_f64.powi(spec.bits_per_sample as i32 - 1);
            for sample in reader.samples::<i32>() {
                if index % 8192 == 0 && CANCEL.load(Ordering::Relaxed) {
                    return Err(truepeak::Error::Cancelled.into());
                }
                channels[index % spec.channels as usize].push((sample? as f64 / scale) as f32);
                index += 1;
            }
        }
    }
    if index % spec.channels as usize != 0 {
        return Err("incomplete WAV frame".into());
    }
    Ok((spec, channels))
}

fn write_wav(
    path: &Path,
    spec: hound::WavSpec,
    channels: &[Vec<f32>],
) -> Result<(), Box<dyn std::error::Error>> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .ok_or("output must name a file")?
        .to_string_lossy();
    let mut pending = None;
    for attempt in 0..100 {
        let temporary = parent.join(format!(
            ".{name}.truepeak-{}-{attempt}.tmp",
            std::process::id()
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(file) => {
                pending = Some((PendingFile(temporary), file));
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    let (temporary, file) = pending.ok_or("could not create a temporary output")?;
    let mut writer = hound::WavWriter::new(BufWriter::new(file), spec)?;
    let scale = 2.0_f64.powi(spec.bits_per_sample as i32 - 1);
    for frame in 0..channels[0].len() {
        if frame % 1024 == 0 && CANCEL.load(Ordering::Relaxed) {
            return Err(truepeak::Error::Cancelled.into());
        }
        for channel in channels {
            if spec.sample_format == hound::SampleFormat::Float {
                writer.write_sample(channel[frame])?;
            } else {
                writer.write_sample(
                    (channel[frame] as f64 * scale)
                        .round()
                        .clamp(-scale, scale - 1.0) as i32,
                )?;
            }
        }
    }
    writer.finalize()?;
    if CANCEL.load(Ordering::Relaxed) {
        return Err(truepeak::Error::Cancelled.into());
    }
    fs::rename(&temporary.0, path)?;
    Ok(())
}

fn run(args: Args, summary: &mut dyn Write) -> Result<bool, Box<dyn std::error::Error>> {
    install_interrupt()?;
    let start = Instant::now();
    let (mut spec, input) = read_wav(&args.input)?;
    if let Some(depth) = args.bit_depth {
        (spec.bits_per_sample, spec.sample_format) = match depth {
            BitDepth::Pcm16 => (16, hound::SampleFormat::Int),
            BitDepth::Pcm24 => (24, hound::SampleFormat::Int),
            BitDepth::Float32 => (32, hound::SampleFormat::Float),
        };
    }
    let config = Config {
        ceiling_dbtp: args.ceiling,
        initial_margin_db: args.initial_margin_db,
        lookahead_ms: args.lookahead_ms,
        release_fast_ms: args.release_fast_ms,
        release_slow_ms: args.release_slow_ms,
        oversampling: args.oversampling,
        verify_bit_depth: if spec.sample_format == hound::SampleFormat::Int
            && spec.bits_per_sample <= 24
        {
            Some(spec.bits_per_sample as u8)
        } else {
            None
        },
        ..Config::default()
    };
    let refs: Vec<&[f32]> = input.iter().map(Vec::as_slice).collect();
    let output = limit(&refs, spec.sample_rate, &config, &CANCEL, &mut |_| {})?;
    write_wav(&args.output, spec, &output.channels)?;
    let elapsed = start.elapsed().as_secs_f64();
    if args.json {
        writeln!(
            summary,
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "input_true_peak_dbtp": output.input_true_peak_dbtp,
                "output_true_peak_dbtp": output.output_true_peak_dbtp,
                "ceiling_dbtp": output.ceiling_dbtp, "ceiling_met": output.ceiling_met,
                "passes": output.passes, "max_reduction_db": output.max_reduction_db,
                "elapsed_seconds": elapsed,
            }))?
        )?;
    } else {
        println!(
            "Input: {:.6} dBTP\nOutput: {:.6} dBTP\nCeiling: {:.3} dBTP\nCeiling met: {}\nPasses: {}\nMax reduction: {:.6} dB\nElapsed: {elapsed:.3} s",
            output.input_true_peak_dbtp,
            output.output_true_peak_dbtp,
            output.ceiling_dbtp,
            output.ceiling_met,
            output.passes,
            output.max_reduction_db
        );
    }
    Ok(output.ceiling_met)
}

fn main() {
    match run(Args::parse(), &mut std::io::stdout().lock()) {
        Ok(true) => {}
        Ok(false) => std::process::exit(2),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_wav_formats_preserve_frames_channels_and_verified_ceiling() {
        let directory = tempfile::tempdir().unwrap();
        for (bits, format) in [
            (16, hound::SampleFormat::Int),
            (24, hound::SampleFormat::Int),
            (32, hound::SampleFormat::Int),
            (32, hound::SampleFormat::Float),
        ] {
            let input = directory.path().join("input.wav");
            let output = directory.path().join("output.wav");
            let spec = hound::WavSpec {
                channels: 2,
                sample_rate: 48000,
                bits_per_sample: bits,
                sample_format: format,
            };
            let audio: Vec<f32> = (0..4800)
                .map(|n| (0.95 * (std::f64::consts::TAU * 997.0 * n as f64 / 48000.0).sin()) as f32)
                .collect();
            write_wav(
                &input,
                spec,
                &[audio.clone(), audio.iter().map(|v| -v * 0.5).collect()],
            )
            .unwrap();
            for override_depth in [None, Some("16"), Some("24"), Some("32f")] {
                let mut argv = vec![
                    "truepeak-cli".into(),
                    input.clone().into_os_string(),
                    output.clone().into_os_string(),
                    "--ceiling".into(),
                    "-1.0".into(),
                    "--json".into(),
                ];
                if let Some(depth) = override_depth {
                    argv.push("--bit-depth".into());
                    argv.push(depth.into());
                }
                let mut summary = Vec::new();
                assert!(run(Args::try_parse_from(argv).unwrap(), &mut summary).unwrap());
                let summary: serde_json::Value = serde_json::from_slice(&summary).unwrap();
                let (actual, decoded) = read_wav(&output).unwrap();
                assert_eq!(actual.sample_rate, spec.sample_rate);
                assert_eq!(actual.channels, 2);
                let expected = match override_depth {
                    Some("16") => (16, hound::SampleFormat::Int),
                    Some("24") => (24, hound::SampleFormat::Int),
                    Some("32f") => (32, hound::SampleFormat::Float),
                    _ => (bits, format),
                };
                assert_eq!((actual.bits_per_sample, actual.sample_format), expected);
                assert!(decoded.iter().all(|c| c.len() == audio.len()));
                let refs: Vec<&[f32]> = decoded.iter().map(Vec::as_slice).collect();
                let peak = truepeak::true_peak(&refs, 48000, 8).unwrap().dbtp;
                assert!(
                    (summary["output_true_peak_dbtp"].as_f64().unwrap() - peak).abs() < 0.000001
                );
                assert!(
                    peak <= -0.95,
                    "input={bits}/{format:?}, output={override_depth:?}, measured={peak}"
                );
            }
        }
    }

    #[test]
    fn invalid_config_preserves_an_existing_destination() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("input.wav");
        let output = directory.path().join("output.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 48000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        write_wav(&input, spec, &[vec![0.0; 100]]).unwrap();
        fs::write(&output, b"previous export").unwrap();
        for (flag, value) in [("--oversampling", "3"), ("--initial-margin-db", "0.101")] {
            let args = Args::try_parse_from([
                "truepeak-cli".into(),
                input.clone().into_os_string(),
                output.clone().into_os_string(),
                flag.into(),
                value.into(),
            ])
            .unwrap();
            assert!(run(args, &mut Vec::new()).is_err());
            assert_eq!(fs::read(&output).unwrap(), b"previous export");
        }
    }
}
