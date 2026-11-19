//! Calling from outside the phone: a `tel:`, `sip:` or `callto:` link
//! clicked in a browser or a mail program, or a number on the command line
//! (`microsip-gui.exe 2205`), as with MicroSIP.
//!
//! Windows starts a new copy of the program for every link. Only one copy
//! may be registered on the exchange, so a second copy hands the number to
//! the one already running and exits. The copies talk over a TCP socket on
//! the loopback interface; the running copy writes its port and a random
//! key into a file in the user's own settings folder, and a request without
//! that key is ignored, so other users of the computer cannot make calls.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

/// The schemes this phone opens.
pub const SCHEMES: [&str; 4] = ["tel", "sip", "sips", "callto"];

/// The number or SIP address to call for a link or a command-line word;
/// `None` for anything else (such as `--minimized`).
///
/// * `tel:+7-495-123-45-67;ext=12` gives `+74951234567`
/// * `sip:2205@pbx.example.com;transport=tcp` gives `2205@pbx.example.com`
/// * `callto://2205` and `callto:2205` give `2205`
/// * a plain `2205` or `+7 (495) 123-45-67` gives the number
pub fn target_from_argument(argument: &str) -> Option<String> {
    let argument = argument.trim().trim_matches('"').trim();
    if argument.is_empty() || argument.starts_with('-') {
        return None;
    }
    let (scheme, rest) = match argument.split_once(':') {
        Some((scheme, rest))
            if SCHEMES
                .iter()
                .any(|known| known.eq_ignore_ascii_case(scheme)) =>
        {
            (scheme.to_ascii_lowercase(), rest)
        }
        // A plain number, with or without separators, or a user name or
        // address as typed into the phone.
        _ => return phone_number(argument).or_else(|| sip_target(argument, false)),
    };
    let rest = percent_decode(cut_parameters(rest.trim_start_matches('/')));
    match scheme.as_str() {
        "tel" => phone_number(&rest),
        // Skype-style names and addresses are allowed after callto: too.
        "callto" => phone_number(&rest).or_else(|| sip_target(&rest, false)),
        _ => sip_target(&rest, true),
    }
}

/// The part before any `;parameters` or `?headers`.
fn cut_parameters(text: &str) -> &str {
    text.split([';', '?']).next().unwrap_or_default()
}

/// Digits, `+`, `*` and `#` of a written number; spaces, dashes, dots and
/// brackets are only there for reading. Anything else is not a number.
fn phone_number(text: &str) -> Option<String> {
    let text = text.trim();
    let allowed = |c: char| c.is_ascii_digit() || " +*#-.()/".contains(c);
    let number: String = text
        .chars()
        .filter(|c| c.is_ascii_digit() || matches!(c, '+' | '*' | '#'))
        .collect();
    (!number.is_empty() && text.chars().all(allowed)).then_some(number)
}

/// `user` or `user@host[:port]`. In a `sip:` link (`with_password`) a
/// password after the user (`user:secret@host`) is dropped, never used.
fn sip_target(text: &str, with_password: bool) -> Option<String> {
    let text = text.trim();
    let (user, host) = match text.split_once('@') {
        Some((user, host)) => (user, Some(host)),
        None => (text, None),
    };
    let user = if with_password {
        user.split(':').next().unwrap_or_default()
    } else {
        user
    };
    let word = |part: &str| {
        !part.is_empty()
            && part
                .chars()
                .all(|c| c.is_alphanumeric() || "+*#-_.!~".contains(c))
    };
    // A number written into an address loses its separators too.
    let user = match phone_number(user) {
        Some(number) => number,
        None if word(user) => user.to_owned(),
        None => return None,
    };
    let Some(host) = host else {
        return Some(user);
    };
    let (name, port) = match host.split_once(':') {
        Some((name, port)) => (name, Some(port)),
        None => (host, None),
    };
    let port_fine = port.is_none_or(|port| !port.is_empty() && port.parse::<u16>().is_ok());
    (word(name) && port_fine).then(|| format!("{user}@{host}"))
}

/// Undoes `%XX` escapes; a broken escape stays as written.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && let Some(hex) = text.get(index + 1..index + 3)
            && let Ok(byte) = u8::from_str_radix(hex, 16)
        {
            out.push(byte);
            index += 3;
            continue;
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// What a second copy of the program asks the running one to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// Come to the front.
    Show,
    /// Call this number or address (and come to the front).
    Dial(String),
}

impl Request {
    fn to_line(&self, key: &str) -> String {
        match self {
            Request::Show => format!("{key} SHOW\n"),
            Request::Dial(target) => format!("{key} DIAL {target}\n"),
        }
    }

