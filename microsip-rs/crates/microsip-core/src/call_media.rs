//! Media side of a call, kept behind a trait so the SIP executor can be tested
//! without sound devices.

use crate::audio::{MAX_VOLUME_PERCENT, Microphone, Speaker, volume_factor};
use crate::codec::Codec;
use crate::gain::AutoGain;
use crate::recording::{CallRecorder, RecorderInput};
use crate::resample::Resampler;
use crate::rtcp::RemoteQuality;
use crate::rtp::PCMU_CLOCK_RATE;
use crate::rtp::session::RtcpConfig;
use crate::rtp::session::{RtpSession, RtpSessionConfig};
use crate::srtp::SrtpKeys;
use crate::tones::{Melody, MelodyPlayer, TONE_RATE, Tone, ToneGenerator};
use crate::voice::{VoiceProcessor, VoiceSettings};
use std::collections::VecDeque;
use std::error::Error;
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
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
    /// What the other side last reported (RTCP) about this phone's audio.
    pub remote: Option<RemoteSummary>,
}

/// The other side's view of this phone's audio, rounded for showing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RemoteSummary {
    /// Share of this phone's packets lost on the way, in percent.
    pub lost_percent: u32,
    /// Packets lost in all.
    pub lost_total: i32,
    /// How unevenly they arrived, in milliseconds.
    pub jitter_ms: u32,
    /// Time there and back, when it could be measured.
    pub round_trip_ms: Option<u32>,
}

impl From<RemoteQuality> for RemoteSummary {
    fn from(quality: RemoteQuality) -> Self {
        Self {
            lost_percent: quality.lost_percent.round() as u32,
            lost_total: quality.lost_total,
            jitter_ms: quality.jitter_ms.round() as u32,
            round_trip_ms: quality.round_trip_ms.map(|ms| ms.round() as u32),
        }
    }
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
        )?;
        if let Some(remote) = self.remote {
            write!(
                formatter,
                "; other side: lost {}% ({} in all), jitter {} ms",
                remote.lost_percent, remote.lost_total, remote.jitter_ms
            )?;
            if let Some(round_trip) = remote.round_trip_ms {
                write!(formatter, ", round trip {round_trip} ms")?;
            }
        }
        Ok(())
    }
}

/// Media for one call at a time.
///
/// Implementations are created on the executor thread and are never moved
/// between threads, so they may hold `!Send` audio streams.
pub trait Media {
    /// Reserves a local RTP port and returns the address to advertise in SDP.
    fn prepare(&mut self, ip: IpAddr) -> Result<SocketAddr, MediaError>;

    /// Tells the media the other side's RTCP port, when its SDP names one
    /// (`a=rtcp`); otherwise it is the RTP port plus one. Called before
    /// `start`.
    fn set_remote_rtcp_port(&mut self, _port: Option<u16>) {}

    /// Asks a STUN server under which address the reserved port is seen
    /// from the internet. `None` when there is no port, the server does
    /// not answer, or this media cannot ask. Called after `prepare`.
    fn public_address(&mut self, _stun: SocketAddr) -> Option<SocketAddr> {
        None
    }

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

    /// Tells the media which codec was agreed for the voice it sends.
    /// Called before `start`; without it PCMU is used.
    fn set_codec(&mut self, _codec: Codec) {}

    /// Tells the media the payload type agreed for that codec and what
    /// else the other side listed and so may send. Called after
    /// [`Media::set_codec`].
    fn set_payload_types(&mut self, _payload_type: u8, _listed: Vec<(u8, Codec)>) {}

    /// Tells the media the keys for encrypting the call's audio (SRTP);
    /// `None` for a call in the clear. Called before `start`.
    fn set_srtp(&mut self, _keys: Option<SrtpKeys>) {}

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

    /// In a conference every line hears the other lines as well as the
    /// microphone. Without a conference a line carries the microphone only.
    fn set_conference(&mut self, _on: bool) {}

    /// Starts recording the running call to this WAV file, or stops the
    /// recording with `None`. Stopping the audio stops it too.
    fn record(&mut self, path: Option<&Path>) -> Result<(), MediaError> {
        match path {
            Some(_) => Err("this phone has no audio to record".into()),
            None => Ok(()),
        }
    }

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

    fn public_address(&mut self, stun: SocketAddr) -> Option<SocketAddr> {
        crate::stun::query(self.socket.as_ref()?, stun).ok()
    }

    fn start(&mut self, _remote: SocketAddr) -> Result<(), MediaError> {
        Ok(())
    }

    fn stop(&mut self) {
        self.socket = None;
    }
}

/// What a [`ToneMedia`] heard during its last call.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ToneReport {
    /// Pitch of the sound heard in the last second, in Hz.
    pub frequency: f32,
    /// Its level: the peak of a steady tone, 0 for silence.
    pub level: f32,
    /// Seconds of sound received in all.
    pub seconds: f32,
}

/// The last second of what was heard, for [`ToneReport`].
#[derive(Default)]
struct ToneMeter {
    rate: u32,
    recent: VecDeque<f32>,
    total: u64,
}

