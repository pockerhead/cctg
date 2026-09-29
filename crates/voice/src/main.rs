//! `cctg-voice` (TASK-085): recognizes one Telegram voice message for the
//! hub. Usage: `cctg-voice <model dir> < voice.ogg`.
//!
//! Reads OGG/Opus from stdin (at most [`MAX_INPUT`]), decodes it to 16 kHz
//! mono (at most [`MAX_SECONDS`], counted while decoding), recognizes it with
//! the sherpa-onnx streaming transducer in `<model dir>` on one thread and
//! prints one JSON line: `{"text", "audio_ms", "took_ms", "peak_rss_kb"}`.
//!
//! Exit codes: 0 done; 2 the input is not OGG/Opus, empty or too big; 3 the
//! audio is longer than [`MAX_SECONDS`]; 4 the model did not load (or this
//! build has no recognizer: only Linux builds link sherpa-onnx); anything
//! else is a failure. stderr gets one short phrase, never the text or paths.

use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use opus_pure::{MAX_PACKET_SAMPLES, OggOpusReader, Trim};

/// Bytes of OGG read from stdin at most.
const MAX_INPUT: u64 = 8 << 20;
/// Seconds of audio recognized at most.
const MAX_SECONDS: usize = 300;
/// The model's sample rate.
const RATE: i32 = 16_000;

#[derive(Debug, PartialEq, Eq)]
enum DecodeError {
    /// Not an OGG/Opus stream this decoder takes (or a broken one).
    NotOpus,
    /// More than the allowed samples.
    TooLong,
}

