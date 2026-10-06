mod audio;
mod db;
mod fingerprint;
mod matcher;
mod mic;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use rayon::prelude::*;

use db::{Database, Song};
use fingerprint::{Fingerprinter, SAMPLE_RATE};
use matcher::{find_matches, MatchResult};

#[derive(Parser)]
#[command(name = "shazam", about = "Identify songs from audio files or your microphone")]
struct Cli {
    /// Fingerprint database file.
    #[arg(long, global = true, default_value = "fingerprints.db")]
    db: PathBuf,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Fingerprint every audio file in a directory (recursively) into the database.
    Index { dir: PathBuf },
    /// Identify an audio file.
    Match { file: PathBuf },
    /// Listen on the microphone and identify what's playing.
    Listen {
        /// Give up after this many seconds.
        #[arg(long, default_value_t = 20.0)]
        max_secs: f32,
        /// Save the recording to this WAV file (handy for debugging).
        #[arg(long)]
        save: Option<PathBuf>,
    },
    /// Accuracy test: match random noisy clips cut from indexed songs, plus
    /// clips from songs NOT in the index (which should not match).
    Eval {
        /// Directory of songs that are in the index.
        dir: PathBuf,
        /// Directory of songs that are not in the index.
        #[arg(long)]
        holdout: Option<PathBuf>,
        /// Length of each test clip, in seconds.
        #[arg(long, default_value_t = 5.0)]
        clip_secs: f32,
        /// Number of random clips taken from each song.
        #[arg(long, default_value_t = 3)]
        clips_per_song: usize,
        /// Signal-to-noise ratio of added white noise, in dB ("inf" for none).
        #[arg(long, default_value_t = 10.0)]
        snr_db: f32,
        /// Random seed for clip positions, volume and noise (same seed = same test).
        #[arg(long, default_value_t = 1)]
        seed: u64,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Index { dir } => index(&dir, &cli.db),
        Cmd::Match { file } => match_file(&file, &cli.db),
        Cmd::Listen { max_secs, save } => listen(&cli.db, max_secs, save.as_deref()),
        Cmd::Eval {
            dir,
            holdout,
            clip_secs,
            clips_per_song,
            snr_db,
            seed,
        } => eval(
            &cli.db,
            &dir,
            holdout.as_deref(),
            clip_secs,
            clips_per_song,
            snr_db,
            seed,
        ),
    }
}

/// Decodes a file and converts it to the fingerprinting sample rate.
fn load_audio(path: &Path) -> Result<Vec<f32>> {
    let (samples, rate) = audio::decode_file(path)?;
    Ok(audio::resample(&samples, rate, SAMPLE_RATE))
}

