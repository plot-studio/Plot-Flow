//! Real-audio check of meeting-mode speaker separation (Sortformer).
//!
//! Cuts a WAV into speech segments the way meeting mode does (≥ `--gap` ms of
//! silence ends a segment, 22 s hard cap), feeds them one by one through the
//! same `SpeakerTracker` + `SortformerEngine` the transcription worker uses,
//! and prints the remote speaker of every piece. With a truth CSV
//! (`t0_ms,t1_ms,label`) it also prints a label × speaker table.
//!
//! ```text
//! cargo run --example diarize_test -- <model.gguf> <audio.wav> [truth.csv]
//!     [--cpu] [--gap 600] [--preset vhl|hl|ll|default] [--libs <dir>]
//! ```
//! The transcribe.cpp DLLs (`src-tauri/transcribe-libs`) must be on PATH.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Instant;

use plot_flow_lib::meeting::diarize::{
    self, Assigned, SortformerEngine, SpeakerTracker, Turn, SAMPLE_RATE,
};
use transcribe_cpp::SortformerPreset;

const FRAME: usize = 480; // 30 ms
const HARD_MAX_MS: u64 = 22_000;
const PREROLL_MS: u64 = 300;

struct Args {
    model: PathBuf,
    audio: PathBuf,
    truth: Option<PathBuf>,
    cpu: bool,
    gap_ms: u64,
    preset: SortformerPreset,
    libs: Option<PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    let mut positional = Vec::new();
    let mut cpu = false;
    let mut gap_ms = 600;
    let mut preset = SortformerPreset::VeryHighLatency;
    let mut libs = None;
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--cpu" => cpu = true,
            "--gap" => {
                gap_ms = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or("--gap needs milliseconds")?
            }
            "--preset" => {
                preset = match it.next().as_deref() {
                    Some("vhl") => SortformerPreset::VeryHighLatency,
                    Some("hl") => SortformerPreset::HighLatency,
                    Some("ll") => SortformerPreset::LowLatency,
                    Some("default") => SortformerPreset::Default,
                    _ => return Err("--preset vhl|hl|ll|default".into()),
                }
            }
            "--libs" => libs = Some(PathBuf::from(it.next().ok_or("--libs needs a dir")?)),
            _ => positional.push(PathBuf::from(arg)),
        }
    }
    if positional.len() < 2 {
        return Err("usage: diarize_test <model.gguf> <audio.wav> [truth.csv] [--cpu] [--gap MS] [--preset vhl|hl|ll|default] [--libs DIR]".into());
    }
    Ok(Args {
        model: positional[0].clone(),
        audio: positional[1].clone(),
        truth: positional.get(2).cloned(),
        cpu,
        gap_ms,
        preset,
        libs,
    })
}

fn read_wav(path: &PathBuf) -> Result<Vec<f32>, String> {
    let mut reader = hound::WavReader::open(path).map_err(|e| e.to_string())?;
    let spec = reader.spec();
    if spec.sample_rate as u64 != SAMPLE_RATE || spec.channels != 1 {
        return Err(format!(
            "need 16 kHz mono, got {} Hz × {} ch",
            spec.sample_rate, spec.channels
        ));
    }
    match spec.sample_format {
        hound::SampleFormat::Int => {
            let scale = (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 / scale).map_err(|e| e.to_string()))
                .collect()
        }
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .map(|s| s.map_err(|e| e.to_string()))
            .collect(),
    }
}

fn read_truth(path: &PathBuf) -> Result<Vec<(u64, u64, String)>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let mut rows = Vec::new();
    for line in text.lines().skip(1) {
        let cols: Vec<&str> = line.trim().split(',').collect();
        if cols.len() < 3 {
            continue;
        }
        let t0 = cols[0].parse().map_err(|_| format!("bad line {line}"))?;
        let t1 = cols[1].parse().map_err(|_| format!("bad line {line}"))?;
        rows.push((t0, t1, cols[2].to_string()));
    }
    Ok(rows)
}

