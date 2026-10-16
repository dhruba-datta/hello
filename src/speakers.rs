//! Tells the other people in a meeting apart by voice, and remembers voices the
//! user has named so later meetings label them automatically.
//!
//! Voices are compared with 3D-Speaker's CAM++ embedding model (Apache-2.0,
//! trained on about 200,000 speakers),
//! run on this Mac through the ONNX Runtime that ships inside Hello.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use color_eyre::eyre::{Result, WrapErr, bail, eyre};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const MODEL_FILE: &str = "3dspeaker_speech_campplus_sv_zh_en_16k-common_advanced.onnx";
const MODEL_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/3dspeaker_speech_campplus_sv_zh_en_16k-common_advanced.onnx";
const MODEL_SHA256: &str = "aa3cfc16963a10586a9393f5035d6d6b57e98d358b347f80c2a30bf4f00ceba2";
pub const MODEL_BYTES: u64 = 28_281_164;
const RUNTIME_LIBRARY: &str = "libonnxruntime.1.23.2.dylib";

pub const SPEAKERS_FILE: &str = "speakers.json";
const VOICES_FILE: &str = "voices.json";

const SAMPLE_RATE: usize = 16_000;
const WINDOW: usize = SAMPLE_RATE * 3 / 2;
const HOP: usize = SAMPLE_RATE * 3 / 4;
/// Windows quieter than this are silence or background noise.
const SPEECH_RMS: f32 = 0.006;
/// Two windows at least this similar are taken to be the same voice.
const SAME_SPEAKER: f32 = 0.48;
/// Speakers this similar after grouping are merged.
const MERGE_SPEAKERS: f32 = 0.58;
/// A speaker matches a remembered voice at this similarity.
const KNOWN_VOICE: f32 = 0.55;
const MAX_SPEAKERS: usize = 8;

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct MeetingSpeaker {
    pub id: u32,
    #[serde(default)]
    pub name: Option<String>,
    /// The remembered voice this speaker was matched to or saved as.
    #[serde(default)]
    pub voice_id: Option<String>,
    /// True when the name came from a remembered voice, not the user.
    #[serde(default)]
    pub recognized: bool,
    pub talk_ms: u64,
    pub centroid: Vec<f32>,
}