fn audio_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            files.extend(audio_files(&path)?);
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| audio::SUPPORTED_EXTENSIONS.contains(&e.to_lowercase().as_str()))
        {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

fn title_of(path: &Path) -> String {
    path.file_stem().map_or_else(
        || path.display().to_string(),
        |s| s.to_string_lossy().into_owned(),
    )
}

fn fmt_time(secs: f32) -> String {
    let s = secs.max(0.0).round() as u32;
    format!("{}:{:02}", s / 60, s % 60)
}

fn index(dir: &Path, db_path: &Path) -> Result<()> {
    let files = audio_files(dir)?;
    if files.is_empty() {
        bail!("no audio files found in {}", dir.display());
    }
    println!("Fingerprinting {} files...", files.len());
    let start = Instant::now();

    let results: Vec<_> = files
        .par_iter()
        .map(|path| {
            let samples = load_audio(path)?;
            let hashes = Fingerprinter::default().fingerprint(&samples);
            let song = Song {
                title: title_of(path),
                path: path.display().to_string(),
                duration_secs: samples.len() as f32 / SAMPLE_RATE as f32,
            };
            println!(
                "  {:<45} {:>5}  {:>7} hashes",
                song.title,
                fmt_time(song.duration_secs),
                hashes.len()
            );
            Ok::<_, anyhow::Error>((song, hashes))
        })
        .collect();

    let mut fingerprinted = Vec::new();
    for (path, result) in files.iter().zip(results) {
        match result {
            Ok(r) => fingerprinted.push(r),
            Err(e) => eprintln!("  skipping {}: {e:#}", path.display()),
        }
    }

    let db = Database::build(fingerprinted);
    db.save(db_path)?;
    println!(
        "Indexed {} songs ({} hashes) into {} in {:.1}s",
        db.songs.len(),
        db.num_hashes(),
        db_path.display(),
        start.elapsed().as_secs_f32()
    );
    Ok(())
}

fn print_result(db: &Database, result: &MatchResult, clip_secs: f32) {
    match result.confident() {
        Some(c) => {
            let song = &db.songs[c.song as usize];
            println!("🎵 {}", song.title);
            println!(
                "   at {} of {} · {} aligned hashes (runner-up {}) out of {}",
                fmt_time(c.offset_secs() + clip_secs),
                fmt_time(song.duration_secs),
                c.score,
                result.runner_up_score(),
                result.query_hashes
            );
        }
        None => {
            print!("No match.");
            if let Some(b) = result.best() {
                print!(
                    " Closest: {} ({} aligned hashes, runner-up {})",
                    db.songs[b.song as usize].title,
                    b.score,
                    result.runner_up_score()
                );
            }
            println!();
        }
    }
}

fn match_file(file: &Path, db_path: &Path) -> Result<()> {
    let db = Database::load(db_path)?;
    let samples = load_audio(file)?;
    let hashes = Fingerprinter::default().fingerprint(&samples);
    let result = find_matches(&db, &hashes);
    print_result(&db, &result, samples.len() as f32 / SAMPLE_RATE as f32);
    Ok(())
}

fn listen(db_path: &Path, max_secs: f32, save: Option<&Path>) -> Result<()> {
    let db = Database::load(db_path)?;
    let recorder = mic::Recorder::start()?;
    println!(
        "Listening on \"{}\" ({} Hz) — play some music. ({} songs indexed)",
        recorder.device_name,
        recorder.sample_rate,
        db.songs.len()
    );
    let fp = Fingerprinter::default();
    let mut warned_silent = false;

    let (raw, result) = loop {
        std::thread::sleep(Duration::from_secs(1));
        let raw = recorder.snapshot();
        let secs = raw.len() as f32 / recorder.sample_rate as f32;
        if secs < 2.0 {
            continue;
        }

        let rms = (raw.iter().map(|x| x * x).sum::<f32>() / raw.len() as f32).sqrt();
        if rms < 1e-5 && !warned_silent {
            eprintln!(
                "  (the microphone is completely silent — check System Settings → Privacy & \
                 Security → Microphone and allow your terminal app)"
            );
            warned_silent = true;
        }

        let samples = audio::resample(&raw, recorder.sample_rate, SAMPLE_RATE);
        let result = find_matches(&db, &fp.fingerprint(&samples));
        if result.confident().is_some() || secs >= max_secs {
            break (raw, result);
        }
        match result.best() {
            Some(b) => println!(
                "  {secs:>4.0}s  best guess: {} ({} vs {})",
                db.songs[b.song as usize].title,
                b.score,
                result.runner_up_score()
            ),
            None => println!("  {secs:>4.0}s  no hash matches yet"),
        }
    };
    let rate = recorder.sample_rate;
    drop(recorder);

    print_result(&db, &result, raw.len() as f32 / rate as f32);
    if let Some(path) = save {
        audio::write_wav(path, &raw, rate)?;
        println!("Saved recording to {}", path.display());
    }
    Ok(())
}

/// Tiny deterministic xorshift RNG, so eval runs are reproducible.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    /// Uniform in [0, 1).
    fn uniform(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }
    fn gaussian(&mut self) -> f32 {
        let u1 = self.uniform().max(1e-7);
        let u2 = self.uniform();
        (-2.0 * u1.ln()).sqrt() * (2.0 * std::f32::consts::PI * u2).cos()
    }
}

enum Outcome {
    Correct,
    WrongOffset,
    WrongSong,
    Missed,
    CorrectReject,
    FalsePositive,
}

struct Trial {
    title: String,
    start_secs: f32,
    outcome: Outcome,
    /// Score of the true song (indexed clips) or of the best guess (holdout clips).
    score: u32,
    runner_up: u32,
}

