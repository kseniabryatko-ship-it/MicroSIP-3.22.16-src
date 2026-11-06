use microsip_core::sip::client::{SipAccount, UdpRegistrationClient};
use microsip_core::sip::message::{Message, Request, ResponseBuilder};
use std::error::Error;
use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::thread;
use std::time::Duration;

fn main() -> Result<(), Box<dyn Error>> {
    let server = UdpSocket::bind("127.0.0.1:0")?;

    server.set_read_timeout(Some(Duration::from_secs(5)))?;

    let server_address = server.local_addr()?;

    println!("Local mock SIP server started at {server_address}");

    let server_thread = thread::spawn(move || run_mock_server(server));

    let client = UdpRegistrationClient::new(
        "127.0.0.1:0".parse::<SocketAddr>()?,
        "127.0.0.1",
        server_address.port(),
    )?;

    // Это только тестовые данные для локального сервера.
    // Реальные логин и пароль не нужны.
    let account = SipAccount::new(
        "127.0.0.1",
        server_address.port(),
        "local.test",
        "demo",
        "demo",
    )
    .with_contact("127.0.0.1", 5062)
    .with_expires(3600)
    .with_timeout(Duration::from_secs(5));

    println!("Starting local REGISTER transaction...");

    let result = client.register(&account)?;

    println!(
        "Local registration succeeded: {} {}",
        result.response.status_code, result.response.reason
    );

    println!(
        "Local registration expires in {} seconds",
        result.expires.unwrap_or_default()
    );

    let server_result = server_thread
        .join()
        .map_err(|_| io::Error::other("Mock SIP server thread panicked"))?;

    server_result.map_err(|error| io::Error::other(format!("Mock SIP server failed: {error}")))?;

    println!("Completed: REGISTER -> 401 -> Digest -> REGISTER -> 200 OK");

    Ok(())
}

fn run_mock_server(server: UdpSocket) -> Result<(), String> {
    let mut buffer = vec![0_u8; 65_535];

    // Первый REGISTER
    let (length, client_address) = server
        .recv_from(&mut buffer)
        .map_err(|error| error.to_string())?;

    let first_request = parse_request(&buffer[..length])?;

    if first_request.method.as_str() != "REGISTER" {
        return Err("First message is not REGISTER".to_owned());
    }

    let via = header_value(&first_request, "via")?;
    let from = header_value(&first_request, "from")?;
    let to = header_value(&first_request, "to")?;
    let call_id = header_value(&first_request, "call-id")?;
    let cseq = header_value(&first_request, "cseq")?;

    println!("Mock server received the first REGISTER");

    let unauthorized = ResponseBuilder::new(401, "Unauthorized")
        .header("via", via)
        .map_err(|error| error.to_string())?
        .header("from", from)
        .map_err(|error| error.to_string())?
        .header("to", to)
        .map_err(|error| error.to_string())?
        .header("call-id", call_id)
        .map_err(|error| error.to_string())?
        .header("cseq", cseq)
        .map_err(|error| error.to_string())?
        .header(
            "www-authenticate",
            "Digest realm=\"local.test\", \
                 nonce=\"local-nonce-1234567890\", \
                 qop=\"auth\", algorithm=MD5",
        )
        .map_err(|error| error.to_string())?
        .build()
        .map_err(|error| error.to_string())?;

    let response_bytes = Message::Response(unauthorized)
        .serialize()
        .map_err(|error| error.to_string())?;

    server
        .send_to(&response_bytes, client_address)
        .map_err(|error| error.to_string())?;

    println!("Mock server sent 401 Unauthorized");

    // Повторный REGISTER с Authorization
    let (length, client_address) = server
        .recv_from(&mut buffer)
        .map_err(|error| error.to_string())?;

    let second_request = parse_request(&buffer[..length])?;

    if second_request.method.as_str() != "REGISTER" {
        return Err("Second message is not REGISTER".to_owned());
    }

    let authorization = second_request
        .headers
        .get("authorization")
        .ok_or_else(|| "Second REGISTER has no Authorization header".to_owned())?;

    if !authorization.starts_with("Digest ") {
        return Err("Authorization header does not use Digest".to_owned());
    }

    if !authorization
        .to_ascii_lowercase()
        .contains("username=\"demo\"")
    {
        return Err("Authorization header has an unexpected username".to_owned());
    }

    println!("Mock server received REGISTER with Digest authorization");

    let success = ResponseBuilder::new(200, "OK")
        .header("via", header_value(&second_request, "via")?)
        .map_err(|error| error.to_string())?
        .header("from", header_value(&second_request, "from")?)
        .map_err(|error| error.to_string())?
        .header("to", header_value(&second_request, "to")?)
        .map_err(|error| error.to_string())?
        .header("call-id", header_value(&second_request, "call-id")?)
        .map_err(|error| error.to_string())?
        .header("cseq", header_value(&second_request, "cseq")?)
        .map_err(|error| error.to_string())?
        .header("expires", "3600")
        .map_err(|error| error.to_string())?
        .build()
        .map_err(|error| error.to_string())?;

    let response_bytes = Message::Response(success)
        .serialize()
        .map_err(|error| error.to_string())?;

    server
        .send_to(&response_bytes, client_address)
        .map_err(|error| error.to_string())?;

    println!("Mock server sent 200 OK");

    Ok(())
}

fn parse_request(data: &[u8]) -> Result<Request, String> {
    let text =
        std::str::from_utf8(data).map_err(|error| format!("SIP message is not UTF-8: {error}"))?;

    match Message::parse(text).map_err(|error| error.to_string())? {
        Message::Request(request) => Ok(request),
        Message::Response(_) => Err("Expected a SIP request, got a response".to_owned()),
    }
}

fn header_value(request: &Request, name: &str) -> Result<String, String> {
    request
        .headers
        .get(name)
        .map(str::to_owned)
        .ok_or_else(|| format!("SIP request has no {name} header"))
}
