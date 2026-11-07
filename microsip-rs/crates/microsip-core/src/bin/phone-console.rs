use microsip_core::audio::{Microphone, Speaker};
use microsip_core::dialog::{DialogConfig, EstablishedDialog};
use microsip_core::rtp::PCMU_CLOCK_RATE;
use microsip_core::rtp::session::{RtpSession, RtpSessionConfig};
use microsip_core::sip::message::{Message, ResponseBuilder};
use microsip_core::sip::sdp::{MediaKind, Sdp};
use std::error::Error;
use std::io::{self, BufRead};
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

const RETRY: Duration = Duration::from_millis(500);
const TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, PartialEq, Eq)]
enum Phase {
    Ringing,
    WaitingAck,
    Active,
    Ending,
}

struct Call {
    invite: Message,
    peer: SocketAddr,
    remote_media: SocketAddr,
    phase: Phase,
    changed: Instant,
    last_sent: Instant,
    cached_ok: Option<Vec<u8>>,
    bye: Option<Message>,
    media: Option<RtpSession>,
    dialog: Option<EstablishedDialog>,
    local_tag: String,
}

struct FinalReply {
    peer: SocketAddr,
    method: String,
    call_id: String,
    cseq: String,
    bytes: Vec<u8>,
    created: Instant,
}

