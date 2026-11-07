use microsip_core::audio::{Microphone, Speaker};
use microsip_core::rtp::{
    PCMU_CLOCK_RATE, PCMU_PAYLOAD_TYPE, PCMU_SAMPLES_PER_PACKET, PcmuStream, RtpError, RtpPacket,
    decode_pcmu,
};
use microsip_core::sip::message::{InviteRequest, Message, Request, Response, ResponseBuilder};
use microsip_core::sip::sdp::{MediaKind, Sdp};
use std::error::Error;
use std::io;
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{
    Arc,
    mpsc::{self, Receiver, Sender},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

type DemoError = Box<dyn Error + Send + Sync>;
type DemoResult<T> = Result<T, DemoError>;

const CALL_ID: &str = "local-sip-audio-call-001";
const INVITE_BRANCH: &str = "z9hG4bK-local-sip-audio-call-001";
const BYE_BRANCH: &str = "z9hG4bK-local-sip-audio-call-001-bye";

struct RawRequest<'a> {
    method: &'a str,
    request_uri: &'a str,
    via: &'a str,
    from: &'a str,
    to: &'a str,
    call_id: &'a str,
    contact: &'a str,
    cseq_number: u32,
}

struct RtpReceiver {
    stop: Arc<AtomicBool>,
    packet_count: Arc<AtomicU64>,
    decoded_samples: Receiver<Vec<f32>>,
    thread: JoinHandle<DemoResult<()>>,
}

fn main() -> DemoResult<()> {
    let duration = parse_duration()?;

    let caller_sip = UdpSocket::bind("127.0.0.1:0")?;
    let caller_rtp = UdpSocket::bind("127.0.0.1:0")?;
    let callee_sip = UdpSocket::bind("127.0.0.1:0")?;
    let callee_rtp = UdpSocket::bind("127.0.0.1:0")?;

    let sip_timeout = Duration::from_secs(10);

    caller_sip.set_read_timeout(Some(sip_timeout))?;
    callee_sip.set_read_timeout(Some(sip_timeout))?;

    let caller_address = caller_sip.local_addr()?;
    let callee_address = callee_sip.local_addr()?;

    println!("Local SIP + real audio demo started");
    println!("Caller SIP address: {caller_address}");
    println!("Callee SIP address: {callee_address}");

    let caller_thread =
        thread::spawn(move || run_caller(caller_sip, caller_rtp, callee_address, duration));

    let callee_result = run_callee(callee_sip, callee_rtp, caller_address);

    caller_thread
        .join()
        .map_err(|_| -> DemoError { io::Error::other("Caller thread panicked").into() })??;

    callee_result?;

    println!("Local SIP call with real PCMU/RTP audio completed");

    Ok(())
}

