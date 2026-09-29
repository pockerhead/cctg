//! `cctg-voice` (TASK-085, a request loop since TASK-086): recognizes
//! Telegram voice messages for the hub, one at a time. Usage:
//! `cctg-voice <model dir>`, requests on stdin.
//!
//! Loads the sherpa-onnx streaming transducer in `<model dir>` once, then
//! answers requests until stdin closes. A request is the byte count of one
//! OGG/Opus voice in ASCII digits and `\n`, then those bytes (at most
//! [`MAX_INPUT`]). The voice is decoded to 16 kHz mono (at most
//! [`MAX_SECONDS`], counted while decoding) and recognized on one thread.
//! The answer is one JSON line on stdout: `{"text", "audio_ms", "took_ms",
//! "peak_rss_kb"}`, or `{"error", "took_ms"}` with the error `not_opus`,
//! `too_long`, `too_big` or `failed`. `took_ms` is the time of this request.
//!
//! Exit codes: 0 stdin closed; 2 a broken request; 4 the model did not load
//! (or this build has no recognizer: only Linux builds link sherpa-onnx);
//! anything else is a failure. stderr gets one short phrase, never the text
//! or paths.

use std::io::{BufRead, Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use opus_pure::{MAX_PACKET_SAMPLES, OggOpusReader, Trim};

/// Bytes of OGG in one request at most.
const MAX_INPUT: u64 = 8 << 20;
/// Bytes of a request header at most: the digits and `\n`.
const MAX_HEADER: u64 = 24;
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

/// Why a request got no words: the `error` of its answer.
#[derive(Debug, PartialEq, Eq)]
enum Refused {
    NotOpus,
    TooLong,
    TooBig,
    Failed,
}

impl Refused {
    fn name(&self) -> &'static str {
        match self {
            Refused::NotOpus => "not_opus",
            Refused::TooLong => "too_long",
            Refused::TooBig => "too_big",
            Refused::Failed => "failed",
        }
    }
}

