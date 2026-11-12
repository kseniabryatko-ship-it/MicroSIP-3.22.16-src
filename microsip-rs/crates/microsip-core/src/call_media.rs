//! Media side of a call, kept behind a trait so the SIP executor can be tested
//! without sound devices.

use crate::audio::{Microphone, Speaker};
use crate::rtp::PCMU_CLOCK_RATE;
use crate::rtp::session::{RtpSession, RtpSessionConfig};
use std::error::Error;
use std::net::{IpAddr, SocketAddr, UdpSocket};

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
}

impl std::fmt::Display for MediaQuality {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "gaps {}, lost packets {}, reordered {}, discarded {}, reserve {} ms",
            self.gaps, self.lost, self.reordered, self.discarded, self.reserve_ms
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
}

struct ActiveAudio {
    microphone: Microphone,
    speaker: Speaker,
    session: RtpSession,
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
        if self.active.is_some() {
            return Err("media is already running".into());
        }
        let socket = self
            .socket
            .take()
            .ok_or("media was not prepared before start")?;
        let microphone = Microphone::start()?;
        let speaker = Speaker::start()?;
        let config = RtpSessionConfig::new(remote, microphone.format().sample_rate);
        self.next_ssrc = self.next_ssrc.wrapping_add(0x0101_0101).max(1);
        let session = RtpSession::from_socket(socket, config, self.next_ssrc)?;
        self.active = Some(ActiveAudio {
            microphone,
            speaker,
            session,
        });
        Ok(())
    }

    fn pump(&mut self) -> Result<(), MediaError> {
        let Some(active) = self.active.as_ref() else {
            return Ok(());
        };
        while let Some(samples) = active.microphone.read_chunk()? {
            active.session.push_pcm(&samples)?;
        }
        while let Some(samples) = active.session.try_receive_decoded()? {
            active.speaker.play_mono(&samples, PCMU_CLOCK_RATE)?;
        }
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

    fn quality(&self) -> Option<MediaQuality> {
        let active = self.active.as_ref()?;
        let order = active.session.order_stats();
        Some(MediaQuality {
            gaps: active.speaker.underruns(),
            lost: order.lost,
            reordered: order.reordered,
            discarded: order.discarded,
            reserve_ms: active.speaker.largest_reserve_ms(),
        })
    }

    fn stop(&mut self) {
        self.socket = None;
        if let Some(mut active) = self.active.take() {
            // Errors while tearing down a finished call have nobody to act on them.
            let _ = active.session.shutdown();
        }
    }
}
