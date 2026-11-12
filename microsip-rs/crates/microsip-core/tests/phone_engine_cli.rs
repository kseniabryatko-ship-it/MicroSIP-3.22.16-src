use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(10);

struct Phone {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    address: String,
    seen: Vec<String>,
}

impl Phone {
    fn start() -> Self {
        // Port 0: the program picks a free port itself, so parallel tests cannot collide.
        let mut child = Command::new(env!("CARGO_BIN_EXE_phone-engine"))
            .args(["127.0.0.1:0", "--no-audio"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("phone-engine should start");
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (sender, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        let mut phone = Self {
            child,
            stdin,
            lines,
            address: String::new(),
            seen: Vec::new(),
        };
        let listening = phone.expect("Listening on ");
        phone.address = listening
            .trim_start_matches("Listening on ")
            .trim()
            .to_owned();
        phone.expect("Commands: call");
        phone
    }

    fn command(&mut self, text: &str) {
        writeln!(self.stdin, "{text}").unwrap();
        self.stdin.flush().unwrap();
    }

    /// Waits for an output line containing `needle` and returns it.
    fn expect(&mut self, needle: &str) -> String {
        let deadline = Instant::now() + WAIT;
        while Instant::now() < deadline {
            if let Ok(line) = self.lines.recv_timeout(Duration::from_millis(50)) {
                self.seen.push(line.clone());
                if line.contains(needle) {
                    return line;
                }
            }
        }
        panic!(
            "timed out waiting for {needle:?}; output so far: {:#?}",
            self.seen
        );
    }

    fn quit(mut self) {
        self.command("quit");
        let deadline = Instant::now() + WAIT;
        while Instant::now() < deadline {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success(), "phone-engine exited with {status}");
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        panic!("phone-engine did not exit after quit");
    }
}

fn connect(a: &mut Phone, b: &mut Phone) {
    let target = b.address.clone();
    a.command(&format!("call {target}"));
    b.expect("Incoming call");
    b.command("answer");
    a.expect("Connected.");
    b.expect("Connected.");
}

#[test]
fn caller_hangup_then_quit() {
    let mut a = Phone::start();
    let mut b = Phone::start();
    connect(&mut a, &mut b);

    a.command("status");
    a.expect("State: Connected");
    a.command("hangup");
    a.expect("Call ended: completed");
    b.expect("Call ended: completed");
    a.command("status");
    a.expect("State: Idle");
    a.quit();
    b.quit();
}

#[test]
fn rejected_call_reports_486_and_allows_another_call() {
    let mut a = Phone::start();
    let mut b = Phone::start();
    let target = b.address.clone();
    a.command(&format!("call {target}"));
    b.expect("Incoming call");
    b.command("reject");
    a.expect("Call ended: rejected (486)");
    b.expect("Call ended: rejected (486)");

    connect(&mut a, &mut b);
    b.command("hangup");
    b.expect("Call ended: completed");
    a.expect("Call ended: completed");
    a.quit();
    b.quit();
}

#[test]
fn quit_during_a_call_hangs_up_first() {
    let mut a = Phone::start();
    let mut b = Phone::start();
    connect(&mut a, &mut b);

    a.command("quit");
    b.expect("Call ended: completed");
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(status) = a.child.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(Instant::now() < deadline, "caller did not exit");
        thread::sleep(Duration::from_millis(20));
    }
    b.quit();
}

#[test]
fn bad_destinations_are_reported_without_a_call() {
    let mut a = Phone::start();
    a.command("call not-an-address");
    a.expect("Error: invalid IP:port");
    a.command("call 0.0.0.0:5060");
    a.expect("Error: use a concrete IP");
    a.command("answer");
    a.expect("No incoming call to answer.");
    a.command("hangup");
    a.expect("No active call.");
    a.command("status");
    a.expect("State: Idle");
    a.quit();
}

#[test]
fn register_and_quit_talk_to_the_server() {
    use microsip_core::sip::message::{Message, Request, ResponseBuilder};
    use std::net::{SocketAddr, UdpSocket};

    fn receive(server: &UdpSocket) -> (Request, SocketAddr) {
        let mut buffer = [0_u8; 65_535];
        let (length, peer) = server.recv_from(&mut buffer).expect("expected a REGISTER");
        let text = std::str::from_utf8(&buffer[..length]).unwrap();
        match Message::parse(text).unwrap() {
            Message::Request(request) => (request, peer),
            Message::Response(_) => panic!("expected a request"),
        }
    }

    fn accept(server: &UdpSocket, request: &Request, peer: SocketAddr) {
        let mut builder = ResponseBuilder::new(200, "OK");
        for name in ["via", "from", "to", "call-id", "cseq"] {
            builder = builder
                .header(name, request.headers.get(name).unwrap())
                .unwrap();
        }
        let response = builder.header("expires", "60").unwrap().build().unwrap();
        let bytes = Message::Response(response).serialize().unwrap();
        server.send_to(&bytes, peer).unwrap();
    }

    let server = UdpSocket::bind("127.0.0.1:0").unwrap();
    server.set_read_timeout(Some(WAIT)).unwrap();
    let address = server.local_addr().unwrap();

    let mut phone = Phone::start();
    phone.command("register");
    phone.expect("Usage: register");
    phone.command(&format!("register {address} alice secret example.com"));

    let (request, peer) = receive(&server);
    assert_eq!(request.target, "sip:example.com");
    assert_eq!(request.headers.get("to"), Some("<sip:alice@example.com>"));
    accept(&server, &request, peer);
    phone.expect("Registration: registered (60 s)");
    phone.command("status");
    phone.expect("registration: registered (60 s)");
    assert!(
        phone.seen.iter().all(|line| !line.contains("secret")),
        "the password must not be printed"
    );

    // Quitting removes the registration before the program exits.
    phone.command("quit");
    let (removal, peer) = receive(&server);
    assert_eq!(removal.headers.get("expires"), Some("0"));
    accept(&server, &removal, peer);
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(status) = phone.child.try_wait().unwrap() {
            assert!(status.success(), "phone-engine exited with {status}");
            break;
        }
        assert!(Instant::now() < deadline, "phone-engine did not exit");
        thread::sleep(Duration::from_millis(20));
    }
}
