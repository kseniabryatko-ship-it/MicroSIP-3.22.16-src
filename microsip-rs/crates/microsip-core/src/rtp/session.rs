use crate::codec::{Codec, VoiceDecoder};
use crate::resample::Resampler;
use crate::rtcp::{self, Reception, RemoteQuality, SenderInfo};
use crate::rtp::order::{OrderStats, PacketOrderer, Released};
use crate::rtp::{PcmuStream, RtpError, RtpPacket};
use crate::srtp::{SrtpKeys, SrtpReceiver, SrtpSender};
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
use std::time::{Duration, Instant};

const SAMPLE_QUEUE_CAPACITY: usize = 256;

/// Receives decoded audio on the receiver thread; see
/// [`RtpSession::from_socket_with_sink`].
pub type DecodedSink = Box<dyn FnMut(Vec<f32>) + Send>;
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
    dtmf_payload_type: Option<u8>,
    codec: Codec,
    payload_type: Option<u8>,
    receive_types: Vec<(u8, Codec)>,
    srtp: Option<SrtpKeys>,
    rtcp: Option<RtcpConfig>,
}

impl RtpSessionConfig {
    pub fn new(remote_address: SocketAddr, source_sample_rate: u32) -> Self {
        Self {
            remote_address,
            expected_source: Some(remote_address),
            source_sample_rate,
            receive_timeout: DEFAULT_RECEIVE_TIMEOUT,
            dtmf_payload_type: None,
            codec: Codec::Pcmu,
            payload_type: None,
            receive_types: Vec::new(),
            srtp: None,
            rtcp: None,
        }
    }

    pub fn with_expected_source(mut self, expected_source: Option<SocketAddr>) -> Self {
        self.expected_source = expected_source;
        self
    }

    /// The telephone-event payload type agreed in SDP. Without it digits
    /// are sent as tones inside the voice.
    pub fn with_dtmf_payload_type(mut self, payload_type: Option<u8>) -> Self {
        self.dtmf_payload_type = payload_type;
        self
    }

    /// The codec agreed in SDP for the voice this session sends. Decoded
    /// sound comes out at this codec's sample rate. Received voice is
    /// decoded by its own payload type: this codec's, any given with
    /// [`RtpSessionConfig::with_receive_types`], and the fixed numbers of
    /// G.711, G.722 and G.729.
    pub fn with_codec(mut self, codec: Codec) -> Self {
        self.codec = codec;
        self
    }

    /// The payload type agreed for the codec, where it is not the codec's
    /// usual one (Opus has no fixed number).
    pub fn with_payload_type(mut self, payload_type: u8) -> Self {
        self.payload_type = Some(payload_type);
        self
    }

    /// Payload types the other side may send besides the agreed one, as
    /// listed in its SDP.
    pub fn with_receive_types(mut self, types: Vec<(u8, Codec)>) -> Self {
        self.receive_types = types;
        self
    }

    /// Encrypts and signs the packets both ways (SRTP) with the keys
    /// agreed in SDP. Received packets that fail the check are dropped.
    /// Sends and reads RTCP reports on this socket. Left out for an
    /// encrypted call: its reports would have to be encrypted too (SRTCP),
    /// which this phone does not do.
    pub fn with_rtcp(mut self, rtcp: Option<RtcpConfig>) -> Self {
        self.rtcp = rtcp;
        self
    }

    pub fn with_srtp(mut self, keys: Option<SrtpKeys>) -> Self {
        self.srtp = keys;
        self
    }

    /// Rate of the sound this session hands out.
    pub fn decoded_sample_rate(&self) -> u32 {
        self.codec.sample_rate()
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
    control: Arc<SenderControl>,
    sender_thread: Option<JoinHandle<Result<(), RtpSessionError>>>,
    receiver_thread: Option<JoinHandle<Result<(), RtpSessionError>>>,
    rtcp_state: SharedRtcp,
    rtcp_thread: Option<JoinHandle<()>>,
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
        Self::from_socket_with_sink(socket, config, ssrc, None)
    }

