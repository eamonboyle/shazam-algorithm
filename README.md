# shazam-algorithm

[![CI](https://github.com/eamonboyle/shazam-algorithm/actions/workflows/ci.yml/badge.svg)](https://github.com/eamonboyle/shazam-algorithm/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

A from-scratch Rust implementation of the audio fingerprinting algorithm behind
Shazam ([Wang, 2003](https://www.ee.columbia.edu/~dpwe/papers/Wang03-shazam.pdf)).
Point it at a folder of music, then play a song near your laptop and it tells
you what's playing, usually within 2–4 seconds.

```console
$ shazam listen
Listening on "MacBook Pro Microphone" (48000 Hz) — play some music. (54 songs indexed)
🎵 C-Funk
   at 0:02 of 2:50 · 143 aligned hashes (runner-up 3) out of 946
```

- **Live microphone recognition** via CoreAudio (macOS); also builds on Linux and Windows through [cpal](https://github.com/RustAudio/cpal)
- **Reads MP3, WAV, FLAC, OGG and AAC/M4A** with pure-Rust decoding, so there are no system audio libraries to install
- **Robust to noise**: 95% of 5-second clips identified with background noise at 10 dB SNR, and no wrong answers or false positives in 1,500+ test clips ([evaluation](docs/evaluation.md))
- **Fast**: indexes 54 songs (2.8 hours of audio) in ~6 s

## Quick start

You'll need [Rust](https://rustup.rs) (stable). On macOS you also need the Xcode
Command Line Tools (`xcode-select --install`).

```sh
git clone https://github.com/eamonboyle/shazam-algorithm.git
cd shazam-algorithm
./scripts/fetch-songs.sh               # 60 free CC-licensed test songs (~420 MB)
cargo build --release
./target/release/shazam index songs/library
./target/release/shazam listen         # now play a song from songs/library
```

To try it on one machine, play a song through the speakers while it listens:

```sh
afplay "songs/library/C-Funk.mp3" & ./target/release/shazam listen
```

The first time, macOS will ask to give your terminal app microphone access. If
you decline, the recording is silent and `listen` will tell you where to
re-enable it.

You can index your own music instead: `shazam index ~/Music/some-folder`.

## Commands

| Command | What it does |
|---|---|
| `shazam index <dir>` | Fingerprints every audio file in `<dir>` (recursively) into `fingerprints.db` |
| `shazam listen [--max-secs 20] [--save rec.wav]` | Listens on the default microphone until it's confident or times out |
| `shazam match <file>` | Identifies an audio file (any length) |
| `shazam eval <dir> [--holdout <dir>] [--snr-db 10] [--clip-secs 5]` | Accuracy test: random noisy clips from indexed songs, plus unindexed songs that must *not* match |

All commands accept `--db <path>` to use a different database file. Run
`shazam <command> --help` for every option.

## How it works

```
audio → mono @ 11 kHz → spectrogram → peaks ("constellation") → peak-pair hashes
                                                                      │
                     song + timestamp  ←  offset voting  ←  database lookup
```

1. **Spectrogram**: a 1024-point FFT every 23 ms.
2. **Constellation map**: keep only local loudness peaks in time and frequency. These survive noise and bad speakers far better than raw audio does.
3. **Hashes**: pair each peak with 20 nearby later peaks, and hash `(freq₁, freq₂, Δtime)`. A hash contains no absolute time, so a clip from mid-song produces the same hashes as the song itself.
4. **Matching**: for each matching hash, compute *song time − clip time*. The right song has a big spike of hashes agreeing on one offset; wrong songs only have scattered coincidences.

The full walkthrough is in [docs/algorithm.md](docs/algorithm.md). Accuracy
results and how the parameters were tuned are in
[docs/evaluation.md](docs/evaluation.md).

## Project layout

```
src/
  main.rs         CLI: index, match, listen, eval
  audio.rs        decoding (symphonia), windowed-sinc resampler, WAV writer
  fingerprint.rs  spectrogram, peak picking, hashing (all tunable constants here)
  db.rs           sorted on-disk hash index
  matcher.rs      offset voting and the confidence rule
  mic.rs          microphone capture (cpal)
scripts/
  fetch-songs.sh  downloads the test library listed in songs.txt
docs/             algorithm walkthrough and evaluation
```

## Development

```sh
cargo test                                 # unit tests use synthetic audio, no music files needed
cargo clippy --all-targets -- -D warnings
cargo fmt
```

If you change any constant in `src/fingerprint.rs`, bump
`FINGERPRINT_VERSION` and re-run `shazam index`. Old databases are then rejected
instead of silently failing to match.

## Credits and licence

- Code: [MIT](LICENSE).
- Algorithm: Avery Li-Chun Wang, *An Industrial-Strength Audio Search
  Algorithm*, ISMIR 2003.
- Test music (downloaded by the fetch script, not stored in this repository): tracks by Kevin MacLeod
  ([incompetech.com](https://incompetech.com)), licensed under
  [Creative Commons: By Attribution 4.0](https://creativecommons.org/licenses/by/4.0/).
  The full track list is in [`scripts/songs.txt`](scripts/songs.txt).

This is an educational project and is not affiliated with or endorsed by
Shazam or Apple.