fn run_caller(
    sip_socket: UdpSocket,
    rtp_socket: UdpSocket,
    callee_sip_address: SocketAddr,
    duration: Duration,
) -> DemoResult<()> {
    let microphone = Microphone::start()?;
    let speaker = Speaker::start()?;

    let caller_sip_address = sip_socket.local_addr()?;
    let caller_rtp_address = rtp_socket.local_addr()?;

    let caller_rtp_ip = caller_rtp_address.ip().to_string();

    let caller_sdp = build_sdp("alice", caller_rtp_ip.as_str(), caller_rtp_address.port());

    let invite = InviteRequest::new(
        "sip:bob@local.test",
        "sip:alice@local.test",
        "sip:bob@local.test",
        format!("sip:alice@{caller_sip_address}"),
        caller_sip_address.to_string(),
        CALL_ID,
        INVITE_BRANCH,
        "alice-tag-001",
    )
    .body(caller_sdp.into_bytes())
    .build()?;

    let invite_bytes = Message::Request(invite).serialize()?;

    sip_socket.send_to(&invite_bytes, callee_sip_address)?;

    println!("Caller sent INVITE with PCMU SDP");

    let invite_request = parse_request(&invite_bytes)?;

    let via = required_header(&invite_request, "via")?;

    let from = required_header(&invite_request, "from")?;

    let initial_to = required_header(&invite_request, "to")?;

    let call_id = required_header(&invite_request, "call-id")?;

    let invite_cseq = required_header(&invite_request, "cseq")?;

    let invite_cseq_number = parse_cseq_number(&invite_cseq)?;

    let trying = receive_response(&sip_socket, callee_sip_address)?;

    ensure_status(&trying, 100)?;

    println!("Caller received 100 Trying");

    let ringing = receive_response(&sip_socket, callee_sip_address)?;

    ensure_status(&ringing, 180)?;

    println!("Caller received 180 Ringing");

    let ok = receive_response(&sip_socket, callee_sip_address)?;

    ensure_status(&ok, 200)?;

    println!("Caller received 200 OK with SDP");

    let answer = parse_sdp(ok.body.as_slice())?;
    let callee_rtp_address = rtp_endpoint(&answer)?;

    let final_to = format!("{initial_to};tag=callee-tag");

    let callee_contact_uri = format!("sip:bob@{callee_sip_address}");

    let caller_contact = format!("<sip:alice@{caller_sip_address}>");

    let ack_request = RawRequest {
        method: "ACK",
        request_uri: callee_contact_uri.as_str(),
        via: via.as_str(),
        from: from.as_str(),
        to: final_to.as_str(),
        call_id: call_id.as_str(),
        contact: caller_contact.as_str(),
        cseq_number: invite_cseq_number,
    };

    sip_socket.send_to(&build_raw_request(&ack_request), callee_sip_address)?;

    println!("Caller sent ACK");

    let receiver_socket = rtp_socket.try_clone()?;

    let RtpReceiver {
        stop: receiver_stop,
        packet_count: receiver_packet_count,
        decoded_samples: decoded_receiver,
        thread: receiver_thread,
    } = start_rtp_receiver(receiver_socket, callee_rtp_address)?;

    let mut pcmu_stream = PcmuStream::new(0x1122_3344, microphone.format().sample_rate)?;

    let started = Instant::now();
    let mut sent_packets = 0_usize;

    println!(
        "Running full-duplex call audio for {} seconds...",
        duration.as_secs()
    );

    while started.elapsed() < duration {
        let packet_count = transmit_microphone(
            &microphone,
            &mut pcmu_stream,
            &rtp_socket,
            callee_rtp_address,
        )?;

        sent_packets += packet_count;

        drain_decoded_audio(&decoded_receiver, &speaker)?;

        thread::sleep(Duration::from_millis(5));
    }

    receiver_stop.store(true, Ordering::Release);

    receiver_thread
        .join()
        .map_err(|_| -> DemoError { io::Error::other("RTP receiver thread panicked").into() })??;

    drain_decoded_audio(&decoded_receiver, &speaker)?;

    let received_packets = receiver_packet_count.load(Ordering::Acquire);

    let bye_cseq_number = invite_cseq_number
        .checked_add(1)
        .ok_or_else(|| -> DemoError { io::Error::other("CSeq number overflow").into() })?;

    let bye_via = format!("SIP/2.0/UDP {caller_sip_address};branch={BYE_BRANCH}");

    let bye_request = RawRequest {
        method: "BYE",
        request_uri: callee_contact_uri.as_str(),
        via: bye_via.as_str(),
        from: from.as_str(),
        to: final_to.as_str(),
        call_id: call_id.as_str(),
        contact: caller_contact.as_str(),
        cseq_number: bye_cseq_number,
    };

    sip_socket.send_to(&build_raw_request(&bye_request), callee_sip_address)?;

    println!("Caller sent BYE");

    let bye_response = receive_response(&sip_socket, callee_sip_address)?;

    ensure_status(&bye_response, 200)?;

    println!("Caller received 200 OK for BYE");

    println!("Caller RTP packets sent: {sent_packets}");
    println!("Caller RTP packets received: {received_packets}");

    if sent_packets == 0 || received_packets == 0 {
        return Err(io::Error::other("No RTP packets were transferred during the call").into());
    }

    Ok(())
}