impl MeetingSpeaker {
    pub fn label(&self) -> String {
        self.name
            .clone()
            .unwrap_or_else(|| format!("Speaker {}", self.id))
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct SpeakerTable {
    pub speakers: Vec<MeetingSpeaker>,
    #[serde(default)]
    pub error: Option<String>,
}

impl SpeakerTable {
    pub fn read(directory: &Path) -> Option<Self> {
        fs::read(directory.join(SPEAKERS_FILE))
            .ok()
            .and_then(|data| serde_json::from_slice(&data).ok())
    }

    pub fn write(&self, directory: &Path) -> Result<()> {
        let temporary = directory.join("speakers.json.tmp");
        fs::write(&temporary, serde_json::to_vec(self)?)?;
        fs::rename(temporary, directory.join(SPEAKERS_FILE))?;
        Ok(())
    }

    pub fn label(&self, id: u32) -> Option<String> {
        self.speakers
            .iter()
            .find(|speaker| speaker.id == id)
            .map(MeetingSpeaker::label)
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct Voice {
    pub id: String,
    pub name: String,
    pub embedding: Vec<f32>,
    pub samples: u32,
    pub updated_at_ms: u64,
}

pub fn model_path() -> Result<PathBuf> {
    Ok(crate::transcription_models::models_dir()?.join(MODEL_FILE))
}

pub fn is_installed() -> bool {
    model_path()
        .ok()
        .and_then(|path| fs::metadata(path).ok())
        .is_some_and(|metadata| metadata.len() == MODEL_BYTES)
}

/// Downloads and verifies the speaker model (about 30 MB).
pub fn download(progress: &AtomicU64) -> Result<()> {
    let destination = model_path()?;
    if is_installed() {
        progress.store(MODEL_BYTES, Ordering::Relaxed);
        return Ok(());
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    let partial = destination.with_extension("onnx.partial");
    let mut child = Command::new("curl")
        .args(["-fsSL", "--retry", "3", "-o"])
        .arg(&partial)
        .arg(MODEL_URL)
        .spawn()
        .wrap_err("could not start the speaker model download")?;
    loop {
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                let _ = fs::remove_file(&partial);
                bail!("the speaker model download failed");
            }
            break;
        }
        if let Ok(metadata) = fs::metadata(&partial) {
            progress.store(metadata.len(), Ordering::Relaxed);
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    let mut file = fs::File::open(&partial)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1 << 20];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let digest = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    if digest != MODEL_SHA256 {
        let _ = fs::remove_file(&partial);
        bail!("the speaker model download was corrupted");
    }
    fs::rename(&partial, &destination)?;
    progress.store(MODEL_BYTES, Ordering::Relaxed);
    Ok(())
}

fn runtime_library() -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    let mut candidates = vec![];
    if let Some(contents) = executable.parent().and_then(Path::parent) {
        candidates.push(contents.join("Frameworks").join(RUNTIME_LIBRARY));
    }
    candidates
        .push(PathBuf::from("/Applications/Hello.app/Contents/Frameworks").join(RUNTIME_LIBRARY));
    candidates.into_iter().find(|path| path.is_file())
}

fn init_runtime() -> Result<()> {
    static READY: OnceLock<std::result::Result<(), String>> = OnceLock::new();
    READY
        .get_or_init(|| {
            let library = runtime_library().ok_or("ONNX Runtime is missing from Hello")?;
            ort::init_from(library)
                .map_err(|error| error.to_string())?
                .commit();
            Ok(())
        })
        .clone()
        .map_err(|error| eyre!(error))
}

pub struct Embedder {
    session: ort::session::Session,
}

impl Embedder {
    pub fn load() -> Result<Self> {
        if !is_installed() {
            bail!("the speaker model is not downloaded");
        }
        init_runtime()?;
        let session = ort::session::Session::builder()
            .map_err(|error| eyre!("{error}"))?
            .with_intra_threads(2)
            .map_err(|error| eyre!("{error}"))?
            .commit_from_file(model_path()?)
            .map_err(|error| eyre!("could not load the speaker model: {error}"))?;
        Ok(Self { session })
    }

    /// A unit-length voice embedding for 16 kHz mono audio.
    pub fn embed(&mut self, samples: &[f32]) -> Result<Vec<f32>> {
        let (frames, features) = fbank(samples);
        if frames < 10 {
            bail!("the clip is too short");
        }
        let input = ort::value::Tensor::from_array(([1_usize, frames, MEL_BINS], features))
            .map_err(|error| eyre!("{error}"))?;
        let outputs = self
            .session
            .run(ort::inputs![input])
            .map_err(|error| eyre!("speaker model failed: {error}"))?;
        let (_, values) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|error| eyre!("{error}"))?;
        Ok(normalized(values.to_vec()))
    }
}

/// Splits the other side of a call into speakers, labels `entries` from that
/// track with a speaker id, and names speakers whose voices were saved before.
pub fn diarize(
    samples: &[f32],
    track_offset_ms: u64,
    entries: &mut [crate::meeting::TranscriptEntry],
) -> Result<SpeakerTable> {
    let mut embedder = Embedder::load()?;
    let mut windows: Vec<(u64, u64, Vec<f32>)> = Vec::new();
    let mut start = 0;
    while start + WINDOW / 2 <= samples.len() {
        let end = (start + WINDOW).min(samples.len());
        let window = &samples[start..end];
        if rms(window) >= SPEECH_RMS
            && let Ok(embedding) = embedder.embed(window)
        {
            windows.push((
                track_offset_ms + (start * 1_000 / SAMPLE_RATE) as u64,
                track_offset_ms + (end * 1_000 / SAMPLE_RATE) as u64,
                embedding,
            ));
        }
        start += HOP;
    }
    let labels = cluster(
        &windows
            .iter()
            .map(|(_, _, embedding)| embedding.clone())
            .collect::<Vec<_>>(),
    );
    let count = labels.iter().copied().max().map_or(0, |max| max + 1);
    let mut speakers: Vec<MeetingSpeaker> = (0..count)
        .map(|index| {
            let members: Vec<&Vec<f32>> = windows
                .iter()
                .zip(&labels)
                .filter(|(_, label)| **label == index)
                .map(|((_, _, embedding), _)| embedding)
                .collect();
            MeetingSpeaker {
                id: 0,
                centroid: mean(&members),
                ..MeetingSpeaker::default()
            }
        })
        .collect();

    // Label each transcript entry with the speaker heard most during it.
    for entry in entries
        .iter_mut()
        .filter(|entry| entry.source == crate::meeting::MeetingSource::System)
    {
        let mut weights = vec![0_u64; count];
        for ((window_start, window_end, _), label) in windows.iter().zip(&labels) {
            let overlap = entry
                .end_ms
                .min(*window_end)
                .saturating_sub(entry.start_ms.max(*window_start));
            weights[*label] += overlap;
        }
        let best = weights
            .iter()
            .enumerate()
            .max_by_key(|(_, weight)| **weight)
            .filter(|(_, weight)| **weight > 0)
            .map(|(index, _)| index)
            .or_else(|| {
                windows
                    .iter()
                    .zip(&labels)
                    .min_by_key(|((window_start, _, _), _)| window_start.abs_diff(entry.start_ms))
                    .map(|(_, label)| *label)
            });
        if let Some(best) = best {
            speakers[best].talk_ms += entry.end_ms.saturating_sub(entry.start_ms);
            entry.speaker = Some(best as u32);
        }
    }

    // Number speakers by who talked most; drop ones that never took a turn.
    let mut order: Vec<usize> = (0..count)
        .filter(|index| speakers[*index].talk_ms > 0)
        .collect();
    order.sort_by_key(|index| std::cmp::Reverse(speakers[*index].talk_ms));
    let mut renumber = vec![None; count];
    for (rank, index) in order.iter().enumerate() {
        renumber[*index] = Some(rank as u32 + 1);
    }
    for entry in entries.iter_mut() {
        if let Some(label) = entry.speaker {
            entry.speaker = renumber.get(label as usize).copied().flatten();
        }
    }
    let mut table = SpeakerTable::default();
    for index in order {
        let mut speaker = speakers[index].clone();
        speaker.id = renumber[index].unwrap_or(0);
        table.speakers.push(speaker);
    }
    if crate::app_settings::meeting_voice_recognition_enabled() {
        recognize(&mut table, &load_voices());
    }
    Ok(table)
}

/// Names speakers whose voice matches one the user labelled before.
pub fn recognize(table: &mut SpeakerTable, voices: &[Voice]) {
    let mut taken: Vec<String> = Vec::new();
    let mut matches: Vec<(f32, usize, usize)> = Vec::new();
    for (speaker_index, speaker) in table.speakers.iter().enumerate() {
        for (voice_index, voice) in voices.iter().enumerate() {
            let similarity = cosine(&speaker.centroid, &voice.embedding);
            if similarity >= KNOWN_VOICE {
                matches.push((similarity, speaker_index, voice_index));
            }
        }
    }
    matches.sort_by(|left, right| right.0.total_cmp(&left.0));
    let mut named = vec![false; table.speakers.len()];
    for (_, speaker_index, voice_index) in matches {
        let voice = &voices[voice_index];
        if named[speaker_index] || taken.contains(&voice.id) {
            continue;
        }
        let speaker = &mut table.speakers[speaker_index];
        if speaker.name.is_none() {
            speaker.name = Some(voice.name.clone());
            speaker.voice_id = Some(voice.id.clone());
            speaker.recognized = true;
        }
        named[speaker_index] = true;
        taken.push(voice.id.clone());
    }
}

/// Gives a meeting speaker a name and remembers the voice for next time.
pub fn name_speaker(directory: &Path, speaker_id: u32, name: &str) -> Result<()> {
    let mut table =
        SpeakerTable::read(directory).ok_or_else(|| eyre!("this meeting has no speakers"))?;
    let name = name.trim();
    let speaker = table
        .speakers
        .iter_mut()
        .find(|speaker| speaker.id == speaker_id)
        .ok_or_else(|| eyre!("that speaker is not in this meeting"))?;
    if name.is_empty() {
        speaker.name = None;
        speaker.recognized = false;
        speaker.voice_id = None;
        return table.write(directory);
    }
    speaker.name = Some(name.to_string());
    speaker.recognized = false;
    if crate::app_settings::meeting_voice_recognition_enabled() && !speaker.centroid.is_empty() {
        let mut voices = load_voices();
        let existing = speaker
            .voice_id
            .clone()
            .and_then(|id| voices.iter().position(|voice| voice.id == id))
            .or_else(|| {
                voices
                    .iter()
                    .position(|voice| voice.name.eq_ignore_ascii_case(name))
            });
        let now = now_ms();
        match existing {
            Some(index) => {
                let voice = &mut voices[index];
                let weight = voice.samples.min(9) as f32;
                voice.embedding = normalized(
                    voice
                        .embedding
                        .iter()
                        .zip(&speaker.centroid)
                        .map(|(old, new)| old * weight + new)
                        .collect(),
                );
                voice.samples += 1;
                voice.name = name.to_string();
                voice.updated_at_ms = now;
                speaker.voice_id = Some(voice.id.clone());
            }
            None => {
                let id = format!("voice-{now}-{speaker_id}");
                voices.push(Voice {
                    id: id.clone(),
                    name: name.to_string(),
                    embedding: speaker.centroid.clone(),
                    samples: 1,
                    updated_at_ms: now,
                });
                speaker.voice_id = Some(id);
            }
        }
        save_voices(&voices)?;
    }
    table.write(directory)
}

pub fn load_voices() -> Vec<Voice> {
    voices_path()
        .ok()
        .and_then(|path| fs::read(path).ok())
        .and_then(|data| serde_json::from_slice(&data).ok())
        .unwrap_or_default()
}

fn save_voices(voices: &[Voice]) -> Result<()> {
    let path = voices_path()?;
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec(voices)?)?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn voices_path() -> Result<PathBuf> {
    Ok(crate::meeting::root()?.join(VOICES_FILE))
}

/// Groups voice embeddings: greedy assignment to the closest group, a merge
/// pass, then two rounds of reassignment to the refined group centres.
fn cluster(embeddings: &[Vec<f32>]) -> Vec<usize> {
    if embeddings.is_empty() {
        return Vec::new();
    }
    let mut centroids: Vec<Vec<f32>> = Vec::new();
    let mut sums: Vec<Vec<f32>> = Vec::new();
    let mut labels = Vec::with_capacity(embeddings.len());
    for embedding in embeddings {
        let best = centroids
            .iter()
            .enumerate()
            .map(|(index, centroid)| (index, cosine(centroid, embedding)))
            .max_by(|left, right| left.1.total_cmp(&right.1));
        let index = match best {
            Some((index, similarity))
                if similarity >= SAME_SPEAKER || centroids.len() >= MAX_SPEAKERS * 2 =>
            {
                index
            }
            _ => {
                centroids.push(embedding.clone());
                sums.push(vec![0.0; embedding.len()]);
                centroids.len() - 1
            }
        };
        labels.push(index);
        for (total, value) in sums[index].iter_mut().zip(embedding) {
            *total += value;
        }
        centroids[index] = normalized(sums[index].clone());
    }
    // Merge groups that turned out to be one voice; fold tiny groups in.
    loop {
        let sizes: Vec<usize> = (0..centroids.len())
            .map(|index| labels.iter().filter(|label| **label == index).count())
            .collect();
        let mut best: Option<(f32, usize, usize)> = None;
        for left in 0..centroids.len() {
            for right in left + 1..centroids.len() {
                if sizes[left] == 0 || sizes[right] == 0 {
                    continue;
                }
                let similarity = cosine(&centroids[left], &centroids[right]);
                let tiny = sizes[left].min(sizes[right]) < 3;
                let needed = if tiny {
                    SAME_SPEAKER - 0.15
                } else {
                    MERGE_SPEAKERS
                };
                let active = sizes.iter().filter(|size| **size > 0).count();
                if (similarity >= needed || active > MAX_SPEAKERS)
                    && best.is_none_or(|(score, _, _)| similarity > score)
                {
                    best = Some((similarity, left, right));
                }
            }
        }
        let Some((_, keep, fold)) = best else {
            break;
        };
        for label in &mut labels {
            if *label == fold {
                *label = keep;
            }
        }
        centroids = recompute(embeddings, &labels, centroids.len());
    }
    for _ in 0..2 {
        let active: Vec<usize> = (0..centroids.len())
            .filter(|index| labels.contains(index))
            .collect();
        for (embedding, label) in embeddings.iter().zip(labels.iter_mut()) {
            if let Some(best) = active.iter().copied().max_by(|left, right| {
                cosine(&centroids[*left], embedding)
                    .total_cmp(&cosine(&centroids[*right], embedding))
            }) {
                *label = best;
            }
        }
        centroids = recompute(embeddings, &labels, centroids.len());
    }
    // Compact the labels to 0..n.
    let mut seen: Vec<usize> = Vec::new();
    labels
        .into_iter()
        .map(
            |label| match seen.iter().position(|known| *known == label) {
                Some(index) => index,
                None => {
                    seen.push(label);
                    seen.len() - 1
                }
            },
        )
        .collect()
}

fn recompute(embeddings: &[Vec<f32>], labels: &[usize], count: usize) -> Vec<Vec<f32>> {
    (0..count)
        .map(|index| {
            let members: Vec<&Vec<f32>> = embeddings
                .iter()
                .zip(labels)
                .filter(|(_, label)| **label == index)
                .map(|(embedding, _)| embedding)
                .collect();
            mean(&members)
        })
        .collect()
}

fn mean(members: &[&Vec<f32>]) -> Vec<f32> {
    let Some(first) = members.first() else {
        return Vec::new();
    };
    let mut sum = vec![0.0; first.len()];
    for member in members {
        for (total, value) in sum.iter_mut().zip(member.iter()) {
            *total += value;
        }
    }
    normalized(sum)
}

pub fn cosine(left: &[f32], right: &[f32]) -> f32 {
    if left.len() != right.len() || left.is_empty() {
        return -1.0;
    }
    left.iter().zip(right).map(|(a, b)| a * b).sum()
}

fn normalized(mut values: Vec<f32>) -> Vec<f32> {
    let norm = values.iter().map(|value| value * value).sum::<f32>().sqrt();
    if norm > 0.0 {
        for value in &mut values {
            *value /= norm;
        }
    }
    values
}

fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|sample| sample * sample).sum::<f32>() / samples.len() as f32).sqrt()
}

