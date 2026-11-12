#![windows_subsystem = "windows"]

use eframe::egui;
use microsip_core::call_media::AudioMedia;
use microsip_core::call_runner::{CallHandle, Event, UiCommand, spawn};
use microsip_core::call_view::{CallView, Phase};
use microsip_core::registration::{Account, RegistrationState};
use microsip_core::settings::{self, Settings};
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

const LOG_LIMIT: usize = 1_000;

struct PhoneWindow {
    local: String,
    remote: String,
    server: String,
    user: String,
    password: String,
    domain: String,
    registration: Option<RegistrationState>,
    handle: Option<CallHandle>,
    view: CallView,
    logs: VecDeque<String>,
    last_error: Option<String>,
    settings_path: Option<PathBuf>,
    saved: Settings,
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
        let mut window = Self {
            local: saved.local.clone(),
            remote: saved.remote.clone(),
            server: saved.server.clone(),
            user: saved.user.clone(),
            password: String::new(),
            domain: saved.domain.clone(),
            registration: None,
            handle: None,
            view: CallView::default(),
            logs: VecDeque::new(),
            last_error: None,
            settings_path,
            saved,
        };
        for note in notes {
            window.log(note);
        }
        window
    }

    /// Remembers the address and account fields; the password is left out.
    fn save_settings(&mut self) {
        let current = Settings {
            local: self.local.trim().to_owned(),
            remote: self.remote.trim().to_owned(),
            server: self.server.trim().to_owned(),
            domain: self.domain.trim().to_owned(),
            user: self.user.trim().to_owned(),
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
        let handle = spawn(local, "phone", AudioMedia::new)
            .map_err(|error| format!("Cannot listen on {local}: {error}"))?;
        self.log(format!("Phone listening on {}", handle.local_address()));
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
        self.save_settings();
        let account = Account::new(self.server.trim(), self.user.trim(), self.password.clone())
            .with_domain(self.domain.trim());
        // The password is never written to the log.
        self.log(format!(
            "> register {} on {}",
            self.user.trim(),
            self.server.trim()
        ));
        self.send(UiCommand::Register(account));
    }

    fn unregister(&mut self) {
        if self.can_unregister() {
            self.log("> unregister");
            self.send(UiCommand::Unregister);
        }
    }

    fn call(&mut self) {
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
                Event::Ended(reason) => self.log(format!("Call ended: {reason}")),
                Event::Registration(state) => {
                    self.log(format!("Account: {state}"));
                    self.registration = Some(state);
                }
                Event::Error(message) => self.error(message),
            }
        }
    }
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
            ui.label("Direct UDP / PCMU. Registration on a SIP server; no NAT or encryption yet.");
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
                    ui.label("Password");
                    ui.add(egui::TextEdit::singleline(&mut self.password).password(true));
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
                "To register, Local must be this computer's network address, not 127.0.0.1. \
                 The server can then send calls here; outgoing calls still go to Remote IP:port.",
            );
            ui.separator();

            ui.add_enabled_ui(running && self.view.can_dial(), |ui| {
                ui.horizontal(|ui| {
                    ui.label("Remote IP:port");
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
            });

            if running {
                let phase = self.view.phase();
                match (phase, self.view.peer()) {
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
