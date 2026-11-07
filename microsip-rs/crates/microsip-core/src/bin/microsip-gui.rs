#![windows_subsystem = "windows"]

use eframe::egui;
use std::collections::VecDeque;
use std::io::{self, BufRead, BufReader, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

const LOG_LIMIT: usize = 1_000;

struct RunningPhone {
    child: Child,
    label: &'static str,
}

impl RunningPhone {
    fn command(&mut self, command: &str) -> io::Result<()> {
        let stdin = self.child.stdin.as_mut().ok_or_else(|| {
            io::Error::new(io::ErrorKind::BrokenPipe, "Process stdin is unavailable")
        })?;
        writeln!(stdin, "{command}")?;
        stdin.flush()
    }
}

struct PhoneWindow {
    local: String,
    remote: String,
    seconds: u32,
    process: Option<RunningPhone>,
    sender: Sender<String>,
    receiver: Receiver<String>,
    logs: VecDeque<String>,
    last_error: Option<String>,
}

impl PhoneWindow {
    fn new() -> Self {
        let (sender, receiver) = mpsc::channel();
        Self {
            local: "127.0.0.1:5070".to_owned(),
            remote: "127.0.0.1:5072".to_owned(),
            seconds: 30,
            process: None,
            sender,
            receiver,
            logs: VecDeque::new(),
            last_error: None,
        }
    }

    fn log(&mut self, text: impl Into<String>) {
        if self.logs.len() >= LOG_LIMIT {
            self.logs.pop_front();
        }
        self.logs.push_back(text.into());
    }

    fn error(&mut self, text: impl Into<String>) {
        let text = text.into();
        self.log(format!("ERROR: {text}"));
        self.last_error = Some(text);
    }

    fn executable(name: &str) -> io::Result<PathBuf> {
        let current = std::env::current_exe()?;
        let directory = current.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "Executable directory is unavailable",
            )
        })?;
        let executable = directory.join(name);
        if !executable.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("Missing companion executable: {}", executable.display()),
            ));
        }
        Ok(executable)
    }

    fn address(text: &str) -> Result<SocketAddr, String> {
        let address = text
            .trim()
            .parse::<SocketAddr>()
            .map_err(|error| format!("Invalid IP:port: {error}"))?;
        if address.ip().is_unspecified() || address.port() == 0 {
            return Err("Use a concrete IP and a nonzero port".to_owned());
        }
        Ok(address)
    }

    fn start(&mut self, incoming: bool) -> Result<(), String> {
        if self.process.is_some() {
            return Err("A phone process is already running".to_owned());
        }
        let local = Self::address(&self.local)?;
        let name = if incoming {
            "phone-console.exe"
        } else {
            "microsip-rs.exe"
        };
        let executable = Self::executable(name).map_err(|error| error.to_string())?;
        let mut command = Command::new(executable);

        if incoming {
            command.arg(local.to_string());
        } else {
            let remote = Self::address(&self.remote)?;
            if remote.is_ipv4() != local.is_ipv4() {
                return Err("Local and remote addresses must use the same IP family".to_owned());
            }
            command.args([
                "call".to_owned(),
                local.to_string(),
                remote.to_string(),
                self.seconds.to_string(),
            ]);
        }

        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }

        let mut child = command.spawn().map_err(|error| error.to_string())?;
        let label = if incoming { "incoming" } else { "outgoing" };
        if let Some(stdout) = child.stdout.take() {
            read_output(stdout, self.sender.clone(), label);
        }
        if let Some(stderr) = child.stderr.take() {
            read_output(stderr, self.sender.clone(), "stderr");
        }
        self.process = Some(RunningPhone { child, label });
        self.last_error = None;
        self.log(format!("Started {label} phone on {local}"));
        Ok(())
    }

    fn send_command(&mut self, command: &str) {
        let result = match self.process.as_mut() {
            Some(process) if process.label == "incoming" => process.command(command),
            _ => {
                self.error("This command requires the incoming phone process");
                return;
            }
        };
        match result {
            Ok(()) => self.log(format!("> {command}")),
            Err(error) => self.error(error.to_string()),
        }
    }

    fn poll(&mut self) {
        while let Ok(line) = self.receiver.try_recv() {
            self.log(line);
        }
        let state = self
            .process
            .as_mut()
            .map(|process| process.child.try_wait());
        match state {
            Some(Ok(Some(status))) => {
                self.process = None;
                self.log(format!("Phone process exited: {status}"));
                if !status.success() {
                    self.last_error = Some(format!("Phone process failed: {status}. See log."));
                }
            }
            Some(Err(error)) => self.error(format!("Cannot inspect process: {error}")),
            _ => {}
        }
    }
}

