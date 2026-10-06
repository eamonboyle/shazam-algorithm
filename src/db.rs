//! The fingerprint database: a flat list of `(hash, song, frame)` entries sorted
//! by hash, so a lookup is a binary search.

use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::fingerprint::{Hash, FINGERPRINT_VERSION};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Song {
    pub title: String,
    pub path: String,
    pub duration_secs: f32,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
pub struct Entry {
    pub hash: u32,
    pub song: u32,
    pub frame: u32,
}

#[derive(Serialize, Deserialize)]
pub struct Database {
    version: u32,
    pub songs: Vec<Song>,
    entries: Vec<Entry>,
}

impl Database {
    pub fn build(fingerprinted: Vec<(Song, Vec<Hash>)>) -> Self {
        let mut songs = Vec::with_capacity(fingerprinted.len());
        let mut entries = Vec::new();
        for (id, (song, hashes)) in fingerprinted.into_iter().enumerate() {
            songs.push(song);
            entries.extend(hashes.iter().map(|h| Entry {
                hash: h.hash,
                song: id as u32,
                frame: h.frame,
            }));
        }
        entries.sort_unstable_by_key(|e| e.hash);
        Self {
            version: FINGERPRINT_VERSION,
            songs,
            entries,
        }
    }

    pub fn lookup(&self, hash: u32) -> &[Entry] {
        let start = self.entries.partition_point(|e| e.hash < hash);
        let end = start + self.entries[start..].partition_point(|e| e.hash == hash);
        &self.entries[start..end]
    }

    pub fn num_hashes(&self) -> usize {
        self.entries.len()
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let w = BufWriter::new(File::create(path).with_context(|| format!("creating {}", path.display()))?);
        bincode::serialize_into(w, self)?;
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Self> {
        let f = File::open(path)
            .with_context(|| format!("can't open {} (run `shazam index <dir>` first)", path.display()))?;
        let db: Self = bincode::deserialize_from(BufReader::new(f))
            .with_context(|| format!("{} is corrupt; re-run `shazam index`", path.display()))?;
        if db.version != FINGERPRINT_VERSION {
            bail!(
                "{} was built with older fingerprint settings; re-run `shazam index`",
                path.display()
            );
        }
        Ok(db)
    }
}