fn eval(
    db_path: &Path,
    dir: &Path,
    holdout: Option<&Path>,
    clip_secs: f32,
    clips_per_song: usize,
    snr_db: f32,
    seed: u64,
) -> Result<()> {
    let db = Database::load(db_path)?;
    let mut files: Vec<(PathBuf, bool)> = audio_files(dir)?.into_iter().map(|p| (p, true)).collect();
    if let Some(h) = holdout {
        files.extend(audio_files(h)?.into_iter().map(|p| (p, false)));
    }
    println!(
        "Evaluating {} files × {clips_per_song} clips of {clip_secs}s at {snr_db} dB SNR...",
        files.len()
    );

    let trials: Vec<Trial> = files
        .par_iter()
        .enumerate()
        .flat_map(|(file_idx, (path, indexed))| {
            let title = title_of(path);
            let expected = if *indexed {
                match db.songs.iter().position(|s| s.title == title) {
                    Some(id) => Some(id as u32),
                    None => {
                        eprintln!("  {title} isn't in the database; skipping");
                        return Vec::new();
                    }
                }
            } else {
                None
            };
            let song = match load_audio(path) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("  skipping {}: {e:#}", path.display());
                    return Vec::new();
                }
            };
            let clip_len = (clip_secs * SAMPLE_RATE as f32) as usize;
            if song.len() <= clip_len {
                return Vec::new();
            }
            let fp = Fingerprinter::default();
            let mut rng = Rng::new(seed ^ ((file_idx as u64 + 1) << 32));

            (0..clips_per_song)
                .map(|_| {
                    let start = (rng.uniform() * (song.len() - clip_len) as f32) as usize;
                    let gain = 0.2 + 0.8 * rng.uniform();
                    let mut clip: Vec<f32> = song[start..start + clip_len].iter().map(|x| x * gain).collect();
                    let rms = (clip.iter().map(|x| x * x).sum::<f32>() / clip.len() as f32).sqrt();
                    let noise = rms / 10f32.powf(snr_db / 20.0);
                    for x in &mut clip {
                        *x += noise * rng.gaussian();
                    }

                    let result = find_matches(&db, &fp.fingerprint(&clip));
                    let start_secs = start as f32 / SAMPLE_RATE as f32;
                    let runner_up = result.runner_up_score();
                    let (outcome, score) = match (expected, result.confident()) {
                        (Some(want), Some(c)) if c.song == want => {
                            let ok = (c.offset_secs() - start_secs).abs() < 0.1;
                            (
                                if ok {
                                    Outcome::Correct
                                } else {
                                    Outcome::WrongOffset
                                },
                                c.score,
                            )
                        }
                        (Some(want), got) => {
                            let true_score = result
                                .ranked
                                .iter()
                                .find(|c| c.song == want)
                                .map_or(0, |c| c.score);
                            (
                                if got.is_some() {
                                    Outcome::WrongSong
                                } else {
                                    Outcome::Missed
                                },
                                true_score,
                            )
                        }
                        (None, got) => {
                            let best = result.best().map_or(0, |c| c.score);
                            (
                                if got.is_some() {
                                    Outcome::FalsePositive
                                } else {
                                    Outcome::CorrectReject
                                },
                                best,
                            )
                        }
                    };
                    Trial {
                        title: title.clone(),
                        start_secs,
                        outcome,
                        score,
                        runner_up,
                    }
                })
                .collect::<Vec<_>>()
        })
        .collect();

    let count = |f: fn(&Outcome) -> bool| trials.iter().filter(|t| f(&t.outcome)).count();
    let indexed = count(|o| {
        matches!(
            o,
            Outcome::Correct | Outcome::WrongOffset | Outcome::WrongSong | Outcome::Missed
        )
    });
    // A "wrong offset" is still the right song: the clip matched an identical
    // repeat (chorus, loop) elsewhere in it.
    let correct = count(|o| matches!(o, Outcome::Correct | Outcome::WrongOffset));
    let other_repeat = count(|o| matches!(o, Outcome::WrongOffset));
    let holdouts = trials.len() - indexed;
    let rejects = count(|o| matches!(o, Outcome::CorrectReject));

    for t in &trials {
        let label = match t.outcome {
            Outcome::WrongSong => "WRONG SONG",
            Outcome::Missed => "MISSED",
            Outcome::FalsePositive => "FALSE POSITIVE",
            _ => continue,
        };
        println!(
            "  {label:<14} {} @ {} (score {}, runner-up {})",
            t.title,
            fmt_time(t.start_secs),
            t.score,
            t.runner_up
        );
    }

    let mut true_scores: Vec<u32> = trials
        .iter()
        .filter(|t| !matches!(t.outcome, Outcome::CorrectReject | Outcome::FalsePositive))
        .map(|t| t.score)
        .collect();
    true_scores.sort_unstable();
    let is_unknown = |t: &&Trial| matches!(t.outcome, Outcome::CorrectReject | Outcome::FalsePositive);
    let max_unknown = trials
        .iter()
        .filter(is_unknown)
        .map(|t| t.score)
        .max()
        .unwrap_or(0);
    let max_runner_up = trials
        .iter()
        .filter(|t| !is_unknown(t))
        .map(|t| t.runner_up)
        .max()
        .unwrap_or(0);

    println!();
    println!(
        "Indexed clips:  {correct}/{indexed} identified correctly ({:.1}%; {other_repeat} matched a repeated section)",
        100.0 * correct as f32 / indexed.max(1) as f32
    );
    if holdouts > 0 {
        println!("Unknown clips:  {rejects}/{holdouts} correctly rejected");
    }
    if !true_scores.is_empty() {
        println!(
            "True-song score: min {} · median {} · max {}   |   best runner-up: {max_runner_up}",
            true_scores[0],
            true_scores[true_scores.len() / 2],
            true_scores[true_scores.len() - 1]
        );
    }
    if holdouts > 0 {
        println!("Best score on an unknown clip: {max_unknown}");
    }
    Ok(())
}
