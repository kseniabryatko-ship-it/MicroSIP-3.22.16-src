use crate::rtp::order::{OrderStats, PacketOrderer, Released};
use crate::rtp::{PCMU_PAYLOAD_TYPE, PcmuStream, RtpError, RtpPacket, decode_pcmu};
use std::error::Error;
use std::fmt;
use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{
    Arc, Mutex,
    mpsc::{self, Receiver, SyncSender, TryRecvError},
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const SAMPLE_QUEUE_CAPACITY: usize = 256;
const DEFAULT_RECEIVE_TIMEOUT: Duration = Duration::from_millis(100);

#[derive(Debug)]
pub enum RtpSessionError {
    Io(io::Error),
    Rtp(RtpError),
    UnsupportedPayloadType(u8),
    InvalidPayloadLength { expected: usize, actual: usize },
    SampleChannelDisconnected,
    DecodedChannelDisconnected,
    WorkerPanicked(&'static str),
}

impl fmt::Display for RtpSessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => {
                write!(formatter, "RTP I/O error: {error}")
            }
            Self::Rtp(error) => {
                write!(formatter, "RTP error: {error}")
            }
            Self::UnsupportedPayloadType(payload_type) => {
                write!(formatter, "Unsupported RTP payload type: {payload_type}")
            }
            Self::InvalidPayloadLength { expected, actual } => {
                write!(
                    formatter,
                    "Invalid RTP payload length: {actual}, expected {expected}"
                )
            }
            Self::SampleChannelDisconnected => {
                formatter.write_str("RTP sample channel was disconnected")
            }
            Self::DecodedChannelDisconnected => {
                formatter.write_str("Decoded RTP channel was disconnected")
            }
            Self::WorkerPanicked(worker) => {
                write!(formatter, "RTP {worker} thread panicked")
            }
        }
    }
}

impl Error for RtpSessionError {}

impl From<io::Error> for RtpSessionError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<RtpError> for RtpSessionError {
    fn from(error: RtpError) -> Self {
        Self::Rtp(error)
    }
}

#[derive(Debug, Clone)]
pub struct RtpSessionConfig {
    remote_address: SocketAddr,
    expected_source: Option<SocketAddr>,
    source_sample_rate: u32,
    receive_timeout: Duration,
}

impl RtpSessionConfig {
    pub fn new(remote_address: SocketAddr, source_sample_rate: u32) -> Self {
        Self {
            remote_address,
            expected_source: Some(remote_address),
            source_sample_rate,
            receive_timeout: DEFAULT_RECEIVE_TIMEOUT,
        }
    }

    pub fn with_expected_source(mut self, expected_source: Option<SocketAddr>) -> Self {
        self.expected_source = expected_source;
        self
    }

    pub fn with_receive_timeout(mut self, receive_timeout: Duration) -> Self {
        self.receive_timeout = receive_timeout;
        self
    }

    pub fn remote_address(&self) -> SocketAddr {
        self.remote_address
    }

    pub fn source_sample_rate(&self) -> u32 {
        self.source_sample_rate
    }
}

pub struct RtpSession {
    socket: UdpSocket,
    remote_address: SocketAddr,
    sample_sender: Option<SyncSender<Vec<f32>>>,
    decoded_samples: Receiver<Vec<f32>>,
    stop: Arc<AtomicBool>,
    sent_packets: Arc<AtomicU64>,
    received_packets: Arc<AtomicU64>,
    order_stats: Arc<Mutex<OrderStats>>,
    sender_thread: Option<JoinHandle<Result<(), RtpSessionError>>>,
    receiver_thread: Option<JoinHandle<Result<(), RtpSessionError>>>,
}

impl RtpSession {
    pub fn bind(
        local_address: SocketAddr,
        config: RtpSessionConfig,
        ssrc: u32,
    ) -> Result<Self, RtpSessionError> {
        let socket = UdpSocket::bind(local_address)?;

        Self::from_socket(socket, config, ssrc)
    }