    /// Like [`RtpSession::from_socket`], but decoded audio is handed to
    /// `sink` on the receiver thread the moment it is ready, instead of
    /// waiting to be fetched with [`RtpSession::try_receive_decoded`].
    pub fn from_socket_with_sink(
        socket: UdpSocket,
        config: RtpSessionConfig,
        ssrc: u32,
        sink: Option<DecodedSink>,
    ) -> Result<Self, RtpSessionError> {
        let RtpSessionConfig {
            remote_address,
            expected_source,
            source_sample_rate,
            receive_timeout,
            dtmf_payload_type,
            codec,
            payload_type,
            receive_types,
            srtp,
            rtcp,
        } = config;
        let rtcp = rtcp.filter(|_| srtp.is_none());
        let rtcp_state = SharedRtcp::default();

        let payload_type = payload_type.unwrap_or(codec.payload_type());
        let mut pcmu_stream = PcmuStream::new(ssrc, source_sample_rate)?;
        pcmu_stream.set_codec(codec);
        pcmu_stream.set_payload_type(payload_type);
        pcmu_stream.set_dtmf_payload_type(dtmf_payload_type);
        let mut receiving = Receiving::new(codec, payload_type, receive_types);
        receiving.srtp = srtp
            .as_ref()
            .map(|keys| SrtpReceiver::new(&keys.remote, keys.suite));
        receiving.rtcp = Some((Arc::clone(&rtcp_state), Instant::now()));
        let outgoing = Outgoing {
            rtcp: Arc::clone(&rtcp_state),
            stream: pcmu_stream,
            srtp: srtp
                .as_ref()
                .map(|keys| SrtpSender::new(&keys.local, keys.suite)),
        };
        let control = Arc::new(SenderControl::default());
        let sender_control = Arc::clone(&control);

        let (sample_sender, sample_receiver) =
            mpsc::sync_channel::<Vec<f32>>(SAMPLE_QUEUE_CAPACITY);

        let sender_socket = socket.try_clone()?;
        let sent_packets = Arc::new(AtomicU64::new(0));
        let sender_counter = Arc::clone(&sent_packets);

        let stop = Arc::new(AtomicBool::new(false));
        let sender_stop = Arc::clone(&stop);

        let sender_thread = thread::spawn(move || {
            run_sender(
                sender_socket,
                sample_receiver,
                outgoing,
                remote_address,
                sender_counter,
                sender_stop,
                sender_control,
            )
        });

        let receiver_socket = socket.try_clone()?;
        receiver_socket.set_read_timeout(Some(receive_timeout))?;

        let (decoded_sender, decoded_receiver) = mpsc::channel::<Vec<f32>>();

        let receiver_stop = Arc::clone(&stop);
        let received_packets = Arc::new(AtomicU64::new(0));
        let receiver_counter = Arc::clone(&received_packets);
        let order_stats = Arc::new(Mutex::new(OrderStats::default()));
        let receiver_order_stats = Arc::clone(&order_stats);

        let receiver_thread = thread::spawn(move || {
            run_receiver(
                receiver_socket,
                expected_source,
                receiver_stop,
                receiver_counter,
                receiver_order_stats,
                match sink {
                    Some(sink) => Output::Sink(sink),
                    None => Output::Channel(decoded_sender),
                },
                receiving,
            )
        });

        let rtcp_thread = rtcp.map(|config| {
            let state = Arc::clone(&rtcp_state);
            let stop = Arc::clone(&stop);
            let clock_rate = codec.clock_rate();
            let cname = format!("microsip-rs-{ssrc:08x}");
            thread::spawn(move || run_rtcp(config, state, stop, ssrc, clock_rate, cname))
        });

        Ok(Self {
            rtcp_state,
            rtcp_thread,
            socket,
            remote_address,
            sample_sender: Some(sample_sender),
            decoded_samples: decoded_receiver,
            stop,
            sent_packets,
            received_packets,
            order_stats,
            control,
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

    /// Sends a keypad digit to the other side. Returns false when the
    /// character is not on a keypad.
    pub fn send_dtmf(&self, digit: char) -> bool {
        if crate::rtp::dtmf::event_code(digit).is_none() {
            return false;
        }
        match self.control.digits.lock() {
            Ok(mut digits) => {
                digits.push(digit);
                true
            }
            Err(_) => false,
        }
    }

    /// While muted, silence is sent instead of the microphone. Digits are
    /// still sent.
    pub fn set_muted(&self, muted: bool) {
        self.control.muted.store(muted, Ordering::Release);
    }

    /// A second input for microphone samples that another thread may own,
    /// for example the sound card's callback. Use `try_send` there so the
    /// audio thread never waits.
    pub fn pcm_input(&self) -> Option<SyncSender<Vec<f32>>> {
        self.sample_sender.clone()
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
    /// What the other side last reported about this phone's audio; `None`
    /// before its first report, or without RTCP.
    pub fn remote_quality(&self) -> Option<RemoteQuality> {
        self.rtcp_state.lock().ok()?.remote
    }

    pub fn order_stats(&self) -> OrderStats {
        self.order_stats
            .lock()
            .map(|stats| *stats)
            .unwrap_or_default()
    }

    pub fn shutdown(&mut self) -> Result<(), RtpSessionError> {
        drop(self.sample_sender.take());
        // Copies of the input handed out by `pcm_input` may still exist, so
        // the sender is told to stop instead of waiting for them to go away.
        self.stop.store(true, Ordering::Release);

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

        if let Some(handle) = self.rtcp_thread.take() {
            let _ = handle.join();
        }

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

/// Requests from the session's owner to its sender thread.
#[derive(Default)]
struct SenderControl {
    muted: AtomicBool,
    digits: Mutex<Vec<char>>,
}

/// The sending side: sound into packets, and the packets encrypted when
/// the call uses SRTP.
struct Outgoing {
    /// Counts for the sender reports.
    rtcp: SharedRtcp,
    stream: PcmuStream,
    srtp: Option<SrtpSender>,
}

fn run_sender(
    socket: UdpSocket,
    samples: Receiver<Vec<f32>>,
    mut outgoing: Outgoing,
    remote_address: SocketAddr,
    sent_packets: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    control: Arc<SenderControl>,
) -> Result<(), RtpSessionError> {
    loop {
        let mut samples = match samples.recv_timeout(Duration::from_millis(50)) {
            Ok(samples) => samples,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if stop.load(Ordering::Acquire) {
                    break;
                }
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        if let Ok(mut digits) = control.digits.lock() {
            for digit in digits.drain(..) {
                outgoing.stream.queue_dtmf(digit);
            }
        }
        if control.muted.load(Ordering::Acquire) {
            samples.fill(0.0);
        }
        let packets = outgoing.stream.push_pcm(&samples);

        for packet in packets {
            let mut bytes = packet.serialize()?;
            if let Some(srtp) = outgoing.srtp.as_mut() {
                // A packet that cannot be protected is not sent in the clear.
                match srtp.protect(&bytes) {
                    Some(protected) => bytes = protected,
                    None => continue,
                }
            }

            socket.send_to(&bytes, remote_address)?;
            if let Ok(mut state) = outgoing.rtcp.lock() {
                state.packets_sent = state.packets_sent.wrapping_add(1);
                let payload = packet.payload.len() as u32;
                state.octets_sent = state.octets_sent.wrapping_add(payload);
                state.last_sent = Some((packet.timestamp, Instant::now()));
            }

            sent_packets.fetch_add(1, Ordering::Relaxed);
        }
    }

    Ok(())
}

/// Where the receiver hands the decoded sound.
enum Output {
    /// Kept until fetched with [`RtpSession::try_receive_decoded`].
    Channel(mpsc::Sender<Vec<f32>>),
    /// Handed over at once, on the receiver thread.
    Sink(DecodedSink),
}

fn run_receiver(
    socket: UdpSocket,
    expected_source: Option<SocketAddr>,
    stop: Arc<AtomicBool>,
    received_packets: Arc<AtomicU64>,
    order_stats: Arc<Mutex<OrderStats>>,
    mut output: Output,
    mut receiving: Receiving,
) -> Result<(), RtpSessionError> {
    let mut buffer = [0_u8; 65_535];
    // Packets wait here still encoded: G.722, G.729 and Opus must be
    // decoded in the order they were sent.
    let mut orderer = PacketOrderer::<(u8, Vec<u8>)>::default();

    while !stop.load(Ordering::Acquire) {
        match socket.recv_from(&mut buffer) {
            Ok((length, source)) => {
                let source_allowed = expected_source.is_none_or(|expected| source == expected);

                if !source_allowed {
                    continue;
                }

                // One odd datagram must not end the audio of the whole call:
                // anything that is not a voice packet of a known codec is
                // skipped. Servers send DTMF events (payload 101) and comfort
                // noise (13) on the same port, and may use 10 or 30 ms
                // packets instead of 20.
                // With SRTP, what does not carry the other side's signature
                // is not listened to.
                let plain;
                let datagram = match receiving.srtp.as_mut() {
                    Some(srtp) => match srtp.unprotect(&buffer[..length]) {
                        Some(unprotected) => {
                            plain = unprotected;
                            &plain[..]
                        }
                        None => continue,
                    },
                    None => &buffer[..length],
                };
                let Ok(packet) = RtpPacket::parse(datagram) else {
                    continue;
                };

                if !receiving.knows(packet.payload_type) || packet.payload.is_empty() {
                    continue;
                }

                received_packets.fetch_add(1, Ordering::Relaxed);
                receiving.count(&packet);

                let encoded = (packet.payload_type, packet.payload);
                for released in orderer.push(packet.ssrc, packet.sequence_number, encoded) {
                    let samples = match released {
                        Released::Packet((payload_type, payload)) => {
                            receiving.decode(payload_type, &payload)
                        }
                        Released::Lost => receiving.lost(),
                    };
                    let Some(samples) = samples.filter(|samples| !samples.is_empty()) else {
                        continue;
                    };
                    match &mut output {
                        Output::Sink(sink) => sink(samples),
                        Output::Channel(sender) => sender
                            .send(samples)
                            .map_err(|_| RtpSessionError::DecodedChannelDisconnected)?,
                    }
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

/// One codec being received: its payload type, its decoder, and a
/// converter when its sound is not at the session's rate.
struct Decoding {
    payload_type: u8,
    decoder: VoiceDecoder,
    resampler: Option<Resampler>,
}

/// The receiving side's codecs. Each payload type gets its decoder when
/// its first packet arrives; all sound leaves at one sample rate.
struct Receiving {
    types: Vec<(u8, Codec)>,
    rate: u32,
    decoders: Vec<Decoding>,
    /// Payload type of the packet decoded last.
    last: Option<u8>,
    /// The sound of that packet, for covering one lost packet where the
    /// codec cannot do it itself.
    previous: Option<Vec<f32>>,
    /// Checks and decrypts the packets when the call uses SRTP.
    srtp: Option<SrtpReceiver>,
    /// Counts for the receiver reports, and the start of the session.
    rtcp: Option<(SharedRtcp, Instant)>,
}

impl Receiving {
    fn new(codec: Codec, payload_type: u8, listed: Vec<(u8, Codec)>) -> Self {
        let mut types = vec![(payload_type, codec)];
        let fixed = Codec::PREFERRED
            .into_iter()
            .filter(|codec| codec.has_fixed_payload_type())
            .map(|codec| (codec.payload_type(), codec));
        // What was agreed comes first, then what the other side listed,
        // then the numbers the standard fixes.
        for entry in listed.into_iter().chain(fixed) {
            if !types.iter().any(|(known, _)| *known == entry.0) {
                types.push(entry);
            }
        }
        Self {
            types,
            rate: codec.sample_rate(),
            decoders: Vec::new(),
            last: None,
            previous: None,
            srtp: None,
            rtcp: None,
        }
    }

    /// Counts a received voice packet for the receiver reports.
    fn count(&self, packet: &RtpPacket) {
        let Some((state, started)) = self.rtcp.as_ref() else {
            return;
        };
        let clock = self
            .types
            .iter()
            .find(|(known, _)| *known == packet.payload_type)
            .map_or(8_000, |(_, codec)| codec.clock_rate());
        // Arrival in units of the packet's own clock.
        let arrival = (started.elapsed().as_secs_f64() * f64::from(clock)) as u64 as u32;
        if let Ok(mut state) = state.lock() {
            state.reception.on_packet(
                packet.ssrc,
                packet.sequence_number,
                packet.timestamp,
                arrival,
            );
        }
    }

    fn knows(&self, payload_type: u8) -> bool {
        self.types.iter().any(|(known, _)| *known == payload_type)
    }

    fn decoding(&mut self, payload_type: u8) -> Option<&mut Decoding> {
        if let Some(index) = self
            .decoders
            .iter()
            .position(|decoding| decoding.payload_type == payload_type)
        {
            return self.decoders.get_mut(index);
        }
        let codec = self
            .types
            .iter()
            .find(|(known, _)| *known == payload_type)
            .map(|(_, codec)| *codec)?;
        let resampler = if codec.sample_rate() == self.rate {
            None
        } else {
            Some(Resampler::new(codec.sample_rate(), self.rate)?)
        };
        self.decoders.push(Decoding {
            payload_type,
            decoder: VoiceDecoder::new(codec),
            resampler,
        });
        self.decoders.last_mut()
    }

    fn decode(&mut self, payload_type: u8, payload: &[u8]) -> Option<Vec<f32>> {
        let decoding = self.decoding(payload_type)?;
        let mut samples = decoding.decoder.decode(payload);
        if let Some(resampler) = decoding.resampler.as_mut() {
            samples = resampler.push(&samples);
        }
        self.last = Some(payload_type);
        self.previous = Some(samples.clone());
        Some(samples)
    }

    /// Sound for the place of one lost packet. A codec that keeps state
    /// makes it up from what came before; for G.711 it is a quieter copy
    /// of the packet before, once, and a longer loss becomes silence.
    fn lost(&mut self) -> Option<Vec<f32>> {
        let payload_type = self.last?;
        let decoding = self.decoding(payload_type)?;
        let frame = decoding.decoder.codec().frame_samples();
        if let Some(mut made) = decoding.decoder.conceal(frame) {
            if let Some(resampler) = decoding.resampler.as_mut() {
                made = resampler.push(&made);
            }
            return Some(made);
        }
        let last = self.previous.take()?;
        Some(last.iter().map(|sample| sample * 0.5).collect())
    }
}

/// Where RTCP goes for a session, and how often.
#[derive(Debug, Clone)]
pub struct RtcpConfig {
    /// This phone's RTCP socket (usually the RTP port plus one).
    pub socket: Arc<UdpSocket>,
    /// The other side's RTCP address.
    pub remote: SocketAddr,
    /// Time between two reports; RFC 3550 suggests about five seconds.
    pub interval: Duration,
}

/// What the RTP threads and the RTCP thread share.
#[derive(Debug, Default)]
struct RtcpState {
    reception: Reception,
    packets_sent: u32,
    octets_sent: u32,
    /// RTP timestamp of the last packet sent, and when it went.
    last_sent: Option<(u32, Instant)>,
    /// The other side's last sender report: its NTP middle, and when it
    /// arrived.
    last_sender_report: Option<(u32, Instant)>,
    /// What the other side last said about this phone's audio.
    remote: Option<RemoteQuality>,
}

type SharedRtcp = Arc<Mutex<RtcpState>>;

/// Sends a report every interval and reads the other side's, until the
/// session stops; then says goodbye.
#[allow(clippy::too_many_arguments)]
fn run_rtcp(
    config: RtcpConfig,
    state: SharedRtcp,
    stop: Arc<AtomicBool>,
    ssrc: u32,
    clock_rate: u32,
    cname: String,
) {
    let socket = config.socket;
    let _ = socket.set_read_timeout(Some(Duration::from_millis(50)));
    let mut buffer = [0_u8; 1_500];
    // The first report soon, so that the other side learns the name early.
    let mut next = Instant::now() + config.interval.min(Duration::from_secs(1));
    while !stop.load(Ordering::Acquire) {
        if let Ok((length, _)) = socket.recv_from(&mut buffer)
            && let Some(report) = rtcp::parse(&buffer[..length])
            && let Ok(mut state) = state.lock()
        {
            if let Some(sender) = report.sender {
                state.last_sender_report = Some((rtcp::ntp_middle(sender.ntp), Instant::now()));
            }
            if let Some(block) = report.blocks.iter().find(|block| block.ssrc == ssrc) {
                state.remote = Some(RemoteQuality::from_block(
                    block,
                    clock_rate,
                    rtcp::ntp_now(),
                ));
            }
        }
        if Instant::now() < next {
            continue;
        }
        next = Instant::now() + config.interval;
        let bytes = match state.lock() {
            Ok(mut state) => {
                let sender = state.last_sent.map(|(timestamp, at)| {
                    // The RTP time that goes with now.
                    let elapsed = at.elapsed().as_secs_f64() * f64::from(clock_rate);
                    SenderInfo {
                        ntp: rtcp::ntp_now(),
                        rtp_timestamp: timestamp.wrapping_add(elapsed as u32),
                        packets: state.packets_sent,
                        octets: state.octets_sent,
                    }
                });
                let (last_report, delay) = match state.last_sender_report {
                    Some((middle, at)) => (middle, (at.elapsed().as_secs_f64() * 65_536.0) as u32),
                    None => (0, 0),
                };
                let block = state.reception.report(last_report, delay);
                rtcp::build_report(ssrc, sender, block, &cname)
            }
            Err(_) => continue,
        };
        let _ = socket.send_to(&bytes, config.remote);
    }
    let _ = socket.send_to(&rtcp::build_bye(ssrc), config.remote);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rtp::{PCMU_PAYLOAD_TYPE, PCMU_SAMPLES_PER_PACKET, decode_pcmu};

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
    fn sink_and_second_input_carry_audio_without_polling() {
        let (heard_sender, heard) = mpsc::channel::<usize>();
        let config = RtpSessionConfig::new(
            "127.0.0.1:0".parse::<SocketAddr>().expect("valid address"),
            48_000,
        )
        .with_expected_source(None);
        let receiver_socket = UdpSocket::bind("127.0.0.1:0").expect("socket should bind");
        let mut receiver = RtpSession::from_socket_with_sink(
            receiver_socket,
            config,
            0x7777_7777,
            Some(Box::new(move |samples| {
                let _ = heard_sender.send(samples.len());
            })),
        )
        .expect("receiver session should start");
        let target = receiver.local_address().expect("address should exist");

        let mut sender = RtpSession::bind(
            "127.0.0.1:0".parse::<SocketAddr>().expect("valid address"),
            RtpSessionConfig::new(target, 48_000),
            0x8888_8888,
        )
        .expect("sender session should start");

        // The way a sound card callback feeds the session.
        let input = sender.pcm_input().expect("input should exist");
        input.try_send(vec![0.25; 960]).expect("queue has room");

        let length = heard
            .recv_timeout(Duration::from_secs(2))
            .expect("the sink should get the packet");
        assert_eq!(length, PCMU_SAMPLES_PER_PACKET);
        // With a sink nothing is left to fetch.
        assert!(matches!(receiver.try_receive_decoded(), Ok(None)));

        // The extra input is still alive, yet shutdown must not hang.
        let started = std::time::Instant::now();
        sender.shutdown().expect("sender should stop cleanly");
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(input.try_send(vec![0.0; 960]).is_err());
        receiver.shutdown().expect("receiver should stop cleanly");
    }

    #[test]
    fn mute_sends_silence_and_digits_are_sent_as_negotiated() {
        let peer = UdpSocket::bind("127.0.0.1:0").expect("peer should bind");
        peer.set_read_timeout(Some(Duration::from_secs(2)))
            .expect("timeout should be set");
        let config = RtpSessionConfig::new(peer.local_addr().expect("address"), 8_000)
            .with_dtmf_payload_type(Some(101));
        let mut session = RtpSession::bind(
            "127.0.0.1:0".parse::<SocketAddr>().expect("valid address"),
            config,
            0x9999_9999,
        )
        .expect("session should start");
        let receive = || {
            let mut buffer = [0_u8; 2_048];
            let (length, _) = peer.recv_from(&mut buffer).expect("packet expected");
            RtpPacket::parse(&buffer[..length]).expect("valid RTP")
        };
        let loud = |packet: &RtpPacket| {
            decode_pcmu(&packet.payload)
                .iter()
                .any(|sample| sample.abs() > 0.1)
        };

        session.push_pcm(&[0.5; 160]).expect("PCM should be queued");
        assert!(loud(&receive()));

        session.set_muted(true);
        session.push_pcm(&[0.5; 160]).expect("PCM should be queued");
        let muted = receive();
        assert_eq!(muted.payload_type, PCMU_PAYLOAD_TYPE);
        assert!(!loud(&muted));

        // A digit goes out even while muted.
        assert!(session.send_dtmf('9'));
        assert!(!session.send_dtmf('z'));
        session.push_pcm(&[0.5; 160]).expect("PCM should be queued");
        let event = receive();
        assert_eq!(event.payload_type, 101);
        assert_eq!(event.payload[0], 9);
        assert!(event.marker);

        session.set_muted(false);
        for _ in 0..12 {
            session.push_pcm(&[0.5; 160]).expect("PCM should be queued");
        }
        let rest: Vec<RtpPacket> = (0..12).map(|_| receive()).collect();
        assert!(rest[..9].iter().all(|packet| packet.payload_type == 101));
        assert!(loud(&rest[11]));
        session.shutdown().expect("session should stop cleanly");
    }

    #[test]
    fn receiver_knows_the_agreed_the_listed_and_the_fixed_payload_types() {
        let receiving = Receiving::new(Codec::G722, 9, vec![(107, Codec::Opus)]);
        // GSM (3) has a number of its own, like G.711 and G.729.
        for known in [9, 107, 0, 8, 18, 3] {
            assert!(receiving.knows(known), "{known}");
        }
        // Keypad events, comfort noise, and the usual numbers of Opus and
        // Speex, which this call did not agree on.
        for unknown in [101, 13, 111, 97, 98] {
            assert!(!receiving.knows(unknown), "{unknown}");
        }
        // A number the other side gave another meaning keeps that meaning.
        let receiving = Receiving::new(Codec::Opus, 96, vec![(9, Codec::G722)]);
        assert_eq!(receiving.types[0], (96, Codec::Opus));
        assert_eq!(receiving.rate, 16_000);
    }

    #[test]
    fn every_received_codec_comes_out_at_the_calls_sample_rate() {
        // The call agreed on G.722 (16 kHz); PCMU arrives all the same.
        let mut receiving = Receiving::new(Codec::G722, 9, Vec::new());
        let mut wide = crate::codec::VoiceEncoder::new(Codec::G722);
        let own = receiving.decode(9, &wide.encode(&[0.2; 320])).unwrap();
        assert_eq!(own.len(), 320);

        let mut narrow_total = 0;
        for _ in 0..10 {
            let narrow = receiving
                .decode(0, &Codec::Pcmu.encode(&[0.2; 160]))
                .unwrap();
            narrow_total += narrow.len();
        }
        // Ten 8 kHz packets are ten 16 kHz packets, give or take the
        // converter's delay.
        assert!((3_100..=3_200).contains(&narrow_total), "{narrow_total}");
        assert!(receiving.decode(101, &[1, 2, 3, 4]).is_none());
    }

    #[test]
    fn lost_packets_are_covered_as_each_codec_allows() {
        // G.711: one quieter copy of the packet before, then nothing.
        let mut receiving = Receiving::new(Codec::Pcmu, 0, Vec::new());
        assert!(receiving.lost().is_none());
        receiving.decode(0, &Codec::Pcmu.encode(&[0.5; 160]));
        let cover = receiving.lost().expect("one packet is covered");
        assert_eq!(cover.len(), 160);
        assert!(cover.iter().all(|sample| (sample - 0.25).abs() < 0.02));
        assert!(receiving.lost().is_none());

        // G.729 makes the missing sound up itself, packet after packet.
        let mut receiving = Receiving::new(Codec::G729, 18, Vec::new());
        let mut encoder = crate::codec::VoiceEncoder::new(Codec::G729);
        for frame in 0..25 {
            let tone: Vec<f32> = (0..160)
                .map(|index| {
                    let time = (frame * 160 + index) as f32 / 8_000.0;
                    0.4 * (std::f32::consts::TAU * 440.0 * time).sin()
                })
                .collect();
            let sound = receiving.decode(18, &encoder.encode(&tone)).unwrap();
            assert_eq!(sound.len(), 160);
        }
        let first = receiving.lost().expect("concealed");
        let second = receiving.lost().expect("concealed again");
        assert_eq!((first.len(), second.len()), (160, 160));
        assert!(first.iter().any(|sample| sample.abs() > 0.05));
    }

    #[test]
    fn pcma_is_sent_when_chosen_and_both_codecs_are_understood() {
        let (heard_sender, heard) = mpsc::channel::<f32>();
        let receiver_socket = UdpSocket::bind("127.0.0.1:0").expect("socket should bind");
        let mut receiver = RtpSession::from_socket_with_sink(
            receiver_socket,
            RtpSessionConfig::new("127.0.0.1:9".parse().expect("valid address"), 8_000)
                .with_expected_source(None),
            0xAAAA_0001,
            Some(Box::new(move |samples| {
                let _ = heard_sender.send(samples[0]);
            })),
        )
        .expect("receiver session should start");
        let target = receiver.local_address().expect("address should exist");

        // A session that agreed on PCMA sends payload type 8 ...
        let mut sender = RtpSession::bind(
            "127.0.0.1:0".parse::<SocketAddr>().expect("valid address"),
            RtpSessionConfig::new(target, 8_000).with_codec(Codec::Pcma),
            0xAAAA_0002,
        )
        .expect("sender session should start");
        sender.push_pcm(&[0.5; 160]).expect("PCM should be queued");
        let level = heard
            .recv_timeout(Duration::from_secs(2))
            .expect("PCMA voice should be heard");
        assert!((level - 0.5).abs() < 0.02, "PCMA decoded as {level}");
        sender.shutdown().expect("sender should stop cleanly");

        // ... and PCMU from another sender is understood by the same receiver.
        let peer = UdpSocket::bind("127.0.0.1:0").expect("peer should bind");
        let packet = RtpPacket {
            marker: false,
            payload_type: 0,
            sequence_number: 1,
            timestamp: 160,
            ssrc: 0xAAAA_0003,
            payload: Codec::Pcmu.encode(&[-0.25; 160]),
        };
        peer.send_to(&packet.serialize().expect("serialize"), target)
            .expect("send");
        let level = heard
            .recv_timeout(Duration::from_secs(2))
            .expect("PCMU voice should be heard");
        assert!((level + 0.25).abs() < 0.02, "PCMU decoded as {level}");
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
