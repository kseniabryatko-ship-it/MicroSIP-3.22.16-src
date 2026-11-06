use microsip_core::sip::message::{InviteRequest, Message, Request, Response, ResponseBuilder};
use microsip_core::sip::sdp::{MediaKind, Sdp};
use std::error::Error;
use std::io;
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::thread;
use std::time::Duration;

type DemoError = Box<dyn Error + Send + Sync>;
type DemoResult<T> = Result<T, DemoError>;

fn main() -> DemoResult<()> {
    let caller_sip = UdpSocket::bind("127.0.0.1:0")?;
    let caller_rtp = UdpSocket::bind("127.0.0.1:0")?;
    let callee_sip = UdpSocket::bind("127.0.0.1:0")?;
    let callee_rtp = UdpSocket::bind("127.0.0.1:0")?;

    for socket in [&caller_sip, &caller_rtp, &callee_sip, &callee_rtp] {
        socket.set_read_timeout(Some(Duration::from_secs(5)))?;
    }

    let caller_address = caller_sip.local_addr()?;
    let callee_address = callee_sip.local_addr()?;

    println!("Local incoming INVITE demo started");
    println!("Caller SIP address: {caller_address}");
    println!("Callee SIP address: {callee_address}");

    let caller_thread = thread::spawn(move || run_caller(caller_sip, caller_rtp, callee_address));

    let callee_result = run_callee(callee_sip, callee_rtp, caller_address);

    let caller_result = caller_thread
        .join()
        .map_err(|_| -> DemoError { io::Error::other("Caller thread panicked").into() })??;

    callee_result?;
    caller_result;

    println!("Incoming INVITE and RTP simulation completed");

    Ok(())
}

fn run_caller(
    sip_socket: UdpSocket,
    rtp_socket: UdpSocket,
    callee_address: SocketAddr,
) -> DemoResult<()> {
    let caller_address = sip_socket.local_addr()?;
    let caller_rtp_address = rtp_socket.local_addr()?;

    let caller_sdp = build_sdp(
        "alice",
        &caller_address.ip().to_string(),
        caller_rtp_address.port(),
    );

    let invite = InviteRequest::new(
        "sip:bob@local.test",
        "sip:alice@local.test",
        "sip:bob@local.test",
        format!("sip:alice@{caller_address}"),
        caller_address.to_string(),
        "local-call-001",
        "z9hG4bK-local-call-001",
        "alice-tag-001",
    )
    .body(caller_sdp.into_bytes())
    .build()?;

    let serialized = Message::Request(invite).serialize()?;

    sip_socket.send_to(&serialized, callee_address)?;

    println!("Caller sent INVITE with SDP");

    let trying = receive_response(&sip_socket, callee_address)?;
    ensure_status(&trying, 100)?;

    println!("Caller received 100 Trying");

    let ringing = receive_response(&sip_socket, callee_address)?;
    ensure_status(&ringing, 180)?;

    println!("Caller received 180 Ringing");

    let ok = receive_response(&sip_socket, callee_address)?;
    ensure_status(&ok, 200)?;

    println!("Caller received 200 OK");

    let answer = parse_sdp(&ok.body)?;
    let callee_rtp_address = rtp_endpoint(&answer)?;

    let (length, source) = receive_rtp(&rtp_socket)?;

    if source != callee_rtp_address {
        return Err(io::Error::other(format!("Unexpected RTP source: {source}")).into());
    }

    println!("Caller received RTP packet from {source}, {} bytes", length);

    rtp_socket.send_to(&rtp_packet(), callee_rtp_address)?;

    println!("Caller sent RTP packet to callee");

    Ok(())
}

fn run_callee(
    sip_socket: UdpSocket,
    rtp_socket: UdpSocket,
    caller_address: SocketAddr,
) -> DemoResult<()> {
    let mut buffer = [0_u8; 65_535];

    let (length, peer_address) = sip_socket.recv_from(&mut buffer)?;
    let request = parse_request(&buffer[..length])?;

    if request.method.as_str() != "INVITE" {
        return Err(io::Error::other("Callee expected an INVITE request").into());
    }

    println!("Callee received incoming INVITE");

    let invite_sdp = parse_sdp(&request.body)?;
    let caller_rtp_address = rtp_endpoint(&invite_sdp)?;

    let via = required_header(&request, "via")?;
    let from = required_header(&request, "from")?;
    let to = format!("{};tag=callee-tag", required_header(&request, "to")?);
    let call_id = required_header(&request, "call-id")?;
    let cseq = required_header(&request, "cseq")?;

    let trying = ResponseBuilder::new(100, "Trying")
        .header("via", via.clone())?
        .header("from", from.clone())?
        .header("to", to.clone())?
        .header("call-id", call_id.clone())?
        .header("cseq", cseq.clone())?
        .build()?;

    send_response(&sip_socket, peer_address, trying)?;

    println!("Callee sent 100 Trying");

    let ringing = ResponseBuilder::new(180, "Ringing")
        .header("via", via.clone())?
        .header("from", from.clone())?
        .header("to", to.clone())?
        .header("call-id", call_id.clone())?
        .header("cseq", cseq.clone())?
        .build()?;

    send_response(&sip_socket, peer_address, ringing)?;

    println!("Callee sent 180 Ringing");

    let callee_rtp_address = rtp_socket.local_addr()?;
    let callee_sdp = build_sdp(
        "bob",
        &callee_rtp_address.ip().to_string(),
        callee_rtp_address.port(),
    );

    let ok = ResponseBuilder::new(200, "OK")
        .header("via", via)?
        .header("from", from)?
        .header("to", to)?
        .header("call-id", call_id)?
        .header("cseq", cseq)?
        .header("content-type", "application/sdp")?
        .body(callee_sdp.into_bytes())
        .build()?;

    send_response(&sip_socket, peer_address, ok)?;

    println!("Callee sent 200 OK with SDP");

    rtp_socket.send_to(&rtp_packet(), caller_rtp_address)?;

    println!("Callee sent RTP packet to caller");

    let (length, source) = receive_rtp(&rtp_socket)?;

    if source != caller_rtp_address {
        return Err(io::Error::other(format!("Unexpected RTP source: {source}")).into());
    }

    println!("Callee received RTP packet from {source}, {} bytes", length);

    Ok(())
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

        let message = parse_message(&buffer[..length])?;

        if let Message::Response(response) = message {
            return Ok(response);
        }
    }
}

fn receive_rtp(socket: &UdpSocket) -> DemoResult<(usize, SocketAddr)> {
    let mut buffer = [0_u8; 65_535];
    let (length, source) = socket.recv_from(&mut buffer)?;

    if length < 12 {
        return Err(io::Error::other("RTP packet is shorter than 12 bytes").into());
    }

    if (buffer[0] & 0xc0) != 0x80 {
        return Err(io::Error::other("Received packet has an invalid RTP version").into());
    }

    Ok((length, source))
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
            io::Error::other(format!("INVITE has no {name} header")).into()
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
s=Local Rust SIP demo\r\n\
c=IN IP4 {address}\r\n\
t=0 0\r\n\
m=audio {rtp_port} RTP/AVP 0\r\n\
a=rtpmap:0 PCMU/8000\r\n\
a=sendrecv\r\n"
    )
}

fn rtp_packet() -> Vec<u8> {
    vec![
        0x80, // RTP version 2, no padding, no extension
        0x00, // payload type: PCMU
        0x00, 0x01, // sequence number
        0x00, 0x00, 0x00, 0x00, // timestamp
        0x11, 0x22, 0x33, 0x44, // SSRC
        0xfa, 0xff, // test PCMU payload
    ]
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
