//! The sound of the keypad: each pressed digit plays its two tones on the
//! speaker for the user, as MicroSIP does. This is separate from what is
//! sent to the other side of a call.
//!
//! The speaker is opened on a thread of its own at the first key and closed
//! again after a few quiet seconds, so the program does not hold the sound
//! card while nobody dials.

use crate::audio::Speaker;
use crate::call_media::AudioControls;
use crate::rtp::dtmf::key_sound;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread;
use std::time::Duration;

/// How long one key sounds.
const KEY_MS: u32 = 120;
/// The speaker is released after this long without a key.
const IDLE: Duration = Duration::from_secs(5);
/// A key waits behind at most this much of the one before.
const MAX_LAG_MS: u32 = 40;

/// Plays key sounds; cheap to keep, the thread waits for keys.
pub struct KeyTones {
    keys: Sender<char>,
}

impl KeyTones {
    /// Plays on the speaker the call audio uses, at its volume.
    pub fn new(controls: AudioControls) -> Self {
        let (keys, presses) = mpsc::channel::<char>();
        thread::spawn(move || {
            let mut speaker: Option<Speaker> = None;
            loop {
                match presses.recv_timeout(IDLE) {
                    Ok(digit) => {
                        if speaker.is_none() {
                            let device = controls.output_device();
                            // Played at once: the buffer for network audio
                            // would wait for a reserve longer than a key.
                            speaker = Speaker::start_immediate(
                                device.as_deref().filter(|name| !name.is_empty()),
                                controls.speaker_volume_handle(),
                            )
                            .ok();
                        }
                        if let Some(speaker) = speaker.as_ref() {
                            // Quick typing: a key cuts the rest of the one
                            // before, so the sound keeps up with the fingers.
                            if speaker.queued_ms() > MAX_LAG_MS {
                                speaker.cut();
                            }
                            // Made at the speaker's own rate: nothing to
                            // convert, nothing to smear.
                            let rate = speaker.format().sample_rate;
                            let _ = speaker.play_mono(&key_sound(digit, rate, KEY_MS), rate);
                        }
                    }
                    // Quiet for a while: the sound card is let go.
                    Err(RecvTimeoutError::Timeout) => speaker = None,
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
        });
        Self { keys }
    }

    /// Sounds a key; characters that are no key are ignored.
    pub fn play(&self, digit: char) {
        let _ = self.keys.send(digit);
    }
}