fn run_callee(
    sip_socket: UdpSocket,
    rtp_socket: UdpSocket,
    expected_caller_address: SocketAddr,
) -> DemoResult<()> {
    let microphone = Microphone::start()?;
    let speaker = Speaker::start()?;

    let callee_sip_address = sip_socket.local_addr()?;
    let callee_rtp_address = rtp_socket.local_addr()?;

    let mut sip_buffer = [0_u8; 65_535];

    let (length, peer_address) = sip_socket.recv_from(&mut sip_buffer)?;

    if peer_address != expected_caller_address {
        return Err(io::Error::other(format!("Unexpected SIP source: {peer_address}")).into());
    }

    let invite_request = parse_request(&sip_buffer[..length])?;

    if invite_request.method.as_str() != "INVITE" {
        return Err(io::Error::other("Callee expected an INVITE request").into());
    }

    println!("Callee received incoming INVITE");

    let invite_sdp = parse_sdp(invite_request.body.as_slice())?;

    let caller_rtp_address = rtp_endpoint(&invite_sdp)?;

    let via = required_header(&invite_request, "via")?;

    let from = required_header(&invite_request, "from")?;

    let initial_to = required_header(&invite_request, "to")?;

    let call_id = required_header(&invite_request, "call-id")?;

    let cseq = required_header(&invite_request, "cseq")?;

    let final_to = format!("{initial_to};tag=callee-tag");

    let trying = ResponseBuilder::new(100, "Trying")
        .header("via", via.clone())?
        .header("from", from.clone())?
        .header("to", final_to.clone())?
        .header("call-id", call_id.clone())?
        .header("cseq", cseq.clone())?
        .build()?;

    send_response(&sip_socket, peer_address, trying)?;

    println!("Callee sent 100 Trying");

    let ringing = ResponseBuilder::new(180, "Ringing")
        .header("via", via.clone())?
        .header("from", from.clone())?
        .header("to", final_to.clone())?
        .header("call-id", call_id.clone())?
        .header("cseq", cseq.clone())?
        .build()?;

    send_response(&sip_socket, peer_address, ringing)?;

    println!("Callee sent 180 Ringing");

    let callee_rtp_ip = callee_rtp_address.ip().to_string();

    let callee_sdp = build_sdp("bob", callee_rtp_ip.as_str(), callee_rtp_address.port());

    let callee_contact_uri = format!("sip:bob@{callee_sip_address}");

    let contact = format!("<{callee_contact_uri}>");

    let ok = ResponseBuilder::new(200, "OK")
        .header("via", via)?
        .header("from", from)?
        .header("to", final_to)?
        .header("call-id", call_id.clone())?
        .header("cseq", cseq)?
        .header("contact", contact)?
        .header("content-type", "application/sdp")?
        .body(callee_sdp.into_bytes())
        .build()?;

    send_response(&sip_socket, peer_address, ok)?;

    println!("Callee sent 200 OK with SDP");

    let ack = wait_for_request_method(&sip_socket, expected_caller_address, "ACK")?;

    if required_header(&ack, "call-id")? != call_id.as_str() {
        return Err(io::Error::other("ACK has an unexpected Call-ID").into());
    }

    println!("Callee received ACK");

    sip_socket.set_read_timeout(Some(Duration::from_millis(20)))?;

    let receiver_socket = rtp_socket.try_clone()?;

    let RtpReceiver {
        stop: receiver_stop,
        packet_count: receiver_packet_count,
        decoded_samples: decoded_receiver,
        thread: receiver_thread,
    } = start_rtp_receiver(receiver_socket, caller_rtp_address)?;

    let mut pcmu_stream = PcmuStream::new(0x5566_7788, microphone.format().sample_rate)?;

    println!("Callee started full-duplex call audio");

    loop {
        transmit_microphone(
            &microphone,
            &mut pcmu_stream,
            &rtp_socket,
            caller_rtp_address,
        )?;

        drain_decoded_audio(&decoded_receiver, &speaker)?;

        match sip_socket.recv_from(&mut sip_buffer) {
            Ok((length, source)) => {
                if source != expected_caller_address {
                    continue;
                }

                let request = parse_request(&sip_buffer[..length])?;

                if request.method.as_str() == "BYE" {
                    if required_header(&request, "call-id")? != call_id.as_str() {
                        return Err(io::Error::other("BYE has an unexpected Call-ID").into());
                    }

                    let bye_via = required_header(&request, "via")?;

                    let bye_from = required_header(&request, "from")?;

                    let bye_to = required_header(&request, "to")?;

                    let bye_call_id = required_header(&request, "call-id")?;

                    let bye_cseq = required_header(&request, "cseq")?;

                    let response = ResponseBuilder::new(200, "OK")
                        .header("via", bye_via)?
                        .header("from", bye_from)?
                        .header("to", bye_to)?
                        .header("call-id", bye_call_id)?
                        .header("cseq", bye_cseq)?
                        .build()?;

                    send_response(&sip_socket, source, response)?;

                    println!("Callee sent 200 OK for BYE");

                    break;
                }
            }

            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}

            Err(error) => {
                return Err(error.into());
            }
        }
    }

    receiver_stop.store(true, Ordering::Release);

    receiver_thread
        .join()
        .map_err(|_| -> DemoError { io::Error::other("RTP receiver thread panicked").into() })??;

    drain_decoded_audio(&decoded_receiver, &speaker)?;

    let received_packets = receiver_packet_count.load(Ordering::Acquire);

    println!("Callee RTP packets received: {received_packets}");

    if received_packets == 0 {
        return Err(io::Error::other("Callee received no RTP packets").into());
    }

    Ok(())
}

