#![windows_subsystem = "windows"]

//! The phone's window: dial pad, call list, contacts and settings, in the
//! manner of MicroSIP. The phone connects to its account by itself at start.

use eframe::egui;
use microsip_core::audio::{input_device_names, output_device_names};
use microsip_core::call_media::{AudioControls, AudioMedia};
use microsip_core::call_runner::{CallHandle, EndReason, Event, TransferState, UiCommand, spawn};
use microsip_core::call_view::{CallView, Phase};
use microsip_core::contacts::{Contacts, dialable};
use microsip_core::history::{CallRecord, Direction, History};
use microsip_core::localtime::{
    DateTime, format_duration, format_timestamp, now_unix, utc_offset_seconds,
};
use microsip_core::recording::recording_file_name;
use microsip_core::registration::{Account, RegistrationState};
use microsip_core::secret;
use microsip_core::settings::{self, Settings, default_recordings_dir};
use microsip_core::transport::{local_ip_towards, resolve_socket_address};
use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const LOG_LIMIT: usize = 1_000;
const KEYPAD: [&str; 4] = ["123", "456", "789", "*0#"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Dialpad,
    Calls,
    Contacts,
}

/// What the user asked for in this frame; carried out after drawing.
enum Action {
    Call(String),
    Answer,
    Reject,
    Hangup,
    ToggleMute,
    ToggleHold,
    Digit(char),
    Connect,
    Disconnect,
    SaveContact(String, String),
    RemoveContact(String),
    ClearHistory,
    AudioChanged,
    ToggleRecording,
    Transfer(String),
    DoNotDisturb(bool),
    /// Puts the clipboard's text into the number field.
    Paste,
    /// Removes the last character of the number.
    Erase,
    /// Opens a file or folder with the program Windows uses for it.
    Open(PathBuf),
}

/// The call in progress, for the call list.
struct CurrentCall {
    incoming: bool,
    number: String,
    name: String,
    started: u64,
    connected: Option<Instant>,
    /// File of the call's recording; empty when it was not recorded.
    recording: String,
}

struct PhoneWindow {
    // Files.
    settings_path: Option<PathBuf>,
    history_path: Option<PathBuf>,
    contacts_path: Option<PathBuf>,
    trace_file: Option<File>,
    saved: Settings,
    history: History,
    contacts: Contacts,
    utc_offset: i64,
    started: Instant,

    // Account, as edited in the settings window.
    server: String,
    domain: String,
    user: String,
    auth_user: String,
    password: String,
    show_password: bool,
    save_password: bool,
    local_port: String,

    // Sound.
    microphone: String,
    speaker: String,
    speaker_volume: u32,
    microphone_volume: u32,
    microphones: Vec<String>,
    speakers: Vec<String>,
    controls: AudioControls,

    // Recording.
    record_calls: bool,
    recordings_dir: String,
    /// The file being written now.
    recording: Option<PathBuf>,

    // Call handling.
    do_not_disturb: bool,
    auto_answer: bool,
    auto_answer_delay: u32,
    /// When the ringing call is to be answered automatically.
    answer_at: Option<Instant>,
    transfer_to: String,
    /// A change of the do-not-disturb box, applied after drawing.
    pending_dnd: Option<bool>,
    /// The number was changed by a button: the typing cursor must follow.
    number_changed: bool,
    focus_number: bool,
    paste_requested: bool,

    // The phone.
    handle: Option<CallHandle>,
    local: Option<SocketAddr>,
    registration: Option<RegistrationState>,
    view: CallView,
    muted: bool,
    current: Option<CurrentCall>,
    caller: Option<(String, String)>,

    // The window.
    tab: Tab,
    number: String,
    contact_search: String,
    new_contact_name: String,
    new_contact_number: String,
    settings_open: bool,
    log_open: bool,
    logs: VecDeque<String>,
    last_error: Option<String>,
    /// Missed calls older than this have been seen.
    missed_seen: u64,
}

