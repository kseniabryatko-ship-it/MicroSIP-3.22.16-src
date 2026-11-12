#![windows_subsystem = "windows"]

use eframe::egui;
use microsip_core::audio::{input_device_names, output_device_names};
use microsip_core::call_media::{AudioControls, AudioMedia};
use microsip_core::call_runner::{CallHandle, Event, UiCommand, spawn};
use microsip_core::call_view::{CallView, Phase};
use microsip_core::registration::{Account, RegistrationState};
use microsip_core::settings::{self, Settings};
use microsip_core::transport::{local_ip_towards, resolve_socket_address};
use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;

const LOG_LIMIT: usize = 1_000;

struct PhoneWindow {
    local: String,
    remote: String,
    server: String,
    user: String,
    auth_user: String,
    password: String,
    show_password: bool,
    /// Device names as saved; empty means the system default.
    microphone: String,
    speaker: String,
    speaker_volume: u32,
    microphone_volume: u32,
    microphones: Vec<String>,
    speakers: Vec<String>,
    controls: AudioControls,
    muted: bool,
    domain: String,
    registration: Option<RegistrationState>,
    handle: Option<CallHandle>,
    view: CallView,
    logs: VecDeque<String>,
    last_error: Option<String>,
    settings_path: Option<PathBuf>,
    saved: Settings,
    /// SIP messages go here, next to the settings file, for diagnostics.
    trace_file: Option<File>,
    started: Instant,
}

impl PhoneWindow {
    fn new() -> Self {
        let settings_path = settings::default_path();
        let mut notes = Vec::new();
        let saved = match settings_path.as_deref().map(Settings::load) {
            Some(Ok(saved)) => saved,
            Some(Err(error)) => {
                notes.push(format!("Cannot read the settings file: {error}"));
                Settings::default()
            }
            None => {
                notes.push("No profile folder was found; settings are not saved.".to_owned());
                Settings::default()
            }
        };
        if let Some(path) = settings_path.as_ref() {
            notes.push(format!(
                "Settings file (the password is never saved): {}",
                path.display()
            ));
        }
        // A fresh trace for every run; the file never holds the password.
        let trace_path = settings_path
            .as_ref()
            .map(|path| path.with_file_name("sip-trace.log"));
        let trace_file = trace_path.as_ref().and_then(|path| {
            std::fs::create_dir_all(path.parent()?).ok()?;
            OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(path)
                .ok()
        });
        match (trace_path.as_ref(), trace_file.is_some()) {
            (Some(path), true) => notes.push(format!("SIP trace file: {}", path.display())),
            (Some(path), false) => notes.push(format!("Cannot write {}", path.display())),
            (None, _) => {}
        }
        let mut window = Self {
            local: saved.local.clone(),
            remote: saved.remote.clone(),
            server: saved.server.clone(),
            user: saved.user.clone(),
            auth_user: saved.auth_user.clone(),
            password: String::new(),
            show_password: false,
            microphone: saved.microphone.clone(),
            speaker: saved.speaker.clone(),
            speaker_volume: saved.speaker_volume,
            microphone_volume: saved.microphone_volume,
            microphones: input_device_names(),
            speakers: output_device_names(),
            controls: AudioControls::default(),
            muted: false,
            domain: saved.domain.clone(),
            registration: None,
            handle: None,
            view: CallView::default(),
            logs: VecDeque::new(),
            last_error: None,
            settings_path,
            saved,
            trace_file,
            started: Instant::now(),
        };
        for note in notes {
            window.log(note);
        }
        window.apply_audio_settings();
        window
    }

    /// Hands the chosen devices and volumes to the phone's audio. Volumes
    /// act at once; a device is used from the next call on.
    fn apply_audio_settings(&mut self) {
        self.controls.set_speaker_volume(self.speaker_volume);
        self.controls.set_microphone_volume(self.microphone_volume);
        self.controls
            .set_input_device(Some(self.microphone.clone()));
        self.controls.set_output_device(Some(self.speaker.clone()));
    }