fn start_rtp_receiver(socket: UdpSocket, expected_source: SocketAddr) -> DemoResult<RtpReceiver> {
    socket.set_read_timeout(Some(Duration::from_millis(100)))?;

    let stop = Arc::new(AtomicBool::new(false));
    let packet_count = Arc::new(AtomicU64::new(0));

    let (decoded_sender, decoded_receiver) = mpsc::channel::<Vec<f32>>();

    let receiver_stop = Arc::clone(&stop);
    let receiver_packet_count = Arc::clone(&packet_count);

    let receiver_thread = thread::spawn(move || {
        run_rtp_receiver(
            socket,
            expected_source,
            decoded_sender,
            receiver_stop,
            receiver_packet_count,
        )
    });

    Ok(RtpReceiver {
        stop,
        packet_count,
        decoded_samples: decoded_receiver,
        thread: receiver_thread,
    })
}

fn run_rtp_receiver(
    socket: UdpSocket,
    expected_source: SocketAddr,
    decoded_sender: Sender<Vec<f32>>,
    stop: Arc<AtomicBool>,
    packet_count: Arc<AtomicU64>,
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
                    .map_err(|_| io::Error::other("Decoded audio channel was disconnected"))?;

                packet_count.fetch_add(1, Ordering::Relaxed);
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

fn transmit_microphone(
    microphone: &Microphone,
    pcmu_stream: &mut PcmuStream,
    rtp_socket: &UdpSocket,
    destination: SocketAddr,
) -> DemoResult<usize> {
    let Some(samples) = microphone.read_chunk()? else {
        return Ok(0);
    };

    let packets = pcmu_stream.push_pcm(&samples);
    let packet_count = packets.len();

    for packet in packets {
        let bytes = packet.serialize()?;

        rtp_socket.send_to(&bytes, destination)?;
    }

    Ok(packet_count)
}

fn drain_decoded_audio(receiver: &Receiver<Vec<f32>>, speaker: &Speaker) -> DemoResult<()> {
    while let Ok(samples) = receiver.try_recv() {
        speaker.play_mono(&samples, PCMU_CLOCK_RATE)?;
    }

    Ok(())
}

fn build_raw_request(request: &RawRequest<'_>) -> Vec<u8> {
    let method = request.method;
    let request_uri = request.request_uri;
    let via = request.via;
    let from = request.from;
    let to = request.to;
    let call_id = request.call_id;
    let contact = request.contact;
    let cseq_number = request.cseq_number;

    format!(
        "{method} {request_uri} SIP/2.0\r\n\
         Via: {via}\r\n\
         Max-Forwards: 70\r\n\
         From: {from}\r\n\
         To: {to}\r\n\
         Call-ID: {call_id}\r\n\
         CSeq: {cseq_number} {method}\r\n\
         Contact: {contact}\r\n\
         Content-Length: 0\r\n\r\n"
    )
    .into_bytes()
}

fn send_response(
    socket: &UdpSocket,
    destination: SocketAddr,
    response: Response,
) -> DemoResult<()> {
    let serialized = Message::Response(response).serialize()?;

    socket.send_to(&serialized, destination)?;

    Ok(())
}

fn receive_response(socket: &UdpSocket, expected_source: SocketAddr) -> DemoResult<Response> {
    let mut buffer = [0_u8; 65_535];

    loop {
        let (length, source) = socket.recv_from(&mut buffer)?;

        if source != expected_source {
            continue;
        }

        match parse_message(&buffer[..length])? {
            Message::Response(response) => {
                return Ok(response);
            }
            Message::Request(_) => {
                continue;
            }
        }
    }
}

fn wait_for_request_method(
    socket: &UdpSocket,
    expected_source: SocketAddr,
    expected_method: &str,
) -> DemoResult<Request> {
    let mut buffer = [0_u8; 65_535];

    loop {
        let (length, source) = socket.recv_from(&mut buffer)?;

        if source != expected_source {
            continue;
        }

        let request = parse_request(&buffer[..length])?;

        if request.method.as_str() == expected_method {
            return Ok(request);
        }
    }
}

fn parse_message(data: &[u8]) -> DemoResult<Message> {
    let text = std::str::from_utf8(data)?;

    Ok(Message::parse(text)?)
}

fn parse_request(data: &[u8]) -> DemoResult<Request> {
    match parse_message(data)? {
        Message::Request(request) => Ok(request),
        Message::Response(_) => {
            Err(io::Error::other("Expected a SIP request, got a response").into())
        }
    }
}

fn required_header(request: &Request, name: &str) -> DemoResult<String> {
    request
        .headers
        .get(name)
        .map(str::to_owned)
        .ok_or_else(|| -> DemoError {
            io::Error::other(format!("SIP request has no {name} header")).into()
        })
}

fn parse_sdp(body: &[u8]) -> DemoResult<Sdp> {
    let text = std::str::from_utf8(body)?;

    Ok(Sdp::parse(text)?)
}

fn rtp_endpoint(sdp: &Sdp) -> DemoResult<SocketAddr> {
    let media = sdp
        .media(&MediaKind::Audio)
        .ok_or_else(|| -> DemoError { io::Error::other("SDP has no audio media").into() })?;

    if media.port == 0 {
        return Err(io::Error::other("SDP audio media is disabled").into());
    }

    let connection = media
        .connection
        .as_ref()
        .or(sdp.connection.as_ref())
        .ok_or_else(|| -> DemoError { io::Error::other("SDP has no connection address").into() })?;

    let address = connection.address.parse::<IpAddr>()?;

    Ok(SocketAddr::new(address, media.port))
}

fn build_sdp(username: &str, address: &str, rtp_port: u16) -> String {
    format!(
        "v=0\r\n\
         o={username} 1 1 IN IP4 {address}\r\n\
         s=Local Rust SIP audio demo\r\n\
         c=IN IP4 {address}\r\n\
         t=0 0\r\n\
         m=audio {rtp_port} RTP/AVP 0\r\n\
         a=rtpmap:0 PCMU/8000\r\n\
         a=sendrecv\r\n"
    )
}

fn parse_cseq_number(value: &str) -> DemoResult<u32> {
    let Some(number) = value.split_whitespace().next() else {
        return Err(io::Error::other("CSeq header has no number").into());
    };

    number
        .parse::<u32>()
        .map_err(|error| -> DemoError { io::Error::new(io::ErrorKind::InvalidInput, error).into() })
}

fn ensure_status(response: &Response, expected: u16) -> DemoResult<()> {
    if response.status_code != expected {
        return Err(io::Error::other(format!(
            "Expected SIP status {expected}, got {} {}",
            response.status_code, response.reason
        ))
        .into());
    }

    Ok(())
}

fn parse_duration() -> DemoResult<Duration> {
    let seconds = match std::env::args().nth(1) {
        Some(value) => value.parse::<u64>().map_err(|error| -> DemoError {
            io::Error::new(io::ErrorKind::InvalidInput, error).into()
        })?,
        None => 5,
    };

    if seconds == 0 {
        return Err(io::Error::other("Duration must be greater than zero").into());
    }

    Ok(Duration::from_secs(seconds))
}