/// Energy VAD + gap rule, like the meeting segmenter (approximation).
fn segments(audio: &[f32], gap_ms: u64) -> Vec<(usize, usize)> {
    let rms: Vec<f32> = audio
        .chunks(FRAME)
        .map(|c| (c.iter().map(|s| s * s).sum::<f32>() / c.len() as f32).sqrt())
        .collect();
    let mut sorted = rms.clone();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let p90 = sorted.get(sorted.len() * 9 / 10).copied().unwrap_or(0.0);
    let threshold = (p90 * 0.05).max(1e-3);
    let gap_frames = (gap_ms as usize * SAMPLE_RATE as usize / 1000).div_ceil(FRAME);
    let max_len = diarize::ms_to_samples(HARD_MAX_MS);
    let preroll = diarize::ms_to_samples(PREROLL_MS);

    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    let mut last_voiced = 0usize;
    for (i, level) in rms.iter().enumerate() {
        let pos = i * FRAME;
        if *level >= threshold {
            if start.is_none() {
                start = Some(pos.saturating_sub(preroll));
            }
            last_voiced = i;
        }
        if let Some(s) = start {
            let end_of_speech = i - last_voiced >= gap_frames;
            let too_long = pos + FRAME - s >= max_len;
            if end_of_speech || too_long {
                let end = ((last_voiced + 1) * FRAME + preroll).min(audio.len());
                let end = if too_long {
                    (pos + FRAME).min(audio.len())
                } else {
                    end
                };
                if end > s {
                    out.push((s, end));
                }
                start = None;
            }
        }
    }
    if let Some(s) = start {
        let end = ((last_voiced + 1) * FRAME + preroll).min(audio.len());
        if end > s {
            out.push((s, end));
        }
    }
    out
}

fn truth_label(truth: &[(u64, u64, String)], t0: u64, t1: u64) -> String {
    let mut acc: BTreeMap<&str, u64> = BTreeMap::new();
    for (a, b, label) in truth {
        let o = t1.min(*b).saturating_sub(t0.max(*a));
        if o > 0 {
            *acc.entry(label.as_str()).or_default() += o;
        }
    }
    let mut v: Vec<_> = acc.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    v.iter()
        .map(|(l, ms)| format!("{l}:{:.1}", *ms as f64 / 1000.0))
        .collect::<Vec<_>>()
        .join(" ")
}

fn secs(ms: u64) -> String {
    format!("{:.2}", ms as f64 / 1000.0)
}

/// Rows = truth label, columns = assigned speaker; cells = ms.
fn print_table(rows: &BTreeMap<String, BTreeMap<String, u64>>) {
    let cols: std::collections::BTreeSet<String> =
        rows.values().flat_map(|r| r.keys().cloned()).collect();
    print!("{:>8}", "truth");
    for c in &cols {
        print!("{c:>9}");
    }
    println!();
    let mut total = 0u64;
    let mut row_best = 0u64;
    for (label, r) in rows {
        print!("{label:>8}");
        for c in &cols {
            print!("{:>9}", secs(*r.get(c).unwrap_or(&0)));
        }
        println!();
        total += r.values().sum::<u64>();
        row_best += r.values().max().copied().unwrap_or(0);
    }
    let mut col_best = 0u64;
    for c in &cols {
        col_best += rows
            .values()
            .map(|r| *r.get(c).unwrap_or(&0))
            .max()
            .unwrap_or(0);
    }
    if total > 0 {
        println!(
            "purity (each speaker is one person): {:.1}%   coverage (each person is one speaker): {:.1}%",
            col_best as f64 * 100.0 / total as f64,
            row_best as f64 * 100.0 / total as f64
        );
    }
}

