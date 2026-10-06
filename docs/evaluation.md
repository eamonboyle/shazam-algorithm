# Evaluation

How accurate is it, and how were the parameters chosen? All numbers here can be
reproduced with the test library (`scripts/fetch-songs.sh`) and the `eval`
command.

## Test library

60 tracks by Kevin MacLeod ([incompetech.com](https://incompetech.com),
CC BY 4.0), picked at random across genres and limited to 1.5–4.5 minutes:
rock, jazz, electronic, classical, ambient, world, polka and so on. The list is in
[`scripts/songs.txt`](../scripts/songs.txt).

- **54 songs are indexed** (`songs/library`).
- **6 songs are never indexed** (`songs/holdout`). Any match on these is a
  false positive.

The library includes some deliberately tricky cases: two tempo versions of the
same tune ("Pixel Peeker Polka - faster" / "- slower"), quiet ambient and
string pieces, and heavily looped electronic tracks.

## Method: `shazam eval`

For each song, `eval` cuts random clips (a random start position that isn't
aligned to frame boundaries), scales them to a random volume between 20% and 100%,
adds white noise at a chosen signal-to-noise ratio (SNR), and runs the clip
through the matcher. Each clip is scored as:

| Outcome | Meaning |
|---|---|
| correct | confident match on the right song, timestamp within 0.1 s |
| matched a repeated section | right song, but the timestamp of an identical repeat elsewhere in it. Counted as correct, since the song *was* identified |
| wrong song | confident match on a different song |
| missed | no confident match |
| false positive | an unindexed song matched something |

Runs are deterministic (`--seed`), so before/after comparisons are like for like.

SNR is the volume of the music relative to the noise: **10 dB** is music clearly
on top of background noise; **0 dB** means noise as loud as the music.

## Results

Final parameters (see [algorithm.md](algorithm.md)), 54 songs × 5 clips:

| Clip | SNR | Identified | Wrong song | Unknown songs rejected |
|---|---|---:|---:|---:|
| 5 s | clean | **100%** (270/270) | 0 | 30/30 |
| 5 s | 10 dB | **95.2%** (257/270) | 0 | 30/30 |
| 5 s | 5 dB | **83.0%** (224/270) | 0 | 30/30 |
| 5 s | 0 dB | **56.7%** (153/270) | 0 | 30/30 |

Across every run in this document, the matcher **never named the wrong song**
and **never matched an unknown song**. When it isn't sure, it says so.

Unknown-song clips at different lengths (6 songs × 20 clips, no noise):

| Clip | Rejected | Highest chance score |
|---|---:|---:|
| 5 s | 120/120 | 61 |
| 10 s | 120/120 | 90 |
| 20 s | 120/120 | 84 |

### Real speaker → microphone

Songs played through the MacBook Pro's speakers with `afplay`, identified by
`shazam listen` through its built-in microphone, in a normal room:

| Song | Result | Time to match | Score (runner-up) |
|---|---|---:|---|
| C-Funk | ✅ | ~2 s | 143 (3) |
| Clear Air (ambient) | ✅ | ~3 s | 119 (25) |
| Frozen Star (strings) | ✅ | ~4 s | 84 (29) |
| Funeral March for Brass | ✅ | ~3 s | 81 (25) |
| Pixel Peeker Polka - faster | ✅ | ~2 s | 209 (18) |
| Pixel Peeker Polka - slower | ✅ | ~2 s | 234 (15) |
| Sad Trio (*not indexed*) | ✅ no match after 15 s | – | best 38 |
| Slow Burn (*not indexed*) | ✅ no match after 15 s | – | best 33 |

The real acoustic path (speaker response, room echo, mic colouring) turned out
easier than heavy synthetic white noise. Quieter, sparser songs take a second or
two longer to build up enough aligned hashes.

## How the parameters were tuned

### Peak picking and fan-out

The first version used a ±10 bin / ±6 frame peak neighbourhood and paired each
anchor with 10 targets. Clean audio was already 100%, but at 5 dB SNR it found
only 59% of songs. Each change below was measured alone on the same 5 s, 5 dB
clips (threshold 30 at the time):

| Variant | Identified @ 5 dB |
|---|---:|
| Baseline (±10 bins, ±6 frames, fan-out 10) | 160/270 (59%) |
| 50 peaks/sec instead of 30 | 150/270 |
| Include bass down to ~86 Hz | 169/270 |
| ±3 frame neighbourhood | 191/270 |
| Fan-out 20 | 199/270 |
| ±5 bin neighbourhood | 202/270 |
| ±5 bins + ±3 frames | 237/270 |
| ±5 bins + fan-out 20 | 242/270 |
| **±5 bins + ±3 frames + fan-out 20** | **255/270 (94%)** |

Smaller neighbourhoods give a denser constellation, so more peaks survive the
noise. A larger fan-out gives more hashes per surviving peak. Together they
raised 5 dB accuracy from 59% to 94%, at the cost of a database twice the size.

### Match threshold

Denser fingerprints raise *all* scores, chance ones included: unknown 20 s
clips reached 84–90 aligned hashes, although always with an almost-equal
runner-up, so the ratio test rejected them. To keep an absolute safety margin
too, the minimum score was raised:

| Min score | clean | 10 dB | 5 dB | 0 dB |
|---|---:|---:|---:|---:|
| 30 | 100% | – | 94% | – |
| **80** (chosen) | 100% | 95.2% | 83.0% | 56.7% |
| 120 | 100% | 93.3% | 71.1% | 47.8% |

(5 s clips. With 10 s clips at threshold 120: 97.0% / 88.5% / 67.0% at
10 / 5 / 0 dB.)

80 sits above every chance score seen for 5–10 s clips. On fixed 5 s eval
clips it costs some noisy-clip accuracy, but `listen` keeps recording and
re-checking every second, so in practice a higher threshold only means
waiting a bit longer before it answers.

### Tried and reverted: counting distinct hashes

Hypothesis: chance scores come from sustained notes repeating the same hash,
which then line up against another song's sustained note. Counting each
distinct hash value only once per alignment should remove them. Measured: the
highest unknown-clip score went from 61 to 60 (5 s) and 84 to 83 (20 s), so
there was no meaningful effect, and the change was reverted.

## Reproducing

```sh
./scripts/fetch-songs.sh
cargo build --release
./target/release/shazam index songs/library
for snr in inf 10 5 0; do
  ./target/release/shazam eval songs/library --holdout songs/holdout --clips-per-song 5 --snr-db $snr
done
```
