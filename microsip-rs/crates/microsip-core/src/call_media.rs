//! Media side of a call, kept behind a trait so the SIP executor can be tested
//! without sound devices.

use crate::audio::{Microphone, Speaker};
use crate::gain::AutoGain;
use crate::rtp::PCMU_CLOCK_RATE;
use crate::rtp::session::{RtpSession, RtpSessionConfig};
use crate::tones::{TONE_RATE, Tone, ToneGenerator};
use std::error::Error;
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub type MediaError = Box<dyn Error + Send + Sync>;

/// RTP packet counters of the running media session.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MediaStats {
    pub sent_packets: u64,
    pub received_packets: u64,
}

/// How well the received audio could be played.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MediaQuality {
    /// Times playback ran out of audio and paused.
    pub gaps: u64,
    /// Packets that never arrived in time.
    pub lost: u64,
    /// Packets that arrived out of order and were put back in place.
    pub reordered: u64,
    /// Duplicates and packets that arrived too late to play.
    pub discarded: u64,
    /// The largest playback reserve the connection needed, in milliseconds.
    pub reserve_ms: u32,
    /// Loudest microphone sample of the call, in percent of full scale,
    /// before any gain. A few percent means the microphone is very quiet.
    pub mic_peak_percent: u32,
    /// Microphone gain at the end of the call, in percent (100 is unchanged).
    pub mic_gain_percent: u32,
}

impl std::fmt::Display for MediaQuality {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "gaps {}, lost packets {}, reordered {}, discarded {}, reserve {} ms, \
             microphone peak {}%, gain x{}.{}",
            self.gaps,
            self.lost,
            self.reordered,
            self.discarded,
            self.reserve_ms,
            self.mic_peak_percent,
            self.mic_gain_percent / 100,
            self.mic_gain_percent % 100 / 10
        )
    }
}

/// Media for one call at a time.
///
/// Implementations are created on the executor thread and are never moved
/// between threads, so they may hold `!Send` audio streams.
pub trait Media {
    /// Reserves a local RTP port and returns the address to advertise in SDP.
    fn prepare(&mut self, ip: IpAddr) -> Result<SocketAddr, MediaError>;

    /// Starts sending and receiving audio. Called after `prepare`.
    fn start(&mut self, remote: SocketAddr) -> Result<(), MediaError>;

    /// Called every executor tick while a call exists. Moves audio between
    /// devices and the RTP session.
    fn pump(&mut self) -> Result<(), MediaError> {
        Ok(())
    }

    /// Packet counters of the running session; zeros when none runs.
    fn stats(&self) -> MediaStats {
        MediaStats::default()
    }

    /// Starts the ring of an incoming call or the ringback of an outgoing
    /// one on the speaker; `None` stops it. Starting the call's own audio
    /// stops it too.
    fn ring(&mut self, _tone: Option<Tone>) -> Result<(), MediaError> {
        Ok(())
    }

    /// Tells the media which telephone-event payload type the other side
    /// agreed to (`None`: digits go as tones in the voice). Called before
    /// `start`.
    fn set_dtmf_payload_type(&mut self, _payload_type: Option<u8>) {}

    /// Sends a keypad digit to the other side.
    fn send_dtmf(&mut self, _digit: char) -> Result<(), MediaError> {
        Err("this phone has no audio to send digits with".into())
    }

    /// Stops or resumes sending the microphone. A new call starts unmuted.
    fn set_muted(&mut self, _muted: bool) {}

    /// While the call is on hold nothing from the microphone is sent,
    /// whatever the mute setting is.
    fn set_hold(&mut self, _hold: bool) {}

    /// Quality of the received audio; `None` when this media plays nothing.
    fn quality(&self) -> Option<MediaQuality> {
        None
    }

    /// Stops audio and releases devices. Safe to call when not started.
    fn stop(&mut self);
}

/// Media that only reserves a port. Used by tests and headless runs.
#[derive(Debug, Default)]
pub struct NullMedia {
    socket: Option<UdpSocket>,
}

impl Media for NullMedia {
    fn prepare(&mut self, ip: IpAddr) -> Result<SocketAddr, MediaError> {
        let socket = UdpSocket::bind(SocketAddr::new(ip, 0))?;
        let address = socket.local_addr()?;
        self.socket = Some(socket);
        Ok(address)
    }

    fn start(&mut self, _remote: SocketAddr) -> Result<(), MediaError> {
        Ok(())
    }

    fn stop(&mut self) {
        self.socket = None;
    }
}

/// Microphone and speaker over a PCMU RTP session.
#[derive(Default)]
pub struct AudioMedia {
    socket: Option<UdpSocket>,
    active: Option<ActiveAudio>,
    next_ssrc: u32,
    dtmf_payload_type: Option<u8>,
    ringer: Option<Ringer>,
    muted: bool,
    held: bool,
}

