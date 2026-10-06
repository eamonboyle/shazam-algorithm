//! Matching by offset voting: for the right song, matching hashes all agree on
//! one time offset between the clip and the song; for wrong songs they scatter.

use std::collections::HashMap;

use crate::db::Database;
use crate::fingerprint::{frames_to_secs, Hash};

/// Minimum number of time-aligned hash matches to call it a match...
const MIN_SCORE: u32 = 80;
/// ...and how many times stronger than the runner-up it must be.
const MIN_RATIO: f32 = 2.5;

#[derive(Clone, Debug)]
pub struct Candidate {
    pub song: u32,
    /// Number of hashes agreeing on `offset_frames` (±1 frame of jitter).
    pub score: u32,
    /// Where in the song the clip starts.
    pub offset_frames: i32,
}

impl Candidate {
    pub fn offset_secs(&self) -> f32 {
        frames_to_secs(self.offset_frames as f32)
    }
}

pub struct MatchResult {
    /// Best candidate per song, strongest first.
    pub ranked: Vec<Candidate>,
    pub query_hashes: usize,
}

impl MatchResult {
    pub fn best(&self) -> Option<&Candidate> {
        self.ranked.first()
    }

    pub fn runner_up_score(&self) -> u32 {
        self.ranked.get(1).map_or(0, |c| c.score)
    }

    /// The confident match, if any.
    pub fn confident(&self) -> Option<&Candidate> {
        let best = self.best()?;
        let ratio_ok = best.score as f32 >= MIN_RATIO * self.runner_up_score().max(1) as f32;
        (best.score >= MIN_SCORE && ratio_ok).then_some(best)
    }
}

pub fn find_matches(db: &Database, query: &[Hash]) -> MatchResult {
    // votes[(song, offset)] = number of hashes implying that alignment.
    let mut votes: HashMap<(u32, i32), u32> = HashMap::new();
    for q in query {
        for e in db.lookup(q.hash) {
            let offset = e.frame as i32 - q.frame as i32;
            *votes.entry((e.song, offset)).or_default() += 1;
        }
    }

    // Peaks can land a frame early or late depending on how the clip's
    // frames line up with the song's, so count neighbouring offsets too.
    let mut best_per_song: HashMap<u32, Candidate> = HashMap::new();
    for (&(song, offset), &count) in &votes {
        let neighbours = votes.get(&(song, offset - 1)).copied().unwrap_or(0)
            + votes.get(&(song, offset + 1)).copied().unwrap_or(0);
        let score = count + neighbours;
        let current = best_per_song.entry(song).or_insert(Candidate {
            song,
            score: 0,
            offset_frames: offset,
        });
        if score > current.score {
            *current = Candidate {
                song,
                score,
                offset_frames: offset,
            };
        }
    }

    let mut ranked: Vec<Candidate> = best_per_song.into_values().collect();
    ranked.sort_unstable_by(|a, b| b.score.cmp(&a.score).then(a.song.cmp(&b.song)));
    MatchResult {
        ranked,
        query_hashes: query.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Song;
    use crate::fingerprint::{Fingerprinter, SAMPLE_RATE};

    /// Deterministic pseudo-random generator for synthetic test music.
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> f32 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (self.0 >> 40) as f32 / (1u64 << 24) as f32
        }
    }

    /// A synthetic "song": a sequence of random three-note chords, each with a
    /// sharp attack and exponential decay, like plucked or struck notes.
    fn synth_song(seed: u64, secs: f32) -> Vec<f32> {
        let mut rng = Lcg(seed);
        let total = (secs * SAMPLE_RATE as f32) as usize;
        let mut out = vec![0.0f32; total];
        let mut start = 0;
        while start < total {
            let len = ((0.15 + 0.35 * rng.next()) * SAMPLE_RATE as f32) as usize;
            let notes: Vec<(f32, f32)> = (0..3)
                .map(|_| (200.0 + 3_800.0 * rng.next(), 0.2 + 0.8 * rng.next()))
                .collect();
            for i in 0..len.min(total - start) {
                let t = i as f32 / SAMPLE_RATE as f32;
                let env = (-t * 6.0).exp();
                out[start + i] += notes
                    .iter()
                    .map(|&(f, a)| a * env * (2.0 * std::f32::consts::PI * f * t).sin())
                    .sum::<f32>()
                    * 0.2;
            }
            start += len;
        }
        out
    }

    fn build_db(songs: &[Vec<f32>]) -> Database {
        let fp = Fingerprinter::default();
        Database::build(
            songs
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    let song = Song {
                        title: format!("song {i}"),
                        path: String::new(),
                        duration_secs: 0.0,
                    };
                    (song, fp.fingerprint(s))
                })
                .collect(),
        )
    }

    #[test]
    fn identifies_song_and_offset_from_noisy_clip() {
        let songs: Vec<Vec<f32>> = (0..6).map(|i| synth_song(i + 1, 40.0)).collect();
        let db = build_db(&songs);

        // 6 s clip from song 3, starting at 17.3 s (deliberately not a whole
        // number of frames), at half volume with noise added.
        let start = (17.3 * SAMPLE_RATE as f32) as usize;
        let mut rng = Lcg(99);
        let clip: Vec<f32> = songs[3][start..start + 6 * SAMPLE_RATE as usize]
            .iter()
            .map(|x| 0.5 * x + 0.02 * (rng.next() - 0.5))
            .collect();

        let result = find_matches(&db, &Fingerprinter::default().fingerprint(&clip));
        let best = result.confident().expect("expected a confident match");
        assert_eq!(best.song, 3);
        assert!(
            (best.offset_secs() - 17.3).abs() < 0.1,
            "offset {}",
            best.offset_secs()
        );
    }

    #[test]
    fn rejects_song_not_in_database() {
        let songs: Vec<Vec<f32>> = (0..6).map(|i| synth_song(i + 1, 40.0)).collect();
        let db = build_db(&songs);
        let unknown = synth_song(1_000, 10.0);
        let result = find_matches(&db, &Fingerprinter::default().fingerprint(&unknown));
        assert!(result.confident().is_none(), "matched {:?}", result.best());
    }

    #[test]
    fn database_round_trips_through_disk() {
        let db = build_db(&[synth_song(7, 10.0)]);
        let path = std::env::temp_dir().join(format!("shazam-test-{}.db", std::process::id()));
        db.save(&path).unwrap();
        let loaded = Database::load(&path).unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(loaded.num_hashes(), db.num_hashes());
        assert_eq!(loaded.songs[0].title, "song 0");
    }
}