impl PhoneWindow {
    fn new() -> Self {
        let settings_path = settings::default_path();
        let sibling = |name: &str| settings_path.as_ref().map(|path| path.with_file_name(name));
        let history_path = sibling("history.tsv");
        let contacts_path = sibling("contacts.tsv");
        let trace_path = sibling("sip-trace.log");

        let mut notes = Vec::new();
        let saved = match settings_path.as_deref().map(Settings::load) {
            Some(Ok(saved)) => saved,
            Some(Err(error)) => {
                notes.push(format!("Не удалось прочитать настройки: {error}"));
                Settings::default()
            }
            None => {
                notes.push("Папка профиля не найдена; настройки не сохраняются.".to_owned());
                Settings::default()
            }
        };
        let history = history_path
            .as_deref()
            .and_then(|path| History::load(path).ok())
            .unwrap_or_default();
        let contacts = contacts_path
            .as_deref()
            .and_then(|path| Contacts::load(path).ok())
            .unwrap_or_default();
        // A fresh trace for every run; it never holds the password.
        let trace_file = trace_path.as_ref().and_then(|path| {
            std::fs::create_dir_all(path.parent()?).ok()?;
            OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(path)
                .ok()
        });
        if let Some(path) = settings_path.as_ref() {
            notes.push(format!("Файл настроек: {}", path.display()));
        }
        if let (Some(path), true) = (trace_path.as_ref(), trace_file.is_some()) {
            notes.push(format!("Журнал SIP: {}", path.display()));
        }

        let password = if saved.save_password && !saved.password_protected.is_empty() {
            secret::unprotect(&saved.password_protected).unwrap_or_else(|| {
                notes.push("Сохранённый пароль не подходит этому пользователю Windows.".to_owned());
                String::new()
            })
        } else {
            String::new()
        };

        let mut window = Self {
            server: saved.server.clone(),
            domain: saved.domain.clone(),
            user: saved.user.clone(),
            auth_user: saved.auth_user.clone(),
            password,
            show_password: false,
            save_password: saved.save_password,
            local_port: if saved.local_port == 0 {
                String::new()
            } else {
                saved.local_port.to_string()
            },
            microphone: saved.microphone.clone(),
            speaker: saved.speaker.clone(),
            speaker_volume: saved.speaker_volume,
            microphone_volume: saved.microphone_volume,
            microphones: input_device_names(),
            speakers: output_device_names(),
            controls: AudioControls::default(),
            record_calls: saved.record_calls,
            recordings_dir: saved.recordings_dir.clone(),
            recording: None,
            do_not_disturb: saved.do_not_disturb,
            auto_answer: saved.auto_answer,
            auto_answer_delay: saved.auto_answer_delay,
            answer_at: None,
            transfer_to: String::new(),
            pending_dnd: None,
            number_changed: false,
            focus_number: true,
            paste_requested: false,
            handle: None,
            local: None,
            registration: None,
            view: CallView::default(),
            muted: false,
            current: None,
            caller: None,
            tab: Tab::Dialpad,
            number: String::new(),
            contact_search: String::new(),
            new_contact_name: String::new(),
            new_contact_number: String::new(),
            settings_open: false,
            log_open: false,
            logs: VecDeque::new(),
            last_error: None,
            missed_seen: now_unix(),
            settings_path,
            history_path,
            contacts_path,
            trace_file,
            saved,
            history,
            contacts,
            utc_offset: utc_offset_seconds(),
            started: Instant::now(),
        };
        for note in notes {
            window.log(note);
        }
        window.apply_audio_settings();

        // Like MicroSIP: go online at once when an account is set up.
        if window.account_is_set() && !window.password.is_empty() {
            window.connect();
        } else {
            window.settings_open = true;
        }
        window
    }

    // ----- log and files -------------------------------------------------

    fn log(&mut self, text: impl Into<String>) {
        if self.logs.len() >= LOG_LIMIT {
            self.logs.pop_front();
        }
        let stamp = format_timestamp(now_unix(), self.utc_offset);
        self.logs.push_back(format!("{stamp}  {}", text.into()));
    }