/// A speaker playing a call-progress tone until it is dropped.
struct Ringer {
    tone: Tone,
    // Dropped after the thread has stopped feeding it.
    _speaker: Speaker,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Ringer {
    fn start(tone: Tone) -> Result<Self, MediaError> {
        let speaker = Speaker::start()?;
        let mut sink = speaker.sink();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            let mut generator = ToneGenerator::new(tone);
            let started = Instant::now();
            while !stopped.load(Ordering::Acquire) {
                // Stay a little ahead of the clock, as a network stream would.
                let wanted = started.elapsed().as_millis() as u64 + 120;
                while generator.produced_ms() < wanted {
                    let block = generator.next_block(TONE_RATE as usize / 50);
                    if sink.push(&block, TONE_RATE).is_err() {
                        return;
                    }
                }
                thread::sleep(Duration::from_millis(20));
            }
        });
        Ok(Self {
            tone,
            _speaker: speaker,
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for Ringer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct ActiveAudio {
    microphone: Microphone,
    speaker: Speaker,
    session: RtpSession,
    mic_level: Arc<MicLevel>,
}

/// Microphone figures written by the sound card thread.
#[derive(Default)]
struct MicLevel {
    peak_percent: AtomicU32,
    gain_percent: AtomicU32,
}

impl AudioMedia {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Media for AudioMedia {
    fn prepare(&mut self, ip: IpAddr) -> Result<SocketAddr, MediaError> {
        let socket = UdpSocket::bind(SocketAddr::new(ip, 0))?;
        let address = socket.local_addr()?;
        self.socket = Some(socket);
        Ok(address)
    }

    fn start(&mut self, remote: SocketAddr) -> Result<(), MediaError> {
        // The call's own audio takes the speaker over from the ringing.
        self.ringer = None;
        if self.active.is_some() {
            return Err("media is already running".into());
        }
        let socket = self
            .socket
            .take()
            .ok_or("media was not prepared before start")?;
        // Audio does not pass through the executor's loop: the receiver
        // thread feeds the speaker and the sound card thread feeds the
        // sender, each the moment a block is ready. Waiting for the loop
        // added up to a packet's worth of uneven delay in both directions.
        let format = Microphone::input_format()?;
        let speaker = Speaker::start()?;
        let mut to_speaker = speaker.sink();
        let config = RtpSessionConfig::new(remote, format.sample_rate)
            .with_dtmf_payload_type(self.dtmf_payload_type);
        self.next_ssrc = self.next_ssrc.wrapping_add(0x0101_0101).max(1);
        let session = RtpSession::from_socket_with_sink(
            socket,
            config,
            self.next_ssrc,
            Some(Box::new(move |samples| {
                // A failure here means the speaker is gone; the call ends soon.
                let _ = to_speaker.push(&samples, PCMU_CLOCK_RATE);
            })),
        )?;

        let to_network = session.pcm_input().ok_or("RTP session has no input")?;
        let mic_level = Arc::new(MicLevel::default());
        let level = Arc::clone(&mic_level);
        let mut gain = AutoGain::new(format.sample_rate);
        let microphone = Microphone::start_with(move |mut samples| {
            let peak = samples
                .iter()
                .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
            level
                .peak_percent
                .fetch_max((peak * 100.0).round() as u32, Ordering::Relaxed);
            gain.process(&mut samples);
            level
                .gain_percent
                .store((gain.gain() * 100.0).round() as u32, Ordering::Relaxed);
            // Never wait on the sound card thread; a full queue drops a block.
            let _ = to_network.try_send(samples);
        })?;
        if microphone.format() != format {
            return Err("the microphone changed while the call was starting".into());
        }

        self.active = Some(ActiveAudio {
            microphone,
            speaker,
            session,
            mic_level,
        });
        // Every call starts with the microphone on and not on hold.
        self.muted = false;
        self.held = false;
        Ok(())
    }

    fn stats(&self) -> MediaStats {
        self.active
            .as_ref()
            .map(|active| MediaStats {
                sent_packets: active.session.sent_packets(),
                received_packets: active.session.received_packets(),
            })
            .unwrap_or_default()
    }

    fn ring(&mut self, tone: Option<Tone>) -> Result<(), MediaError> {
        match tone {
            // Already ringing this way: keep the rhythm going.
            Some(tone)
                if self
                    .ringer
                    .as_ref()
                    .is_some_and(|ringer| ringer.tone == tone) => {}
            Some(tone) if self.active.is_none() => {
                self.ringer = None;
                self.ringer = Some(Ringer::start(tone)?);
            }
            // During a conversation nothing rings over it.
            Some(_) => {}
            None => self.ringer = None,
        }
        Ok(())
    }

    fn set_dtmf_payload_type(&mut self, payload_type: Option<u8>) {
        self.dtmf_payload_type = payload_type;
    }

    fn send_dtmf(&mut self, digit: char) -> Result<(), MediaError> {
        let active = self.active.as_ref().ok_or("no audio is running")?;
        if active.session.send_dtmf(digit) {
            Ok(())
        } else {
            Err(format!("'{digit}' is not a keypad digit").into())
        }
    }

    fn set_muted(&mut self, muted: bool) {
        self.muted = muted;
        if let Some(active) = self.active.as_ref() {
            active.session.set_muted(self.muted || self.held);
        }
    }

    fn set_hold(&mut self, hold: bool) {
        self.held = hold;
        if let Some(active) = self.active.as_ref() {
            active.session.set_muted(self.muted || self.held);
        }
    }

    fn quality(&self) -> Option<MediaQuality> {
        let active = self.active.as_ref()?;
        let order = active.session.order_stats();
        Some(MediaQuality {
            gaps: active.speaker.underruns(),
            lost: order.lost,
            reordered: order.reordered,
            discarded: order.discarded,
            reserve_ms: active.speaker.largest_reserve_ms(),
            mic_peak_percent: active.mic_level.peak_percent.load(Ordering::Relaxed),
            mic_gain_percent: active.mic_level.gain_percent.load(Ordering::Relaxed),
        })
    }

    fn stop(&mut self) {
        self.socket = None;
        self.ringer = None;
        if let Some(active) = self.active.take() {
            let ActiveAudio {
                microphone,
                speaker,
                mut session,
                ..
            } = active;
            // The microphone first: its callback feeds the session.
            drop(microphone);
            // Errors while tearing down a finished call have nobody to act on them.
            let _ = session.shutdown();
            drop(speaker);
        }
    }
}