    pub fn from_socket(
        socket: UdpSocket,
        config: RtpSessionConfig,
        ssrc: u32,
    ) -> Result<Self, RtpSessionError> {
        let RtpSessionConfig {
            remote_address,
            expected_source,
            source_sample_rate,
            receive_timeout,
        } = config;

        let pcmu_stream = PcmuStream::new(ssrc, source_sample_rate)?;

        let (sample_sender, sample_receiver) =
            mpsc::sync_channel::<Vec<f32>>(SAMPLE_QUEUE_CAPACITY);

        let sender_socket = socket.try_clone()?;
        let sent_packets = Arc::new(AtomicU64::new(0));
        let sender_counter = Arc::clone(&sent_packets);

        let sender_thread = thread::spawn(move || {
            run_sender(
                sender_socket,
                sample_receiver,
                pcmu_stream,
                remote_address,
                sender_counter,
            )
        });

        let receiver_socket = socket.try_clone()?;
        receiver_socket.set_read_timeout(Some(receive_timeout))?;

        let (decoded_sender, decoded_receiver) = mpsc::channel::<Vec<f32>>();

        let stop = Arc::new(AtomicBool::new(false));
        let receiver_stop = Arc::clone(&stop);
        let received_packets = Arc::new(AtomicU64::new(0));
        let receiver_counter = Arc::clone(&received_packets);
        let order_stats = Arc::new(Mutex::new(OrderStats::default()));
        let receiver_order_stats = Arc::clone(&order_stats);

        let receiver_thread = thread::spawn(move || {
            run_receiver(
                receiver_socket,
                expected_source,
                decoded_sender,
                receiver_stop,
                receiver_counter,
                receiver_order_stats,
            )
        });

        Ok(Self {
            socket,
            remote_address,
            sample_sender: Some(sample_sender),
            decoded_samples: decoded_receiver,
            stop,
            sent_packets,
            received_packets,
            order_stats,
            sender_thread: Some(sender_thread),
            receiver_thread: Some(receiver_thread),
        })
    }

    pub fn local_address(&self) -> Result<SocketAddr, RtpSessionError> {
        Ok(self.socket.local_addr()?)
    }

    pub fn remote_address(&self) -> SocketAddr {
        self.remote_address
    }

    pub fn push_pcm(&self, samples: &[f32]) -> Result<(), RtpSessionError> {
        let sender = self
            .sample_sender
            .as_ref()
            .ok_or(RtpSessionError::SampleChannelDisconnected)?;

        sender
            .send(samples.to_vec())
            .map_err(|_| RtpSessionError::SampleChannelDisconnected)
    }