fn fail(text: impl Into<String>) -> Box<dyn Error + Send + Sync> {
    io::Error::other(text.into()).into()
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() != 1 || args[0] == "--help" {
        println!("Usage: phone-console.exe LOCAL_IP:SIP_PORT");
        println!("Commands: answer, reject, hangup, status, quit");
        println!("Direct incoming UDP calls only; no registration or proxy.");
        return Ok(());
    }

    let local: SocketAddr = args[0].parse()?;
    if local.ip().is_unspecified() || local.port() == 0 {
        return Err(fail("Use a concrete local IP and a nonzero SIP port"));
    }

    let sip = UdpSocket::bind(local)?;
    sip.set_nonblocking(true)?;
    let rtp = UdpSocket::bind(SocketAddr::new(local.ip(), 0))?;
    let microphone = Microphone::start()?;
    let speaker = Speaker::start()?;
    let commands = command_channel();
    let mut call: Option<Call> = None;
    let mut final_reply: Option<FinalReply> = None;
    let mut running = true;

    println!("Listening on {local}");
    println!("Commands: answer | reject | hangup | status | quit");
    println!("Use headphones. Incoming calls require answer.");

    while running {
        // Always drain capture, including while idle, to avoid accumulating
        // old microphone samples in the existing audio capture channel.
        while let Some(samples) = microphone.read_chunk()? {
            if let Some(current) = call.as_ref()
                && current.phase == Phase::Active
                && let Some(media) = current.media.as_ref()
            {
                media.push_pcm(&samples)?;
            }
        }

        if let Some(current) = call.as_ref()
            && let Some(media) = current.media.as_ref()
        {
            while let Some(samples) = media.try_receive_decoded()? {
                speaker.play_mono(&samples, PCMU_CLOCK_RATE)?;
            }
        }

        match commands.try_recv() {
            Ok(command) => match command.as_str() {
                "answer" => {
                    if let Some(current) = call.as_mut() {
                        if current.phase == Phase::Ringing {
                            answer(&sip, &rtp, &microphone, current)?;
                        } else {
                            println!("No ringing call to answer.");
                        }
                    } else {
                        println!("No incoming call.");
                    }
                }
                "reject" => {
                    if call
                        .as_ref()
                        .is_some_and(|value| value.phase == Phase::Ringing)
                    {
                        let current = call.take().ok_or_else(|| fail("Missing call"))?;
                        final_reply = Some(reject(&sip, &current, 486)?);
                        println!("Call rejected: 486 Busy Here.");
                    } else {
                        println!("No ringing call to reject.");
                    }
                }
                "hangup" => {
                    if let Some(current) = call.as_mut() {
                        if current.phase == Phase::Active {
                            begin_bye(&sip, current)?;
                        } else {
                            println!("Use reject while ringing; otherwise wait for ACK/BYE.");
                        }
                    } else {
                        println!("No active call.");
                    }
                }
                "status" => {
                    if let Some(current) = call.as_ref() {
                        println!("State: {:?}; peer: {}", current.phase, current.peer);
                        if let Some(media) = current.media.as_ref() {
                            println!(
                                "RTP sent: {}; received: {}",
                                media.sent_packets(),
                                media.received_packets()
                            );
                        }
                    } else {
                        println!("State: Idle");
                    }
                }
                "quit" => {
                    if call
                        .as_ref()
                        .is_some_and(|value| value.phase == Phase::Ringing)
                    {
                        let current = call.take().ok_or_else(|| fail("Missing call"))?;
                        reject(&sip, &current, 486)?;
                        running = false;
                    } else if let Some(current) = call.as_mut() {
                        if current.phase == Phase::Active {
                            begin_bye(&sip, current)?;
                        }
                        println!("Finish the call, then enter quit again.");
                    } else {
                        running = false;
                    }
                }
                "" => {}
                _ => println!("Commands: answer | reject | hangup | status | quit"),
            },
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                stop_media(&mut call)?;
                return Err(fail("Console input closed; client stopped"));
            }
        }

        if let Some((message, peer)) = receive(&sip)? {
            if let Some(cached) = final_reply.as_ref()
                && cached.peer == peer
                && message.headers().get("call-id") == Some(cached.call_id.as_str())
                && message.headers().get("cseq") == Some(cached.cseq.as_str())
                && matches!(&message, Message::Request(req) if req.method.as_str() == cached.method)
            {
                sip.send_to(&cached.bytes, peer)?;
                continue;
            }

            let mut finished = false;
            if let Some(current) = call.as_mut() {
                if peer == current.peer
                    && message.headers().get("call-id") == current.invite.headers().get("call-id")
                {
                    match &message {
                        Message::Request(request) if request.method.as_str() == "INVITE" => {
                            if message.headers().get("cseq") == current.invite.headers().get("cseq")
                            {
                                if let Some(bytes) = current.cached_ok.as_ref() {
                                    sip.send_to(bytes, peer)?;
                                } else {
                                    let bytes = make_response(
                                        &message,
                                        180,
                                        &current.local_tag,
                                        None,
                                        local,
                                    )?;
                                    sip.send_to(&bytes, peer)?;
                                }
                            }
                        }
                        Message::Request(request) if request.method.as_str() == "ACK" => {
                            let expected = format!("{} ACK", invite_number(&current.invite)?);
                            if current.phase == Phase::WaitingAck
                                && request.headers.get("cseq") == Some(expected.as_str())
                            {
                                current.phase = Phase::Active;
                                current.changed = Instant::now();
                                println!("Connected. Commands: hangup | status");
                            }
                        }
                        Message::Request(request) if request.method.as_str() == "BYE" => {
                            if matches!(current.phase, Phase::Active | Phase::Ending) {
                                let bytes =
                                    make_response(&message, 200, &current.local_tag, None, local)?;
                                sip.send_to(&bytes, peer)?;
                                final_reply = Some(FinalReply {
                                    peer,
                                    method: "BYE".to_owned(),
                                    call_id: required(&message, "call-id")?.to_owned(),
                                    cseq: required(&message, "cseq")?.to_owned(),
                                    bytes,
                                    created: Instant::now(),
                                });
                                println!("Remote party ended the call.");
                                finished = true;
                            }
                        }
                        Message::Response(reply) => {
                            if let Some(bye) = current.bye.as_ref()
                                && reply.headers.get("cseq") == bye.headers().get("cseq")
                                && reply.headers.get("via") == bye.headers().get("via")
                                && reply.status_code >= 200
                            {
                                println!("BYE response: {} {}", reply.status_code, reply.reason);
                                finished = true;
                            }
                        }
                        _ => {}
                    }
                }
            } else if let Message::Request(request) = &message
                && request.method.as_str() == "INVITE"
            {
                match incoming(message, peer) {
                    Ok(current) => {
                        let bytes =
                            make_response(&current.invite, 180, &current.local_tag, None, local)?;
                        sip.send_to(&bytes, peer)?;
                        println!("Incoming call from {peer}. Enter answer or reject.");
                        call = Some(current);
                    }
                    Err(error) => eprintln!("Incoming call was not accepted: {error}"),
                }
            }
            if finished {
                stop_media(&mut call)?;
                println!("State: Idle");
            }
        }

        let mut expired = false;
        if let Some(current) = call.as_mut() {
            if current.phase != Phase::Active && current.changed.elapsed() >= TIMEOUT {
                if current.phase == Phase::Ringing {
                    final_reply = Some(reject(&sip, current, 480)?);
                }
                println!("Call setup/termination timed out: {:?}", current.phase);
                expired = true;
            } else if current.last_sent.elapsed() >= RETRY {
                if current.phase == Phase::WaitingAck {
                    if let Some(bytes) = current.cached_ok.as_ref() {
                        sip.send_to(bytes, current.peer)?;
                    }
                } else if current.phase == Phase::Ending
                    && let Some(bye) = current.bye.as_ref()
                {
                    sip.send_to(&bye.serialize()?, current.peer)?;
                }
                current.last_sent = Instant::now();
            }
        }
        if expired {
            stop_media(&mut call)?;
        }
        if final_reply
            .as_ref()
            .is_some_and(|value| value.created.elapsed() >= TIMEOUT)
        {
            final_reply = None;
        }
        thread::sleep(Duration::from_millis(2));
    }
    stop_media(&mut call)?;
    Ok(())
}