    fn from_line(line: &str, key: &str) -> Option<Self> {
        let rest = line.trim_end().strip_prefix(key)?.strip_prefix(' ')?;
        match rest.split_once(' ') {
            Some(("DIAL", target)) => {
                // Only what a link could have given: never an arbitrary text.
                target_from_argument(target).map(Request::Dial)
            }
            None if rest == "SHOW" => Some(Request::Show),
            _ => None,
        }
    }
}

/// The file the running copy announces itself in.
pub fn instance_file(settings_dir: &Path) -> PathBuf {
    settings_dir.join("instance.lock")
}

/// This copy is the only one running: it takes requests from later ones.
pub struct Instance {
    requests: Receiver<Request>,
    file: PathBuf,
    key: String,
}

impl Instance {
    /// Requests that have come since the last look.
    pub fn requests(&self) -> Vec<Request> {
        self.requests.try_iter().collect()
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        // Only our own announcement is removed: a newer copy may have
        // replaced it.
        if fs::read_to_string(&self.file).is_ok_and(|text| text.contains(&self.key)) {
            let _ = fs::remove_file(&self.file);
        }
    }
}

/// How starting the program went.
pub enum Start {
    /// No other copy runs; this one now takes requests.
    First(Instance),
    /// Another copy runs and has been given the request; this copy exits.
    Handed,
}

/// Becomes the running copy, or hands `request` to the copy that already
/// runs. `wake` is called (from another thread) when a request arrives, so
/// that the window can look at it. When anything about the file or socket
/// fails, the program simply runs on its own, as before.
pub fn start<F>(settings_dir: &Path, request: Option<Request>, wake: F) -> Start
where
    F: Fn() + Send + 'static,
{
    let file = instance_file(settings_dir);
    if let Ok(text) = fs::read_to_string(&file)
        && let Some((port, key)) = parse_announcement(&text)
        && send(port, &key, request.as_ref().unwrap_or(&Request::Show)).is_ok()
    {
        return Start::Handed;
    }
    match listen(&file, wake) {
        Ok(instance) => Start::First(instance),
        // Without the socket the phone still works; only links do not
        // reach it while it runs.
        Err(_) => {
            let (_, requests) = mpsc::channel();
            Start::First(Instance {
                requests,
                file,
                key: String::new(),
            })
        }
    }
}

fn parse_announcement(text: &str) -> Option<(u16, String)> {
    let mut words = text.split_whitespace();
    let port = words.next()?.parse().ok()?;
    let key = words.next()?.to_owned();
    (key.len() >= 16).then_some((port, key))
}

/// Gives the running copy a request; fails when nothing answers.
fn send(port: u16, key: &str, request: &Request) -> std::io::Result<()> {
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(2))?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.write_all(request.to_line(key).as_bytes())?;
    let mut answer = String::new();
    BufReader::new(stream).read_line(&mut answer)?;
    if answer.trim() == "OK" {
        Ok(())
    } else {
        Err(std::io::Error::other(
            "the running copy refused the request",
        ))
    }
}

fn listen<F>(file: &Path, wake: F) -> std::io::Result<Instance>
where
    F: Fn() + Send + 'static,
{
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let port = listener.local_addr()?.port();
    let key = new_key()?;
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(file, format!("{port} {key}\n"))?;
    let (sender, requests) = mpsc::channel();
    let expected = key.clone();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else {
                continue;
            };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            let mut line = String::new();
            // One short line per connection; anything longer is not ours.
            let read = BufReader::new((&stream).take(512)).read_line(&mut line);
            let answer = match read.ok().and_then(|_| Request::from_line(&line, &expected)) {
                Some(request) => {
                    if sender.send(request).is_err() {
                        return;
                    }
                    wake();
                    "OK\n"
                }
                None => "NO\n",
            };
            let _ = stream.write_all(answer.as_bytes());
        }
    });
    Ok(Instance {
        requests,
        file: file.to_owned(),
        key,
    })
}