impl ToneMeter {
    fn push(&mut self, samples: &[f32], rate: u32) {
        self.rate = rate;
        self.total += samples.len() as u64;
        self.recent.extend(samples.iter().copied());
        let excess = self.recent.len().saturating_sub(rate as usize);
        self.recent.drain(..excess);
    }

    fn report(&self) -> ToneReport {
        if self.rate == 0 || self.recent.is_empty() {
            return ToneReport::default();
        }
        let count = self.recent.len() as f32;
        let power: f32 = self.recent.iter().map(|sample| sample * sample).sum();
        // Each cycle of a tone crosses zero upwards once.
        let crossings = self
            .recent
            .iter()
            .zip(self.recent.iter().skip(1))
            .filter(|(before, after)| **before < 0.0 && **after >= 0.0)
            .count();
        ToneReport {
            frequency: crossings as f32 * self.rate as f32 / count,
            level: (2.0 * power / count).sqrt(),
            seconds: self.total as f32 / self.rate as f32,
        }
    }
}

/// Media without sound devices that sends a steady tone and measures the
/// tone it hears. With two of them a call's whole audio path, codec
/// included, can be checked on a machine that has no sound card.
pub struct ToneMedia {
    frequency: f32,
    socket: Option<UdpSocket>,
    codec: Codec,
    payload_type: Option<u8>,
    listed: Vec<(u8, Codec)>,
    srtp: Option<SrtpKeys>,
    rtcp_socket: Option<UdpSocket>,
    remote_rtcp_port: Option<u16>,
    dtmf_payload_type: Option<u8>,
    session: Option<RtpSession>,
    feeder: Option<(Arc<AtomicBool>, JoinHandle<()>)>,
    meter: Arc<Mutex<ToneMeter>>,
    report: Arc<Mutex<ToneReport>>,
    next_ssrc: u32,
}

impl ToneMedia {
    /// `frequency` is the tone to send, in Hz. What was heard is written to
    /// `report` when a call's audio stops.
    pub fn new(frequency: f32, report: Arc<Mutex<ToneReport>>) -> Self {
        Self {
            frequency,
            socket: None,
            codec: Codec::Pcmu,
            payload_type: None,
            listed: Vec::new(),
            srtp: None,
            rtcp_socket: None,
            remote_rtcp_port: None,
            dtmf_payload_type: None,
            session: None,
            feeder: None,
            meter: Arc::default(),
            report,
            next_ssrc: 0x7000_0000,
        }
    }
}

impl Media for ToneMedia {
    fn prepare(&mut self, ip: IpAddr) -> Result<SocketAddr, MediaError> {
        let (socket, rtcp) = bind_pair(ip)?;
        let address = socket.local_addr()?;
        self.socket = Some(socket);
        self.rtcp_socket = rtcp;
        Ok(address)
    }

    fn set_remote_rtcp_port(&mut self, port: Option<u16>) {
        self.remote_rtcp_port = port;
    }

    fn public_address(&mut self, stun: SocketAddr) -> Option<SocketAddr> {
        crate::stun::query(self.socket.as_ref()?, stun).ok()
    }

    fn start(&mut self, remote: SocketAddr) -> Result<(), MediaError> {
        let socket = self
            .socket
            .take()
            .ok_or("media was not prepared before start")?;
        let rate = self.codec.sample_rate();
        let config = RtpSessionConfig::new(remote, rate)
            .with_dtmf_payload_type(self.dtmf_payload_type)
            .with_codec(self.codec)
            .with_payload_type(self.payload_type.unwrap_or(self.codec.payload_type()))
            .with_receive_types(self.listed.clone())
            .with_srtp(self.srtp.clone())
            .with_rtcp(rtcp_for(
                self.rtcp_socket.take(),
                remote,
                self.remote_rtcp_port,
            ));
        if let Ok(mut meter) = self.meter.lock() {
            *meter = ToneMeter::default();
        }
        let meter = Arc::clone(&self.meter);
        self.next_ssrc = self.next_ssrc.wrapping_add(0x0101_0101).max(1);
        let session = RtpSession::from_socket_with_sink(
            socket,
            config,
            self.next_ssrc,
            Some(Box::new(move |samples| {
                if let Ok(mut meter) = meter.lock() {
                    meter.push(&samples, rate);
                }
            })),
        )?;
        let input = session.pcm_input().ok_or("RTP session has no input")?;
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let frequency = self.frequency;
        let feeder = thread::spawn(move || {
            let frame = rate as usize / 50;
            let started = Instant::now();
            let mut position = 0_u64;
            while !stopped.load(Ordering::Acquire) {
                // One 20 ms block for every 20 ms that has passed.
                while position * 1_000 / u64::from(rate) < started.elapsed().as_millis() as u64 {
                    let block: Vec<f32> = (position..position + frame as u64)
                        .map(|index| {
                            let time = index as f32 / rate as f32;
                            0.4 * (std::f32::consts::TAU * frequency * time).sin()
                        })
                        .collect();
                    position += frame as u64;
                    let _ = input.try_send(block);
                }
                thread::sleep(Duration::from_millis(5));
            }
        });
        self.session = Some(session);
        self.feeder = Some((stop, feeder));
        Ok(())
    }

