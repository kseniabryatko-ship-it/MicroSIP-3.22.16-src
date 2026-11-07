use microsip_core::audio::{Microphone, Speaker};
use microsip_core::rtp::{
    PCMU_CLOCK_RATE, PCMU_PAYLOAD_TYPE, PCMU_SAMPLES_PER_PACKET, PcmuStream, RtpError, RtpPacket,
    decode_pcmu,
};
use std::error::Error;
use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{
    Arc,
    mpsc::{self, Receiver, Sender},
};
use std::thread;
use std::time::{Duration, Instant};

type DemoError = Box<dyn Error + Send + Sync>;
type DemoResult<T> = Result<T, DemoError>;

fn main() -> DemoResult<()> {
    let duration = parse_duration()?;

    println!("Starting microphone...");

    let microphone = Microphone::start()?;

    println!("Starting speaker...");

    let speaker = Speaker::start()?;

    println!(
        "Microphone format: {} Hz, {} channel(s)",
        microphone.format().sample_rate,
        microphone.format().channels
    );

    println!(
        "Speaker format: {} Hz, {} channel(s)",
        speaker.format().sample_rate,
        speaker.format().channels
    );

    let receiver_socket = UdpSocket::bind("127.0.0.1:0")?;

    receiver_socket.set_read_timeout(Some(Duration::from_millis(100)))?;

    let sender_socket = UdpSocket::bind("127.0.0.1:0")?;

    let receiver_address = receiver_socket.local_addr()?;

    let sender_address = sender_socket.local_addr()?;

    println!("RTP receiver: {receiver_address}");
    println!("RTP sender: {sender_address}");

    let stop = Arc::new(AtomicBool::new(false));
    let received_packets = Arc::new(AtomicU64::new(0));

    let (decoded_sender, decoded_receiver) = mpsc::channel::<Vec<f32>>();

    let receiver_stop = Arc::clone(&stop);
    let receiver_counter = Arc::clone(&received_packets);

    let receiver_thread = thread::spawn(move || {
        run_receiver(
            receiver_socket,
            sender_address,
            decoded_sender,
            receiver_stop,
            receiver_counter,
        )
    });

    let mut pcmu_stream = PcmuStream::new(0x1234_5678, microphone.format().sample_rate)?;

    let started = Instant::now();
    let mut sent_packets = 0_u64;

    println!(
        "Running real PCMU/RTP audio for {} seconds...",
        duration.as_secs()
    );

    while started.elapsed() < duration {
        if let Some(samples) = microphone.read_chunk()? {
            let packets = pcmu_stream.push_pcm(&samples);

            for packet in packets {
                let bytes = packet.serialize()?;

                sender_socket.send_to(&bytes, receiver_address)?;

                sent_packets += 1;
            }
        }

        drain_decoded_audio(&decoded_receiver, &speaker)?;

        thread::sleep(Duration::from_millis(5));
    }

    stop.store(true, Ordering::Release);

    match receiver_thread.join() {
        Ok(result) => result?,
        Err(_) => {
            return Err(io::Error::other("RTP receiver thread panicked").into());
        }
    }

    drain_decoded_audio(&decoded_receiver, &speaker)?;

    thread::sleep(Duration::from_millis(300));

    let received_packets = received_packets.load(Ordering::Acquire);

    println!("RTP packets sent: {sent_packets}");
    println!("RTP packets received: {received_packets}");

    if sent_packets == 0 || received_packets == 0 {
        return Err(io::Error::other("No RTP packets were transferred").into());
    }

    println!(
        "Real microphone -> PCMU -> RTP/UDP -> PCMU -> \
         speaker test completed"
    );

    Ok(())
}

fn run_receiver(
    socket: UdpSocket,
    expected_source: SocketAddr,
    decoded_sender: Sender<Vec<f32>>,
    stop: Arc<AtomicBool>,
    received_packets: Arc<AtomicU64>,
) -> DemoResult<()> {
    let mut buffer = [0_u8; 65_535];

    while !stop.load(Ordering::Acquire) {
        match socket.recv_from(&mut buffer) {
            Ok((length, source)) => {
                if source != expected_source {
                    continue;
                }

                let packet = RtpPacket::parse(&buffer[..length])?;

                if packet.payload_type != PCMU_PAYLOAD_TYPE {
                    return Err(RtpError::UnsupportedPayloadType(packet.payload_type).into());
                }

                if packet.payload.len() != PCMU_SAMPLES_PER_PACKET {
                    return Err(RtpError::InvalidPayloadLength {
                        expected: PCMU_SAMPLES_PER_PACKET,
                        actual: packet.payload.len(),
                    }
                    .into());
                }

                let samples = decode_pcmu(&packet.payload);

                decoded_sender
                    .send(samples)
                    .map_err(|_| io::Error::other("Decoded audio receiver was disconnected"))?;

                received_packets.fetch_add(1, Ordering::Relaxed);
            }

            Err(error)
                if error.kind() == io::ErrorKind::WouldBlock
                    || error.kind() == io::ErrorKind::TimedOut =>
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

fn drain_decoded_audio(receiver: &Receiver<Vec<f32>>, speaker: &Speaker) -> DemoResult<()> {
    while let Ok(samples) = receiver.try_recv() {
        speaker.play_mono(&samples, PCMU_CLOCK_RATE)?;
    }

    Ok(())
}

fn parse_duration() -> DemoResult<Duration> {
    let seconds = match std::env::args().nth(1) {
        Some(value) => value
            .parse::<u64>()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?,
        None => 5,
    };

    if seconds == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Duration must be greater than zero",
        )
        .into());
    }

    Ok(Duration::from_secs(seconds))
}