fn command_channel() -> Receiver<String> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        for line in io::stdin().lock().lines() {
            match line {
                Ok(line) => {
                    if sender.send(line.trim().to_ascii_lowercase()).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
    receiver
}

fn receive(socket: &UdpSocket) -> Result<Option<(Message, SocketAddr)>> {
    let mut buffer = [0_u8; 65_535];
    match socket.recv_from(&mut buffer) {
        Ok((length, peer)) => {
            let parsed = std::str::from_utf8(&buffer[..length])
                .map_err(|error| error.to_string())
                .and_then(|text| Message::parse(text).map_err(|error| error.to_string()));
            match parsed {
                Ok(message) => Ok(Some((message, peer))),
                Err(error) => {
                    eprintln!("Ignoring malformed SIP packet from {peer}: {error}");
                    Ok(None)
                }
            }
        }
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn required<'a>(message: &'a Message, name: &str) -> Result<&'a str> {
    message
        .headers()
        .get(name)
        .ok_or_else(|| fail(format!("Missing {name}")))
}

fn invite_number(message: &Message) -> Result<u32> {
    let mut parts = required(message, "cseq")?.split_whitespace();
    let number: u32 = parts
        .next()
        .ok_or_else(|| fail("Missing CSeq number"))?
        .parse()?;
    if number >= (1_u32 << 31) || parts.next() != Some("INVITE") || parts.next().is_some() {
        return Err(fail("Invalid INVITE CSeq"));
    }
    Ok(number)
}

fn incoming(invite: Message, peer: SocketAddr) -> Result<Call> {
    invite_number(&invite)?;
    for name in ["via", "from", "to", "call-id", "contact"] {
        required(&invite, name)?;
    }
    let remote_media = media_address(&invite)?;
    if remote_media.ip() != peer.ip() {
        return Err(fail(
            "This direct-client version requires SIP and RTP on the same remote IP",
        ));
    }
    if !invite.headers().get_all("record-route").is_empty() {
        return Err(fail("Proxy route sets are not supported"));
    }
    let now = Instant::now();
    Ok(Call {
        invite,
        peer,
        remote_media,
        phase: Phase::Ringing,
        changed: now,
        last_sent: now,
        cached_ok: None,
        bye: None,
        media: None,
        dialog: None,
        local_tag: format!("console-{}", unique()),
    })
}

fn unique() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{:x}-{nanos:x}", std::process::id())
}

fn answer(
    socket: &UdpSocket,
    rtp: &UdpSocket,
    microphone: &Microphone,
    call: &mut Call,
) -> Result<()> {
    // Discard queued datagrams left by a previous call before reusing the port.
    rtp.set_nonblocking(true)?;
    let mut buffer = [0_u8; 65_535];
    loop {
        match rtp.recv_from(&mut buffer) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
            Err(error) => return Err(error.into()),
        }
    }
    rtp.set_nonblocking(false)?;

    let local = socket.local_addr()?;
    let remote_target = contact_uri(required(&call.invite, "contact")?)?;
    let local_from = format!("{};tag={}", required(&call.invite, "to")?, call.local_tag);
    let dialog = EstablishedDialog::new(DialogConfig {
        remote_target,
        local_from,
        remote_to: required(&call.invite, "from")?.to_owned(),
        call_id: required(&call.invite, "call-id")?.to_owned(),
        local_sent_by: local.to_string(),
        local_contact: format!("<sip:phone@{local}>"),
        // Local and remote CSeq spaces are independent.
        invite_cseq: 1,
        invite_branch: format!("z9hG4bK-local-{}", unique()),
    })?;
    let media = RtpSession::from_socket(
        rtp.try_clone()?,
        RtpSessionConfig::new(call.remote_media, microphone.format().sample_rate),
        0x3344_5566,
    )?;
    let bytes = make_response(
        &call.invite,
        200,
        &call.local_tag,
        Some(sdp(rtp.local_addr()?)),
        local,
    )?;
    socket.send_to(&bytes, call.peer)?;
    call.dialog = Some(dialog);
    call.media = Some(media);
    call.cached_ok = Some(bytes);
    call.phase = Phase::WaitingAck;
    call.changed = Instant::now();
    call.last_sent = Instant::now();
    println!("Answer sent; waiting for ACK.");
    Ok(())
}

