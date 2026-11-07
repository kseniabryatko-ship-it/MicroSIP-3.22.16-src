use microsip_core::audio::{Microphone, Speaker};
use microsip_core::dialog::{DialogConfig, EstablishedDialog};
use microsip_core::rtp::PCMU_CLOCK_RATE;
use microsip_core::rtp::session::{RtpSession, RtpSessionConfig};
use microsip_core::sip::message::{InviteRequest, Message, Request, Response, ResponseBuilder};
use microsip_core::sip::sdp::{MediaKind, Sdp};
use std::error::Error;
use std::io;
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

const WAIT: Duration = Duration::from_secs(30);
const RETRY: Duration = Duration::from_millis(500);

struct Settings {
    listen: bool,
    local: SocketAddr,
    remote: Option<SocketAddr>,
    duration: Duration,
}

struct Connected {
    peer: SocketAddr,
    remote_rtp: SocketAddr,
    call_id: String,
    dialog: Option<EstablishedDialog>,
    cached_invite_response: Option<Vec<u8>>,
    cached_ack: Option<Vec<u8>>,
}

fn failure(message: impl Into<String>) -> Box<dyn Error + Send + Sync> {
    io::Error::other(message.into()).into()
}

fn main() -> Result<()> {
    let Some(settings) = settings()? else {
        return Ok(());
    };

    let sip = UdpSocket::bind(settings.local)?;
    sip.set_read_timeout(Some(Duration::from_millis(50)))?;
    let rtp_socket = UdpSocket::bind(SocketAddr::new(settings.local.ip(), 0))?;
    let local_rtp = rtp_socket.local_addr()?;

    println!("SIP: {}", sip.local_addr()?);
    println!("RTP: {local_rtp}");
    println!("Opening microphone and speaker...");
    let microphone = Microphone::start()?;
    let speaker = Speaker::start()?;

    let connected = if settings.listen {
        println!("Waiting up to 30 seconds. Incoming call is AUTO-ANSWERED.");
        accept_call(&sip, local_rtp)?
    } else {
        let peer = settings
            .remote
            .ok_or_else(|| failure("Missing destination"))?;
        originate_call(&sip, local_rtp, peer)?
    };

    let config = RtpSessionConfig::new(connected.remote_rtp, microphone.format().sample_rate);
    let mut media = RtpSession::from_socket(
        rtp_socket,
        config,
        if settings.listen {
            0x2233_4455
        } else {
            0x1122_3344
        },
    )?;

    println!("Connected. Use headphones to prevent acoustic feedback.");
    let outcome = run_call(
        &sip,
        &microphone,
        &speaker,
        &media,
        connected,
        settings.duration,
    );
    let stopped = media.shutdown();
    println!("RTP sent: {}", media.sent_packets());
    println!("RTP received: {}", media.received_packets());
    outcome?;
    stopped?;
    println!("Call finished.");
    Ok(())
}

fn settings() -> Result<Option<Settings>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args[0] == "--help" {
        println!("Direct UDP phone, one call, automatic answer.");
        println!("Listen: direct_phone.exe listen LOCAL_IP:PORT");
        println!("Call:   direct_phone.exe call LOCAL_IP:PORT REMOTE_IP:PORT [SECONDS]");
        println!("Use a concrete local IP, not 0.0.0.0. Default duration: 10 seconds.");
        return Ok(None);
    }

    let listen = match args[0].as_str() {
        "listen" if args.len() == 2 => true,
        "call" if (3..=4).contains(&args.len()) => false,
        _ => return Err(failure("Invalid arguments; run with --help")),
    };
    let local: SocketAddr = args[1].parse()?;
    if local.ip().is_unspecified() || local.port() == 0 {
        return Err(failure(
            "Specify a concrete local IP and a nonzero SIP port",
        ));
    }
    let remote = if listen {
        None
    } else {
        Some(args[2].parse::<SocketAddr>()?)
    };
    if let Some(peer) = remote
        && (peer.port() == 0 || peer.ip().is_unspecified() || peer.is_ipv4() != local.is_ipv4())
    {
        return Err(failure("Invalid destination or different IP family"));
    }
    let seconds = args
        .get(3)
        .map(|text| text.parse::<u64>())
        .transpose()?
        .unwrap_or(10);
    if !(1..=300).contains(&seconds) {
        return Err(failure("Duration must be between 1 and 300 seconds"));
    }
    Ok(Some(Settings {
        listen,
        local,
        remote,
        duration: Duration::from_secs(seconds),
    }))
}