const MEL_BINS: usize = 80;
const FRAME_LENGTH: usize = 400;
const FRAME_SHIFT: usize = 160;
const FFT_SIZE: usize = 512;

/// Kaldi-style 80-bin log mel filterbank features with per-clip mean removed,
/// matching how 3D-Speaker models are trained.
fn fbank(samples: &[f32]) -> (usize, Vec<f32>) {
    static BANKS: OnceLock<Vec<Vec<(usize, f32)>>> = OnceLock::new();
    static WINDOW_SHAPE: OnceLock<Vec<f32>> = OnceLock::new();
    if samples.len() < FRAME_LENGTH {
        return (0, Vec::new());
    }
    let banks = BANKS.get_or_init(mel_banks);
    let window = WINDOW_SHAPE.get_or_init(|| {
        (0..FRAME_LENGTH)
            .map(|index| {
                let hann = 0.5
                    - 0.5
                        * (2.0 * std::f32::consts::PI * index as f32 / (FRAME_LENGTH - 1) as f32)
                            .cos();
                hann.powf(0.85)
            })
            .collect()
    });
    let frames = 1 + (samples.len() - FRAME_LENGTH) / FRAME_SHIFT;
    let mut planner = realfft::RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(FFT_SIZE);
    let mut input = fft.make_input_vec();
    let mut spectrum = fft.make_output_vec();
    let mut features = Vec::with_capacity(frames * MEL_BINS);
    let mut frame = vec![0.0_f32; FRAME_LENGTH];
    for index in 0..frames {
        let start = index * FRAME_SHIFT;
        // Kaldi works on 16-bit sample values.
        for (target, sample) in frame.iter_mut().zip(&samples[start..start + FRAME_LENGTH]) {
            *target = sample * 32_768.0;
        }
        let dc = frame.iter().sum::<f32>() / FRAME_LENGTH as f32;
        for value in &mut frame {
            *value -= dc;
        }
        for position in (1..FRAME_LENGTH).rev() {
            frame[position] -= 0.97 * frame[position - 1];
        }
        frame[0] -= 0.97 * frame[0];
        input.iter_mut().for_each(|value| *value = 0.0);
        for (position, value) in frame.iter().enumerate() {
            input[position] = value * window[position];
        }
        if fft.process(&mut input, &mut spectrum).is_err() {
            return (0, Vec::new());
        }
        for bank in banks {
            let energy: f32 = bank
                .iter()
                .map(|(bin, weight)| spectrum[*bin].norm_sqr() * weight)
                .sum();
            features.push(energy.max(f32::EPSILON).ln());
        }
    }
    for bin in 0..MEL_BINS {
        let average = (0..frames)
            .map(|frame| features[frame * MEL_BINS + bin])
            .sum::<f32>()
            / frames as f32;
        for frame in 0..frames {
            features[frame * MEL_BINS + bin] -= average;
        }
    }
    (frames, features)
}