fn main() -> ExitCode {
    let started = Instant::now();
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let model = match args.as_slice() {
        [flag] if flag == "--version" => {
            println!("cctg-voice {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        [model] => PathBuf::from(model),
        _ => {
            eprintln!("usage: cctg-voice <model dir> < voice.ogg");
            return ExitCode::from(2);
        }
    };
    lower_priority();
    let mut input = Vec::new();
    let read = std::io::stdin()
        .lock()
        .take(MAX_INPUT + 1)
        .read_to_end(&mut input);
    if read.is_err() || input.len() as u64 > MAX_INPUT {
        eprintln!("input unreadable or larger than 8 MiB");
        return ExitCode::from(2);
    }
    let samples = match decode(&input, MAX_SECONDS * RATE as usize) {
        Ok(samples) => samples,
        Err(DecodeError::NotOpus) => {
            eprintln!("input is not OGG/Opus");
            return ExitCode::from(2);
        }
        Err(DecodeError::TooLong) => {
            eprintln!("audio longer than {MAX_SECONDS} s");
            return ExitCode::from(3);
        }
    };
    drop(input);
    let text = match recognize(&model, &samples) {
        Ok(text) => text,
        Err(code) => {
            eprintln!("recognition failed");
            return ExitCode::from(code);
        }
    };
    let audio_ms = samples.len() as u64 * 1000 / RATE as u64;
    let took_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let line = output_line(&text, audio_ms, took_ms, peak_rss_kb());
    let mut stdout = std::io::stdout().lock();
    if writeln!(stdout, "{line}")
        .and_then(|()| stdout.flush())
        .is_err()
    {
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

/// Recognition runs in the background of the hub's machine.
#[cfg(target_os = "linux")]
fn lower_priority() {
    // SAFETY: setpriority only reads its arguments; a failure is harmless.
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, 0, 10);
    }
}

#[cfg(not(target_os = "linux"))]
fn lower_priority() {}

/// `ogg` as 16 kHz mono samples, the channels averaged and the RFC 7845
/// pre-skip and end trim applied; stops with [`DecodeError::TooLong`] as
/// soon as there are more than `max_samples`, so a long file never fills
/// memory (a sender sets the duration Telegram shows).
fn decode(ogg: &[u8], max_samples: usize) -> Result<Vec<f32>, DecodeError> {
    let mut reader = OggOpusReader::new(Cursor::new(ogg)).map_err(|_| DecodeError::NotOpus)?;
    let head = reader.head().clone();
    let channels = usize::from(head.channel_count);
    if channels == 0 {
        return Err(DecodeError::NotOpus);
    }
    let mut decoder = head.decoder(RATE).map_err(|_| DecodeError::NotOpus)?;
    let mut trim = Trim::new(&head, RATE, channels).map_err(|_| DecodeError::NotOpus)?;
    let mut block = vec![0.0f32; MAX_PACKET_SAMPLES * channels];
    let mut mono = Vec::new();
    for packet in reader.packets() {
        let packet = packet.map_err(|_| DecodeError::NotOpus)?;
        let decoded = decoder
            .decode(&packet.data, MAX_PACKET_SAMPLES, &mut block)
            .map_err(|_| DecodeError::NotOpus)?;
        let kept = trim.keep(&packet, &block[..decoded * channels]);
        mono.extend(
            kept.chunks(channels)
                .map(|frame| frame.iter().sum::<f32>() / channels as f32),
        );
        if mono.len() > max_samples {
            return Err(DecodeError::TooLong);
        }
    }
    Ok(mono)
}

/// The words of `samples` by the model in `model`, trimmed; `Err` is the
/// exit code.
#[cfg(target_os = "linux")]
fn recognize(model: &Path, samples: &[f32]) -> Result<String, u8> {
    use sherpa_onnx::{OnlineRecognizer, OnlineRecognizerConfig, OnlineTransducerModelConfig};

    // Silence after the words: the streaming model decides its last tokens
    // on audio that follows them.
    const TAIL_SAMPLES: usize = RATE as usize * 6 / 10;

    let file = |name: &str| Some(model.join(name).to_string_lossy().into_owned());
    let mut config = OnlineRecognizerConfig::default();
    config.model_config.transducer = OnlineTransducerModelConfig {
        encoder: file("encoder.int8.onnx"),
        decoder: file("decoder.int8.onnx"),
        joiner: file("joiner.int8.onnx"),
    };
    config.model_config.tokens = file("tokens.txt");
    config.model_config.num_threads = 1;
    config.model_config.debug = false;
    config.decoding_method = Some("greedy_search".to_owned());
    let recognizer = OnlineRecognizer::create(&config).ok_or(4)?;
    let stream = recognizer.create_stream();
    stream.accept_waveform(RATE, samples);
    stream.accept_waveform(RATE, &vec![0.0; TAIL_SAMPLES]);
    stream.input_finished();
    while recognizer.is_ready(&stream) {
        recognizer.decode(&stream);
    }
    let result = recognizer.get_result(&stream).ok_or(1)?;
    Ok(result.text.trim().to_owned())
}

#[cfg(not(target_os = "linux"))]
fn recognize(_model: &Path, _samples: &[f32]) -> Result<String, u8> {
    Err(4)
}

/// The process's peak resident memory (`VmHWM`), in kB.
#[cfg(target_os = "linux")]
fn peak_rss_kb() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    status
        .lines()
        .find_map(|line| line.strip_prefix("VmHWM:"))?
        .trim()
        .trim_end_matches("kB")
        .trim()
        .parse()
        .ok()
}

#[cfg(not(target_os = "linux"))]
fn peak_rss_kb() -> Option<u64> {
    None
}

/// The one JSON line of the answer; `peak_rss_kb` only when known.
fn output_line(text: &str, audio_ms: u64, took_ms: u64, peak_rss_kb: Option<u64>) -> String {
    let mut line = serde_json::json!({
        "text": text,
        "audio_ms": audio_ms,
        "took_ms": took_ms,
    });
    if let Some(peak) = peak_rss_kb {
        line["peak_rss_kb"] = peak.into();
    }
    line.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &[u8] = include_bytes!("../tests/fixtures/voice-ru.ogg");

    #[test]
    fn the_fixture_decodes_to_its_length_at_16_khz() {
        let samples = decode(FIXTURE, MAX_SECONDS * RATE as usize).unwrap();
        // 4.59 s: 73 440 samples, give or take one 60 ms packet.
        assert!(
            (73_440 - 960..=73_440 + 960).contains(&samples.len()),
            "{}",
            samples.len()
        );
        assert!(samples.iter().any(|sample| sample.abs() > 0.01));
    }

    #[test]
    fn decoding_stops_at_the_limit() {
        assert_eq!(decode(FIXTURE, 16_000), Err(DecodeError::TooLong));
    }

    #[test]
    fn empty_and_foreign_input_is_not_opus() {
        assert_eq!(decode(b"", 16_000), Err(DecodeError::NotOpus));
        assert_eq!(
            decode(b"OggS but not really a stream", 16_000),
            Err(DecodeError::NotOpus)
        );
        let mut noise = vec![0u8; 4096];
        for (n, byte) in noise.iter_mut().enumerate() {
            *byte = (n * 37 % 251) as u8;
        }
        assert_eq!(decode(&noise, 16_000), Err(DecodeError::NotOpus));
    }

    #[test]
    fn the_answer_is_one_json_line_that_reads_back() {
        let text = "он сказал \"привет\"\nи ушёл \\ всё";
        let line = output_line(text, 4590, 812, Some(131_072));
        assert!(!line.contains('\n'), "{line}");
        let back: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(back["text"], text);
        assert_eq!(back["audio_ms"], 4590);
        assert_eq!(back["took_ms"], 812);
        assert_eq!(back["peak_rss_kb"], 131_072);
        let without = output_line("", 0, 1, None);
        assert!(!without.contains("peak_rss_kb"), "{without}");
    }
}