fn unique() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{:x}-{nanos:x}", std::process::id())
}

fn branch() -> String {
    format!("z9hG4bK-{}", unique())
}

fn header<'a>(message: &'a Message, name: &str) -> Result<&'a str> {
    message
        .headers()
        .get(name)
        .ok_or_else(|| failure(format!("Missing {name}")))
}

fn receive(socket: &UdpSocket) -> Result<Option<(Message, SocketAddr)>> {
    let mut buffer = [0_u8; 65_535];
    match socket.recv_from(&mut buffer) {
        Ok((length, peer)) => {
            let text = std::str::from_utf8(&buffer[..length])?;
            Ok(Some((Message::parse(text)?, peer)))
        }
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(error.into()),
    }
}

fn response(
    request: &Message,
    status: u16,
    body: Option<String>,
    local: SocketAddr,
) -> Result<Vec<u8>> {
    let to = header(request, "to")?;
    let to = if to.split(';').any(|part| part.trim().starts_with("tag=")) {
        to.to_owned()
    } else {
        format!("{to};tag=direct-{}", local.port())
    };
    let reason = match status {
        100 => "Trying",
        180 => "Ringing",
        200 => "OK",
        _ => "Decline",
    };
    let mut builder = ResponseBuilder::new(status, reason);
    for via in request.headers().get_all("via") {
        builder = builder.append_header("via", via)?;
    }
    builder = builder
        .header("from", header(request, "from")?)?
        .header("to", to)?
        .header("call-id", header(request, "call-id")?)?
        .header("cseq", header(request, "cseq")?)?
        .header("contact", format!("<sip:phone@{local}>"))?;
    if let Some(body) = body {
        builder = builder
            .header("content-type", "application/sdp")?
            .body(body.into_bytes());
    }
    Ok(builder.build()?.serialize()?)
}

fn make_sdp(address: SocketAddr) -> String {
    let family = if address.is_ipv4() { "IP4" } else { "IP6" };
    let ip = address.ip();
    format!(
        "v=0\r\no=phone 1 1 IN {family} {ip}\r\ns=Rust direct call\r\n\
         c=IN {family} {ip}\r\nt=0 0\r\nm=audio {} RTP/AVP 0\r\n\
         a=rtpmap:0 PCMU/8000\r\na=sendrecv\r\n",
        address.port()
    )
}

fn media_address(message: &Message) -> Result<SocketAddr> {
    let sdp = Sdp::parse(std::str::from_utf8(message.body())?)?;
    let audio = sdp
        .media(&MediaKind::Audio)
        .ok_or_else(|| failure("No audio in SDP"))?;
    if audio.port == 0
        || audio.protocol != "RTP/AVP"
        || !audio.formats.iter().any(|value| value == "0")
    {
        return Err(failure(
            "This client requires enabled RTP/AVP audio with PCMU payload 0",
        ));
    }
    let connection = audio
        .connection
        .as_ref()
        .or(sdp.connection.as_ref())
        .ok_or_else(|| failure("No media connection address"))?;
    let ip: IpAddr = connection.address.parse()?;
    if ip.is_unspecified() {
        return Err(failure("Hold/unspecified media address is not supported"));
    }
    Ok(SocketAddr::new(ip, audio.port))
}

fn remote_contact(message: &Message) -> Result<String> {
    let value = header(message, "contact")?.trim();
    let uri = if let Some((_, tail)) = value.split_once('<') {
        tail.split_once('>')
            .map(|(uri, _)| uri)
            .ok_or_else(|| failure("Invalid Contact"))?
    } else {
        value
    };
    if !uri.starts_with("sip:") || uri.contains(',') {
        return Err(failure("Unsupported Contact"));
    }
    Ok(uri.to_owned())
}

