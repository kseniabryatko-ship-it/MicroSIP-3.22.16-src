#![windows_subsystem = "windows"]

use eframe::egui;
use microsip_core::call_media::AudioMedia;
use microsip_core::call_runner::{CallHandle, Event, UiCommand, spawn};
use microsip_core::call_view::{CallView, Phase};
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::time::Duration;

const LOG_LIMIT: usize = 1_000;

struct PhoneWindow {
    local: String,
    remote: String,
    handle: Option<CallHandle>,
    view: CallView,
    logs: VecDeque<String>,
    last_error: Option<String>,
}

impl PhoneWindow {
    fn new() -> Self {
        Self {
            local: "127.0.0.1:5070".to_owned(),
            remote: "127.0.0.1:5072".to_owned(),
            handle: None,
            view: CallView::default(),
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
        Ok(())
    }

    fn stop(&mut self) {
        if let Some(handle) = self.handle.take() {
            self.log("Stopping the phone; an active call is ended with BYE.");
            // Dropping the handle ends any call properly and joins the thread.
            drop(handle);
            self.view = CallView::default();
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
        loop {
            let Some(event) = self.handle.as_ref().and_then(|handle| handle.try_event()) else {
                break;
            };
            self.view.on_event(&event);
            match event {
                Event::Incoming { peer } => self.log(format!("Incoming call from {peer}")),
                Event::Ringing => self.log("Remote is ringing"),
                Event::Established => self.log("Connected"),
                Event::MediaStats { sent, received } => {
                    self.log(format!("RTP sent: {sent}; RTP received: {received}"));
                }
                Event::Ended(reason) => self.log(format!("Call ended: {reason}")),
                Event::Error(message) => self.error(message),
            }
        }
    }
}

impl eframe::App for PhoneWindow {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll();
        ctx.request_repaint_after(Duration::from_millis(50));
        let running = self.handle.is_some();

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("MicroSIP Rust — direct-call preview");
            ui.label("Direct UDP / PCMU. No provider registration, NAT or encryption yet.");
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
            ui.label("Closing the window ends an active call with a proper BYE.");
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
