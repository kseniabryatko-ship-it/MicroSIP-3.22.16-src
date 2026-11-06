use microsip_core::sip::client::{SipAccount, UdpRegistrationClient};
use std::env;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::process;
use std::time::Duration;

const USAGE: &str = r#"
MicroSIP Rust - UDP SIP registration test

Usage:
cargo run -p microsip-core --example register -- \
  <server> <domain> <username> <contact-host> [server-port] [contact-port]

Arguments:
  server        SIP server hostname or IP address
  domain        SIP domain
  username      SIP username
  contact-host  IP address reachable by the SIP server
  server-port   SIP server UDP port, default: 5060
  contact-port  Local and Contact UDP port, default: 5060

The password must be set in MICROSIP_PASSWORD.

Example:
set "MICROSIP_PASSWORD=your_password"
cargo run -p microsip-core --example register -- sip.example.com example.com alice 203.0.113.10 5060 5062
"#;

fn main() {
    if let Err(error) = run() {
        eprintln!("Registration failed: {error}");
        process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let arguments: Vec<String> = env::args().skip(1).collect();

    if arguments
        .iter()
        .any(|argument| argument == "--help" || argument == "-h")
    {
        print!("{USAGE}");
        return Ok(());
    }

    if arguments.len() < 4 {
        return Err(format!("Not enough arguments.\n\n{USAGE}"));
    }

    if arguments.len() > 6 {
        return Err(format!("Too many arguments.\n\n{USAGE}"));
    }

    let server = arguments[0].as_str();
    let domain = arguments[1].as_str();
    let username = arguments[2].as_str();
    let contact_host = arguments[3].as_str();

    let server_port = parse_port(arguments.get(4), 5060)?;
    let contact_port = parse_port(arguments.get(5), 5060)?;

    let password =
        env::var("MICROSIP_PASSWORD").map_err(|_| "MICROSIP_PASSWORD is not set".to_owned())?;

    if password.is_empty() {
        return Err("MICROSIP_PASSWORD cannot be empty".to_owned());
    }

    let bind_address = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), contact_port);

    let account = SipAccount::new(server, server_port, domain, username, password)
        .with_contact(contact_host, contact_port)
        .with_expires(3600)
        .with_timeout(Duration::from_secs(10));

    let client = UdpRegistrationClient::new(bind_address, server, server_port)
        .map_err(|error| error.to_string())?;

    println!("Connecting to {server}:{server_port} as {username}@{domain}");

    println!("Local UDP address: {bind_address}, contact port: {contact_port}");

    println!("Public contact: sip:{username}@{contact_host}:{contact_port}");

    let result = client
        .register(&account)
        .map_err(|error| error.to_string())?;

    println!(
        "Registration succeeded: {} {}",
        result.response.status_code, result.response.reason
    );

    println!(
        "Registration expires in {} seconds",
        result.expires.unwrap_or_default()
    );

    println!("Call-ID: {}", result.call_id);

    Ok(())
}

fn parse_port(value: Option<&String>, default: u16) -> Result<u16, String> {
    let port = match value {
        Some(value) => value
            .parse::<u16>()
            .map_err(|_| format!("Invalid port value: {value}"))?,
        None => default,
    };

    if port == 0 {
        return Err("Port cannot be zero".to_owned());
    }

    Ok(port)
}