fn matches_response(request: &Request, reply: &Response) -> bool {
    request.headers.get("call-id") == reply.headers.get("call-id")
        && request.headers.get("cseq") == reply.headers.get("cseq")
        && request.headers.get("via") == reply.headers.get("via")
}

fn originate_call(socket: &UdpSocket, rtp: SocketAddr, peer: SocketAddr) -> Result<Connected> {
    let local = socket.local_addr()?;
    let id = unique();
    let invite_branch = branch();
    let request = InviteRequest::new(
        format!("sip:phone@{peer}"),
        format!("sip:phone@{local}"),
        format!("sip:phone@{peer}"),
        format!("sip:phone@{local}"),
        local.to_string(),
        id.clone(),
        invite_branch.clone(),
        unique(),
    )
    .body(make_sdp(rtp).into_bytes())
    .build()?;
    let bytes = request.serialize()?;
    socket.send_to(&bytes, peer)?;
    println!("INVITE sent to {peer}");
    let started = Instant::now();
    let mut last_sent = started;
    let mut provisional = false;

    while started.elapsed() < WAIT {
        if !provisional && last_sent.elapsed() >= RETRY {
            socket.send_to(&bytes, peer)?;
            last_sent = Instant::now();
        }
        let Some((message, source)) = receive(socket)? else {
            continue;
        };
        if source != peer {
            continue;
        }
        let Message::Response(reply) = &message else {
            continue;
        };
        if !matches_response(&request, reply) {
            continue;
        }
        println!("SIP {} {}", reply.status_code, reply.reason);
        if reply.status_code < 200 {
            provisional = true;
            continue;
        }
        if !(200..300).contains(&reply.status_code) {
            return Err(failure(
                "Call rejected; non-2xx ACK handling is not implemented in this version",
            ));
        }
        if !message.headers().get_all("record-route").is_empty() {
            return Err(failure(
                "Proxy route sets are not supported by this direct client",
            ));
        }
        let remote_rtp = media_address(&message)?;
        let mut dialog = EstablishedDialog::new(DialogConfig {
            remote_target: remote_contact(&message)?,
            local_from: request
                .headers
                .get("from")
                .ok_or_else(|| failure("Missing From"))?
                .to_owned(),
            remote_to: header(&message, "to")?.to_owned(),
            call_id: id.clone(),
            local_sent_by: local.to_string(),
            local_contact: format!("<sip:phone@{local}>"),
            invite_cseq: 1,
            invite_branch,
        })?;
        let ack = dialog.build_success_ack(&branch())?.serialize()?;
        socket.send_to(&ack, peer)?;
        println!("ACK sent");
        return Ok(Connected {
            peer,
            remote_rtp,
            call_id: id,
            dialog: Some(dialog),
            cached_invite_response: None,
            cached_ack: Some(ack),
        });
    }
    Err(failure("INVITE response timeout"))
}

fn accept_call(socket: &UdpSocket, rtp: SocketAddr) -> Result<Connected> {
    let started = Instant::now();
    while started.elapsed() < WAIT {
        let Some((message, peer)) = receive(socket)? else {
            continue;
        };
        let Message::Request(request) = &message else {
            continue;
        };
        if request.method.as_str() != "INVITE" {
            continue;
        }
        let remote_rtp = media_address(&message)?;
        let id = header(&message, "call-id")?.to_owned();
        let number = header(&message, "cseq")?
            .split_whitespace()
            .next()
            .ok_or_else(|| failure("Invalid CSeq"))?
            .to_owned();
        let local = socket.local_addr()?;
        socket.send_to(&response(&message, 100, None, local)?, peer)?;
        socket.send_to(&response(&message, 180, None, local)?, peer)?;
        let ok = response(&message, 200, Some(make_sdp(rtp)), local)?;
        socket.send_to(&ok, peer)?;
        println!("Auto-answer sent to {peer}; waiting for ACK");
        let ack_started = Instant::now();
        let mut last_sent = ack_started;
        while ack_started.elapsed() < WAIT {
            if last_sent.elapsed() >= RETRY {
                socket.send_to(&ok, peer)?;
                last_sent = Instant::now();
            }
            let Some((next, source)) = receive(socket)? else {
                continue;
            };
            if source != peer || next.headers().get("call-id") != Some(id.as_str()) {
                continue;
            }
            if let Message::Request(request) = &next
                && request.method.as_str() == "ACK"
                && next.headers().get("cseq") == Some(format!("{number} ACK").as_str())
            {
                println!("ACK received");
                return Ok(Connected {
                    peer,
                    remote_rtp,
                    call_id: id,
                    dialog: None,
                    cached_invite_response: Some(ok),
                    cached_ack: None,
                });
            }
        }
        return Err(failure("ACK timeout"));
    }
    Err(failure("No incoming call within 30 seconds"))
}