fn mel_banks() -> Vec<Vec<(usize, f32)>> {
    let mel = |frequency: f32| 1127.0 * (1.0 + frequency / 700.0).ln();
    let low = mel(20.0);
    let high = mel(SAMPLE_RATE as f32 / 2.0);
    let delta = (high - low) / (MEL_BINS + 1) as f32;
    let bin_width = SAMPLE_RATE as f32 / FFT_SIZE as f32;
    (0..MEL_BINS)
        .map(|bank| {
            let left = low + bank as f32 * delta;
            let center = left + delta;
            let right = center + delta;
            (0..FFT_SIZE / 2)
                .filter_map(|bin| {
                    let value = mel(bin as f32 * bin_width);
                    if value <= left || value >= right {
                        return None;
                    }
                    let weight = if value <= center {
                        (value - left) / (center - left)
                    } else {
                        (right - value) / (right - center)
                    };
                    Some((bin, weight))
                })
                .collect()
        })
        .collect()
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(values: &[f32]) -> Vec<f32> {
        normalized(values.to_vec())
    }

    #[test]
    fn clusters_two_distinct_voices() {
        let a = unit(&[1.0, 0.1, 0.0]);
        let a2 = unit(&[0.95, 0.15, 0.05]);
        let b = unit(&[0.0, 1.0, 0.1]);
        let b2 = unit(&[0.1, 0.9, 0.0]);
        let labels = cluster(&[a.clone(), b.clone(), a2, b2, a, b]);
        assert_eq!(labels, vec![0, 1, 0, 1, 0, 1]);
    }

    #[test]
    fn recognizes_a_saved_voice_once() {
        let mut table = SpeakerTable {
            speakers: vec![
                MeetingSpeaker {
                    id: 1,
                    centroid: unit(&[1.0, 0.0]),
                    ..MeetingSpeaker::default()
                },
                MeetingSpeaker {
                    id: 2,
                    centroid: unit(&[0.98, 0.2]),
                    ..MeetingSpeaker::default()
                },
            ],
            error: None,
        };
        let voices = vec![Voice {
            id: "v1".into(),
            name: "Rahim".into(),
            embedding: unit(&[1.0, 0.0]),
            samples: 1,
            updated_at_ms: 0,
        }];
        recognize(&mut table, &voices);
        assert_eq!(table.speakers[0].name.as_deref(), Some("Rahim"));
        assert!(table.speakers[0].recognized);
        assert_eq!(table.speakers[1].name, None);
    }

    #[test]
    fn fbank_has_eighty_bins_per_frame() {
        let samples: Vec<f32> = (0..16_000)
            .map(|index| (index as f32 * 0.05).sin() * 0.1)
            .collect();
        let (frames, features) = fbank(&samples);
        assert_eq!(frames, 98);
        assert_eq!(features.len(), frames * MEL_BINS);
        assert!(features.iter().all(|value| value.is_finite()));
    }

    #[test]
    #[ignore = "needs the speaker model and HELLO_SPEAKER_TURNS=<dir of turnN.wav>"]
    fn separates_speakers_in_a_recorded_conversation() {
        let directory = std::env::var("HELLO_SPEAKER_TURNS").unwrap();
        let mut samples = Vec::new();
        let mut entries = Vec::new();
        for index in 0.. {
            let path = Path::new(&directory).join(format!("turn{index}.wav"));
            let Ok(mut reader) = hound::WavReader::open(&path) else {
                break;
            };
            let turn: Vec<f32> = reader.samples::<f32>().map(Result::unwrap).collect();
            let start_ms = (samples.len() * 1_000 / SAMPLE_RATE) as u64;
            samples.extend_from_slice(&turn);
            let end_ms = (samples.len() * 1_000 / SAMPLE_RATE) as u64;
            samples.extend(std::iter::repeat_n(0.0, SAMPLE_RATE / 2));
            entries.push(crate::meeting::TranscriptEntry {
                source: crate::meeting::MeetingSource::System,
                start_ms,
                end_ms,
                text: format!("turn {index}"),
                speaker: None,
            });
        }
        let started = std::time::Instant::now();
        let table = diarize(&samples, 0, &mut entries).unwrap();
        eprintln!(
            "diarized {} s of audio in {:?}",
            samples.len() / SAMPLE_RATE,
            started.elapsed()
        );
        let labels: Vec<Option<u32>> = entries.iter().map(|entry| entry.speaker).collect();
        eprintln!("labels {labels:?}");
        for left in &table.speakers {
            for right in &table.speakers {
                eprintln!(
                    "{} vs {}: {:.2}",
                    left.id,
                    right.id,
                    cosine(&left.centroid, &right.centroid)
                );
            }
        }
        assert_eq!(table.speakers.len(), 3);
        assert_eq!(labels[0], labels[3]);
        assert_eq!(labels[1], labels[4]);
        assert_eq!(labels[2], labels[5]);
        assert_ne!(labels[0], labels[1]);
        assert_ne!(labels[1], labels[2]);
        assert_ne!(labels[0], labels[2]);
    }
}
