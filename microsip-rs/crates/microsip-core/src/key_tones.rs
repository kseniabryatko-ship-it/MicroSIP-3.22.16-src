//! The sound of the keypad: each pressed digit plays its two tones on the
//! speaker for the user, as MicroSIP does. This is separate from what is
//! sent to the other side of a call.
//!
//! Windows needs a moment to wake a sound device, headsets most of all, and
//! eats or garbles what is played in that moment. So the speaker is opened
//! ahead of time, while the keypad is on the screen ([`KeyTones::warm`]),
//! and kept open for a while after the last key; a freshly opened one gets
//! a little silence before its first tone.

use crate::audio::Speaker;
use crate::call_media::AudioControls;
use crate::rtp::dtmf::key_sound;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

/// How long one key sounds.
const KEY_MS: u32 = 120;
/// The speaker is released after this long without a key or a warm-up.
const IDLE: Duration = Duration::from_secs(60);
/// Silence before the first tone of a speaker just opened.
const WAKE_MS: u32 = 30;
/// A key waits behind at most this much of the one before.
const MAX_LAG_MS: u32 = 40;

enum Request {
    Key(char),
    Warm,
}

/// Plays key sounds; cheap to keep, the thread waits for keys.
pub struct KeyTones {
    requests: Sender<Request>,
}

/// The open speaker and the device it was opened for.
struct Open {
    speaker: Speaker,
    device: Option<String>,
    /// Nothing has been played on it yet.
    fresh: bool,
}

impl KeyTones {
    /// Plays on the speaker the call audio uses, at its volume.
    pub fn new(controls: AudioControls) -> Self {
        let (requests, received) = mpsc::channel::<Request>();
        thread::spawn(move || {
            let mut open: Option<Open> = None;
            let mut last_use = Instant::now();
            loop {
                let request = match received.recv_timeout(Duration::from_secs(5)) {
                    Ok(request) => request,
                    Err(RecvTimeoutError::Timeout) => {
                        if last_use.elapsed() >= IDLE {
                            // Quiet for a while: the sound card is let go.
                            open = None;
                        }
                        continue;
                    }
                    Err(RecvTimeoutError::Disconnected) => return,
                };
                last_use = Instant::now();
                let device = controls
                    .output_device()
                    .filter(|name| !name.trim().is_empty());
                // Another speaker was chosen in the settings: that one now.
                if open.as_ref().is_some_and(|open| open.device != device) {
                    open = None;
                }
                if open.is_none() {
                    open = Speaker::start_immediate(
                        device.as_deref(),
                        controls.speaker_volume_handle(),
                    )
                    .ok()
                    .map(|speaker| Open {
                        speaker,
                        device: device.clone(),
                        fresh: true,
                    });
                }
                let (Request::Key(digit), Some(open)) = (request, open.as_mut()) else {
                    continue;
                };
                let speaker = &open.speaker;
                // Quick typing: a key fades out the rest of the one before,
                // so the sound keeps up with the fingers.
                if speaker.queued_ms() > MAX_LAG_MS {
                    speaker.cut();
                }
                // Made at the speaker's own rate: nothing to convert.
                let rate = speaker.format().sample_rate;
                let mut sound = Vec::new();
                if open.fresh {
                    sound.resize((rate * WAKE_MS / 1_000) as usize, 0.0);
                    open.fresh = false;
                }
                sound.extend(key_sound(digit, rate, KEY_MS));
                let _ = speaker.play_mono(&sound, rate);
            }
        });
        Self { requests }
    }

    /// Sounds a key; characters that are no key are ignored.
    pub fn play(&self, digit: char) {
        let _ = self.requests.send(Request::Key(digit));
    }

    /// Opens the speaker now, so that the next key sounds at once and
    /// whole; call it while the keypad is shown.
    pub fn warm(&self) {
        let _ = self.requests.send(Request::Warm);
    }
}
