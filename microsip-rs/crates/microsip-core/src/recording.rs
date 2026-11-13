//! Recording a call to a WAV file.
//!
//! The file is 8 kHz, 16-bit stereo: the left channel is this phone's
//! microphone as it was sent, the right channel is the other side. Any
//! player opens it, and the two speakers can be told apart.
//!
//! The two directions arrive on different threads and not evenly, so a small
//! thread of its own writes the file by the clock, a moment behind real
//! time; a side that has nothing to give is written as silence. The header
//! is brought up to date every second, so a recording survives even if the
//! program is closed abruptly.

use crate::resample::Resampler;
use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::{self, BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Sample rate of the recording.
pub const RECORDING_RATE: u32 = 8_000;
const CHANNELS: u16 = 2;
const HEADER_LENGTH: u64 = 44;
/// The writer stays this far behind real time, so audio that arrives in
/// bursts is already there when its turn comes.
const WRITE_LAG: Duration = Duration::from_millis(300);
/// More than this much waiting audio means the writer fell behind; the
/// oldest part is dropped.
const QUEUE_LIMIT: usize = RECORDING_RATE as usize * 2;

/// Writes PCM samples as a WAV file.
pub struct WavWriter<W: Write + Seek> {
    out: W,
    channels: u16,
    /// Samples written, counting every channel.
    samples: u64,
}

impl<W: Write + Seek> WavWriter<W> {
    pub fn new(mut out: W, sample_rate: u32, channels: u16) -> io::Result<Self> {
        let block = channels * 2;
        out.write_all(b"RIFF")?;
        out.write_all(&0_u32.to_le_bytes())?;
        out.write_all(b"WAVEfmt ")?;
        out.write_all(&16_u32.to_le_bytes())?;
        out.write_all(&1_u16.to_le_bytes())?; // PCM
        out.write_all(&channels.to_le_bytes())?;
        out.write_all(&sample_rate.to_le_bytes())?;
        out.write_all(&(sample_rate * u32::from(block)).to_le_bytes())?;
        out.write_all(&block.to_le_bytes())?;
        out.write_all(&16_u16.to_le_bytes())?;
        out.write_all(b"data")?;
        out.write_all(&0_u32.to_le_bytes())?;
        Ok(Self {
            out,
            channels,
            samples: 0,
        })
    }

    /// Appends samples in `-1.0..=1.0`, interleaved by channel.
    pub fn write_samples(&mut self, interleaved: &[f32]) -> io::Result<()> {
        for sample in interleaved {
            let value = (sample.clamp(-1.0, 1.0) * 32_767.0).round() as i16;
            self.out.write_all(&value.to_le_bytes())?;
        }
        self.samples += interleaved.len() as u64;
        Ok(())
    }

    /// Length of the recording so far.
    pub fn duration(&self, sample_rate: u32) -> Duration {
        let frames = self.samples / u64::from(self.channels.max(1));
        Duration::from_millis(frames * 1_000 / u64::from(sample_rate.max(1)))
    }

    /// Writes the current sizes into the header, so the file is valid as it
    /// stands, and returns to the end.
    pub fn update_header(&mut self) -> io::Result<()> {
        let data = u32::try_from(self.samples * 2).unwrap_or(u32::MAX);
        self.out.seek(SeekFrom::Start(4))?;
        self.out
            .write_all(&(data.saturating_add(36)).to_le_bytes())?;
        self.out.seek(SeekFrom::Start(40))?;
        self.out.write_all(&data.to_le_bytes())?;
        self.out
            .seek(SeekFrom::Start(HEADER_LENGTH + u64::from(data)))?;
        self.out.flush()
    }

    pub fn finish(mut self) -> io::Result<W> {
        self.update_header()?;
        Ok(self.out)
    }
}

#[derive(Default)]
struct Shared {
    local: Mutex<VecDeque<f32>>,
    remote: Mutex<VecDeque<f32>>,
    /// While set, the microphone is recorded as silence: it is not sent.
    local_muted: AtomicBool,
    stop: AtomicBool,
}

#[derive(Clone, Copy)]
enum Side {
    Local,
    Remote,
}

/// One direction's way into a recording. It can be moved to the thread
/// that has the audio.
pub struct RecorderInput {
    shared: Arc<Shared>,
    side: Side,
    resampler: Option<(u32, Resampler)>,
}

impl RecorderInput {
    /// Adds mono samples recorded at `sample_rate`.
    pub fn push(&mut self, samples: &[f32], sample_rate: u32) {
        if self
            .resampler
            .as_ref()
            .is_none_or(|(rate, _)| *rate != sample_rate)
        {
            let Some(resampler) = Resampler::new(sample_rate, RECORDING_RATE) else {
                return;
            };
            self.resampler = Some((sample_rate, resampler));
        }
        let Some((_, resampler)) = self.resampler.as_mut() else {
            return;
        };
        let converted = resampler.push(samples);
        let queue = match self.side {
            Side::Local => &self.shared.local,
            Side::Remote => &self.shared.remote,
        };
        if let Ok(mut queue) = queue.lock() {
            queue.extend(converted);
            if queue.len() > QUEUE_LIMIT {
                let excess = queue.len() - QUEUE_LIMIT;
                queue.drain(..excess);
            }
        }
    }
}

/// Takes `frames` stereo frames from the two queues; a side that has run
/// out, or a muted microphone, gives silence.
fn take_frames(shared: &Shared, frames: usize) -> Vec<f32> {
    let muted = shared.local_muted.load(Ordering::Relaxed);
    let mut interleaved = vec![0.0; frames * usize::from(CHANNELS)];
    if let Ok(mut local) = shared.local.lock() {
        for frame in interleaved.as_chunks_mut::<2>().0 {
            let sample = local.pop_front().unwrap_or(0.0);
            frame[0] = if muted { 0.0 } else { sample };
        }
    }
    if let Ok(mut remote) = shared.remote.lock() {
        for frame in interleaved.as_chunks_mut::<2>().0 {
            frame[1] = remote.pop_front().unwrap_or(0.0);
        }
    }
    interleaved
}

/// A recording in progress. Dropping it finishes the file.
pub struct CallRecorder {
    path: PathBuf,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<io::Result<()>>>,
}

impl CallRecorder {
    /// Creates the file (and its folder) and starts writing.
    pub fn start(path: &Path) -> io::Result<Self> {
        if let Some(directory) = path.parent() {
            fs::create_dir_all(directory)?;
        }
        let file = BufWriter::new(File::create(path)?);
        let mut writer = WavWriter::new(file, RECORDING_RATE, CHANNELS)?;
        let shared = Arc::new(Shared::default());
        let for_thread = Arc::clone(&shared);
        let thread = thread::spawn(move || {
            let started = Instant::now();
            let mut written: u64 = 0;
            let mut last_header = Instant::now();
            loop {
                let stopping = for_thread.stop.load(Ordering::Acquire);
                // When stopping, what is still waiting is written out too.
                let lag = if stopping { Duration::ZERO } else { WRITE_LAG };
                let due = started.elapsed().saturating_sub(lag).as_millis() as u64
                    * u64::from(RECORDING_RATE)
                    / 1_000;
                if due > written {
                    let frames = (due - written) as usize;
                    writer.write_samples(&take_frames(&for_thread, frames))?;
                    written = due;
                }
                if stopping {
                    // Whatever is still waiting belongs to the call as well.
                    let waiting = |queue: &Mutex<VecDeque<f32>>| {
                        queue.lock().map(|queue| queue.len()).unwrap_or(0)
                    };
                    let rest = waiting(&for_thread.local).max(waiting(&for_thread.remote));
                    writer.write_samples(&take_frames(&for_thread, rest))?;
                    return writer.finish().map(|_| ());
                }
                if last_header.elapsed() >= Duration::from_secs(1) {
                    writer.update_header()?;
                    last_header = Instant::now();
                }
                thread::sleep(Duration::from_millis(20));
            }
        });
        Ok(Self {
            path: path.to_owned(),
            shared,
            thread: Some(thread),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Input for this phone's microphone.
    pub fn local_input(&self) -> RecorderInput {
        self.input(Side::Local)
    }

    /// Input for the audio received from the other side.
    pub fn remote_input(&self) -> RecorderInput {
        self.input(Side::Remote)
    }

    fn input(&self, side: Side) -> RecorderInput {
        RecorderInput {
            shared: Arc::clone(&self.shared),
            side,
            resampler: None,
        }
    }

    /// While muted (or on hold) the microphone is not sent, so it is not
    /// recorded either.
    pub fn set_local_muted(&self, muted: bool) {
        self.shared.local_muted.store(muted, Ordering::Relaxed);
    }

    /// Finishes the file and reports whether writing it worked.
    pub fn stop(mut self) -> io::Result<()> {
        self.finish()
    }

    fn finish(&mut self) -> io::Result<()> {
        self.shared.stop.store(true, Ordering::Release);
        match self.thread.take() {
            Some(thread) => thread
                .join()
                .unwrap_or_else(|_| Err(io::Error::other("the recording thread failed"))),
            None => Ok(()),
        }
    }
}

impl Drop for CallRecorder {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

/// A file name for a recording: `2026-11-13_14-05-09_out_74959999999.wav`.
/// Characters that cannot be in a Windows file name are left out.
pub fn recording_file_name(
    time: crate::localtime::DateTime,
    incoming: bool,
    number: &str,
) -> String {
    let number: String = number
        .chars()
        .filter(|character| character.is_alphanumeric() || matches!(character, '+' | '-' | '_'))
        .take(40)
        .collect();
    let number = if number.is_empty() {
        "unknown".to_owned()
    } else {
        number
    };
    format!(
        "{:04}-{:02}-{:02}_{:02}-{:02}-{:02}_{}_{number}.wav",
        time.year,
        time.month,
        time.day,
        time.hour,
        time.minute,
        time.second,
        if incoming { "in" } else { "out" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::localtime::DateTime;
    use std::io::Cursor;

    fn u16_at(bytes: &[u8], offset: usize) -> u16 {
        u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
    }

    fn u32_at(bytes: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
    }

    fn i16_at(bytes: &[u8], offset: usize) -> i16 {
        i16::from_le_bytes([bytes[offset], bytes[offset + 1]])
    }

    #[test]
    fn wav_header_describes_the_audio_exactly() {
        let mut writer = WavWriter::new(Cursor::new(Vec::new()), 8_000, 2).unwrap();
        writer.write_samples(&[0.0, 1.0, -1.0, 0.5]).unwrap();
        assert_eq!(writer.duration(8_000), Duration::ZERO);
        let bytes = writer.finish().unwrap().into_inner();

        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(u32_at(&bytes, 4) as usize, bytes.len() - 8);
        assert_eq!(&bytes[8..16], b"WAVEfmt ");
        assert_eq!(u32_at(&bytes, 16), 16);
        assert_eq!(u16_at(&bytes, 20), 1, "PCM");
        assert_eq!(u16_at(&bytes, 22), 2, "channels");
        assert_eq!(u32_at(&bytes, 24), 8_000, "sample rate");
        assert_eq!(u32_at(&bytes, 28), 32_000, "bytes per second");
        assert_eq!(u16_at(&bytes, 32), 4, "bytes per frame");
        assert_eq!(u16_at(&bytes, 34), 16, "bits per sample");
        assert_eq!(&bytes[36..40], b"data");
        assert_eq!(u32_at(&bytes, 40), 8);
        assert_eq!(bytes.len(), 44 + 8);
        let samples: Vec<i16> = (0..4).map(|index| i16_at(&bytes, 44 + index * 2)).collect();
        assert_eq!(samples, [0, 32_767, -32_767, 16_384]);
    }

    #[test]
    fn header_can_be_brought_up_to_date_while_writing_goes_on() {
        let mut writer = WavWriter::new(Cursor::new(Vec::new()), 8_000, 2).unwrap();
        writer.write_samples(&[0.25; 16_000]).unwrap();
        writer.update_header().unwrap();
        // The file is valid now ...
        assert_eq!(writer.duration(8_000), Duration::from_secs(1));
        // ... and writing continues at the end, not over the header.
        writer.write_samples(&[0.5; 2]).unwrap();
        let bytes = writer.finish().unwrap().into_inner();
        assert_eq!(u32_at(&bytes, 40), 32_004);
        assert_eq!(bytes.len(), 44 + 32_004);
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(i16_at(&bytes, 44 + 32_000), 16_384);
    }

    #[test]
    fn frames_carry_microphone_left_and_other_side_right() {
        let shared = Shared::default();
        shared.local.lock().unwrap().extend([0.1, 0.2, 0.3]);
        shared.remote.lock().unwrap().extend([0.7]);
        // Three frames: the right side runs out and becomes silence.
        assert_eq!(take_frames(&shared, 3), [0.1, 0.7, 0.2, 0.0, 0.3, 0.0]);
        assert_eq!(take_frames(&shared, 1), [0.0, 0.0]);

        // A muted microphone is consumed but recorded as silence.
        shared.local_muted.store(true, Ordering::Relaxed);
        shared.local.lock().unwrap().extend([0.9, 0.9]);
        shared.remote.lock().unwrap().extend([0.4, 0.4]);
        assert_eq!(take_frames(&shared, 2), [0.0, 0.4, 0.0, 0.4]);
        assert!(shared.local.lock().unwrap().is_empty());
    }

    #[test]
    fn input_converts_to_the_recording_rate_and_limits_its_queue() {
        let shared = Arc::new(Shared::default());
        let mut input = RecorderInput {
            shared: Arc::clone(&shared),
            side: Side::Local,
            resampler: None,
        };
        // 20 ms at 48 kHz is 160 samples at 8 kHz.
        input.push(&[0.5; 960], 48_000);
        assert_eq!(shared.local.lock().unwrap().len(), 160);
        assert!(shared.remote.lock().unwrap().is_empty());

        // Far more than the writer could be behind: only the newest is kept.
        input.push(&vec![0.5; 48_000 * 5], 48_000);
        assert_eq!(shared.local.lock().unwrap().len(), QUEUE_LIMIT);
    }

    #[test]
    fn a_call_is_recorded_to_a_playable_file() {
        let directory =
            std::env::temp_dir().join(format!("microsip-rs-recording-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        let path = directory.join("nested").join("call.wav");

        let recorder = CallRecorder::start(&path).unwrap();
        assert_eq!(recorder.path(), path);
        let mut local = recorder.local_input();
        let mut remote = recorder.remote_input();
        // Half a second of talk in real time: you at 48 kHz, them at 8 kHz.
        for _ in 0..25 {
            local.push(&[0.5; 960], 48_000);
            remote.push(&[-0.25; 160], RECORDING_RATE);
            thread::sleep(Duration::from_millis(20));
        }
        recorder.stop().unwrap();

        let bytes = fs::read(&path).unwrap();
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(u16_at(&bytes, 22), 2);
        assert_eq!(u32_at(&bytes, 24), RECORDING_RATE);
        let data = u32_at(&bytes, 40) as usize;
        assert_eq!(bytes.len(), 44 + data);
        // About half a second of stereo frames (4 bytes each).
        let frames = data / 4;
        assert!((3_500..=6_000).contains(&frames), "{frames} frames");

        // Both voices are in the file, each on its own channel.
        let left: Vec<i16> = (0..frames)
            .map(|frame| i16_at(&bytes, 44 + frame * 4))
            .collect();
        let right: Vec<i16> = (0..frames)
            .map(|frame| i16_at(&bytes, 44 + frame * 4 + 2))
            .collect();
        let near =
            |value: i16, expected: f32| (f32::from(value) / 32_767.0 - expected).abs() < 0.02;
        let voiced_left = left.iter().filter(|sample| near(**sample, 0.5)).count();
        let voiced_right = right.iter().filter(|sample| near(**sample, -0.25)).count();
        assert!(voiced_left > 3_000, "left {voiced_left}");
        assert!(voiced_right > 3_000, "right {voiced_right}");
        // Nothing of one side leaked into the other channel: the level of
        // the other voice does not appear there. (Small swings around the
        // start are the rate converter settling, not a leak.)
        let leaked_left = left.iter().filter(|sample| near(**sample, -0.25)).count();
        let leaked_right = right.iter().filter(|sample| near(**sample, 0.5)).count();
        assert!(
            leaked_left < 20,
            "left carries the other side: {leaked_left}"
        );
        assert_eq!(leaked_right, 0, "right carries the microphone");
        fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn dropping_the_recorder_still_leaves_a_valid_file() {
        let directory =
            std::env::temp_dir().join(format!("microsip-rs-recording-drop-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        let path = directory.join("call.wav");
        {
            let recorder = CallRecorder::start(&path).unwrap();
            recorder.remote_input().push(&[0.3; 800], RECORDING_RATE);
        }
        let bytes = fs::read(&path).unwrap();
        assert_eq!(bytes.len(), 44 + u32_at(&bytes, 40) as usize);
        assert!(u32_at(&bytes, 40) >= 3_200, "the waiting audio was written");
        fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn file_names_sort_by_time_and_are_safe_for_windows() {
        let time = DateTime {
            year: 2026,
            month: 11,
            day: 13,
            hour: 9,
            minute: 5,
            second: 7,
        };
        assert_eq!(
            recording_file_name(time, false, "+7 (495) 999-99-99"),
            "2026-11-13_09-05-07_out_+7495999-99-99.wav"
        );
        assert_eq!(
            recording_file_name(time, true, "4010"),
            "2026-11-13_09-05-07_in_4010.wav"
        );
        // Nothing a file system objects to survives.
        let name = recording_file_name(time, true, "a/b\\c:d*e?f\"g<h>i|j");
        assert_eq!(name, "2026-11-13_09-05-07_in_abcdefghij.wav");
        assert!(recording_file_name(time, true, "///").ends_with("_in_unknown.wav"));
    }
}