fn new_key() -> std::io::Result<String> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(|error| std::io::Error::other(error.to_string()))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// The `reg.exe` commands (their arguments) that make Windows open the
/// link schemes with `program`, or that undo it. The program is registered
/// like other applications: a link class, its capabilities for the
/// "Default apps" settings, and the schemes themselves for the current
/// user. Windows may still keep an earlier choice of the user; that is
/// changed in "Default apps".
pub fn registry_commands(program: &str, enable: bool) -> Vec<Vec<String>> {
    let open = format!("\"{program}\" \"%1\"");
    let icon = format!("\"{program}\",0");
    let mut commands = Vec::new();
    let add = |key: &str, value: Option<&str>, data: &str| -> Vec<String> {
        let mut command = vec!["add".to_owned(), key.to_owned()];
        match value {
            Some(name) => command.extend(["/v".to_owned(), name.to_owned()]),
            None => command.push("/ve".to_owned()),
        }
        command.extend([
            "/t".to_owned(),
            "REG_SZ".to_owned(),
            "/d".to_owned(),
            data.to_owned(),
            "/f".to_owned(),
        ]);
        command
    };
    let delete = |key: &str, value: Option<&str>| -> Vec<String> {
        let mut command = vec!["delete".to_owned(), key.to_owned()];
        if let Some(name) = value {
            command.extend(["/v".to_owned(), name.to_owned()]);
        }
        command.push("/f".to_owned());
        command
    };
    if enable {
        commands.push(add(LINK_CLASS, None, "URL:MicroSIP-Rust"));
        commands.push(add(LINK_CLASS, Some("URL Protocol"), ""));
        commands.push(add(&format!("{LINK_CLASS}\\DefaultIcon"), None, &icon));
        commands.push(add(
            &format!("{LINK_CLASS}\\shell\\open\\command"),
            None,
            &open,
        ));
        commands.push(add(CAPABILITIES, Some("ApplicationName"), "MicroSIP-Rust"));
        commands.push(add(
            CAPABILITIES,
            Some("ApplicationDescription"),
            "SIP softphone",
        ));
        for scheme in SCHEMES {
            commands.push(add(
                &format!("{CAPABILITIES}\\URLAssociations"),
                Some(scheme),
                "MicroSIPRust.Link",
            ));
            let class = format!("{CLASSES}\\{scheme}");
            commands.push(add(&class, None, &format!("URL:{scheme}")));
            commands.push(add(&class, Some("URL Protocol"), ""));
            commands.push(add(&format!("{class}\\shell\\open\\command"), None, &open));
        }
        commands.push(add(
            REGISTERED_APPLICATIONS,
            Some("MicroSIP-Rust"),
            "Software\\MicroSIP-Rust\\Capabilities",
        ));
    } else {
        commands.push(delete(REGISTERED_APPLICATIONS, Some("MicroSIP-Rust")));
        commands.push(delete(CAPABILITIES_ROOT, None));
        commands.push(delete(LINK_CLASS, None));
        // The schemes' own keys are removed only where they still open
        // this program; see `owns_scheme`.
    }
    commands
}

/// Registry key of a scheme for the current user.
pub fn scheme_key(scheme: &str) -> String {
    format!("{CLASSES}\\{scheme}")
}

/// True when `reg query` output of a scheme's open command names `program`.
pub fn owns_scheme(query_output: &str, program: &str) -> bool {
    query_output
        .to_ascii_lowercase()
        .contains(&program.to_ascii_lowercase())
}