    fn pump(&mut self) -> Result<(), MediaError> {
        Ok(())
    }

    fn quality(&self) -> Option<MediaQuality> {
        let session = self.session.as_ref()?;
        Some(MediaQuality {
            remote: session.remote_quality().map(RemoteSummary::from),
            ..MediaQuality::default()
        })
    }

    fn stats(&self) -> MediaStats {
        self.session
            .as_ref()
            .map(|session| MediaStats {
                sent_packets: session.sent_packets(),
                received_packets: session.received_packets(),
            })
            .unwrap_or_default()
    }

    fn set_codec(&mut self, codec: Codec) {
        self.codec = codec;
        self.payload_type = None;
        self.listed.clear();
    }

    fn set_payload_types(&mut self, payload_type: u8, listed: Vec<(u8, Codec)>) {
        self.payload_type = Some(payload_type);
        self.listed = listed;
    }

    fn set_srtp(&mut self, keys: Option<SrtpKeys>) {
        self.srtp = keys;
    }

    fn set_dtmf_payload_type(&mut self, payload_type: Option<u8>) {
        self.dtmf_payload_type = payload_type;
    }

    fn send_dtmf(&mut self, digit: char) -> Result<(), MediaError> {
        match self.session.as_ref() {
            Some(session) if session.send_dtmf(digit) => Ok(()),
            Some(_) => Err(format!("'{digit}' is not a keypad digit").into()),
            None => Err("no call is connected".into()),
        }
    }

    fn stop(&mut self) {
        self.socket = None;
        if let Some((stop, feeder)) = self.feeder.take() {
            stop.store(true, Ordering::Release);
            let _ = feeder.join();
        }
        if let Some(mut session) = self.session.take() {
            let _ = session.shutdown();
            if let (Ok(meter), Ok(mut report)) = (self.meter.lock(), self.report.lock()) {
                *report = meter.report();
            }
        }
    }
}

/// Reserves an RTP port and the RTCP port after it. RTCP goes without a
/// port when no such pair can be had; the call works without reports.
fn bind_pair(ip: IpAddr) -> std::io::Result<(UdpSocket, Option<UdpSocket>)> {
    for _ in 0..16 {
        let rtp = UdpSocket::bind(SocketAddr::new(ip, 0))?;
        let port = rtp.local_addr()?.port();
        // RTP takes an even port and RTCP the odd one above it.
        if port % 2 != 0 || port == u16::MAX {
            continue;
        }
        if let Ok(rtcp) = UdpSocket::bind(SocketAddr::new(ip, port + 1)) {
            return Ok((rtp, Some(rtcp)));
        }
    }
    Ok((UdpSocket::bind(SocketAddr::new(ip, 0))?, None))
}

/// Where the other side's RTCP goes, and the socket it goes from.
fn rtcp_for(
    socket: Option<UdpSocket>,
    remote: SocketAddr,
    remote_port: Option<u16>,
) -> Option<RtcpConfig> {
    let port = remote_port.or_else(|| remote.port().checked_add(1))?;
    Some(RtcpConfig {
        socket: Arc::new(socket?),
        remote: SocketAddr::new(remote.ip(), port),
        interval: RTCP_INTERVAL,
    })
}

/// Time between two RTCP reports.
const RTCP_INTERVAL: Duration = Duration::from_secs(5);

/// Sound settings shared between the interface and the phone's audio.
/// Volumes act at once, also during a call; a newly chosen device is used
/// from the next call (and the next ringing) on.
#[derive(Clone)]
pub struct AudioControls {
    inner: Arc<ControlsInner>,
}

/// What one line hears from its other side, at 8 kHz, kept briefly so the
/// other lines of a conference can send it on.
type HeardQueue = Arc<Mutex<VecDeque<f32>>>;

/// Audio waiting in a [`HeardQueue`] beyond this is dropped (half a second).
const HEARD_LIMIT: usize = 4_000;

struct ControlsInner {
    /// All connected lines talk together.
    conference: AtomicBool,
    /// One queue per line of the phone, in the order the lines were made.
    heard: Mutex<Vec<HeardQueue>>,
    speaker_percent: Arc<AtomicU32>,
    microphone_percent: AtomicU32,
    input_device: Mutex<Option<String>>,
    output_device: Mutex<Option<String>>,
    ring_device: Mutex<Option<String>>,
    ringtone: Mutex<Option<Arc<Melody>>>,
    echo_cancellation: AtomicBool,
    noise_suppression: AtomicBool,
    /// Loudest sample heard from the other side since the window last
    /// looked, in thousandths of full scale.
    heard_level: AtomicU32,
    /// The same for the microphone, as it goes out.
    microphone_level: AtomicU32,
}