    pub fn try_receive_decoded(&self) -> Result<Option<Vec<f32>>, RtpSessionError> {
        match self.decoded_samples.try_recv() {
            Ok(samples) => Ok(Some(samples)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(RtpSessionError::DecodedChannelDisconnected),
        }
    }

    pub fn sent_packets(&self) -> u64 {
        self.sent_packets.load(Ordering::Acquire)
    }

    pub fn received_packets(&self) -> u64 {
        self.received_packets.load(Ordering::Acquire)
    }

    /// Lost, reordered and discarded packets of the received stream.
    pub fn order_stats(&self) -> OrderStats {
        self.order_stats
            .lock()
            .map(|stats| *stats)
            .unwrap_or_default()
    }

    pub fn shutdown(&mut self) -> Result<(), RtpSessionError> {
        drop(self.sample_sender.take());

        let sender_result = if let Some(handle) = self.sender_thread.take() {
            match handle.join() {
                Ok(result) => result,
                Err(_) => Err(RtpSessionError::WorkerPanicked("sender")),
            }
        } else {
            Ok(())
        };

        self.stop.store(true, Ordering::Release);

        let receiver_result = if let Some(handle) = self.receiver_thread.take() {
            match handle.join() {
                Ok(result) => result,
                Err(_) => Err(RtpSessionError::WorkerPanicked("receiver")),
            }
        } else {
            Ok(())
        };

        sender_result?;
        receiver_result?;

        Ok(())
    }
}

impl Drop for RtpSession {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

fn run_sender(
    socket: UdpSocket,
    samples: Receiver<Vec<f32>>,
    mut pcmu_stream: PcmuStream,
    remote_address: SocketAddr,
    sent_packets: Arc<AtomicU64>,
) -> Result<(), RtpSessionError> {
    while let Ok(samples) = samples.recv() {
        let packets = pcmu_stream.push_pcm(&samples);

        for packet in packets {
            let bytes = packet.serialize()?;

            socket.send_to(&bytes, remote_address)?;

            sent_packets.fetch_add(1, Ordering::Relaxed);
        }
    }

    Ok(())
}

fn run_receiver(
    socket: UdpSocket,
    expected_source: Option<SocketAddr>,
    decoded_sender: mpsc::Sender<Vec<f32>>,
    stop: Arc<AtomicBool>,
    received_packets: Arc<AtomicU64>,
    order_stats: Arc<Mutex<OrderStats>>,
) -> Result<(), RtpSessionError> {
    let mut buffer = [0_u8; 65_535];
    let mut orderer = PacketOrderer::<Vec<f32>>::default();
    // The last packet played, for covering a lost one.
    let mut previous: Option<Vec<f32>> = None;

    while !stop.load(Ordering::Acquire) {
        match socket.recv_from(&mut buffer) {
            Ok((length, source)) => {
                let source_allowed = expected_source.is_none_or(|expected| source == expected);

                if !source_allowed {
                    continue;
                }

                // One odd datagram must not end the audio of the whole call:
                // anything that is not a PCMU voice packet is skipped. Servers
                // send DTMF events (payload 101) and comfort noise (13) on the
                // same port, and may use 10 or 30 ms packets instead of 20.
                let Ok(packet) = RtpPacket::parse(&buffer[..length]) else {
                    continue;
                };

                if packet.payload_type != PCMU_PAYLOAD_TYPE || packet.payload.is_empty() {
                    continue;
                }

                received_packets.fetch_add(1, Ordering::Relaxed);

                let samples = decode_pcmu(&packet.payload);
                for released in orderer.push(packet.ssrc, packet.sequence_number, samples) {
                    let samples = match released {
                        Released::Packet(samples) => {
                            previous = Some(samples.clone());
                            samples
                        }
                        // Cover one lost packet with a quieter copy of the
                        // packet before it; a longer loss becomes silence.
                        Released::Lost => match previous.take() {
                            Some(last) => last.iter().map(|sample| sample * 0.5).collect(),
                            None => continue,
                        },
                    };
                    decoded_sender
                        .send(samples)
                        .map_err(|_| RtpSessionError::DecodedChannelDisconnected)?;
                }
                if let Ok(mut stats) = order_stats.lock() {
                    *stats = orderer.stats();
                }
            }

            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                continue;
            }

            Err(error) => {
                return Err(error.into());
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rtp::PCMU_SAMPLES_PER_PACKET;

    #[test]
    fn transfers_pcmu_between_sessions() {
        let receiver_config = RtpSessionConfig::new(
            "127.0.0.1:0".parse::<SocketAddr>().expect("valid address"),
            48_000,
        )
        .with_expected_source(None);

        let mut receiver = RtpSession::bind(
            "127.0.0.1:0".parse::<SocketAddr>().expect("valid address"),
            receiver_config,
            0x1111_1111,
        )
        .expect("receiver session should start");

        let receiver_address = receiver
            .local_address()
            .expect("receiver address should exist");

        let sender_config = RtpSessionConfig::new(receiver_address, 48_000);

        let mut sender = RtpSession::bind(
            "127.0.0.1:0".parse::<SocketAddr>().expect("valid address"),
            sender_config,
            0x2222_2222,
        )
        .expect("sender session should start");

        sender.push_pcm(&[0.25; 960]).expect("PCM should be queued");

        let decoded = wait_for_decoded(&receiver);
        let sent = wait_for_counter(&sender, |count| count >= 1);

        let sender_shutdown = sender.shutdown();
        let receiver_shutdown = receiver.shutdown();
        let received = receiver.received_packets();

        assert!(decoded.is_some());

        assert_eq!(
            decoded.expect("decoded samples should exist").len(),
            PCMU_SAMPLES_PER_PACKET
        );

        assert_eq!(sent, 1);
        assert_eq!(received, 1);

        sender_shutdown.expect("sender should stop cleanly");

        receiver_shutdown.expect("receiver should stop cleanly");
    }

    #[test]
    fn foreign_packets_are_skipped_and_other_packet_sizes_are_played() {
        let config = RtpSessionConfig::new(
            "127.0.0.1:0".parse::<SocketAddr>().expect("valid address"),
            48_000,
        )
        .with_expected_source(None);
        let mut receiver = RtpSession::bind(
            "127.0.0.1:0".parse::<SocketAddr>().expect("valid address"),
            config,
            0x5555_5555,
        )
        .expect("receiver session should start");
        let target = receiver.local_address().expect("address should exist");
        let peer = UdpSocket::bind("127.0.0.1:0").expect("peer should bind");

        let packet = |payload_type: u8, sequence_number: u16, payload: Vec<u8>| {
            RtpPacket {
                marker: false,
                payload_type,
                sequence_number,
                timestamp: u32::from(sequence_number) * 160,
                ssrc: 7,
                payload,
            }
            .serialize()
            .expect("packet should serialize")
        };

        // Not RTP at all, a DTMF event, comfort noise, an empty voice packet.
        peer.send_to(b"junk", target).expect("send");
        peer.send_to(&packet(101, 1, vec![1, 0, 0, 160]), target)
            .expect("send");
        peer.send_to(&packet(13, 2, vec![40]), target)
            .expect("send");
        peer.send_to(&packet(PCMU_PAYLOAD_TYPE, 3, Vec::new()), target)
            .expect("send");
        // Voice in 10 ms and 30 ms packets.
        peer.send_to(&packet(PCMU_PAYLOAD_TYPE, 4, vec![0xFF; 80]), target)
            .expect("send");
        peer.send_to(&packet(PCMU_PAYLOAD_TYPE, 5, vec![0xFF; 240]), target)
            .expect("send");

        let first = wait_for_decoded(&receiver).expect("10 ms packet should be decoded");
        let second = wait_for_decoded(&receiver).expect("30 ms packet should be decoded");
        assert_eq!(first.len(), 80);
        assert_eq!(second.len(), 240);
        assert_eq!(receiver.received_packets(), 2);
        receiver.shutdown().expect("receiver should stop cleanly");
    }

    #[test]
    fn swapped_duplicated_and_lost_packets_are_repaired() {
        let config = RtpSessionConfig::new(
            "127.0.0.1:0".parse::<SocketAddr>().expect("valid address"),
            48_000,
        )
        .with_expected_source(None);
        let mut receiver = RtpSession::bind(
            "127.0.0.1:0".parse::<SocketAddr>().expect("valid address"),
            config,
            0x6666_6666,
        )
        .expect("receiver session should start");
        let target = receiver.local_address().expect("address should exist");
        let peer = UdpSocket::bind("127.0.0.1:0").expect("peer should bind");

        // Every packet carries its own constant level, so the order of the
        // decoded audio shows the order of play.
        let levels = [0.1_f32, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8];
        let send = |sequence_number: u16| {
            let payload = crate::rtp::encode_pcmu(&[levels[usize::from(sequence_number)]; 160]);
            let bytes = RtpPacket {
                marker: false,
                payload_type: PCMU_PAYLOAD_TYPE,
                sequence_number,
                timestamp: u32::from(sequence_number) * 160,
                ssrc: 9,
                payload,
            }
            .serialize()
            .expect("packet should serialize");
            peer.send_to(&bytes, target).expect("send");
            // Keep the arrival order of these datagrams.
            thread::sleep(Duration::from_millis(5));
        };
        // 2 and 1 swapped, 3 twice, 4 lost.
        for sequence_number in [0, 2, 1, 3, 3, 5, 6, 7] {
            send(sequence_number);
        }

        let mut played = Vec::new();
        while let Some(samples) = wait_for_decoded(&receiver) {
            assert_eq!(samples.len(), 160);
            played.push(samples[0]);
            if played.len() == 8 {
                break;
            }
        }
        let near = |value: f32, expected: f32| (value - expected).abs() < 0.03;
        assert_eq!(played.len(), 8, "{played:?}");
        for (index, expected) in [0.1, 0.2, 0.3, 0.4].into_iter().enumerate() {
            assert!(near(played[index], expected), "{played:?}");
        }
        // The lost packet is covered by a half-level copy of the one before.
        assert!(near(played[4], 0.2), "{played:?}");
        for (index, expected) in [0.6, 0.7, 0.8].into_iter().enumerate() {
            assert!(near(played[5 + index], expected), "{played:?}");
        }

        let stats = receiver.order_stats();
        assert_eq!(stats.lost, 1);
        assert_eq!(stats.discarded, 1);
        receiver.shutdown().expect("receiver should stop cleanly");
    }

    #[test]
    fn accepts_prebound_socket() {
        let socket = UdpSocket::bind("127.0.0.1:0".parse::<SocketAddr>().expect("valid address"))
            .expect("socket should bind");

        let expected_address = socket.local_addr().expect("socket address should exist");

        let config = RtpSessionConfig::new(expected_address, 48_000);

        let mut session =
            RtpSession::from_socket(socket, config, 0x3333_3333).expect("session should start");

        assert_eq!(
            session
                .local_address()
                .expect("session address should exist"),
            expected_address
        );

        session.shutdown().expect("session should stop cleanly");
    }

    #[test]
    fn shutdown_is_idempotent() {
        let config = RtpSessionConfig::new(
            "127.0.0.1:0".parse::<SocketAddr>().expect("valid address"),
            48_000,
        );

        let mut session = RtpSession::bind(
            "127.0.0.1:0".parse::<SocketAddr>().expect("valid address"),
            config,
            0x4444_4444,
        )
        .expect("session should start");

        session.shutdown().expect("first shutdown should succeed");

        session.shutdown().expect("second shutdown should succeed");
    }

    fn wait_for_decoded(session: &RtpSession) -> Option<Vec<f32>> {
        for _ in 0..100 {
            if let Ok(Some(samples)) = session.try_receive_decoded() {
                return Some(samples);
            }

            thread::sleep(Duration::from_millis(10));
        }

        None
    }

    fn wait_for_counter<F>(session: &RtpSession, predicate: F) -> u64
    where
        F: Fn(u64) -> bool,
    {
        for _ in 0..100 {
            let count = session.sent_packets();

            if predicate(count) {
                return count;
            }

            thread::sleep(Duration::from_millis(10));
        }

        session.sent_packets()
    }
}