const CLASSES: &str = "HKCU\\Software\\Classes";
const LINK_CLASS: &str = "HKCU\\Software\\Classes\\MicroSIPRust.Link";
const CAPABILITIES_ROOT: &str = "HKCU\\Software\\MicroSIP-Rust";
const CAPABILITIES: &str = "HKCU\\Software\\MicroSIP-Rust\\Capabilities";
const REGISTERED_APPLICATIONS: &str = "HKCU\\Software\\RegisteredApplications";

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[test]
    fn links_and_command_line_words_become_numbers() {
        for (argument, target) in [
            ("tel:+7-495-123-45-67", Some("+74951234567")),
            ("tel:+7 (495) 123.45.67;ext=12", Some("+74951234567")),
            ("TEL:%2B74951234567", Some("+74951234567")),
            ("tel:2205", Some("2205")),
            ("tel:*21#", Some("*21#")),
            ("sip:2205@pbx.example.com", Some("2205@pbx.example.com")),
            (
                "sip:2205@pbx.example.com;transport=tcp?subject=x",
                Some("2205@pbx.example.com"),
            ),
            (
                "sips:alice@example.org:5061",
                Some("alice@example.org:5061"),
            ),
            ("sip:2205", Some("2205")),
            ("sip:alice", Some("alice")),
            ("sip:+7%20495%20123-45-67@pbx", Some("+74951234567@pbx")),
            ("sip:bob:secret@pbx", Some("bob@pbx")),
            ("callto:2205", Some("2205")),
            ("callto://2205", Some("2205")),
            ("callto:+7 495 123 45 67", Some("+74951234567")),
            ("callto:alice@example.org", Some("alice@example.org")),
            ("\"tel:2205\"", Some("2205")),
            ("2205", Some("2205")),
            ("+7 (495) 123-45-67", Some("+74951234567")),
            ("alice", Some("alice")),
            // Not something to call.
            ("--minimized", None),
            ("", None),
            ("tel:", None),
            ("tel:call-me", None),
            ("sip:", None),
            ("sip:@pbx", None),
            ("sip:a b@pbx", None),
            ("sip:2205@pb x", None),
            ("http://example.com", None),
            ("mailto:x@example.com", None),
            ("C:\\Users\\file.txt", None),
            ("2205; rm -rf", None),
        ] {
            assert_eq!(
                target_from_argument(argument).as_deref(),
                target,
                "{argument}"
            );
        }
    }

    #[test]
    fn requests_need_the_key_and_a_proper_target() {
        let key = "0123456789abcdef0123456789abcdef";
        for request in [Request::Show, Request::Dial("+74951234567".to_owned())] {
            assert_eq!(
                Request::from_line(&request.to_line(key), key),
                Some(request)
            );
        }
        assert_eq!(Request::from_line("wrong SHOW\n", key), None);
        assert_eq!(Request::from_line(&format!("{key}SHOW\n"), key), None);
        assert_eq!(
            Request::from_line(&format!("{key} DIAL a b c\n"), key),
            None
        );
        assert_eq!(Request::from_line(&format!("{key} HANGUP\n"), key), None);
        assert_eq!(Request::from_line(&format!("{key} DIAL \n"), key), None);
    }

    fn scratch_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("microsip-links-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_second_copy_hands_its_number_to_the_first() {
        let dir = scratch_dir("hand");
        let woken = Arc::new(AtomicU32::new(0));
        let counter = Arc::clone(&woken);
        let Start::First(first) = start(&dir, None, move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }) else {
            panic!("nothing else runs yet");
        };
        assert!(instance_file(&dir).exists());
        // The second copy, started for a link, hands it over.
        let request = Some(Request::Dial("2205".to_owned()));
        assert!(matches!(start(&dir, request, || {}), Start::Handed));
        // A copy started without a number only brings the first forward.
        assert!(matches!(start(&dir, None, || {}), Start::Handed));
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut got = Vec::new();
        while got.len() < 2 && std::time::Instant::now() < deadline {
            got.extend(first.requests());
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(got, [Request::Dial("2205".to_owned()), Request::Show]);
        assert_eq!(woken.load(Ordering::SeqCst), 2);

        // Somebody without the key gets nothing done.
        let text = fs::read_to_string(instance_file(&dir)).unwrap();
        let (port, _) = parse_announcement(&text).unwrap();
        let wrong = "ffffffffffffffffffffffffffffffff";
        assert!(send(port, wrong, &Request::Dial("2205".to_owned())).is_err());
        thread::sleep(Duration::from_millis(100));
        assert!(first.requests().is_empty());

        // When the first copy ends, the next one takes its place.
        drop(first);
        assert!(!instance_file(&dir).exists());
        assert!(matches!(start(&dir, None, || {}), Start::First(_)));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_announcement_left_by_a_crashed_copy_is_taken_over() {
        let dir = scratch_dir("stale");
        fs::create_dir_all(&dir).unwrap();
        // A port nothing listens on any more.
        let port = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        fs::write(
            instance_file(&dir),
            format!("{port} 0123456789abcdef0123456789abcdef\n"),
        )
        .unwrap();
        let Start::First(instance) = start(&dir, Some(Request::Dial("1".to_owned())), || {}) else {
            panic!("the old copy is gone");
        };
        let text = fs::read_to_string(instance_file(&dir)).unwrap();
        assert!(text.contains(&instance.key));
        drop(instance);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn windows_is_told_to_open_the_links_with_the_program() {
        let program = "C:\\Programs\\MicroSIP-Rust\\microsip-gui.exe";
        let commands = registry_commands(program, true);
        let open = format!("\"{program}\" \"%1\"");
        for scheme in SCHEMES {
            let key = format!("HKCU\\Software\\Classes\\{scheme}\\shell\\open\\command");
            assert!(
                commands
                    .iter()
                    .any(|command| command[1] == key && command.contains(&open)),
                "{scheme}"
            );
            assert!(commands.iter().any(|command| {
                command[1] == "HKCU\\Software\\MicroSIP-Rust\\Capabilities\\URLAssociations"
                    && command.contains(&scheme.to_owned())
            }));
        }
        assert!(commands.iter().all(|command| command[0] == "add"
            && command[1].starts_with("HKCU\\")
            && command.last().map(String::as_str) == Some("/f")));
        // Undoing never touches the machine-wide keys or other programs'.
        let undo = registry_commands(program, false);
        assert!(undo.iter().all(|command| command[0] == "delete"
            && command[1].starts_with("HKCU\\")
            && !command[1].ends_with("\\tel")));
        assert!(owns_scheme(
            &format!("(Default)    REG_SZ    {}", open.to_uppercase()),
            program
        ));
        assert!(!owns_scheme(
            "(Default)    REG_SZ    \"C:\\Skype.exe\" \"%1\"",
            program
        ));
    }
}