    /// Remembers the address and account fields; the password is left out.
    fn save_settings(&mut self) {
        let current = Settings {
            local: self.local.trim().to_owned(),
            remote: self.remote.trim().to_owned(),
            server: self.server.trim().to_owned(),
            domain: self.domain.trim().to_owned(),
            user: self.user.trim().to_owned(),
            auth_user: self.auth_user.trim().to_owned(),
            microphone: self.microphone.clone(),
            speaker: self.speaker.clone(),
            speaker_volume: self.speaker_volume,
            microphone_volume: self.microphone_volume,
        };
        if current == self.saved {
            return;
        }
        let Some(path) = self.settings_path.clone() else {
            return;
        };
        match current.save(&path) {
            Ok(()) => self.saved = current,
            Err(error) => self.log(format!("Cannot save the settings: {error}")),
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

    fn start(&mut self) -> Result<(), String> {
        if self.handle.is_some() {
            return Err("The phone is already running".to_owned());
        }
        let local = Self::address(&self.local)?;
        let controls = self.controls.clone();
        let handle = spawn(local, "phone", move || AudioMedia::with_controls(controls))
            .map_err(|error| format!("Cannot listen on {local}: {error}"))?;
        self.log(format!("Phone listening on {}", handle.local_address()));
        handle.send(UiCommand::Trace(self.trace_file.is_some()));
        self.handle = Some(handle);
        self.view = CallView::default();
        self.last_error = None;
        self.save_settings();
        Ok(())
    }

    fn stop(&mut self) {
        if let Some(handle) = self.handle.take() {
            self.log("Stopping the phone; an active call is ended with BYE.");
            // Dropping the handle ends any call properly and joins the thread.
            drop(handle);
            self.view = CallView::default();
            self.registration = None;
            self.log("Phone stopped.");
        }
    }

    fn send(&mut self, command: UiCommand) {
        let delivered = self
            .handle
            .as_ref()
            .is_some_and(|handle| handle.send(command));
        if !delivered {
            self.error("The phone has stopped unexpectedly; start it again.");
            self.handle = None;
            self.view = CallView::default();
            self.registration = None;
        }
    }

    fn can_register(&self) -> bool {
        !matches!(
            self.registration,
            Some(RegistrationState::Registering | RegistrationState::Registered { .. })
        )
    }

    fn can_unregister(&self) -> bool {
        !matches!(
            self.registration,
            None | Some(RegistrationState::Unregistered)
        )
    }

    fn register(&mut self) {
        if !self.can_register() {
            return;
        }
        if self.server.trim().is_empty() || self.user.trim().is_empty() {
            return self.error("Enter the SIP server and the user name");
        }
        self.last_error = None;
        let account = Account::new(self.server.trim(), self.user.trim(), "");
        if let Err(error) = self.use_address_that_reaches(&account) {
            return self.error(error);
        }
        self.save_settings();
        // A password pasted from elsewhere often carries a stray space or
        // line break at either end; the server would refuse it.
        let account = Account::new(self.server.trim(), self.user.trim(), self.password.trim())
            .with_domain(self.domain.trim())
            .with_auth_username(self.auth_user.trim());
        // The password is never written to the log.
        self.log(format!(
            "> register {} on {}",
            self.user.trim(),
            self.server.trim()
        ));
        self.send(UiCommand::Register(account));
    }

    /// A phone started on 127.0.0.1 cannot talk to a server on another
    /// machine. Like MicroSIP, pick the right network address automatically:
    /// restart the phone on the address the system uses to reach the server.
    fn use_address_that_reaches(&mut self, account: &Account) -> Result<(), String> {
        let local = Self::address(&self.local)?;
        if !local.ip().is_loopback() {
            return Ok(());
        }
        let (host, port) = account.server_host_port()?;
        let server = resolve_socket_address(&host, port)
            .map_err(|error| format!("Cannot resolve {host}: {error}"))?;
        if server.ip().is_loopback() {
            return Ok(());
        }
        if !self.view.can_dial() {
            return Err("Finish the call before registering".to_owned());
        }
        let ip = local_ip_towards(server)
            .map_err(|error| format!("No network route to {server}: {error}"))?;
        if ip.is_loopback() || ip.is_unspecified() {
            return Err(format!("No network address can reach {server}"));
        }
        let address = SocketAddr::new(ip, local.port());
        self.log(format!(
            "Local address changed from {local} to {address} to reach the server"
        ));
        self.stop();
        self.local = address.to_string();
        self.start()
    }

    fn unregister(&mut self) {
        if self.can_unregister() {
            self.log("> unregister");
            self.send(UiCommand::Unregister);
        }
    }

    fn call(&mut self) {
        let target = self.remote.trim().to_owned();
        if target.parse::<SocketAddr>().is_err() {
            return self.call_number(target);
        }
        let local = match Self::address(&self.local) {
            Ok(local) => local,
            Err(error) => return self.error(error),
        };
        let remote = match Self::address(&self.remote) {
            Ok(remote) => remote,
            Err(error) => return self.error(error),
        };
        if remote.is_ipv4() != local.is_ipv4() {
            return self.error("Local and remote addresses must use the same IP family");
        }
        if !self.view.can_dial() {
            return;
        }
        self.last_error = None;
        self.save_settings();
        self.view.dialing(remote);
        self.log(format!("Calling {remote}"));
        self.send(UiCommand::Dial(remote));
    }

    /// Anything that is not `IP:port` is a number on the SIP server.
    fn call_number(&mut self, target: String) {
        if target.is_empty() {
            return self.error("Enter IP:port or a number");
        }
        if !matches!(
            self.registration,
            Some(RegistrationState::Registered { .. })
        ) {
            return self.error("Register on a SIP server to call a number, or enter IP:port");
        }
        if !self.view.can_dial() {
            return;
        }
        self.last_error = None;
        self.save_settings();
        self.view.dialing_target(&target);
        self.log(format!("Calling {target} through the SIP server"));
        self.send(UiCommand::DialTarget(target));
    }

    fn answer(&mut self) {
        if self.view.can_answer() {
            self.view.answering();
            self.log("> answer");
            self.send(UiCommand::Answer);
        }
    }

    fn reject(&mut self) {
        if self.view.can_reject() {
            self.log("> reject");
            self.send(UiCommand::Reject);
        }
    }

    fn hangup(&mut self) {
        if self.view.can_hangup() {
            self.log("> hang up");
            self.send(UiCommand::Hangup);
        }
    }

    fn toggle_mute(&mut self) {
        if self.view.phase() != Phase::Connected {
            return;
        }
        self.muted = !self.muted;
        self.log(if self.muted {
            "> mute: the other side does not hear you"
        } else {
            "> unmute"
        });
        self.send(UiCommand::Mute(self.muted));
    }

    fn toggle_hold(&mut self) {
        if self.view.can_hold() {
            let hold = !self.view.is_held();
            self.log(if hold { "> hold" } else { "> resume" });
            self.send(UiCommand::Hold(hold));
        }
    }

    fn dtmf(&mut self, digit: char) {
        if self.view.phase() == Phase::Connected {
            self.log(format!("> digit {digit}"));
            self.send(UiCommand::Dtmf(digit));
        }
    }

    fn poll(&mut self) {
        while let Some(event) = self.handle.as_ref().and_then(|handle| handle.try_event()) {
            self.view.on_event(&event);
            match event {
                Event::Incoming { peer } => self.log(format!("Incoming call from {peer}")),
                Event::Ringing => self.log("Remote is ringing"),
                Event::Established => self.log("Connected"),
                Event::MediaStats { sent, received } => {
                    self.log(format!("RTP sent: {sent}; RTP received: {received}"));
                }
                Event::AudioQuality(quality) => self.log(format!("Audio: {quality}")),
                Event::Hold(true) => self.log("Call is on hold"),
                Event::Hold(false) => self.log("Call resumed"),
                Event::RemoteHold(true) => self.log("The other side put the call on hold"),
                Event::RemoteHold(false) => self.log("The other side resumed the call"),
                Event::Ended(reason) => {
                    // Every call starts with the microphone on.
                    self.muted = false;
                    self.log(format!("Call ended: {reason}"));
                }
                Event::Registration(state) => {
                    self.log(format!("Account: {state}"));
                    self.registration = Some(state);
                }
                Event::Trace {
                    outgoing,
                    peer,
                    text,
                } => self.trace(outgoing, peer, &text),
                Event::Error(message) => self.error(message),
            }
        }
    }

    fn trace(&mut self, outgoing: bool, peer: SocketAddr, text: &str) {
        let Some(file) = self.trace_file.as_mut() else {
            return;
        };
        let arrow = if outgoing { "SENT TO" } else { "RECEIVED FROM" };
        let seconds = self.started.elapsed().as_secs_f64();
        let written = writeln!(
            file,
            "===== {seconds:9.3} s  {arrow} {peer} =====\n{}\n",
            text.trim_end()
        )
        .and_then(|()| file.flush());
        if written.is_err() {
            self.trace_file = None;
            self.log("Cannot write the SIP trace file; tracing stopped.");
        }
    }
}

/// A drop-down list of devices; an empty choice is the system default.
fn device_picker(ui: &mut egui::Ui, label: &str, chosen: &mut String, names: &[String]) {
    let shown = if chosen.is_empty() {
        "System default".to_owned()
    } else {
        chosen.clone()
    };
    egui::ComboBox::from_label(label)
        .selected_text(shown)
        .width(260.0)
        .show_ui(ui, |ui| {
            ui.selectable_value(chosen, String::new(), "System default");
            for name in names {
                ui.selectable_value(chosen, name.clone(), name);
            }
        });
}

impl Drop for PhoneWindow {
    fn drop(&mut self) {
        // Closing the window keeps whatever was typed last.
        self.save_settings();
    }
}

impl eframe::App for PhoneWindow {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll();
        ctx.request_repaint_after(Duration::from_millis(50));
        let running = self.handle.is_some();

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("MicroSIP Rust — direct-call preview");
            ui.label("UDP / PCMU. Direct calls and calls through a SIP server; basic NAT, no encryption yet.");
            ui.separator();

            ui.add_enabled_ui(!running, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Local IP:port");
                    ui.text_edit_singleline(&mut self.local);
                });
            });
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!running, egui::Button::new("Start phone"))
                    .clicked()
                    && let Err(error) = self.start()
                {
                    self.error(error);
                }
                if ui
                    .add_enabled(running, egui::Button::new("Stop phone"))
                    .clicked()
                {
                    self.stop();
                }
            });

            ui.separator();
            let editable = running && self.can_register();
            ui.add_enabled_ui(editable, |ui| {
                ui.horizontal(|ui| {
                    ui.label("SIP server");
                    ui.text_edit_singleline(&mut self.server);
                    ui.label("Domain (optional)");
                    ui.text_edit_singleline(&mut self.domain);
                });
                ui.horizontal(|ui| {
                    ui.label("User");
                    ui.text_edit_singleline(&mut self.user);
                    ui.label("Login (if different)");
                    ui.text_edit_singleline(&mut self.auth_user);
                });
                ui.horizontal(|ui| {
                    ui.label("Password");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.password)
                            .password(!self.show_password),
                    );
                    ui.checkbox(&mut self.show_password, "Show");
                    ui.label(format!("{} characters", self.password.trim().chars().count()));
                });
            });
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(editable, egui::Button::new("Register"))
                    .clicked()
                {
                    self.register();
                }
                if ui
                    .add_enabled(
                        running && self.can_unregister(),
                        egui::Button::new("Unregister"),
                    )
                    .clicked()
                {
                    self.unregister();
                }
                match self.registration.as_ref() {
                    Some(state) => ui.label(format!("Account: {state}")),
                    None => ui.label("Account: not registered"),
                };
            });
            ui.label(
                "Register switches Local from 127.0.0.1 to this computer's network address \
                 by itself. Once registered, a number typed into Remote is called through \
                 the server.",
            );
            ui.separator();

            ui.add_enabled_ui(running && self.view.can_dial(), |ui| {
                ui.horizontal(|ui| {
                    ui.label("Remote IP:port or number");
                    ui.text_edit_singleline(&mut self.remote);
                });
            });

            ui.horizontal(|ui| {
                let view = self.view.clone();
                if ui
                    .add_enabled(running && view.can_dial(), egui::Button::new("Call"))
                    .clicked()
                {
                    self.call();
                }
                if ui
                    .add_enabled(running && view.can_answer(), egui::Button::new("Answer"))
                    .clicked()
                {
                    self.answer();
                }
                if ui
                    .add_enabled(running && view.can_reject(), egui::Button::new("Reject"))
                    .clicked()
                {
                    self.reject();
                }
                if ui
                    .add_enabled(running && view.can_hangup(), egui::Button::new("Hang up"))
                    .clicked()
                {
                    self.hangup();
                }
                let connected = running && view.phase() == Phase::Connected;
                let label = if self.muted { "Unmute" } else { "Mute" };
                if ui
                    .add_enabled(connected, egui::Button::new(label))
                    .clicked()
                {
                    self.toggle_mute();
                }
                let label = if view.is_held() { "Resume" } else { "Hold" };
                if ui
                    .add_enabled(running && view.can_hold(), egui::Button::new(label))
                    .clicked()
                {
                    self.toggle_hold();
                }
                if self.muted {
                    ui.colored_label(egui::Color32::YELLOW, "Microphone is off");
                }
                if view.is_held() {
                    ui.colored_label(egui::Color32::YELLOW, "On hold");
                }
                if view.is_remote_held() {
                    ui.colored_label(egui::Color32::YELLOW, "Held by the other side");
                }
            });

            // Sound devices and volumes.
            ui.horizontal(|ui| {
                let before = (self.microphone.clone(), self.speaker.clone());
                device_picker(ui, "Microphone", &mut self.microphone, &self.microphones);
                device_picker(ui, "Speaker", &mut self.speaker, &self.speakers);
                if ui.button("Refresh devices").clicked() {
                    self.microphones = input_device_names();
                    self.speakers = output_device_names();
                }
                if before != (self.microphone.clone(), self.speaker.clone()) {
                    self.apply_audio_settings();
                    self.save_settings();
                    if self.view.phase() != Phase::Idle {
                        self.log("The new sound device is used from the next call on.");
                    }
                }
            });
            ui.horizontal(|ui| {
                let before = (self.speaker_volume, self.microphone_volume);
                ui.label("Speaker volume");
                ui.add(egui::Slider::new(&mut self.speaker_volume, 0..=200).suffix("%"));
                ui.label("Microphone volume");
                ui.add(egui::Slider::new(&mut self.microphone_volume, 0..=200).suffix("%"));
                if before != (self.speaker_volume, self.microphone_volume) {
                    self.apply_audio_settings();
                }
            });

            // Keypad for menus such as "press 1 for ...", active during a call.
            let connected = running && self.view.phase() == Phase::Connected;
            ui.add_enabled_ui(connected, |ui| {
                for row in ["123", "456", "789", "*0#"] {
                    ui.horizontal(|ui| {
                        for digit in row.chars() {
                            let button = egui::Button::new(digit.to_string())
                                .min_size(egui::vec2(36.0, 24.0));
                            if ui.add(button).clicked() {
                                self.dtmf(digit);
                            }
                        }
                    });
                }
            });

            if running {
                let phase = self.view.phase();
                match (phase, self.view.peer_label()) {
                    (Phase::Idle, _) | (_, None) => {
                        ui.label(format!("Status: {}", phase.label()));
                    }
                    (_, Some(peer)) => {
                        ui.label(format!("Status: {} — {peer}", phase.label()));
                    }
                }
                if let Some((sent, received)) = self.view.stats() {
                    ui.label(format!("Last call RTP: sent {sent}, received {received}"));
                }
            } else {
                ui.label("The phone is not running.");
            }
            ui.label("Closing the window ends an active call with a proper BYE and removes the registration.");
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