fn begin_bye(socket: &UdpSocket, call: &mut Call) -> Result<()> {
    let dialog = call.dialog.as_mut().ok_or_else(|| fail("Missing dialog"))?;
    let request = dialog.build_bye(&format!("z9hG4bK-bye-{}", unique()))?;
    let message = Message::Request(request);
    socket.send_to(&message.serialize()?, call.peer)?;
    call.bye = Some(message);
    call.phase = Phase::Ending;
    call.changed = Instant::now();
    call.last_sent = Instant::now();
    println!("BYE sent.");
    Ok(())
}

fn reject(socket: &UdpSocket, call: &Call, status: u16) -> Result<FinalReply> {
    let bytes = make_response(
        &call.invite,
        status,
        &call.local_tag,
        None,
        socket.local_addr()?,
    )?;
    socket.send_to(&bytes, call.peer)?;
    Ok(FinalReply {
        peer: call.peer,
        method: "INVITE".to_owned(),
        call_id: required(&call.invite, "call-id")?.to_owned(),
        cseq: required(&call.invite, "cseq")?.to_owned(),
        bytes,
        created: Instant::now(),
    })
}

fn stop_media(call: &mut Option<Call>) -> Result<()> {
    if let Some(mut current) = call.take()
        && let Some(mut media) = current.media.take()
    {
        let result = media.shutdown();
        println!(
            "RTP sent: {}; received: {}",
            media.sent_packets(),
            media.received_packets()
        );
        result?;
    }
    Ok(())
}

fn make_response(
    request: &Message,
    status: u16,
    tag: &str,
    body: Option<String>,
    local: SocketAddr,
) -> Result<Vec<u8>> {
    let reason = match status {
        180 => "Ringing",
        200 => "OK",
        480 => "Temporarily Unavailable",
        486 => "Busy Here",
        _ => return Err(fail("Unsupported response status")),
    };
    let to = required(request, "to")?;
    let to = if to.split(';').any(|part| part.trim().starts_with("tag=")) {
        to.to_owned()
    } else {
        format!("{to};tag={tag}")
    };
    let mut builder = ResponseBuilder::new(status, reason);
    for via in request.headers().get_all("via") {
        builder = builder.append_header("via", via)?;
    }
    builder = builder
        .header("from", required(request, "from")?)?
        .header("to", to)?
        .header("call-id", required(request, "call-id")?)?
        .header("cseq", required(request, "cseq")?)?;
    if let Some(body) = body {
        builder = builder
            .header("contact", format!("<sip:phone@{local}>"))?
            .header("content-type", "application/sdp")?
            .body(body.into_bytes());
    }
    Ok(builder.build()?.serialize()?)
}

fn contact_uri(value: &str) -> Result<String> {
    let value = value.trim();
    let uri = if let Some((_, tail)) = value.split_once('<') {
        tail.split_once('>')
            .map(|(uri, _)| uri)
            .ok_or_else(|| fail("Invalid Contact"))?
    } else {
        value
    };
    if !uri.starts_with("sip:") || uri.contains(',') {
        return Err(fail("Unsupported Contact URI"));
    }
    Ok(uri.to_owned())
}

fn media_address(message: &Message) -> Result<SocketAddr> {
    let description = Sdp::parse(std::str::from_utf8(message.body())?)?;
    let audio = description
        .media(&MediaKind::Audio)
        .ok_or_else(|| fail("No audio in SDP"))?;
    if audio.port == 0
        || audio.protocol != "RTP/AVP"
        || !audio.formats.iter().any(|value| value == "0")
    {
        return Err(fail("Enabled PCMU RTP/AVP audio is required"));
    }
    let connection = audio
        .connection
        .as_ref()
        .or(description.connection.as_ref())
        .ok_or_else(|| fail("Missing RTP address"))?;
    let ip: IpAddr = connection.address.parse()?;
    if ip.is_unspecified() {
        return Err(fail("Unspecified RTP address is not supported"));
    }
    Ok(SocketAddr::new(ip, audio.port))
}

fn sdp(address: SocketAddr) -> String {
    let family = if address.is_ipv4() { "IP4" } else { "IP6" };
    let ip = address.ip();
    format!(
        "v=0\r\no=phone 1 1 IN {family} {ip}\r\ns=Rust console\r\n\
         c=IN {family} {ip}\r\nt=0 0\r\nm=audio {} RTP/AVP 0\r\n\
         a=rtpmap:0 PCMU/8000\r\na=sendrecv\r\n",
        address.port()
    )
}
