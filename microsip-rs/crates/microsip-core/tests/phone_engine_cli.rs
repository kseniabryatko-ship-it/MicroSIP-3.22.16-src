use std::io::{BufRead, BufReader, Write};
use std::net::UdpSocket;
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

fn free_port() -> u16 {
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    socket.local_addr().unwrap().port()
}

impl Phone {
    fn start() -> Self {
        let address = format!("127.0.0.1:{}", free_port());
        let mut child = Command::new(env!("CARGO_BIN_EXE_phone-engine"))
            .args([address.as_str(), "--no-audio"])
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
            address,
            seen: Vec::new(),
        };
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