fn run_call(
    socket: &UdpSocket,
    microphone: &Microphone,
    speaker: &Speaker,
    media: &RtpSession,
    mut call: Connected,
    duration: Duration,
) -> Result<()> {
    socket.set_nonblocking(true)?;
    let started = Instant::now();
    let mut bye: Option<Request> = None;
    let mut bye_started = None;
    let mut last_bye = Instant::now();

    loop {
        // Drain microphone data independently of SIP receive timeouts.
        while let Some(samples) = microphone.read_chunk()? {
            media.push_pcm(&samples)?;
        }
        while let Some(samples) = media.try_receive_decoded()? {
            speaker.play_mono(&samples, PCMU_CLOCK_RATE)?;
        }

        if bye.is_none()
            && started.elapsed() >= duration
            && let Some(dialog) = call.dialog.as_mut()
        {
            let request = dialog.build_bye(&branch())?;
            socket.send_to(&request.serialize()?, call.peer)?;
            bye = Some(request);
            bye_started = Some(Instant::now());
            last_bye = Instant::now();
            println!("BYE sent");
        }

        if let Some(request) = &bye {
            if bye_started.is_some_and(|time: Instant| time.elapsed() >= WAIT) {
                return Err(failure("BYE response timeout"));
            }
            if last_bye.elapsed() >= RETRY {
                socket.send_to(&request.serialize()?, call.peer)?;
                last_bye = Instant::now();
            }
        }

        if started.elapsed() > Duration::from_secs(360) {
            return Err(failure("Maximum call lifetime exceeded"));
        }

        if let Some((message, source)) = receive(socket)? {
            if source != call.peer
                || message.headers().get("call-id") != Some(call.call_id.as_str())
            {
                continue;
            }
            match &message {
                Message::Request(request) if request.method.as_str() == "BYE" => {
                    let bytes = response(&message, 200, None, socket.local_addr()?)?;
                    socket.send_to(&bytes, source)?;
                    println!("BYE received; 200 OK sent");
                    // Retain the final response briefly for duplicate BYE requests.
                    let linger = Instant::now();
                    while linger.elapsed() < Duration::from_secs(2) {
                        if let Some((duplicate, peer)) = receive(socket)?
                            && peer == source
                            && duplicate.headers().get("call-id")
                                == message.headers().get("call-id")
                            && duplicate.headers().get("cseq") == message.headers().get("cseq")
                            && matches!(&duplicate, Message::Request(req) if req.method.as_str() == "BYE")
                        {
                            socket.send_to(&bytes, peer)?;
                        }
                        thread::sleep(Duration::from_millis(5));
                    }
                    return Ok(());
                }
                Message::Request(request) if request.method.as_str() == "INVITE" => {
                    if let Some(bytes) = &call.cached_invite_response {
                        socket.send_to(bytes, source)?;
                    }
                }
                Message::Response(reply) => {
                    if let Some(request) = &bye
                        && matches_response(request, reply)
                    {
                        if (200..300).contains(&reply.status_code) {
                            if let Some(dialog) = call.dialog.as_mut() {
                                dialog.finish_local_bye()?;
                            }
                            println!("BYE confirmed");
                            return Ok(());
                        }
                        if reply.status_code >= 300 {
                            return Err(failure(format!("BYE rejected: {}", reply.status_code)));
                        }
                    }
                    if (200..300).contains(&reply.status_code)
                        && reply.headers.get("cseq") == Some("1 INVITE")
                        && let Some(bytes) = &call.cached_ack
                    {
                        socket.send_to(bytes, source)?;
                    }
                }
                _ => {}
            }
        }
        thread::sleep(Duration::from_millis(2));
    }
}