impl Default for AudioControls {
    fn default() -> Self {
        Self {
            inner: Arc::new(ControlsInner {
                conference: AtomicBool::new(false),
                heard: Mutex::new(Vec::new()),
                speaker_percent: Arc::new(AtomicU32::new(100)),
                microphone_percent: AtomicU32::new(100),
                input_device: Mutex::new(None),
                output_device: Mutex::new(None),
                ring_device: Mutex::new(None),
                ringtone: Mutex::new(None),
                echo_cancellation: AtomicBool::new(true),
                noise_suppression: AtomicBool::new(true),
                heard_level: AtomicU32::new(0),
                microphone_level: AtomicU32::new(0),
            }),
        }
    }
}

impl AudioControls {
    /// Speaker volume in percent, 0 to [`MAX_VOLUME_PERCENT`]; 100 is unchanged.
    pub fn set_speaker_volume(&self, percent: u32) {
        self.inner
            .speaker_percent
            .store(percent.min(MAX_VOLUME_PERCENT), Ordering::Relaxed);
    }

    pub fn speaker_volume(&self) -> u32 {
        self.inner.speaker_percent.load(Ordering::Relaxed)
    }

    /// Microphone volume in percent, applied after the automatic gain.
    pub fn set_microphone_volume(&self, percent: u32) {
        self.inner
            .microphone_percent
            .store(percent.min(MAX_VOLUME_PERCENT), Ordering::Relaxed);
    }

    pub fn microphone_volume(&self) -> u32 {
        self.inner.microphone_percent.load(Ordering::Relaxed)
    }

    /// Microphone to use; `None` or an empty name is the system default.
    pub fn set_input_device(&self, name: Option<String>) {
        if let Ok(mut device) = self.inner.input_device.lock() {
            *device = name.filter(|name| !name.is_empty());
        }
    }

    pub fn input_device(&self) -> Option<String> {
        self.inner.input_device.lock().ok()?.clone()
    }

    /// Speaker or headphones to use; `None` or an empty name is the default.
    pub fn set_output_device(&self, name: Option<String>) {
        if let Ok(mut device) = self.inner.output_device.lock() {
            *device = name.filter(|name| !name.is_empty());
        }
    }

    pub fn output_device(&self) -> Option<String> {
        self.inner.output_device.lock().ok()?.clone()
    }

    /// Device the ring of an incoming call plays on, so that it can be
    /// heard from loudspeakers while the conversation is in a headset.
    /// `None` or an empty name means the same device as the conversation.
    pub fn set_ring_device(&self, name: Option<String>) {
        if let Ok(mut device) = self.inner.ring_device.lock() {
            *device = name.filter(|name| !name.is_empty());
        }
    }

    pub fn ring_device(&self) -> Option<String> {
        self.inner.ring_device.lock().ok()?.clone()
    }

    /// Melody played for an incoming call; `None` is the built-in ring.
    pub fn set_ringtone(&self, melody: Option<Melody>) {
        if let Ok(mut ringtone) = self.inner.ringtone.lock() {
            *ringtone = melody.map(Arc::new);
        }
    }

    pub fn ringtone(&self) -> Option<Arc<Melody>> {
        self.inner.ringtone.lock().ok()?.clone()
    }

    /// Echo cancellation and noise suppression of the microphone; both
    /// are on unless switched off. A change counts from the next call.
    pub fn set_voice_settings(&self, settings: VoiceSettings) {
        self.inner
            .echo_cancellation
            .store(settings.echo_cancellation, Ordering::Relaxed);
        self.inner
            .noise_suppression
            .store(settings.noise_suppression, Ordering::Relaxed);
    }

    pub fn voice_settings(&self) -> VoiceSettings {
        VoiceSettings {
            echo_cancellation: self.inner.echo_cancellation.load(Ordering::Relaxed),
            noise_suppression: self.inner.noise_suppression.load(Ordering::Relaxed),
        }
    }

    /// How loud the other side and the microphone were since the last
    /// call: the loudest sample of each, from 0.0 to 1.0. For level bars.
    pub fn take_levels(&self) -> (f32, f32) {
        let heard = self.inner.heard_level.swap(0, Ordering::Relaxed);
        let microphone = self.inner.microphone_level.swap(0, Ordering::Relaxed);
        (heard as f32 / 1_000.0, microphone as f32 / 1_000.0)
    }

    fn note_level(level: &AtomicU32, samples: &[f32]) {
        let peak = samples
            .iter()
            .fold(0.0_f32, |peak, sample| peak.max(sample.abs()))
            .min(1.0);
        level.fetch_max((peak * 1_000.0).round() as u32, Ordering::Relaxed);
    }

    /// The speaker volume shared with the sound card, for other players.
    pub fn speaker_volume_handle(&self) -> Arc<AtomicU32> {
        Arc::clone(&self.inner.speaker_percent)
    }