fn read_output<R>(reader: R, sender: Sender<String>, label: &'static str)
where
    R: io::Read + Send + 'static,
{
    thread::spawn(move || {
        for line in BufReader::new(reader).lines() {
            match line {
                Ok(line) => {
                    if sender.send(format!("[{label}] {line}")).is_err() {
                        break;
                    }
                }
                Err(error) => {
                    if let Err(send_error) = sender.send(format!("[{label}] Read error: {error}")) {
                        eprintln!("Cannot report output error: {send_error}");
                    }
                    break;
                }
            }
        }
    });
}

impl eframe::App for PhoneWindow {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll();
        ctx.request_repaint_after(Duration::from_millis(50));
        let busy = self.process.is_some();
        let incoming = self
            .process
            .as_ref()
            .is_some_and(|process| process.label == "incoming");

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("MicroSIP Rust — direct-call preview");
            ui.label("Direct UDP / PCMU. No provider registration, NAT or encryption yet.");
            ui.separator();

            ui.add_enabled_ui(!busy, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Local IP:port");
                    ui.text_edit_singleline(&mut self.local);
                });
                ui.horizontal(|ui| {
                    ui.label("Remote IP:port");
                    ui.text_edit_singleline(&mut self.remote);
                });
                ui.horizontal(|ui| {
                    ui.label("Outgoing duration, seconds");
                    ui.add(egui::Slider::new(&mut self.seconds, 1..=300));
                });
                ui.horizontal(|ui| {
                    if ui.button("Listen for incoming calls").clicked()
                        && let Err(error) = self.start(true)
                    {
                        self.error(error);
                    }
                    if ui.button("Call").clicked()
                        && let Err(error) = self.start(false)
                    {
                        self.error(error);
                    }
                });
            });

            ui.add_enabled_ui(incoming, |ui| {
                ui.horizontal(|ui| {
                    for (label, command) in [
                        ("Answer", "answer"),
                        ("Reject", "reject"),
                        ("Hang up", "hangup"),
                        ("Status", "status"),
                        ("Quit listener", "quit"),
                    ] {
                        if ui.button(label).clicked() {
                            self.send_command(command);
                        }
                    }
                });
            });

            if let Some(process) = self.process.as_ref() {
                ui.label(format!("Running: {}", process.label));
            } else {
                ui.label("No phone process running");
            }
            ui.label("Outgoing calls end after the selected duration or remote BYE.");
            ui.label("Close the call and quit the listener before closing this window.");
            if let Some(error) = self.last_error.as_ref() {
                ui.colored_label(egui::Color32::RED, error);
            }
            ui.separator();
            if ui.button("Clear log").clicked() {
                self.logs.clear();
            }
            egui::ScrollArea::vertical()
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    for line in &self.logs {
                        ui.monospace(line);
                    }
                });
        });
    }
}

impl Drop for PhoneWindow {
    fn drop(&mut self) {
        if let Some(mut process) = self.process.take() {
            match process.child.try_wait() {
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => {
                    // Emergency cleanup only: this is not a SIP hangup.
                    if let Err(error) = process.child.kill() {
                        eprintln!("Cannot stop phone process: {error}");
                    }
                    if let Err(error) = process.child.wait() {
                        eprintln!("Cannot wait for phone process: {error}");
                    }
                }
            }
        }
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([900.0, 600.0])
            .with_min_inner_size([700.0, 450.0]),
        ..Default::default()
    };
    eframe::run_native(
        "MicroSIP Rust preview",
        options,
        Box::new(|_creation_context| Ok(Box::new(PhoneWindow::new()))),
    )
}