fn add_overlap(
    table: &mut BTreeMap<String, BTreeMap<String, u64>>,
    truth: &[(u64, u64, String)],
    t0: u64,
    t1: u64,
    column: &str,
) {
    for (a, b, label) in truth {
        let o = t1.min(*b).saturating_sub(t0.max(*a));
        if o > 0 {
            *table
                .entry(label.clone())
                .or_default()
                .entry(column.to_string())
                .or_default() += o;
        }
    }
}

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let init = match &args.libs {
        Some(dir) => transcribe_cpp::init_backends(dir),
        None => transcribe_cpp::init_backends_default(),
    };
    if let Err(e) = init {
        eprintln!("transcribe.cpp backends: {e}");
    }
    for d in transcribe_cpp::devices() {
        println!("device: {} ({})", d.name, d.kind);
    }

    let audio = read_wav(&args.audio).unwrap_or_else(|e| {
        eprintln!("audio: {e}");
        std::process::exit(1)
    });
    let truth = match &args.truth {
        Some(p) => read_truth(p).unwrap_or_else(|e| {
            eprintln!("truth: {e}");
            std::process::exit(1)
        }),
        None => Vec::new(),
    };
    let total_ms = diarize::samples_to_ms(audio.len());

    let started = Instant::now();
    let mut engine = SortformerEngine::load(&args.model, args.cpu).unwrap_or_else(|e| {
        eprintln!("model: {e}");
        std::process::exit(1)
    });
    engine.set_preset(args.preset);
    println!(
        "model loaded on {} in {} ms; audio {} s; preset {:?}",
        engine.backend(),
        started.elapsed().as_millis(),
        secs(total_ms),
        args.preset
    );

    // 1. Whole file in one run (the model's own consistency, no tracker).
    let t = Instant::now();
    let turns: Vec<Turn> = engine.run(&audio).unwrap_or_else(|e| {
        eprintln!("run: {e}");
        std::process::exit(1)
    });
    let whole_ms = t.elapsed().as_millis();
    println!("\n== whole file: {} turns in {} ms", turns.len(), whole_ms);
    let mut table = BTreeMap::new();
    for turn in &turns {
        if turn.t1_ms - turn.t0_ms >= 300 {
            println!(
                "  {:>7}–{:<7} S{}",
                secs(turn.t0_ms),
                secs(turn.t1_ms),
                turn.speaker
            );
        }
        add_overlap(
            &mut table,
            &truth,
            turn.t0_ms,
            turn.t1_ms,
            &format!("S{}", turn.speaker),
        );
    }
    if !truth.is_empty() {
        print_table(&table);
    }

    // 2. Meeting simulation: segment by segment with the tracker.
    let segs = segments(&audio, args.gap_ms);
    println!(
        "\n== meeting simulation: {} segments (gap {} ms)",
        segs.len(),
        args.gap_ms
    );
    let mut tracker = SpeakerTracker::new();
    let mut table = BTreeMap::new();
    let mut busy_ms = 0u128;
    let mut worst_ms = 0u128;
    for (n, (a, b)) in segs.iter().enumerate() {
        let segment = &audio[*a..*b];
        let seg_t0 = diarize::samples_to_ms(*a);
        let seg_t1 = diarize::samples_to_ms(*b);
        let input_ms = diarize::samples_to_ms(tracker.prepare(segment).input.len());
        let t = Instant::now();
        let assigned: Vec<Assigned> = match engine.diarize_segment(&mut tracker, segment) {
            Ok(v) => diarize::snap_pieces(&v, segment),
            Err(e) => {
                println!("  #{n:<3} {e}");
                continue;
            }
        };
        let took = t.elapsed().as_millis();
        busy_ms += took;
        worst_ms = worst_ms.max(took);
        let pieces: Vec<String> = assigned
            .iter()
            .map(|p| {
                format!(
                    "{}–{} «Собеседник {}» [{}]",
                    secs(seg_t0 + p.t0_ms),
                    secs(seg_t0 + p.t1_ms),
                    p.slot,
                    truth_label(&truth, seg_t0 + p.t0_ms, seg_t0 + p.t1_ms)
                )
            })
            .collect();
        println!(
            "  #{n:<3} {:>6}–{:<6} ({:>4} s, input {:>4} s, {:>4} ms)  {}",
            secs(seg_t0),
            secs(seg_t1),
            secs(seg_t1 - seg_t0),
            secs(input_ms),
            took,
            pieces.join(" | ")
        );
        for p in &assigned {
            add_overlap(
                &mut table,
                &truth,
                seg_t0 + p.t0_ms,
                seg_t0 + p.t1_ms,
                &format!("S{}", p.slot),
            );
        }
    }
    println!(
        "remote speakers: {}; diarizer time {} ms total, {} ms worst segment, RTF {:.3}",
        tracker.speaker_count(),
        busy_ms,
        worst_ms,
        busy_ms as f64 / total_ms.max(1) as f64
    );
    for slot in 1..=tracker.speaker_count() {
        println!("  clip S{slot}: {} s", secs(tracker.clip_ms(slot)));
    }
    if !truth.is_empty() {
        print_table(&table);
    }
}