    /// Adds a line and returns its number among the lines sharing these
    /// controls.
    fn register_line(&self) -> usize {
        match self.inner.heard.lock() {
            Ok(mut heard) => {
                heard.push(HeardQueue::default());
                heard.len() - 1
            }
            Err(_) => 0,
        }
    }

    fn heard_queue(&self, line: usize) -> Option<HeardQueue> {
        self.inner.heard.lock().ok()?.get(line).cloned()
    }

    /// The queues of every line but this one.
    fn other_heard_queues(&self, line: usize) -> Vec<HeardQueue> {
        match self.inner.heard.lock() {
            Ok(heard) => heard
                .iter()
                .enumerate()
                .filter(|(index, _)| *index != line)
                .map(|(_, queue)| Arc::clone(queue))
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    pub fn is_conference(&self) -> bool {
        self.inner.conference.load(Ordering::Relaxed)
    }

    fn set_conference(&self, on: bool) {
        self.inner.conference.store(on, Ordering::Relaxed);
        // Old audio must not be heard when a conference starts or ends.
        if let Ok(heard) = self.inner.heard.lock() {
            for queue in heard.iter() {
                if let Ok(mut queue) = queue.lock() {
                    queue.clear();
                }
            }
        }
    }
}

/// Adds what the other lines hear to the audio a line sends, so that in a
/// conference every party hears every other one.
struct ConferenceMixer {
    /// The other lines, each with a converter from 8 kHz to the microphone's
    /// rate.
    others: Vec<(HeardQueue, Resampler)>,
    rate: u32,
    /// Part of an 8 kHz sample still owed from earlier blocks.
    owed: u64,
}

impl ConferenceMixer {
    fn new(others: Vec<HeardQueue>, rate: u32) -> Self {
        Self {
            others: others
                .into_iter()
                .filter_map(|queue| Some((queue, Resampler::new(PCMU_CLOCK_RATE, rate)?)))
                .collect(),
            rate: rate.max(1),
            owed: 0,
        }
    }

    /// Mixes the other lines into `samples` (recorded at the mixer's rate).
    fn mix(&mut self, samples: &mut [f32]) {
        // How many 8 kHz samples cover this block, without losing fractions.
        let scaled = samples.len() as u64 * u64::from(PCMU_CLOCK_RATE) + self.owed;
        let wanted = (scaled / u64::from(self.rate)) as usize;
        self.owed = scaled % u64::from(self.rate);
        for (queue, resampler) in &mut self.others {
            let mut heard = vec![0.0; wanted];
            if let Ok(mut queue) = queue.lock() {
                for slot in &mut heard {
                    // A line with nothing to say adds silence.
                    *slot = queue.pop_front().unwrap_or(0.0);
                }
            }
            for (sample, other) in samples.iter_mut().zip(resampler.push(&heard)) {
                *sample = (*sample + other).clamp(-1.0, 1.0);
            }
        }
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
    controls: AudioControls,
    /// This line's number among the lines sharing `controls`.
    line: Option<usize>,
    codec: Codec,
    /// Payload type agreed for `codec`; `None` is the codec's usual one.
    payload_type: Option<u8>,
    /// What else the other side may send.
    listed: Vec<(u8, Codec)>,
    /// Keys of an encrypted call.
    srtp: Option<SrtpKeys>,
    /// The RTCP port reserved with the RTP one.
    rtcp_socket: Option<UdpSocket>,
    /// The other side's RTCP port, when its SDP named one.
    remote_rtcp_port: Option<u16>,
    /// Playback reserve the last call ended with; the next call on the
    /// same connection starts from it instead of learning it again.
    reserve_ms: u32,
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
    fn start(tone: Tone, controls: &AudioControls) -> Result<Self, MediaError> {
        let incoming = tone == Tone::Ringtone;
        // Only the ring of an incoming call has a device and a melody of
        // its own; the ringback belongs to the conversation.
        let ring_device = incoming.then(|| controls.ring_device()).flatten();
        let volume = controls.speaker_volume_handle();
        let speaker = match ring_device {
            // A ring device that was unplugged must not silence the phone.
            Some(device) => Speaker::start_on(Some(&device), Arc::clone(&volume), 0)
                .or_else(|_| Speaker::start_on(controls.output_device().as_deref(), volume, 0)),
            None => Speaker::start_on(controls.output_device().as_deref(), volume, 0),
        }?;
        let mut melody = incoming
            .then(|| controls.ringtone())
            .flatten()
            .map(MelodyPlayer::new);
        let mut sink = speaker.sink();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            let mut generator = ToneGenerator::new(tone);
            let started = Instant::now();
            while !stopped.load(Ordering::Acquire) {
                // Stay a little ahead of the clock, as a network stream would.
                let wanted = started.elapsed().as_millis() as u64 + 120;
                loop {
                    let (block, rate) = match melody.as_mut() {
                        Some(player) if player.produced_ms() < wanted => {
                            let rate = player.rate();
                            (player.next_block(rate as usize / 50), rate)
                        }
                        None if generator.produced_ms() < wanted => {
                            (generator.next_block(TONE_RATE as usize / 50), TONE_RATE)
                        }
                        _ => break,
                    };
                    if sink.push(&block, rate).is_err() {
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
    /// Where the two audio threads hand their samples while recording.
    record_local: RecorderSlot,
    record_remote: RecorderSlot,
    recorder: Option<CallRecorder>,
}

/// A recorder input that may be put in place or taken away during a call.
type RecorderSlot = Arc<Mutex<Option<RecorderInput>>>;

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

    /// Mute and hold both stop the microphone from being sent, and so from
    /// being recorded.
    fn apply_silence(&mut self) {
        let silent = self.muted || self.held;
        if let Some(active) = self.active.as_ref() {
            active.session.set_muted(silent);
            if let Some(recorder) = active.recorder.as_ref() {
                recorder.set_local_muted(silent);
            }
        }
    }

    /// Audio that follows the given settings (devices and volumes).
    pub fn with_controls(controls: AudioControls) -> Self {
        let line = Some(controls.register_line());
        Self {
            controls,
            line,
            ..Self::default()
        }
    }
}

impl Media for AudioMedia {
    fn prepare(&mut self, ip: IpAddr) -> Result<SocketAddr, MediaError> {
        let (socket, rtcp) = bind_pair(ip)?;
        let address = socket.local_addr()?;
        self.socket = Some(socket);
        self.rtcp_socket = rtcp;
        Ok(address)
    }

    fn set_remote_rtcp_port(&mut self, port: Option<u16>) {
        self.remote_rtcp_port = port;
    }

    fn public_address(&mut self, stun: SocketAddr) -> Option<SocketAddr> {
        crate::stun::query(self.socket.as_ref()?, stun).ok()
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
        let input = self.controls.input_device();
        let format = Microphone::input_format_of(input.as_deref())?;
        let speaker = Speaker::start_on(
            self.controls.output_device().as_deref(),
            self.controls.speaker_volume_handle(),
            self.reserve_ms,
        )?;
        let mut to_speaker = speaker.sink();
        let record_local = RecorderSlot::default();
        let record_remote = RecorderSlot::default();
        let (tap_local, tap_remote) = (Arc::clone(&record_local), Arc::clone(&record_remote));
        let heard_controls = self.controls.clone();
        // For a conference: what this line hears is kept for the others,
        // and what the others hear is added to what this line sends.
        let heard = self.line.and_then(|line| self.controls.heard_queue(line));
        let others = self
            .line
            .map(|line| self.controls.other_heard_queues(line))
            .unwrap_or_default();
        let mut mixer = ConferenceMixer::new(others, format.sample_rate);
        let (conference_in, conference_out) = (self.controls.clone(), self.controls.clone());
        let config = RtpSessionConfig::new(remote, format.sample_rate)
            .with_dtmf_payload_type(self.dtmf_payload_type)
            .with_codec(self.codec)
            .with_payload_type(self.payload_type.unwrap_or(self.codec.payload_type()))
            .with_receive_types(self.listed.clone())
            .with_srtp(self.srtp.clone())
            .with_rtcp(rtcp_for(
                self.rtcp_socket.take(),
                remote,
                self.remote_rtcp_port,
            ));
        // The sound of this call: 8 kHz for G.711 and G.729, 16 kHz for
        // G.722 and Opus.
        let heard_rate = config.decoded_sample_rate();
        let mut to_conference = Resampler::new(heard_rate, PCMU_CLOCK_RATE);
        // Echo cancellation learns from what is played and cleans what the
        // microphone picks up; one processor serves both sides.
        let cleaner = VoiceProcessor::new(
            self.controls.voice_settings(),
            format.sample_rate,
            heard_rate,
        );
        let played = cleaner.clone();
        self.next_ssrc = self.next_ssrc.wrapping_add(0x0101_0101).max(1);
        let session = RtpSession::from_socket_with_sink(
            socket,
            config,
            self.next_ssrc,
            Some(Box::new(move |samples| {
                if let Some(played) = played.as_ref() {
                    played.played(&samples);
                }
                if conference_in.is_conference()
                    && let Some(heard) = heard.as_ref()
                    && let Some(to_conference) = to_conference.as_mut()
                    && let Ok(mut heard) = heard.lock()
                {
                    // The lines of a conference meet at 8 kHz.
                    if heard_rate == PCMU_CLOCK_RATE {
                        heard.extend(samples.iter().copied());
                    } else {
                        heard.extend(to_conference.push(&samples));
                    }
                    if heard.len() > HEARD_LIMIT {
                        let excess = heard.len() - HEARD_LIMIT;
                        heard.drain(..excess);
                    }
                }
                if let Ok(mut tap) = tap_remote.lock()
                    && let Some(input) = tap.as_mut()
                {
                    input.push(&samples, heard_rate);
                }
                AudioControls::note_level(&heard_controls.inner.heard_level, &samples);
                // A failure here means the speaker is gone; the call ends soon.
                let _ = to_speaker.push(&samples, heard_rate);
            })),
        )?;

        let to_network = session.pcm_input().ok_or("RTP session has no input")?;
        let mic_level = Arc::new(MicLevel::default());
        let level = Arc::clone(&mic_level);
        let mut gain = AutoGain::new(format.sample_rate);
        let controls = self.controls.clone();
        let microphone = Microphone::start_on(input.as_deref(), move |mut samples| {
            let peak = samples
                .iter()
                .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
            level
                .peak_percent
                .fetch_max((peak * 100.0).round() as u32, Ordering::Relaxed);
            // Echo and noise come out first: the gain must not raise them.
            if let Some(cleaner) = cleaner.as_ref() {
                cleaner.clean(&mut samples);
            }
            gain.process(&mut samples);
            level
                .gain_percent
                .store((gain.gain() * 100.0).round() as u32, Ordering::Relaxed);
            let volume = volume_factor(controls.microphone_volume());
            if volume != 1.0 {
                for sample in &mut samples {
                    *sample = (*sample * volume).clamp(-1.0, 1.0);
                }
            }
            if conference_out.is_conference() {
                mixer.mix(&mut samples);
            }
            // The recording gets the voice as it is sent, after the gain.
            if let Ok(mut tap) = tap_local.lock()
                && let Some(input) = tap.as_mut()
            {
                input.push(&samples, format.sample_rate);
            }
            AudioControls::note_level(&controls.inner.microphone_level, &samples);
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
            record_local,
            record_remote,
            recorder: None,
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
                self.ringer = Some(Ringer::start(tone, &self.controls)?);
            }
            // During a conversation nothing rings over it.
            Some(_) => {}
            None => self.ringer = None,
        }
        Ok(())
    }

    fn set_codec(&mut self, codec: Codec) {
        self.codec = codec;
        self.payload_type = None;
        self.listed.clear();
    }

    fn set_payload_types(&mut self, payload_type: u8, listed: Vec<(u8, Codec)>) {
        self.payload_type = Some(payload_type);
        self.listed = listed;
    }

    fn set_srtp(&mut self, keys: Option<SrtpKeys>) {
        self.srtp = keys;
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
        self.apply_silence();
    }

    fn set_hold(&mut self, hold: bool) {
        self.held = hold;
        self.apply_silence();
    }

    fn set_conference(&mut self, on: bool) {
        if self.controls.is_conference() != on {
            self.controls.set_conference(on);
        }
    }

    fn record(&mut self, path: Option<&Path>) -> Result<(), MediaError> {
        let silent = self.muted || self.held;
        let Some(active) = self.active.as_mut() else {
            return match path {
                Some(_) => Err("only a connected call can be recorded".into()),
                None => Ok(()),
            };
        };
        // Taking the inputs away first lets the old file be finished.
        for slot in [&active.record_local, &active.record_remote] {
            if let Ok(mut slot) = slot.lock() {
                *slot = None;
            }
        }
        let finished = active.recorder.take().map(CallRecorder::stop);
        if let Some(path) = path {
            let recorder = CallRecorder::start(path)?;
            recorder.set_local_muted(silent);
            if let Ok(mut slot) = active.record_local.lock() {
                *slot = Some(recorder.local_input());
            }
            if let Ok(mut slot) = active.record_remote.lock() {
                *slot = Some(recorder.remote_input());
            }
            active.recorder = Some(recorder);
        }
        match finished {
            Some(Err(error)) => Err(format!("the recording could not be finished: {error}").into()),
            _ => Ok(()),
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
            remote: active.session.remote_quality().map(RemoteSummary::from),
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
            self.reserve_ms = speaker.reserve_ms();
            // The recording ends with the call; its file is completed here.
            drop(active.recorder);
            // The microphone first: its callback feeds the session.
            drop(microphone);
            // Errors while tearing down a finished call have nobody to act on them.
            let _ = session.shutdown();
            drop(speaker);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controls_start_neutral_and_are_shared_between_copies() {
        let controls = AudioControls::default();
        assert_eq!(controls.speaker_volume(), 100);
        assert_eq!(controls.microphone_volume(), 100);
        assert_eq!(controls.input_device(), None);
        assert_eq!(controls.output_device(), None);

        // The interface keeps one copy, the audio another.
        let other = controls.clone();
        controls.set_speaker_volume(40);
        controls.set_microphone_volume(150);
        controls.set_input_device(Some("Headset".to_owned()));
        controls.set_output_device(Some("Speakers".to_owned()));
        assert_eq!(other.speaker_volume(), 40);
        assert_eq!(other.microphone_volume(), 150);
        assert_eq!(other.input_device().as_deref(), Some("Headset"));
        assert_eq!(other.output_device().as_deref(), Some("Speakers"));
        // The running speaker reads the very same number.
        assert_eq!(other.speaker_volume_handle().load(Ordering::Relaxed), 40);
    }

    #[test]
    fn volumes_are_limited_and_an_empty_name_means_the_default_device() {
        let controls = AudioControls::default();
        controls.set_speaker_volume(10_000);
        controls.set_microphone_volume(201);
        assert_eq!(controls.speaker_volume(), MAX_VOLUME_PERCENT);
        assert_eq!(controls.microphone_volume(), MAX_VOLUME_PERCENT);

        controls.set_input_device(Some("Headset".to_owned()));
        controls.set_input_device(Some(String::new()));
        assert_eq!(controls.input_device(), None);
        controls.set_output_device(None);
        assert_eq!(controls.output_device(), None);
    }

    fn queue_of(samples: &[f32]) -> HeardQueue {
        Arc::new(Mutex::new(samples.iter().copied().collect()))
    }

    #[test]
    fn conference_adds_the_other_line_to_what_is_sent() {
        // The other line hears a steady level; this line's microphone is
        // silent. After mixing, this line sends that level on.
        let other = queue_of(&[0.4; 8_000]);
        let mut mixer = ConferenceMixer::new(vec![Arc::clone(&other)], 48_000);
        let mut sent = Vec::new();
        for _ in 0..50 {
            let mut block = vec![0.0; 480];
            mixer.mix(&mut block);
            sent.extend(block);
        }
        // Half a second at 48 kHz took exactly half a second at 8 kHz.
        assert_eq!(other.lock().unwrap().len(), 4_000);
        let settled = &sent[2_000..];
        assert!(settled.iter().all(|sample| (sample - 0.4).abs() < 0.01));
    }

    #[test]
    fn conference_keeps_the_microphone_and_limits_the_sum() {
        let other = queue_of(&[0.8; 1_000]);
        let mut mixer = ConferenceMixer::new(vec![other], 8_000);
        let mut block = vec![0.5; 160];
        mixer.mix(&mut block);
        // 0.5 + 0.8 would clip: the sum stops at full scale.
        assert!(block.iter().all(|sample| *sample == 1.0));

        // A line that has nothing to say changes nothing.
        let mut mixer = ConferenceMixer::new(vec![queue_of(&[])], 8_000);
        let mut block = vec![0.5; 160];
        mixer.mix(&mut block);
        assert!(block.iter().all(|sample| *sample == 0.5));
    }

    #[test]
    fn odd_block_sizes_do_not_drift() {
        // 441 samples at 44.1 kHz are 80 at 8 kHz: 100 blocks take 8000.
        let other = queue_of(&vec![0.1; 9_000]);
        let mut mixer = ConferenceMixer::new(vec![Arc::clone(&other)], 44_100);
        for _ in 0..100 {
            mixer.mix(&mut [0.0; 441]);
        }
        assert_eq!(other.lock().unwrap().len(), 1_000);
        // And with a block size that does not divide evenly.
        let other = queue_of(&vec![0.1; 9_000]);
        let mut mixer = ConferenceMixer::new(vec![Arc::clone(&other)], 48_000);
        for _ in 0..1_000 {
            mixer.mix(&mut [0.0; 47]);
        }
        // 47_000 samples at 48 kHz are 7833.33 at 8 kHz.
        assert_eq!(other.lock().unwrap().len(), 9_000 - 7_833);
    }

    #[test]
    fn lines_sharing_controls_find_each_other_for_a_conference() {
        let controls = AudioControls::default();
        let first = AudioMedia::with_controls(controls.clone());
        let second = AudioMedia::with_controls(controls.clone());
        assert_eq!((first.line, second.line), (Some(0), Some(1)));
        assert_eq!(controls.other_heard_queues(0).len(), 1);
        assert!(Arc::ptr_eq(
            &controls.other_heard_queues(0)[0],
            &controls.heard_queue(1).unwrap()
        ));
        // A phone made without shared controls has no conference partner.
        assert_eq!(AudioMedia::new().line, None);

        // Switching the conference clears what was waiting.
        controls
            .heard_queue(1)
            .unwrap()
            .lock()
            .unwrap()
            .push_back(0.5);
        assert!(!controls.is_conference());
        controls.set_conference(true);
        assert!(controls.is_conference());
        assert!(controls.heard_queue(1).unwrap().lock().unwrap().is_empty());
    }

    #[test]
    fn quality_report_reads_as_one_line() {
        let quality = MediaQuality {
            gaps: 2,
            lost: 5,
            reordered: 1,
            discarded: 0,
            reserve_ms: 160,
            mic_peak_percent: 12,
            mic_gain_percent: 420,
            remote: None,
        };
        assert_eq!(
            quality.to_string(),
            "gaps 2, lost packets 5, reordered 1, discarded 0, reserve 160 ms, \
             microphone peak 12%, gain x4.2"
        );
        // With the other side's report, its view follows.
        let reported = MediaQuality {
            remote: Some(RemoteSummary {
                lost_percent: 3,
                lost_total: 41,
                jitter_ms: 12,
                round_trip_ms: Some(85),
            }),
            ..quality
        };
        assert!(
            reported.to_string().ends_with(
                "gain x4.2; other side: lost 3% (41 in all), jitter 12 ms, round trip 85 ms"
            ),
            "{reported}"
        );
    }
}
