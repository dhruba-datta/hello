//! Opt-in collection of the user's own bilingual dictations, kept on this Mac,
//! so language routing can be checked and tuned against real speech.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use color_eyre::eyre::Result;
use hound::{SampleFormat, WavSpec, WavWriter};
use serde::Serialize;

/// Enough to tune on; collection stops here so the folder stays small.
pub const MAX_SAMPLES: usize = 100;

#[derive(Serialize)]
pub struct PieceGuess {
    pub start_ms: u64,
    pub end_ms: u64,
    pub language: Option<String>,
}

#[derive(Serialize)]
pub struct SampleSpan {
    pub start_ms: u64,
    pub end_ms: u64,
    pub second_language: bool,
}

#[derive(Serialize)]
pub struct SampleRecord {
    pub recorded_at_ms: u64,
    pub duration_ms: u64,
    pub routing: String,
    pub pieces: Vec<PieceGuess>,
    pub spans: Vec<SampleSpan>,
    pub text: String,
}

pub fn directory() -> Result<PathBuf> {
    Ok(crate::app_paths::support_dir()?.join("language-samples"))
}

pub fn count() -> usize {
    directory()
        .ok()
        .and_then(|directory| fs::read_dir(directory).ok())
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok())
                .filter(|entry| {
                    entry
                        .path()
                        .extension()
                        .is_some_and(|extension| extension == "wav")
                })
                .count()
        })
        .unwrap_or(0)
}

/// Saves one dictation (16 kHz audio) and what Hello made of it.
pub fn save(samples: &[f32], record: &SampleRecord) -> Result<()> {
    if count() >= MAX_SAMPLES {
        return Ok(());
    }
    let directory = directory()?;
    fs::create_dir_all(&directory)?;
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
    let stem = format!("dictation-{}", record.recorded_at_ms);
    let audio = directory.join(format!("{stem}.wav"));
    let mut writer = WavWriter::create(
        &audio,
        WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: SampleFormat::Int,
        },
    )?;
    for sample in samples {
        writer.write_sample((sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16)?;
    }
    writer.finalize()?;
    fs::set_permissions(&audio, fs::Permissions::from_mode(0o600))?;
    let details = directory.join(format!("{stem}.json"));
    fs::write(&details, serde_json::to_vec_pretty(record)?)?;
    fs::set_permissions(&details, fs::Permissions::from_mode(0o600))?;
    Ok(())
}