/// How the request loop ended.
#[derive(Debug, PartialEq, Eq)]
enum End {
    /// stdin closed between requests.
    Closed,
    /// A header that is not digits and `\n`, or a request cut short.
    Broken,
    /// An answer could not be written.
    Unwritable,
}

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let model = match args.as_slice() {
        [flag] if flag == "--version" => {
            println!("cctg-voice {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        [model] => PathBuf::from(model),
        _ => {
            eprintln!("usage: cctg-voice <model dir>, requests on stdin");
            return ExitCode::from(2);
        }
    };
    lower_priority();
    let recognizer = match Recognizer::load(&model) {
        Ok(recognizer) => recognizer,
        Err(code) => {
            eprintln!("model not loaded");
            return ExitCode::from(code);
        }
    };
    let hear = |ogg: &[u8]| {
        let samples = decode(ogg, MAX_SECONDS * RATE as usize).map_err(|error| match error {
            DecodeError::NotOpus => Refused::NotOpus,
            DecodeError::TooLong => Refused::TooLong,
        })?;
        let text = recognizer.recognize(&samples).ok_or(Refused::Failed)?;
        Ok((text, samples.len() as u64 * 1000 / RATE as u64))
    };
    match serve(std::io::stdin().lock(), std::io::stdout().lock(), hear) {
        End::Closed => ExitCode::SUCCESS,
        End::Broken => {
            eprintln!("broken request");
            ExitCode::from(2)
        }
        End::Unwritable => ExitCode::FAILURE,
    }
}

/// Answers the requests on `input` with `hear` (the words and the audio
/// length in ms), one JSON line each on `output`, until `input` closes.
fn serve(
    mut input: impl BufRead,
    mut output: impl Write,
    mut hear: impl FnMut(&[u8]) -> Result<(String, u64), Refused>,
) -> End {
    loop {
        let size = match read_header(&mut input) {
            Ok(Some(size)) => size,
            Ok(None) => return End::Closed,
            Err(()) => return End::Broken,
        };
        let started = Instant::now();
        let heard = if size > MAX_INPUT {
            // Skipped, so the next request is read from its header.
            match std::io::copy(&mut (&mut input).take(size), &mut std::io::sink()) {
                Ok(skipped) if skipped == size => Err(Refused::TooBig),
                _ => return End::Broken,
            }
        } else {
            let mut ogg = Vec::new();
            match (&mut input).take(size).read_to_end(&mut ogg) {
                Ok(read) if read as u64 == size => hear(&ogg),
                _ => return End::Broken,
            }
        };
        let took_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let line = match heard {
            Ok((text, audio_ms)) => output_line(&text, audio_ms, took_ms, peak_rss_kb()),
            Err(refused) => error_line(&refused, took_ms),
        };
        if writeln!(output, "{line}")
            .and_then(|()| output.flush())
            .is_err()
        {
            return End::Unwritable;
        }
    }
}

/// The byte count of the next request; `None` when `input` closed before
/// the request began.
fn read_header(input: &mut impl BufRead) -> Result<Option<u64>, ()> {
    let mut header = Vec::new();
    input
        .take(MAX_HEADER)
        .read_until(b'\n', &mut header)
        .map_err(|_| ())?;
    if header.is_empty() {
        return Ok(None);
    }
    let digits = header.strip_suffix(b"\n").ok_or(())?;
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return Err(());
    }
    std::str::from_utf8(digits)
        .ok()
        .and_then(|digits| digits.parse().ok())
        .map(Some)
        .ok_or(())
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

/// The model in memory, loaded once for every request.
#[cfg(target_os = "linux")]
struct Recognizer(sherpa_onnx::OnlineRecognizer);

#[cfg(target_os = "linux")]
impl Recognizer {
    /// The model in `model`; `Err` is the exit code.
    fn load(model: &Path) -> Result<Self, u8> {
        use sherpa_onnx::{OnlineRecognizer, OnlineRecognizerConfig, OnlineTransducerModelConfig};

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
        OnlineRecognizer::create(&config).map(Self).ok_or(4)
    }

    /// The words of `samples`, trimmed, on a stream of their own.
    fn recognize(&self, samples: &[f32]) -> Option<String> {
        // Silence after the words: the streaming model decides its last
        // tokens on audio that follows them.
        const TAIL_SAMPLES: usize = RATE as usize * 6 / 10;

        let recognizer = &self.0;
        let stream = recognizer.create_stream();
        stream.accept_waveform(RATE, samples);
        stream.accept_waveform(RATE, &vec![0.0; TAIL_SAMPLES]);
        stream.input_finished();
        while recognizer.is_ready(&stream) {
            recognizer.decode(&stream);
        }
        let result = recognizer.get_result(&stream)?;
        Some(result.text.trim().to_owned())
    }
}

/// No recognizer outside Linux: it never loads.
#[cfg(not(target_os = "linux"))]
enum Recognizer {}

#[cfg(not(target_os = "linux"))]
impl Recognizer {
    fn load(_model: &Path) -> Result<Self, u8> {
        Err(4)
    }

    fn recognize(&self, _samples: &[f32]) -> Option<String> {
        match *self {}
    }
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

/// The one JSON line of a request without words.
fn error_line(refused: &Refused, took_ms: u64) -> String {
    serde_json::json!({ "error": refused.name(), "took_ms": took_ms }).to_string()
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

    fn request(body: &[u8]) -> Vec<u8> {
        let mut request = format!("{}\n", body.len()).into_bytes();
        request.extend_from_slice(body);
        request
    }

    /// The answers of `serve` over `input`, `hear` answering with the
    /// request's bytes as text (`bad` refused as not Opus), and its end.
    fn served(input: &[u8]) -> (Vec<serde_json::Value>, End, Vec<Vec<u8>>) {
        let mut output = Vec::new();
        let mut heard = Vec::new();
        let end = serve(input, &mut output, |ogg| {
            heard.push(ogg.to_vec());
            if ogg == b"bad" {
                return Err(Refused::NotOpus);
            }
            Ok((String::from_utf8_lossy(ogg).into_owned(), 7))
        });
        let answers = output
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice(line).unwrap())
            .collect();
        (answers, end, heard)
    }

    #[test]
    fn requests_are_answered_one_line_each_until_stdin_closes() {
        let input = [request("раз".as_bytes()), request(b"bad"), request(b"")].concat();
        let (answers, end, heard) = served(&input);
        assert_eq!(end, End::Closed);
        assert_eq!(heard, ["раз".as_bytes(), b"bad", b""]);
        assert_eq!(answers.len(), 3, "{answers:?}");
        assert_eq!(answers[0]["text"], "раз");
        assert_eq!(answers[0]["audio_ms"], 7);
        assert_eq!(answers[1]["error"], "not_opus");
        assert!(answers[1].get("text").is_none(), "{:?}", answers[1]);
        assert_eq!(answers[2]["text"], "");
        assert!(answers.iter().all(|answer| answer["took_ms"].is_u64()));
        assert_eq!(served(b""), (Vec::new(), End::Closed, Vec::new()));
    }

    #[test]
    fn a_too_big_request_is_skipped_and_the_next_one_is_heard() {
        let big = vec![b'x'; MAX_INPUT as usize + 1];
        let input = [request(&big), request(b"next")].concat();
        let (answers, end, heard) = served(&input);
        assert_eq!(end, End::Closed);
        assert_eq!(heard, [b"next"]);
        assert_eq!(answers[0]["error"], "too_big");
        assert_eq!(answers[1]["text"], "next");
    }

    #[test]
    fn a_broken_request_ends_the_loop_after_the_answers_before_it() {
        for broken in [
            &b"abc\n"[..],
            b"\n",
            b"-1\n",
            b"12",
            b"10\nshort",
            b"99999999999999999999999999\n",
        ] {
            let input = [request(b"one"), broken.to_vec()].concat();
            let (answers, end, heard) = served(&input);
            assert_eq!(end, End::Broken, "{:?}", String::from_utf8_lossy(broken));
            assert_eq!(heard, [b"one"]);
            assert_eq!(answers.len(), 1);
        }
    }
}