    fn error(&mut self, text: impl Into<String>) {
        let text = text.into();
        self.log(format!("ОШИБКА: {text}"));
        self.last_error = Some(text);
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
            self.log("Не удалось записать журнал SIP; запись остановлена.");
        }
    }

    fn apply_audio_settings(&mut self) {
        self.controls.set_speaker_volume(self.speaker_volume);
        self.controls.set_microphone_volume(self.microphone_volume);
        self.controls
            .set_input_device(Some(self.microphone.clone()));
        self.controls.set_output_device(Some(self.speaker.clone()));
    }

    fn port(&self) -> u16 {
        self.local_port.trim().parse().unwrap_or(0)
    }

    /// Remembers everything typed. The password goes in encrypted, and only
    /// when the user allows it.
    fn save_settings(&mut self) {
        let password_protected = if self.save_password && !self.password.is_empty() {
            // Keep the stored form while the password is unchanged, so the
            // file is not rewritten at every start.
            match secret::unprotect(&self.saved.password_protected) {
                Some(stored) if stored == self.password.trim() => {
                    self.saved.password_protected.clone()
                }
                _ => secret::protect(self.password.trim()).unwrap_or_default(),
            }
        } else {
            String::new()
        };
        let current = Settings {
            local: self.saved.local.clone(),
            remote: self.number.trim().to_owned(),
            server: self.server.trim().to_owned(),
            domain: self.domain.trim().to_owned(),
            user: self.user.trim().to_owned(),
            auth_user: self.auth_user.trim().to_owned(),
            microphone: self.microphone.clone(),
            speaker: self.speaker.clone(),
            speaker_volume: self.speaker_volume,
            microphone_volume: self.microphone_volume,
            local_port: self.port(),
            save_password: self.save_password,
            password_protected,
            record_calls: self.record_calls,
            recordings_dir: self.recordings_dir.trim().to_owned(),
            do_not_disturb: self.do_not_disturb,
            auto_answer: self.auto_answer,
            auto_answer_delay: self.auto_answer_delay,
        };
        if current == self.saved {
            return;
        }
        let Some(path) = self.settings_path.clone() else {
            return;
        };
        match current.save(&path) {
            Ok(()) => self.saved = current,
            Err(error) => self.log(format!("Не удалось сохранить настройки: {error}")),
        }
    }

    fn save_history(&mut self) {
        if let Some(path) = self.history_path.clone()
            && let Err(error) = self.history.save(&path)
        {
            self.log(format!("Не удалось сохранить список звонков: {error}"));
        }
    }

    fn save_contacts(&mut self) {
        if let Some(path) = self.contacts_path.clone()
            && let Err(error) = self.contacts.save(&path)
        {
            self.log(format!("Не удалось сохранить контакты: {error}"));
        }
    }

    // ----- connecting ----------------------------------------------------

    fn account_is_set(&self) -> bool {
        !self.server.trim().is_empty() && !self.user.trim().is_empty()
    }

    /// Starts the phone on the network address that reaches the server and
    /// registers the account.
    fn connect(&mut self) {
        self.disconnect();
        self.last_error = None;
        if !self.account_is_set() {
            self.settings_open = true;
            return self.error("Укажите сервер и номер в настройках");
        }
        self.save_settings();
        let account = Account::new(self.server.trim(), self.user.trim(), self.password.trim())
            .with_domain(self.domain.trim())
            .with_auth_username(self.auth_user.trim());
        let server = match account.server_host_port().and_then(|(host, port)| {
            resolve_socket_address(&host, port)
                .map_err(|error| format!("сервер {host} не найден: {error}"))
        }) {
            Ok(server) => server,
            Err(error) => return self.error(format!("Не удалось подключиться: {error}")),
        };
        let ip = match local_ip_towards(server) {
            Ok(ip) if !ip.is_unspecified() => ip,
            Ok(_) | Err(_) => return self.error("Нет сети: сервер недоступен с этого компьютера"),
        };
        let local = SocketAddr::new(ip, self.port());
        let controls = self.controls.clone();
        let handle = match spawn(local, "phone", move || AudioMedia::with_controls(controls)) {
            Ok(handle) => handle,
            Err(error) => {
                return self.error(format!("Не удалось открыть порт {local}: {error}"));
            }
        };
        let local = handle.local_address();
        self.log(format!("Телефон запущен на {local}"));
        handle.send(UiCommand::Trace(self.trace_file.is_some()));
        handle.send(UiCommand::DoNotDisturb(self.do_not_disturb));
        handle.send(UiCommand::Register(account));
        self.local = Some(local);
        self.handle = Some(handle);
        self.view = CallView::default();
        self.registration = Some(RegistrationState::Registering);
    }

    fn disconnect(&mut self) {
        if let Some(handle) = self.handle.take() {
            // Dropping the handle ends a call and removes the registration.
            drop(handle);
            self.log("Телефон отключён");
        }
        self.finish_call(EndReason::Completed, false);
        self.local = None;
        self.registration = None;
        self.view = CallView::default();
        self.muted = false;
    }

    fn send(&mut self, command: UiCommand) {
        let delivered = self
            .handle
            .as_ref()
            .is_some_and(|handle| handle.send(command));
        if !delivered {
            self.handle = None;
            self.registration = None;
            self.view = CallView::default();
            self.error("Телефон не подключён. Нажмите «Сохранить и подключить» в настройках.");
        }
    }

    fn is_online(&self) -> bool {
        matches!(
            self.registration,
            Some(RegistrationState::Registered { .. })
        )
    }

    // ----- calls ---------------------------------------------------------

    fn call(&mut self, target: String) {
        // Brackets, spaces and dashes of a pasted number are not dialled.
        let target = dialable(&target);
        if target.is_empty() || !self.view.can_dial() {
            return;
        }
        if self.handle.is_none() {
            return self.error("Телефон не подключён");
        }
        let direct = target.parse::<SocketAddr>().is_ok();
        if !direct && !self.is_online() {
            return self.error("Нет регистрации на сервере — позвонить на номер нельзя");
        }
        self.last_error = None;
        self.number = target.clone();
        self.save_settings();
        let name = self.contacts.name_of(&target).unwrap_or("").to_owned();
        self.log(format!("Вызов {target}"));
        self.current = Some(CurrentCall {
            incoming: false,
            number: target.clone(),
            name,
            started: now_unix(),
            connected: None,
            recording: String::new(),
        });
        self.view.dialing_target(&target);
        self.send(UiCommand::DialTarget(target));
        self.tab = Tab::Dialpad;
    }

    fn answer(&mut self) {
        if self.view.can_answer() {
            self.view.answering();
            self.send(UiCommand::Answer);
        }
    }

    fn reject(&mut self) {
        if self.view.can_reject() {
            self.send(UiCommand::Reject);
        }
    }

    fn hangup(&mut self) {
        if self.view.can_hangup() {
            self.send(UiCommand::Hangup);
        }
    }

    fn toggle_mute(&mut self) {
        if self.view.phase() == Phase::Connected {
            self.muted = !self.muted;
            self.send(UiCommand::Mute(self.muted));
        }
    }

    fn toggle_hold(&mut self) {
        if self.view.can_hold() {
            self.send(UiCommand::Hold(!self.view.is_held()));
        }
    }

    /// Folder for recordings: the one from the settings, or the default.
    fn recordings_folder(&self) -> Option<PathBuf> {
        let chosen = self.recordings_dir.trim();
        if chosen.is_empty() {
            default_recordings_dir()
        } else {
            Some(PathBuf::from(chosen))
        }
    }

    fn start_recording(&mut self) {
        if self.view.phase() != Phase::Connected || self.recording.is_some() {
            return;
        }
        let Some(folder) = self.recordings_folder() else {
            return self.error("Не задана папка для записей");
        };
        let (incoming, number) = self
            .current
            .as_ref()
            .map_or((false, String::new()), |call| {
                (call.incoming, call.number.clone())
            });
        let time = DateTime::from_unix(now_unix(), self.utc_offset);
        let path = folder.join(recording_file_name(time, incoming, &number));
        self.send(UiCommand::Record(Some(path)));
    }

    fn toggle_recording(&mut self) {
        if self.recording.is_some() {
            self.send(UiCommand::Record(None));
        } else {
            self.start_recording();
        }
    }

    /// Opens a recording or the recordings folder.
    fn open(&mut self, path: PathBuf) {
        if !path.exists() {
            return self.error(format!("Не найдено: {}", path.display()));
        }
        let program = if cfg!(windows) {
            "explorer"
        } else {
            "xdg-open"
        };
        if let Err(error) = std::process::Command::new(program).arg(&path).spawn() {
            self.error(format!("Не удалось открыть {}: {error}", path.display()));
        }
    }

    /// A keypad press: a tone during a call, a digit of the number otherwise.
    fn digit(&mut self, digit: char) {
        if self.view.phase() == Phase::Connected {
            self.send(UiCommand::Dtmf(digit));
        } else if self.view.can_dial() {
            self.number.push(digit);
            self.number_changed = true;
        }
    }

    /// Ends the bookkeeping of the current call and, when `record` is set,
    /// writes it to the call list.
    fn finish_call(&mut self, reason: EndReason, record: bool) {
        self.muted = false;
        self.answer_at = None;
        self.transfer_to.clear();
        // The phone finishes the file itself when the call ends.
        self.recording = None;
        let Some(call) = self.current.take() else {
            return;
        };
        if !record {
            return;
        }
        let seconds = call
            .connected
            .map_or(0, |connected| connected.elapsed().as_secs());
        let direction = match (call.incoming, call.connected.is_some(), reason) {
            (false, _, _) => Direction::Outgoing,
            (true, true, _) => Direction::Incoming,
            // Declined by the user: seen, so not a missed call.
            (true, false, EndReason::Rejected(_)) => Direction::Incoming,
            (true, false, _) => Direction::Missed,
        };
        self.history.add(CallRecord {
            time: call.started,
            direction,
            seconds,
            number: call.number,
            name: call.name,
            recording: call.recording,
        });
        self.save_history();
    }

    fn poll(&mut self, ctx: &egui::Context) {
        while let Some(event) = self.handle.as_ref().and_then(|handle| handle.try_event()) {
            self.view.on_event(&event);
            match event {
                Event::Caller { number, name } => self.caller = Some((number, name)),
                Event::Incoming { peer } => {
                    let (number, name) = self
                        .caller
                        .take()
                        .unwrap_or_else(|| (peer.to_string(), String::new()));
                    // A saved contact name wins over what the network sent.
                    let name = self
                        .contacts
                        .name_of(&number)
                        .map(str::to_owned)
                        .unwrap_or(name);
                    self.log(format!("Входящий вызов: {number} {name}"));
                    self.current = Some(CurrentCall {
                        incoming: true,
                        number,
                        name,
                        started: now_unix(),
                        connected: None,
                        recording: String::new(),
                    });
                    self.tab = Tab::Dialpad;
                    self.answer_at = self.auto_answer.then(|| {
                        Instant::now() + Duration::from_secs(u64::from(self.auto_answer_delay))
                    });
                    // Bring the window to the user's attention.
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                    ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
                        egui::UserAttentionType::Critical,
                    ));
                }
                Event::Ringing => self.log("У абонента звонит телефон"),
                Event::Established => {
                    self.log("Соединено");
                    if let Some(call) = self.current.as_mut() {
                        call.connected = Some(Instant::now());
                    }
                    if self.record_calls {
                        self.start_recording();
                    }
                }
                Event::Recording(Some(path)) => {
                    self.log(format!("Запись разговора: {}", path.display()));
                    if let Some(call) = self.current.as_mut() {
                        call.recording = path.display().to_string();
                    }
                    self.recording = Some(path);
                }
                Event::Recording(None) => {
                    self.log("Запись остановлена");
                    self.recording = None;
                }
                Event::Transfer(TransferState::Accepted) => self.log("Перевод: вызываем номер…"),
                Event::Transfer(TransferState::Completed) => self.log("Перевод выполнен"),
                Event::Transfer(TransferState::Failed(status)) => {
                    self.error(format!(
                        "Перевод не удался (код {status}); разговор продолжается"
                    ));
                }
                Event::Declined { number, name } => {
                    // Refused by do-not-disturb: it still belongs in the list.
                    let name = self
                        .contacts
                        .name_of(&number)
                        .map(str::to_owned)
                        .unwrap_or(name);
                    self.log(format!("Не беспокоить: отклонён вызов {number} {name}"));
                    self.history.add(CallRecord {
                        time: now_unix(),
                        direction: Direction::Missed,
                        seconds: 0,
                        number,
                        name,
                        recording: String::new(),
                    });
                    self.save_history();
                }
                Event::MediaStats { sent, received } => {
                    self.log(format!("RTP: отправлено {sent}, принято {received}"));
                }
                Event::AudioQuality(quality) => self.log(format!("Звук: {quality}")),
                Event::Hold(true) => self.log("Вызов на удержании"),
                Event::Hold(false) => self.log("Вызов возобновлён"),
                Event::RemoteHold(true) => self.log("Собеседник поставил вызов на удержание"),
                Event::RemoteHold(false) => self.log("Собеседник возобновил вызов"),
                Event::Ended(reason) => {
                    self.log(format!("Вызов завершён: {}", end_reason_text(reason)));
                    self.finish_call(reason, true);
                }
                Event::Registration(state) => {
                    self.log(format!("Аккаунт: {}", registration_text(&state)));
                    self.registration = Some(state);
                }
                Event::Trace {
                    outgoing,
                    peer,
                    text,
                } => self.trace(outgoing, peer, &text),
                Event::Error(message) => self.error(error_text(&message)),
            }
        }
        // Auto-answer, once the chosen time of ringing has passed.
        if self.answer_at.is_some_and(|at| Instant::now() >= at) {
            self.answer_at = None;
            self.answer();
        }
    }

    fn apply(&mut self, action: Action) {
        match action {
            Action::Call(target) => self.call(target),
            Action::Answer => self.answer(),
            Action::Reject => self.reject(),
            Action::Hangup => self.hangup(),
            Action::ToggleMute => self.toggle_mute(),
            Action::ToggleHold => self.toggle_hold(),
            Action::Digit(digit) => self.digit(digit),
            Action::Connect => self.connect(),
            Action::Disconnect => {
                self.save_settings();
                self.disconnect();
            }
            Action::SaveContact(name, number) => {
                if self.contacts.set(&name, &number) {
                    self.save_contacts();
                    self.new_contact_name.clear();
                    self.new_contact_number.clear();
                } else {
                    self.error("Укажите имя и номер контакта");
                }
            }
            Action::RemoveContact(number) => {
                if self.contacts.remove(&number) {
                    self.save_contacts();
                }
            }
            Action::ClearHistory => {
                self.history.clear();
                self.save_history();
            }
            Action::AudioChanged => {
                self.apply_audio_settings();
                self.save_settings();
            }
            Action::ToggleRecording => self.toggle_recording(),
            Action::Transfer(target) => {
                if target.trim().is_empty() {
                    self.error("Введите номер, на который перевести");
                } else if self.view.phase() == Phase::Connected {
                    self.last_error = None;
                    self.log(format!("Перевод на {}", target.trim()));
                    self.send(UiCommand::Transfer(target.trim().to_owned()));
                }
            }
            Action::Paste => {
                // The field is focused and the system is asked to paste; the
                // text arrives in the next frame like a Ctrl+V would.
                self.focus_number = true;
                self.paste_requested = true;
            }
            Action::Erase => {
                self.number.pop();
                self.number_changed = true;
            }
            Action::DoNotDisturb(on) => {
                self.do_not_disturb = on;
                self.save_settings();
                if self.handle.is_some() {
                    self.send(UiCommand::DoNotDisturb(on));
                }
            }
            Action::Open(path) => self.open(path),
        }
    }

    // ----- drawing -------------------------------------------------------

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let (color, text) = match self.registration.as_ref() {
                Some(RegistrationState::Registered { .. }) => {
                    (egui::Color32::from_rgb(60, 180, 75), "В сети".to_owned())
                }
                Some(RegistrationState::Registering) => {
                    (egui::Color32::YELLOW, "Подключение…".to_owned())
                }
                Some(state @ RegistrationState::Failed(_)) => {
                    (egui::Color32::RED, registration_text(state))
                }
                Some(RegistrationState::Unregistered) | None => {
                    (egui::Color32::GRAY, "Не в сети".to_owned())
                }
            };
            // A drawn dot: the fonts have no glyph for a round bullet.
            let (dot, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
            ui.painter().circle_filled(dot.center(), 5.0, color);
            let user = self.user.trim();
            let status = if user.is_empty() {
                text
            } else {
                format!("{user} — {text}")
            };
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Журнал").clicked() {
                    self.log_open = !self.log_open;
                }
                if ui.button("Настройки").clicked() {
                    self.settings_open = !self.settings_open;
                }
                let mut dnd = self.do_not_disturb;
                let label = if dnd {
                    egui::RichText::new("Не беспокоить").color(egui::Color32::RED)
                } else {
                    egui::RichText::new("Не беспокоить")
                };
                if ui.checkbox(&mut dnd, label).changed() {
                    self.pending_dnd = Some(dnd);
                }
                // The status takes the room that is left and is cut short
                // rather than running under the buttons.
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.add(egui::Label::new(status).truncate());
                });
            });
        });
    }

    fn dialpad_tab(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let phase = self.view.phase();
        let idle = phase == Phase::Idle;

        let number_id = egui::Id::new("number-field");
        let field = egui::TextEdit::singleline(&mut self.number)
            .id(number_id)
            .hint_text("Номер")
            .font(egui::TextStyle::Heading)
            .desired_width(f32::INFINITY);
        let response = ui.add_enabled(idle, field);
        if response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
            actions.push(Action::Call(self.number.clone()));
        }
        // Like a telephone, the number field is where typing and pasting go:
        // it takes the focus back after a button was pressed, unless another
        // text field (search, contact, settings) is being typed into.
        let windows_open = self.settings_open || self.log_open;
        let typing_elsewhere = ui.ctx().wants_keyboard_input() && !response.has_focus();
        let button_has_focus =
            ui.memory(|memory| memory.focused().is_some()) && !ui.ctx().wants_keyboard_input();
        if idle
            && !windows_open
            && !typing_elsewhere
            && (self.focus_number || self.number_changed || button_has_focus)
        {
            response.request_focus();
        }
        if idle && !windows_open && ui.memory(|memory| memory.focused().is_none()) {
            response.request_focus();
        }
        self.focus_number = false;
        if self.number_changed {
            // Keep the typing cursor at the end after a keypad button.
            self.number_changed = false;
            if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), number_id) {
                let end = egui::text::CCursor::new(self.number.chars().count());
                state
                    .cursor
                    .set_char_range(Some(egui::text::CCursorRange::one(end)));
                state.store(ui.ctx(), number_id);
            }
        }
        if self.paste_requested && response.has_focus() {
            self.paste_requested = false;
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::RequestPaste);
        }
        if idle && let Some(name) = self.contacts.name_of(&self.number) {
            ui.label(name.to_owned());
        }
        ui.add_space(6.0);

        let size = egui::vec2((ui.available_width() - 16.0) / 3.0, 44.0);
        for row in KEYPAD {
            ui.horizontal(|ui| {
                for digit in row.chars() {
                    let label = egui::RichText::new(digit.to_string()).size(22.0);
                    if ui.add_sized(size, egui::Button::new(label)).clicked() {
                        actions.push(Action::Digit(digit));
                    }
                }
            });
        }
        ui.add_space(6.0);

        let wide = egui::vec2(ui.available_width(), 40.0);
        let half = egui::vec2((ui.available_width() - 8.0) / 2.0, 40.0);
        let green = egui::Color32::from_rgb(40, 140, 60);
        let red = egui::Color32::from_rgb(170, 50, 50);
        let big = |text: &str| {
            egui::RichText::new(text)
                .size(18.0)
                .color(egui::Color32::WHITE)
        };
        match phase {
            Phase::Idle => {
                ui.horizontal(|ui| {
                    let call = egui::Button::new(big("Позвонить")).fill(green);
                    if ui.add_sized(half, call).clicked() {
                        actions.push(Action::Call(self.number.clone()));
                    }
                    let quarter = egui::vec2((half.x - 8.0) / 2.0, half.y);
                    if ui
                        .add_sized(quarter, egui::Button::new("Вставить"))
                        .clicked()
                    {
                        actions.push(Action::Paste);
                    }
                    if ui
                        .add_sized(quarter, egui::Button::new("Стереть"))
                        .clicked()
                    {
                        actions.push(Action::Erase);
                    }
                });
            }
            Phase::Incoming => {
                ui.horizontal(|ui| {
                    let answer = egui::Button::new(big("Ответить")).fill(green);
                    if ui.add_sized(half, answer).clicked() {
                        actions.push(Action::Answer);
                    }
                    let reject = egui::Button::new(big("Отклонить")).fill(red);
                    if ui.add_sized(half, reject).clicked() {
                        actions.push(Action::Reject);
                    }
                });
            }
            _ => {
                let hangup = egui::Button::new(big("Завершить")).fill(red);
                let pressed = ui
                    .add_enabled_ui(self.view.can_hangup(), |ui| ui.add_sized(wide, hangup))
                    .inner
                    .clicked();
                if pressed {
                    actions.push(Action::Hangup);
                }
                let connected = phase == Phase::Connected;
                let mute = if self.muted {
                    "Включить микрофон"
                } else {
                    "Выключить микрофон"
                };
                let hold = if self.view.is_held() {
                    "Продолжить"
                } else {
                    "Удержание"
                };
                let record = if self.recording.is_some() {
                    "Остановить запись"
                } else {
                    "Записать разговор"
                };
                ui.add_enabled_ui(connected, |ui| {
                    ui.horizontal(|ui| {
                        if ui.add_sized(half, egui::Button::new(mute)).clicked() {
                            actions.push(Action::ToggleMute);
                        }
                        if ui.add_sized(half, egui::Button::new(hold)).clicked() {
                            actions.push(Action::ToggleHold);
                        }
                    });
                    if ui.add_sized(wide, egui::Button::new(record)).clicked() {
                        actions.push(Action::ToggleRecording);
                    }
                    ui.horizontal(|ui| {
                        let field = egui::TextEdit::singleline(&mut self.transfer_to)
                            .hint_text("Перевести на номер")
                            .desired_width(half.x);
                        let typed = ui.add(field);
                        let enter = typed.lost_focus()
                            && ui.input(|input| input.key_pressed(egui::Key::Enter));
                        if ui.button("Перевести").clicked() || enter {
                            actions.push(Action::Transfer(self.transfer_to.clone()));
                        }
                    });
                });
            }
        }

        ui.add_space(6.0);
        if let Some(call) = self.current.as_ref() {
            let who = if call.name.is_empty() {
                call.number.clone()
            } else {
                format!("{} ({})", call.name, call.number)
            };
            let state = match (phase, call.connected) {
                (Phase::Connected, Some(connected)) => {
                    format!(
                        "Разговор {}",
                        format_duration(connected.elapsed().as_secs())
                    )
                }
                _ => phase_text(phase).to_owned(),
            };
            ui.label(egui::RichText::new(who).size(18.0));
            ui.label(state);
            if let Some(at) = self.answer_at {
                let left = at.saturating_duration_since(Instant::now()).as_secs() + 1;
                ui.label(format!("Автоответ через {left} с"));
            }
            if self.recording.is_some() {
                ui.colored_label(egui::Color32::RED, "Идёт запись разговора");
            }
            if self.muted {
                ui.colored_label(egui::Color32::YELLOW, "Микрофон выключен");
            }
            if self.view.is_held() {
                ui.colored_label(egui::Color32::YELLOW, "Вызов на удержании");
            }
            if self.view.is_remote_held() {
                ui.colored_label(egui::Color32::YELLOW, "Собеседник поставил на удержание");
            }
        }
        if let Some(error) = self.last_error.as_ref() {
            ui.colored_label(egui::Color32::RED, error);
        }

        ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
            let before = (self.speaker_volume, self.microphone_volume);
            ui.horizontal(|ui| {
                ui.label("Микрофон");
                ui.add(egui::Slider::new(&mut self.microphone_volume, 0..=200).suffix("%"));
            });
            ui.horizontal(|ui| {
                ui.label("Динамик   ");
                ui.add(egui::Slider::new(&mut self.speaker_volume, 0..=200).suffix("%"));
            });
            if before != (self.speaker_volume, self.microphone_volume) {
                // Volumes act at once; they are saved when the window closes.
                self.apply_audio_settings();
            }
        });
    }

    fn calls_tab(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        self.missed_seen = now_unix();
        ui.horizontal(|ui| {
            ui.label(format!("Звонков: {}", self.history.records().len()));
            if !self.history.is_empty() && ui.button("Очистить").clicked() {
                actions.push(Action::ClearHistory);
            }
        });
        ui.separator();
        let can_dial = self.view.can_dial();
        let mut to_contacts = None;
        egui::ScrollArea::vertical().show(ui, |ui| {
            for (index, record) in self.history.records().iter().enumerate() {
                let (mark, color) = match record.direction {
                    Direction::Outgoing => ("Исх.", egui::Color32::LIGHT_BLUE),
                    Direction::Incoming => ("Вх.", egui::Color32::LIGHT_GREEN),
                    Direction::Missed => ("Проп.", egui::Color32::RED),
                };
                // A contact saved later names old calls too.
                let name = self
                    .contacts
                    .name_of(&record.number)
                    .unwrap_or(&record.name);
                let who = if name.is_empty() {
                    record.number.clone()
                } else {
                    format!("{name} ({})", record.number)
                };
                ui.push_id(index, |ui| {
                    ui.horizontal(|ui| {
                        ui.colored_label(color, mark);
                        ui.label(who);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .add_enabled(can_dial, egui::Button::new("Позвонить"))
                                .clicked()
                            {
                                actions.push(Action::Call(record.number.clone()));
                            }
                            if name.is_empty() && ui.button("В контакты").clicked() {
                                to_contacts = Some(record.number.clone());
                            }
                            if !record.recording.is_empty() && ui.button("Запись").clicked() {
                                actions.push(Action::Open(PathBuf::from(&record.recording)));
                            }
                        });
                    });
                    let length = if record.seconds > 0 {
                        format!(", {}", format_duration(record.seconds))
                    } else {
                        String::new()
                    };
                    ui.weak(format!(
                        "{}{length}",
                        format_timestamp(record.time, self.utc_offset)
                    ));
                    ui.separator();
                });
            }
        });
        if let Some(number) = to_contacts {
            self.new_contact_number = number;
            self.tab = Tab::Contacts;
        }
    }

    fn contacts_tab(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        ui.horizontal(|ui| {
            ui.label("Имя");
            ui.add(egui::TextEdit::singleline(&mut self.new_contact_name).desired_width(120.0));
            ui.label("Номер");
            ui.add(egui::TextEdit::singleline(&mut self.new_contact_number).desired_width(110.0));
        });
        if ui.button("Сохранить контакт").clicked() {
            actions.push(Action::SaveContact(
                self.new_contact_name.clone(),
                self.new_contact_number.clone(),
            ));
        }
        ui.separator();
        ui.add(
            egui::TextEdit::singleline(&mut self.contact_search)
                .hint_text("Поиск")
                .desired_width(f32::INFINITY),
        );
        let can_dial = self.view.can_dial();
        egui::ScrollArea::vertical().show(ui, |ui| {
            let found = self.contacts.matching(&self.contact_search);
            if found.is_empty() {
                ui.weak("Контактов нет");
            }
            for (index, contact) in found.into_iter().enumerate() {
                ui.push_id(index, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(&contact.name);
                        ui.weak(&contact.number);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("Удалить").clicked() {
                                actions.push(Action::RemoveContact(contact.number.clone()));
                            }
                            if ui
                                .add_enabled(can_dial, egui::Button::new("Позвонить"))
                                .clicked()
                            {
                                actions.push(Action::Call(contact.number.clone()));
                            }
                        });
                    });
                    ui.separator();
                });
            }
        });
    }

    fn settings_window(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let mut open = self.settings_open;
        egui::Window::new("Настройки")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.heading("Аккаунт");
                egui::Grid::new("account").num_columns(2).show(ui, |ui| {
                    ui.label("SIP-сервер");
                    ui.text_edit_singleline(&mut self.server);
                    ui.end_row();
                    ui.label("Номер (пользователь)");
                    ui.text_edit_singleline(&mut self.user);
                    ui.end_row();
                    ui.label("Пароль");
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.password)
                                .password(!self.show_password)
                                .desired_width(150.0),
                        );
                        ui.checkbox(&mut self.show_password, "Показать");
                    });
                    ui.end_row();
                    ui.label("Домен (если отличается)");
                    ui.text_edit_singleline(&mut self.domain);
                    ui.end_row();
                    ui.label("Логин (если отличается)");
                    ui.text_edit_singleline(&mut self.auth_user);
                    ui.end_row();
                    ui.label("Локальный порт");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.local_port)
                            .hint_text("автоматически")
                            .desired_width(100.0),
                    );
                    ui.end_row();
                });
                if secret::is_available() {
                    ui.checkbox(&mut self.save_password, "Запомнить пароль");
                    ui.weak("Пароль хранится зашифрованным для этого пользователя Windows.");
                } else {
                    ui.weak("На этой системе пароль не сохраняется.");
                }
                ui.horizontal(|ui| {
                    if ui.button("Сохранить и подключить").clicked() {
                        actions.push(Action::Connect);
                    }
                    if ui
                        .add_enabled(self.handle.is_some(), egui::Button::new("Отключить"))
                        .clicked()
                    {
                        actions.push(Action::Disconnect);
                    }
                });
                if let Some(local) = self.local {
                    ui.weak(format!("Адрес телефона: {local}"));
                }

                ui.separator();
                ui.heading("Звук");
                let before = (self.microphone.clone(), self.speaker.clone());
                device_picker(ui, "Микрофон", &mut self.microphone, &self.microphones);
                device_picker(ui, "Динамик", &mut self.speaker, &self.speakers);
                if ui.button("Обновить список устройств").clicked() {
                    self.microphones = input_device_names();
                    self.speakers = output_device_names();
                }
                if before != (self.microphone.clone(), self.speaker.clone()) {
                    actions.push(Action::AudioChanged);
                }
                ui.weak("Новое устройство используется со следующего звонка.");

                ui.separator();
                ui.heading("Входящие вызовы");
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.auto_answer, "Автоответ через");
                    ui.add(egui::DragValue::new(&mut self.auto_answer_delay).range(0..=60));
                    ui.label("с");
                });
                ui.weak("«Не беспокоить» включается галочкой в верхней строке окна.");

                ui.separator();
                ui.heading("Запись разговоров");
                ui.checkbox(&mut self.record_calls, "Записывать все разговоры");
                let default = default_recordings_dir()
                    .map(|folder| folder.display().to_string())
                    .unwrap_or_default();
                ui.horizontal(|ui| {
                    ui.label("Папка");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.recordings_dir)
                            .hint_text(default)
                            .desired_width(260.0),
                    );
                });
                if ui.button("Открыть папку с записями").clicked()
                    && let Some(folder) = self.recordings_folder()
                {
                    // The folder appears with the first recording; make it
                    // now so that there is something to open.
                    let _ = std::fs::create_dir_all(&folder);
                    actions.push(Action::Open(folder));
                }
                ui.weak("Файлы WAV: слева ваш голос, справа собеседник.");
            });
        self.settings_open = open;
    }

    fn log_window(&mut self, ctx: &egui::Context) {
        let mut open = self.log_open;
        egui::Window::new("Журнал")
            .open(&mut open)
            .default_size([520.0, 320.0])
            .show(ctx, |ui| {
                if ui.button("Очистить").clicked() {
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
        self.log_open = open;
    }
}

/// A drop-down list of devices; an empty choice is the system default.
fn device_picker(ui: &mut egui::Ui, label: &str, chosen: &mut String, names: &[String]) {
    let shown = if chosen.is_empty() {
        "По умолчанию".to_owned()
    } else {
        chosen.clone()
    };
    egui::ComboBox::from_label(label)
        .selected_text(shown)
        .width(280.0)
        .show_ui(ui, |ui| {
            ui.selectable_value(chosen, String::new(), "По умолчанию");
            for name in names {
                ui.selectable_value(chosen, name.clone(), name);
            }
        });
}

fn phase_text(phase: Phase) -> &'static str {
    match phase {
        Phase::Idle => "Готов",
        Phase::Calling => "Вызов…",
        Phase::Ringing => "У абонента звонит телефон",
        Phase::Incoming => "Входящий вызов",
        Phase::Answering => "Соединение…",
        Phase::Connected => "Разговор",
    }
}

fn end_reason_text(reason: EndReason) -> String {
    match reason {
        EndReason::Completed => "разговор окончен".to_owned(),
        EndReason::Rejected(486 | 600) => "занято".to_owned(),
        EndReason::Rejected(404) => "номер не найден".to_owned(),
        EndReason::Rejected(480) => "абонент недоступен".to_owned(),
        EndReason::Rejected(603) => "вызов отклонён".to_owned(),
        EndReason::Rejected(status) => format!("вызов не прошёл (код {status})"),
        EndReason::Cancelled => "отменён".to_owned(),
        EndReason::Timeout => "нет ответа".to_owned(),
        EndReason::Failed => "не удалось позвонить".to_owned(),
    }
}

/// Messages of the phone's core, in Russian where they are known. The text
/// after the first colon (a system detail) is kept as it is.
fn error_text(message: &str) -> String {
    const KNOWN: [(&str, &str); 11] = [
        (
            "Cannot play the ringing sound",
            "Не удалось воспроизвести звонок",
        ),
        ("Cannot start audio", "Не удалось включить звук"),
        ("Audio failed", "Сбой звука"),
        ("Hold did not work", "Удержание не сработало"),
        ("Resume did not work", "Возобновление не сработало"),
        ("Transfer did not work", "Перевод не удался"),
        (
            "only a connected call can be recorded",
            "Записывать можно только во время разговора",
        ),
        (
            "register on a SIP server before calling a number",
            "Нет регистрации на сервере — позвонить на номер нельзя",
        ),
        ("invalid number or SIP address", "Неверный номер"),
        ("a call is already in progress", "Уже идёт вызов"),
        (
            "digits can be sent only during a connected call",
            "Цифры можно отправлять только во время разговора",
        ),
    ];
    for (english, russian) in KNOWN {
        if let Some(rest) = message.strip_prefix(english) {
            return format!("{russian}{rest}");
        }
    }
    message.to_owned()
}

fn registration_text(state: &RegistrationState) -> String {
    match state {
        RegistrationState::Registering => "подключение…".to_owned(),
        RegistrationState::Registered { .. } => "в сети".to_owned(),
        RegistrationState::Unregistered => "не в сети".to_owned(),
        RegistrationState::Failed(reason) => {
            let reason = if reason.starts_with("wrong user name or password") {
                "неверный номер или пароль"
            } else if reason.starts_with("no response") {
                "сервер не отвечает"
            } else {
                reason.as_str()
            };
            format!("Ошибка: {reason}")
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
        self.poll(ctx);
        ctx.request_repaint_after(Duration::from_millis(100));
        let mut actions = Vec::new();

        egui::TopBottomPanel::top("status").show(ctx, |ui| {
            ui.add_space(4.0);
            self.status_bar(ui);
            ui.add_space(4.0);
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            let missed = self.history.missed_since(self.missed_seen);
            let calls = if missed > 0 {
                format!("Звонки ({missed})")
            } else {
                "Звонки".to_owned()
            };
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::Dialpad, "Набор");
                ui.selectable_value(&mut self.tab, Tab::Calls, calls);
                ui.selectable_value(&mut self.tab, Tab::Contacts, "Контакты");
            });
            ui.separator();
            match self.tab {
                Tab::Dialpad => self.dialpad_tab(ui, &mut actions),
                Tab::Calls => self.calls_tab(ui, &mut actions),
                Tab::Contacts => self.contacts_tab(ui, &mut actions),
            }
        });

        if self.settings_open {
            self.settings_window(ctx, &mut actions);
        }
        if self.log_open {
            self.log_window(ctx);
        }
        if let Some(on) = self.pending_dnd.take() {
            actions.push(Action::DoNotDisturb(on));
        }
        for action in actions {
            self.apply(action);
        }
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([400.0, 640.0])
            .with_min_inner_size([360.0, 560.0]),
        ..Default::default()
    };
    eframe::run_native(
        "MicroSIP Rust",
        options,
        Box::new(|_creation_context| Ok(Box::new(PhoneWindow::new()))),
    )
}
