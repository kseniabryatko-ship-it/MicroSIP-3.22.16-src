#![windows_subsystem = "windows"]

//! The phone's window: dial pad, call list, contacts and settings, in the
//! manner of MicroSIP. The phone connects to its account by itself at start.

use eframe::egui;
use microsip_core::audio::{input_device_names, output_device_names};
use microsip_core::call_media::{AudioControls, AudioMedia};
use microsip_core::call_runner::{
    CallHandle, EndReason, Event, Timers, TransferState, UiCommand, spawn_lines,
};
use microsip_core::call_view::{CallView, Phase};
use microsip_core::codec::Codec;
use microsip_core::contacts::{Contacts, dialable};
use microsip_core::history::{CallRecord, Direction, History};
use microsip_core::localtime::{
    DateTime, format_duration, format_timestamp, now_unix, utc_offset_seconds,
};
use microsip_core::recording::recording_file_name;
use microsip_core::registration::{Account, RegistrationState};
use microsip_core::secret;
use microsip_core::settings::{self, Settings, default_recordings_dir};
use microsip_core::srtp::MediaEncryption;
use microsip_core::stream::SipTransport;
use microsip_core::tones::Melody;
use microsip_core::transport::{local_ip_towards, resolve_socket_address};
use microsip_core::winshell::{self, Shell, ShellOptions};
use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

const LOG_LIMIT: usize = 1_000;
const WINDOW_TITLE: &str = "MicroSIP Rust";
/// Contacts offered under the number while it is typed.
const SUGGESTIONS: usize = 4;
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
    /// Looks at this line; a held call on it is resumed.
    SwitchLine(usize),
    /// Connects the two calls with each other and leaves both.
    JoinLines,
    Conference(bool),
    ForwardChanged,
    DoNotDisturb(bool),
    /// Puts the clipboard's text into the number field.
    Paste,
    /// Removes the last character of the number.
    Erase,
    /// Opens a file or folder with the program Windows uses for it.
    Open(PathBuf),
    /// Puts a contact's number into the number field.
    Suggest(String),
    /// The ring device or the melody file was changed.
    RingChanged,
    /// Shows a system dialog; its answer is taken when it closes.
    Browse(Dialog),
    Autostart(bool),
    /// The codecs or their order were changed.
    CodecsChanged,
    /// A box of the "Window" settings was changed.
    WindowChanged,
}

/// What a system dialog was opened for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dialog {
    RecordingsFolder,
    Ringtone,
    ImportContacts,
    ExportContacts,
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

/// Number of lines: one call can be held while another is talked on.
const LINES: usize = 2;

/// The state of one line as the window shows it.
#[derive(Default)]
struct LineUi {
    view: CallView,
    muted: bool,
    current: Option<CurrentCall>,
    caller: Option<(String, String)>,
    /// The file being written now.
    recording: Option<PathBuf>,
    /// When the ringing call is to be answered automatically.
    answer_at: Option<Instant>,
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
    transport: SipTransport,
    proxy: String,
    tls_verify: bool,
    encryption: MediaEncryption,
    stun: String,

    // Sound.
    microphone: String,
    speaker: String,
    speaker_volume: u32,
    microphone_volume: u32,
    microphones: Vec<String>,
    speakers: Vec<String>,
    controls: AudioControls,

    /// Codecs in use, the preferred one first.
    codecs: Vec<Codec>,
    /// Device and melody of the incoming ring.
    ring_speaker: String,
    ringtone: String,

    // Recording.
    record_calls: bool,
    recordings_dir: String,

    // The window among other programs.
    shell: Option<Shell>,
    close_to_tray: bool,
    hotkeys: bool,
    light_theme: bool,
    autostart: bool,
    /// Started with Windows: the window goes out of the way once drawn.
    start_hidden: bool,
    /// A system dialog that is open now, and what it answers.
    dialog: Option<(Dialog, Receiver<Option<PathBuf>>)>,
    /// The number dialled last, offered again by an empty "Call".
    last_dialled: String,

    // Call handling.
    do_not_disturb: bool,
    auto_answer: bool,
    auto_answer_delay: u32,
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
    /// What each line is doing; `at` says which one the code works on.
    lines: [LineUi; LINES],
    at: usize,
    /// The line the phone's events are about.
    event_line: usize,
    /// The line the buttons act on.
    selected: usize,
    conference: bool,
    forward: bool,
    forward_to: String,

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
    fn new(ctx: &egui::Context, start_hidden: bool) -> Self {
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

        let last_dialled = history
            .records()
            .iter()
            .find(|record| record.direction == Direction::Outgoing)
            .map(|record| record.number.clone())
            .unwrap_or_default();
        let shell = {
            let repaint = ctx.clone();
            Shell::start(
                ShellOptions {
                    window_title: WINDOW_TITLE.to_owned(),
                    tip: WINDOW_TITLE.to_owned(),
                    menu_open: "Открыть".to_owned(),
                    menu_exit: "Выход".to_owned(),
                    hotkeys: saved.hotkeys,
                },
                move || repaint.request_repaint(),
            )
        };

        let mut window = Self {
            codecs: Codec::parse_list(&saved.codecs),
            ring_speaker: saved.ring_speaker.clone(),
            ringtone: saved.ringtone.clone(),
            shell,
            close_to_tray: saved.close_to_tray,
            hotkeys: saved.hotkeys,
            light_theme: saved.light_theme,
            autostart: winshell::autostart_enabled(),
            start_hidden,
            dialog: None,
            last_dialled,
            server: saved.server.clone(),
            domain: saved.domain.clone(),
            user: saved.user.clone(),
            auth_user: saved.auth_user.clone(),
            password,
            show_password: false,
            save_password: saved.save_password,
            transport: SipTransport::from_name(&saved.transport),
            proxy: saved.proxy.clone(),
            tls_verify: saved.tls_verify,
            encryption: MediaEncryption::from_name(&saved.media_encryption),
            stun: saved.stun.clone(),
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
            do_not_disturb: saved.do_not_disturb,
            auto_answer: saved.auto_answer,
            auto_answer_delay: saved.auto_answer_delay,
            transfer_to: String::new(),
            pending_dnd: None,
            number_changed: false,
            focus_number: true,
            paste_requested: false,
            handle: None,
            local: None,
            registration: None,
            lines: Default::default(),
            at: 0,
            event_line: 0,
            selected: 0,
            conference: false,
            forward: saved.forward,
            forward_to: saved.forward_to.clone(),
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
        window.apply_ring_settings();
        window.apply_theme(ctx);

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

    /// Tells the sound where and what to ring. A melody that cannot be
    /// played is reported and the built-in ring stays.
    fn apply_ring_settings(&mut self) {
        self.controls
            .set_ring_device(Some(self.ring_speaker.clone()));
        let file = self.ringtone.trim().to_owned();
        if file.is_empty() {
            return self.controls.set_ringtone(None);
        }
        match Melody::load(std::path::Path::new(&file)) {
            Ok(melody) => self.controls.set_ringtone(Some(melody)),
            Err(error) => {
                self.controls.set_ringtone(None);
                let reason = melody_error_text(&error);
                self.error(format!("Мелодия звонка не подходит: {reason}"));
            }
        }
    }

    fn apply_theme(&self, ctx: &egui::Context) {
        ctx.set_visuals(if self.light_theme {
            let mut light = egui::Visuals::light();
            // White text fields are lost on the almost white window.
            light.extreme_bg_color = egui::Color32::from_gray(228);
            light
        } else {
            egui::Visuals::dark()
        });
    }

    /// A colour for marks and states that reads on the current theme.
    fn tint(&self, on_dark: egui::Color32, on_light: egui::Color32) -> egui::Color32 {
        if self.light_theme { on_light } else { on_dark }
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
            forward: self.forward,
            forward_to: self.forward_to.trim().to_owned(),
            ring_speaker: self.ring_speaker.clone(),
            ringtone: self.ringtone.trim().to_owned(),
            close_to_tray: self.close_to_tray,
            hotkeys: self.hotkeys,
            light_theme: self.light_theme,
            codecs: Codec::format_list(&self.codecs),
            transport: self.transport.name().to_owned(),
            proxy: self.proxy.trim().to_owned(),
            tls_verify: self.tls_verify,
            media_encryption: self.encryption.name().to_owned(),
            stun: self.stun.trim().to_owned(),
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
            .with_auth_username(self.auth_user.trim())
            .with_transport(self.transport)
            .with_proxy(self.proxy.trim())
            .with_tls_verify(self.tls_verify);
        let server = match account.connect_host_port().and_then(|(host, port)| {
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
        let make_media = move || AudioMedia::with_controls(controls.clone());
        let started = spawn_lines(local, "phone", Timers::default(), LINES, make_media);
        let handle = match started {
            Ok(handle) => handle,
            Err(error) => {
                return self.error(format!("Не удалось открыть порт {local}: {error}"));
            }
        };
        let local = handle.local_address();
        self.log(format!("Телефон запущен на {local}"));
        handle.send(UiCommand::Trace(self.trace_file.is_some()));
        handle.send(UiCommand::DoNotDisturb(self.do_not_disturb));
        handle.send(UiCommand::Forward(self.forward_target()));
        handle.send(UiCommand::Codecs(self.codecs.clone()));
        handle.send(UiCommand::Encryption(self.encryption));
        handle.send(UiCommand::Stun(Some(self.stun.trim().to_owned())));
        handle.send(UiCommand::Register(account));
        if let Some(shell) = self.shell.as_ref() {
            shell.watch_ringing(Some(handle.ringing_flag()));
        }
        self.local = Some(local);
        self.handle = Some(handle);
        self.reset_lines();
        self.registration = Some(RegistrationState::Registering);
    }

    fn disconnect(&mut self) {
        if let Some(shell) = self.shell.as_ref() {
            shell.watch_ringing(None);
        }
        if let Some(handle) = self.handle.take() {
            // Dropping the handle ends a call and removes the registration.
            drop(handle);
            self.log("Телефон отключён");
        }
        self.local = None;
        self.registration = None;
        self.reset_lines();
    }

    /// Forgets every call; used when the phone itself stops or starts.
    fn reset_lines(&mut self) {
        self.lines = Default::default();
        self.at = 0;
        self.event_line = 0;
        self.selected = 0;
        self.conference = false;
        self.transfer_to.clear();
    }

    /// The number calls are forwarded to, when forwarding is switched on.
    fn forward_target(&self) -> Option<String> {
        let target = self.forward_to.trim();
        (self.forward && !target.is_empty()).then(|| target.to_owned())
    }

    fn send(&mut self, command: UiCommand) {
        let delivered = self
            .handle
            .as_ref()
            .is_some_and(|handle| handle.send(command));
        if !delivered {
            self.handle = None;
            self.registration = None;
            self.reset_lines();
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
        if target.is_empty() {
            // "Call" on an empty field brings back the last number; a
            // second press dials it.
            if !self.last_dialled.is_empty() && self.lines[self.at].view.can_dial() {
                self.number = self.last_dialled.clone();
                self.number_changed = true;
                self.tab = Tab::Dialpad;
            }
            return;
        }
        // During a call, a new one goes out on the free line; the phone
        // holds the first call by itself.
        if !self.lines[self.at].view.can_dial()
            && let Some(line) = (0..LINES).find(|line| self.lines[*line].view.can_dial())
        {
            self.selected = line;
            self.at = line;
            self.send(UiCommand::SelectLine(line));
        }
        if !self.lines[self.at].view.can_dial() {
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
        self.last_dialled = target.clone();
        self.save_settings();
        let name = self.contacts.name_of(&target).unwrap_or("").to_owned();
        self.log(format!("Вызов {target}"));
        self.lines[self.at].current = Some(CurrentCall {
            incoming: false,
            number: target.clone(),
            name,
            started: now_unix(),
            connected: None,
            recording: String::new(),
        });
        self.lines[self.at].view.dialing_target(&target);
        self.send(UiCommand::DialTarget(target));
        self.tab = Tab::Dialpad;
    }

    fn answer(&mut self) {
        // The ringing call may be on the line that is not shown; the phone
        // then selects that line itself and holds the other call.
        if !self.lines[self.at].view.can_answer()
            && let Some(line) = (0..LINES).find(|line| self.lines[*line].view.can_answer())
        {
            self.at = line;
            self.selected = line;
        }
        if self.lines[self.at].view.can_answer() {
            self.lines[self.at].view.answering();
            self.send(UiCommand::Answer);
        }
    }

    fn reject(&mut self) {
        // As with answering, the ringing call may be on the other line.
        if !self.lines[self.at].view.can_reject()
            && let Some(line) = (0..LINES).find(|line| self.lines[*line].view.can_reject())
        {
            self.at = line;
            self.send(UiCommand::SelectLine(line));
            self.selected = line;
        }
        if self.lines[self.at].view.can_reject() {
            self.send(UiCommand::Reject);
        }
    }

    fn hangup(&mut self) {
        if self.lines[self.at].view.can_hangup() {
            self.send(UiCommand::Hangup);
        }
    }

    fn toggle_mute(&mut self) {
        if self.lines[self.at].view.phase() == Phase::Connected {
            self.lines[self.at].muted = !self.lines[self.at].muted;
            self.send(UiCommand::Mute(self.lines[self.at].muted));
        }
    }

    fn toggle_hold(&mut self) {
        if self.lines[self.at].view.can_hold() {
            self.send(UiCommand::Hold(!self.lines[self.at].view.is_held()));
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
        if self.lines[self.at].view.phase() != Phase::Connected
            || self.lines[self.at].recording.is_some()
        {
            return;
        }
        let Some(folder) = self.recordings_folder() else {
            return self.error("Не задана папка для записей");
        };
        let (incoming, number) = self.lines[self.at]
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
        if self.lines[self.at].recording.is_some() {
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
        if self.lines[self.at].view.phase() == Phase::Connected {
            self.send(UiCommand::Dtmf(digit));
        } else if self.lines[self.at].view.can_dial() {
            self.number.push(digit);
            self.number_changed = true;
        }
    }

    /// Ends the bookkeeping of the current call and, when `record` is set,
    /// writes it to the call list.
    fn finish_call(&mut self, reason: EndReason, record: bool) {
        self.lines[self.at].muted = false;
        self.lines[self.at].answer_at = None;
        self.transfer_to.clear();
        // The phone finishes the file itself when the call ends.
        self.lines[self.at].recording = None;
        let Some(call) = self.lines[self.at].current.take() else {
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
        // Events are about the line the phone last named.
        self.at = self.event_line;
        while let Some(event) = self.handle.as_ref().and_then(|handle| handle.try_event()) {
            if let Event::Line(line) = event {
                self.event_line = line.min(LINES - 1);
                self.at = self.event_line;
                continue;
            }
            self.lines[self.at].view.on_event(&event);
            match event {
                Event::Caller { number, name } => self.lines[self.at].caller = Some((number, name)),
                Event::Incoming { peer } => {
                    let (number, name) = self.lines[self.at]
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
                    self.lines[self.at].current = Some(CurrentCall {
                        incoming: true,
                        number,
                        name,
                        started: now_unix(),
                        connected: None,
                        recording: String::new(),
                    });
                    self.tab = Tab::Dialpad;
                    self.lines[self.at].answer_at = self.auto_answer.then(|| {
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
                    if let Some(call) = self.lines[self.at].current.as_mut() {
                        call.connected = Some(Instant::now());
                    }
                    if self.record_calls {
                        self.start_recording();
                    }
                }
                Event::Recording(Some(path)) => {
                    self.log(format!("Запись разговора: {}", path.display()));
                    if let Some(call) = self.lines[self.at].current.as_mut() {
                        call.recording = path.display().to_string();
                    }
                    self.lines[self.at].recording = Some(path);
                }
                Event::Recording(None) => {
                    self.log("Запись остановлена");
                    self.lines[self.at].recording = None;
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
                Event::LocalAddress(address) => {
                    self.local = Some(address);
                    self.log(format!("Сеть изменилась: телефон теперь на {address}"));
                }
                Event::Trace {
                    outgoing,
                    peer,
                    text,
                } => self.trace(outgoing, peer, &text),
                Event::Line(_) | Event::Codec(_) | Event::Encrypted(_) => {}
                Event::Selected(line) => self.selected = line.min(LINES - 1),
                Event::Conference(on) => {
                    self.conference = on;
                    self.log(if on {
                        "Конференция: все линии соединены"
                    } else {
                        "Конференция завершена"
                    });
                }
                Event::Forwarded { number, name, to } => {
                    let name = self
                        .contacts
                        .name_of(&number)
                        .map(str::to_owned)
                        .unwrap_or(name);
                    self.log(format!("Переадресован на {to}: {number} {name}"));
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
                Event::Error(message) => self.error(error_text(&message)),
            }
        }
        // Auto-answer, once the chosen time of ringing has passed.
        for line in 0..LINES {
            if self.lines[line]
                .answer_at
                .is_some_and(|at| Instant::now() >= at)
            {
                self.lines[line].answer_at = None;
                self.at = line;
                self.answer();
            }
        }
        // From here on the code is about the line the user looks at.
        self.at = self.selected;
    }

    /// The other line, when it has a call.
    fn other_line(&self) -> Option<usize> {
        let other = (self.selected + 1) % LINES;
        (self.lines[other].view.phase() != Phase::Idle).then_some(other)
    }

    /// Makes a line the selected one; a held call on it is taken back, which
    /// puts the other call on hold.
    fn switch_line(&mut self, line: usize) {
        if line >= LINES || self.handle.is_none() {
            return;
        }
        self.selected = line;
        self.at = line;
        self.send(UiCommand::SelectLine(line));
        let view = &self.lines[line].view;
        if view.phase() == Phase::Connected && view.is_held() && !self.conference {
            self.send(UiCommand::Hold(false));
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
                } else if self.lines[self.at].view.phase() == Phase::Connected {
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
            Action::SwitchLine(line) => self.switch_line(line),
            Action::JoinLines => {
                if let Some(other) = self.other_line() {
                    self.last_error = None;
                    self.log("Соединяю двух собеседников между собой");
                    self.send(UiCommand::TransferToLine(other));
                }
            }
            Action::Conference(on) => self.send(UiCommand::Conference(on)),
            Action::ForwardChanged => {
                self.save_settings();
                if self.handle.is_some() {
                    self.send(UiCommand::Forward(self.forward_target()));
                }
            }
            Action::DoNotDisturb(on) => {
                self.do_not_disturb = on;
                self.save_settings();
                if self.handle.is_some() {
                    self.send(UiCommand::DoNotDisturb(on));
                }
            }
            Action::Open(path) => self.open(path),
            Action::Suggest(number) => {
                self.number = number;
                self.number_changed = true;
                self.focus_number = true;
            }
            Action::RingChanged => {
                self.last_error = None;
                self.apply_ring_settings();
                self.save_settings();
            }
            Action::Browse(dialog) => self.browse(dialog),
            Action::Autostart(on) => match winshell::set_autostart(on) {
                Ok(()) => self.autostart = on,
                Err(error) => {
                    self.autostart = winshell::autostart_enabled();
                    self.error(format!("Не удалось изменить автозапуск: {error}"));
                }
            },
            Action::WindowChanged => self.save_settings(),
            Action::CodecsChanged => {
                self.save_settings();
                if self.handle.is_some() {
                    self.send(UiCommand::Codecs(self.codecs.clone()));
                }
            }
        }
    }

    // ----- system dialogs, contacts as a table -----------------------------

    /// Opens a system dialog without stopping the phone's window: the
    /// dialog runs beside it and its answer is picked up when it closes.
    fn browse(&mut self, dialog: Dialog) {
        if self.dialog.is_some() {
            return;
        }
        if !winshell::is_available() {
            // No system dialogs here: the contacts table lives next to the
            // settings, and folders and files are typed in by hand.
            let table = self
                .contacts_path
                .as_ref()
                .map(|path| path.with_file_name("contacts.csv"));
            return match (dialog, table) {
                (Dialog::ImportContacts, Some(path)) => self.import_contacts(&path),
                (Dialog::ExportContacts, Some(path)) => self.export_contacts(&path),
                _ => self.error("Выбор в окне доступен только в Windows — впишите путь вручную"),
            };
        }
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let chosen = match dialog {
                Dialog::RecordingsFolder => winshell::pick_folder(),
                Dialog::Ringtone => winshell::pick_open_file("wav"),
                Dialog::ImportContacts => winshell::pick_open_file("csv"),
                Dialog::ExportContacts => winshell::pick_save_file("contacts.csv", "csv"),
            };
            let _ = sender.send(chosen);
        });
        self.dialog = Some((dialog, receiver));
    }

    /// Takes the answer of a closed dialog.
    fn poll_dialog(&mut self) {
        let Some((dialog, receiver)) = self.dialog.as_ref() else {
            return;
        };
        let dialog = *dialog;
        let chosen = match receiver.try_recv() {
            Ok(chosen) => chosen,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => None,
        };
        self.dialog = None;
        let Some(path) = chosen else {
            return;
        };
        match dialog {
            Dialog::RecordingsFolder => {
                self.recordings_dir = path.display().to_string();
                self.save_settings();
            }
            Dialog::Ringtone => {
                self.ringtone = path.display().to_string();
                self.last_error = None;
                self.apply_ring_settings();
                self.save_settings();
            }
            Dialog::ImportContacts => self.import_contacts(&path),
            Dialog::ExportContacts => self.export_contacts(&path),
        }
    }

    fn import_contacts(&mut self, path: &std::path::Path) {
        match std::fs::read(path) {
            Ok(bytes) => {
                let taken = self.contacts.import_csv(&bytes);
                if taken > 0 {
                    self.save_contacts();
                    self.last_error = None;
                    self.log(format!("Импорт контактов: {taken} из {}", path.display()));
                } else {
                    self.error(format!("В файле {} контактов не найдено", path.display()));
                }
            }
            Err(error) => self.error(format!("Не удалось прочитать {}: {error}", path.display())),
        }
    }

    fn export_contacts(&mut self, path: &std::path::Path) {
        match std::fs::write(path, self.contacts.to_csv()) {
            Ok(()) => {
                self.last_error = None;
                self.log(format!("Контакты сохранены в {}", path.display()));
            }
            Err(error) => self.error(format!("Не удалось записать {}: {error}", path.display())),
        }
    }

    // ----- tray, keys, closing ---------------------------------------------

    fn any_line(&self, phase: Phase) -> bool {
        self.lines.iter().any(|line| line.view.phase() == phase)
    }

    /// Keys of the window itself and of the whole system, and what the
    /// icon next to the clock asked for.
    fn keys_and_tray(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let ringing = self.any_line(Phase::Incoming);
        // Enter answers and Esc refuses a ringing call, unless a text is
        // being typed somewhere.
        if ringing && !ctx.wants_keyboard_input() {
            if ctx.input(|input| input.key_pressed(egui::Key::Enter)) {
                actions.push(Action::Answer);
            } else if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
                actions.push(Action::Reject);
            }
        }
        let Some(signals) = self.shell.as_ref().map(|shell| shell.signals().clone()) else {
            return;
        };
        if signals.answer.swap(false, Ordering::Relaxed) && ringing {
            actions.push(Action::Answer);
        }
        if signals.hangup.swap(false, Ordering::Relaxed) {
            actions.push(if ringing {
                Action::Reject
            } else {
                Action::Hangup
            });
        }
        if signals.paste.swap(false, Ordering::Relaxed) {
            self.tab = Tab::Dialpad;
            self.settings_open = false;
            self.log_open = false;
            actions.push(Action::Paste);
        }

        // Closing the window only hides it; "Exit" of the icon's menu and a
        // phone without an icon really close.
        if ctx.input(|input| input.viewport().close_requested())
            && self.close_to_tray
            && !signals.exit.load(Ordering::Relaxed)
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.save_settings();
            if let Some(shell) = self.shell.as_ref() {
                shell.hide_window();
            }
        }
        // Started with Windows: out of the way once the window exists.
        if self.start_hidden && ctx.cumulative_pass_nr() >= 3 {
            self.start_hidden = false;
            match self.shell.as_ref() {
                Some(shell) if self.close_to_tray => shell.hide_window(),
                _ => ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true)),
            }
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
                    let yellow =
                        self.tint(egui::Color32::YELLOW, egui::Color32::from_rgb(200, 140, 0));
                    (yellow, "Подключение…".to_owned())
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

    /// What a line's button says: its number and what is on it.
    fn line_label(&self, line: usize) -> String {
        let state = &self.lines[line];
        let who = state
            .current
            .as_ref()
            .map(|call| {
                if call.name.is_empty() {
                    call.number.clone()
                } else {
                    call.name.clone()
                }
            })
            .unwrap_or_default();
        let what = match state.view.phase() {
            Phase::Idle => "свободна".to_owned(),
            Phase::Incoming => format!("входящий {who}"),
            Phase::Connected if state.view.is_held() => format!("удержание {who}"),
            Phase::Connected => format!("разговор {who}"),
            _ => format!("вызов {who}"),
        };
        format!("Линия {}: {what}", line + 1)
    }

    fn lines_bar(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        ui.horizontal(|ui| {
            for line in 0..LINES {
                let label = self.line_label(line);
                let ringing = self.lines[line].view.phase() == Phase::Incoming;
                let text = if ringing {
                    egui::RichText::new(label).color(egui::Color32::YELLOW)
                } else {
                    egui::RichText::new(label)
                };
                if ui.selectable_label(self.selected == line, text).clicked()
                    && self.selected != line
                {
                    actions.push(Action::SwitchLine(line));
                }
            }
        });
        // A call ringing on the line that is not shown must not be missed.
        let other = (self.selected + 1) % LINES;
        if self.lines[other].view.phase() == Phase::Incoming {
            ui.horizontal(|ui| {
                ui.colored_label(egui::Color32::YELLOW, "Второй вызов:");
                if ui.button("Ответить").clicked() {
                    actions.push(Action::Answer);
                }
                if ui.button("Показать").clicked() {
                    actions.push(Action::SwitchLine(other));
                }
            });
        }
        // With two calls connected they can be joined.
        let both = (0..LINES).all(|line| self.lines[line].view.phase() == Phase::Connected);
        if both {
            ui.horizontal(|ui| {
                if ui.button("Соединить собеседников").clicked() {
                    actions.push(Action::JoinLines);
                }
                let label = if self.conference {
                    "Завершить конференцию"
                } else {
                    "Конференция"
                };
                if ui.button(label).clicked() {
                    actions.push(Action::Conference(!self.conference));
                }
            });
        }
        ui.separator();
    }

    fn dialpad_tab(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        self.lines_bar(ui, actions);
        let phase = self.lines[self.at].view.phase();
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
        } else if idle && self.number.trim().chars().count() >= 2 {
            // Contacts that fit what has been typed so far; a click puts
            // the number into the field.
            let found = self.contacts.matching(&self.number);
            if !found.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    for contact in found.into_iter().take(SUGGESTIONS) {
                        let label = format!("{} {}", contact.name, contact.number);
                        if ui.small_button(label).clicked() {
                            actions.push(Action::Suggest(contact.number.clone()));
                        }
                    }
                });
            }
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
                    .add_enabled_ui(self.lines[self.at].view.can_hangup(), |ui| {
                        ui.add_sized(wide, hangup)
                    })
                    .inner
                    .clicked();
                if pressed {
                    actions.push(Action::Hangup);
                }
                let connected = phase == Phase::Connected;
                let mute = if self.lines[self.at].muted {
                    "Включить микрофон"
                } else {
                    "Выключить микрофон"
                };
                let hold = if self.lines[self.at].view.is_held() {
                    "Продолжить"
                } else {
                    "Удержание"
                };
                let record = if self.lines[self.at].recording.is_some() {
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
        if let Some(call) = self.lines[self.at].current.as_ref() {
            let who = if call.name.is_empty() {
                call.number.clone()
            } else {
                format!("{} ({})", call.name, call.number)
            };
            let state = match (phase, call.connected) {
                (Phase::Connected, Some(connected)) => {
                    let time = format_duration(connected.elapsed().as_secs());
                    let secure = if self.lines[self.at].view.is_encrypted() {
                        " · шифруется"
                    } else {
                        ""
                    };
                    match self.lines[self.at].view.codec() {
                        Some(codec) => format!("Разговор {time} · {codec}{secure}"),
                        None => format!("Разговор {time}{secure}"),
                    }
                }
                _ => phase_text(phase).to_owned(),
            };
            ui.label(egui::RichText::new(who).size(18.0));
            ui.label(state);
            if let Some(at) = self.lines[self.at].answer_at {
                let left = at.saturating_duration_since(Instant::now()).as_secs() + 1;
                ui.label(format!("Автоответ через {left} с"));
            }
            if self.lines[self.at].recording.is_some() {
                ui.colored_label(egui::Color32::RED, "Идёт запись разговора");
            }
            if self.lines[self.at].muted {
                ui.colored_label(egui::Color32::YELLOW, "Микрофон выключен");
            }
            if self.lines[self.at].view.is_held() {
                ui.colored_label(egui::Color32::YELLOW, "Вызов на удержании");
            }
            if self.lines[self.at].view.is_remote_held() {
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
        let can_dial = self.lines[self.at].view.can_dial();
        let mut to_contacts = None;
        egui::ScrollArea::vertical().show(ui, |ui| {
            for (index, record) in self.history.records().iter().enumerate() {
                let (mark, color) = match record.direction {
                    Direction::Outgoing => (
                        "Исх.",
                        self.tint(
                            egui::Color32::LIGHT_BLUE,
                            egui::Color32::from_rgb(30, 90, 170),
                        ),
                    ),
                    Direction::Incoming => (
                        "Вх.",
                        self.tint(
                            egui::Color32::LIGHT_GREEN,
                            egui::Color32::from_rgb(30, 130, 50),
                        ),
                    ),
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
        ui.horizontal(|ui| {
            if ui.button("Сохранить контакт").clicked() {
                actions.push(Action::SaveContact(
                    self.new_contact_name.clone(),
                    self.new_contact_number.clone(),
                ));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let free = self.dialog.is_none();
                if ui
                    .add_enabled(
                        free && !self.contacts.is_empty(),
                        egui::Button::new("Экспорт…"),
                    )
                    .on_hover_text("Сохранить контакты в файл CSV (открывается в Excel)")
                    .clicked()
                {
                    actions.push(Action::Browse(Dialog::ExportContacts));
                }
                if ui
                    .add_enabled(free, egui::Button::new("Импорт…"))
                    .on_hover_text("Добавить контакты из файла CSV: имя и номер")
                    .clicked()
                {
                    actions.push(Action::Browse(Dialog::ImportContacts));
                }
            });
        });
        ui.separator();
        ui.add(
            egui::TextEdit::singleline(&mut self.contact_search)
                .hint_text("Поиск")
                .desired_width(f32::INFINITY),
        );
        let can_dial = self.lines[self.at].view.can_dial();
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
                // The settings are taller than the phone's window.
                let room = (ui.ctx().screen_rect().height() - 70.0).max(200.0);
                egui::ScrollArea::vertical()
                    .max_height(room)
                    .show(ui, |ui| {
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
ui.label("Транспорт");
ui.horizontal(|ui| {
    for transport in SipTransport::ALL {
        ui.radio_value(&mut self.transport, transport, transport.name());
    }
});
ui.end_row();
ui.label("STUN-сервер");
ui.add(
    egui::TextEdit::singleline(&mut self.stun)
        .hint_text("не используется")
        .desired_width(170.0),
);
ui.end_row();
ui.label("Шифрование (SRTP)");
egui::ComboBox::from_id_salt("media-encryption")
    .selected_text(encryption_text(self.encryption))
    .width(170.0)
    .show_ui(ui, |ui| {
        for mode in MediaEncryption::ALL {
            ui.selectable_value(&mut self.encryption, mode, encryption_text(mode));
        }
    });
ui.end_row();
ui.label("Прокси (если есть)");
ui.add(
    egui::TextEdit::singleline(&mut self.proxy)
        .hint_text("не используется")
        .desired_width(170.0),
);
ui.end_row();
                            ui.label("Локальный порт");
                            ui.add(
                                egui::TextEdit::singleline(&mut self.local_port)
                                    .hint_text("автоматически")
                                    .desired_width(100.0),
                            );
                            ui.end_row();
                        });
if self.transport == SipTransport::Tls {
    ui.checkbox(&mut self.tls_verify, "Проверять сертификат сервера (TLS)");
    if !self.tls_verify {
        ui.weak("Без проверки связь шифруется, но сервер может оказаться подменённым.");
    }
}
                        if secret::is_available() {
                            ui.checkbox(&mut self.save_password, "Запомнить пароль");
                            ui.weak(
                                "Пароль хранится зашифрованным для этого пользователя Windows.",
                            );
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
                        let before = (self.ring_speaker.clone(), self.ringtone.clone());
                        device_picker(ui, "Звонок", &mut self.ring_speaker, &self.speakers);
                        ui.horizontal(|ui| {
                            ui.label("Мелодия");
                            let field = egui::TextEdit::singleline(&mut self.ringtone)
                                .hint_text("встроенный звонок")
                                .desired_width(170.0);
                            let typed = ui.add(field).lost_focus();
                            if ui
                                .add_enabled(self.dialog.is_none(), egui::Button::new("Обзор…"))
                                .clicked()
                            {
                                actions.push(Action::Browse(Dialog::Ringtone));
                            }
                            if ui.button("Сбросить").clicked() {
                                self.ringtone.clear();
                            }
                            let changed =
                                before != (self.ring_speaker.clone(), self.ringtone.clone());
                            // A path being typed is read when the typing is over.
                            let typing = ui.ctx().wants_keyboard_input() && !typed;
                            if (changed || typed) && !typing && self.ring_is_unsaved() {
                                actions.push(Action::RingChanged);
                            }
                        });
                        ui.weak(
                    "Мелодия — файл WAV. «Звонок» на «По умолчанию» звучит там же, где разговор.",
                );

                        ui.separator();
                        ui.heading("Кодеки");
                        if self.codecs_editor(ui) {
                            actions.push(Action::CodecsChanged);
                        }
                        ui.weak(
                            "Сверху — предпочитаемый. Изменения действуют со следующего звонка. \
     Если АТС выбирает кодек сама, решает её порядок.",
                        );

                        ui.separator();
                        ui.heading("Входящие вызовы");
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut self.auto_answer, "Автоответ через");
                            ui.add(egui::DragValue::new(&mut self.auto_answer_delay).range(0..=60));
                            ui.label("с");
                        });
                        ui.weak("«Не беспокоить» включается галочкой в верхней строке окна.");
                        let before = (self.forward, self.forward_to.clone());
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut self.forward, "Переадресация на номер");
                            ui.add(
                                egui::TextEdit::singleline(&mut self.forward_to)
                                    .desired_width(140.0),
                            );
                        });
                        if before != (self.forward, self.forward_to.clone()) {
                            actions.push(Action::ForwardChanged);
                        }

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
                                    .desired_width(200.0),
                            );
                            if ui
                                .add_enabled(self.dialog.is_none(), egui::Button::new("Обзор…"))
                                .clicked()
                            {
                                actions.push(Action::Browse(Dialog::RecordingsFolder));
                            }
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

                        ui.separator();
                        ui.heading("Окно");
                        let before = (self.close_to_tray, self.hotkeys, self.light_theme);
                        ui.checkbox(&mut self.light_theme, "Светлая тема");
                        if self.shell.is_some() {
                            ui.checkbox(
                                &mut self.close_to_tray,
                                "При закрытии сворачивать к часам (в трей)",
                            );
                            let mut autostart = self.autostart;
                            if ui
                                .checkbox(&mut autostart, "Запускать вместе с Windows")
                                .changed()
                            {
                                actions.push(Action::Autostart(autostart));
                            }
                            ui.checkbox(&mut self.hotkeys, "Горячие клавиши в любой программе");
                            ui.weak(
                                "Ctrl+Alt+PageUp — ответить, Ctrl+Alt+PageDown — завершить или \
                         отклонить, Ctrl+Alt+Insert — номер из буфера обмена.\n\
                         Галочка клавиш действует после перезапуска программы.",
                            );
                        } else {
                            ui.weak(
                                "Значок у часов, автозапуск и общие клавиши есть только в Windows.",
                            );
                        }
                        ui.weak("В окне: Enter — ответить, Esc — отклонить входящий.");
                        if before != (self.close_to_tray, self.hotkeys, self.light_theme) {
                            if before.2 != self.light_theme {
                                self.apply_theme(ui.ctx());
                            }
                            actions.push(Action::WindowChanged);
                        }
                    });
            });
        self.settings_open = open;
    }

    /// The list of codecs: a box to switch each on or off and buttons to
    /// move it up and down. Returns true when something was changed.
    fn codecs_editor(&mut self, ui: &mut egui::Ui) -> bool {
        let mut changed = false;
        // Those in use first, in their order; the rest below, switched off.
        let mut rows: Vec<(Codec, bool)> = self.codecs.iter().map(|codec| (*codec, true)).collect();
        for codec in Codec::PREFERRED {
            if !self.codecs.contains(&codec) {
                rows.push((codec, false));
            }
        }
        let in_use = self.codecs.len();
        for (index, (codec, on)) in rows.into_iter().enumerate() {
            ui.push_id(("codec", index), |ui| {
                ui.horizontal(|ui| {
                    let mut enabled = on;
                    // The last codec cannot be switched off.
                    let may_toggle = !on || in_use > 1;
                    let label = format!("{codec} — {}", codec_note(codec));
                    if ui
                        .add_enabled(may_toggle, egui::Checkbox::new(&mut enabled, label))
                        .changed()
                    {
                        if enabled {
                            self.codecs.push(codec);
                        } else {
                            self.codecs.retain(|kept| *kept != codec);
                        }
                        changed = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let down = on && index + 1 < in_use;
                        if ui.add_enabled(down, egui::Button::new("Ниже")).clicked() {
                            self.codecs.swap(index, index + 1);
                            changed = true;
                        }
                        let up = on && index > 0;
                        if ui.add_enabled(up, egui::Button::new("Выше")).clicked() {
                            self.codecs.swap(index, index - 1);
                            changed = true;
                        }
                    });
                });
            });
        }
        changed
    }

    /// The ring device or melody differs from what is saved and in use.
    fn ring_is_unsaved(&self) -> bool {
        self.ring_speaker != self.saved.ring_speaker || self.ringtone.trim() != self.saved.ringtone
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
    const KNOWN: [(&str, &str); 13] = [
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
            "both lines must have a connected call",
            "Соединить можно только два идущих разговора",
        ),
        (
            "only a connected call can be recorded",
            "Записывать можно только во время разговора",
        ),
        (
            "register on a SIP server before calling a number",
            "Нет регистрации на сервере — позвонить на номер нельзя",
        ),
        ("invalid number or SIP address", "Неверный номер"),
        ("STUN server", "STUN-сервер"),
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

/// The encryption modes as the settings list them.
fn encryption_text(mode: MediaEncryption) -> &'static str {
    match mode {
        MediaEncryption::Off => "выключено",
        MediaEncryption::Optional => "если АТС умеет",
        MediaEncryption::Required => "обязательно",
    }
}

/// A few words on what a codec is good for.
fn codec_note(codec: Codec) -> &'static str {
    match codec {
        Codec::Pcmu | Codec::Pcma => "обычная телефония, есть везде",
        Codec::G722 => "чистый широкополосный звук",
        Codec::Opus => "широкополосный, стойкий к потерям",
        Codec::G729 => "для медленных каналов",
    }
}

/// Why a melody file was refused, in the user's language.
fn melody_error_text(error: &str) -> String {
    match error {
        "not a WAV file" => "это не файл WAV".to_owned(),
        "the file is larger than 20 MB" => "файл больше 20 МБ".to_owned(),
        "the WAV file has no sound in it" | "the WAV file has no format" => {
            "в файле нет звука".to_owned()
        }
        other if other.starts_with("this kind of WAV") => {
            "сжатый WAV не поддерживается — сохраните его как обычный PCM".to_owned()
        }
        other => format!("файл не читается ({other})"),
    }
}

fn registration_text(state: &RegistrationState) -> String {
    match state {
        RegistrationState::Registering => "подключение…".to_owned(),
        RegistrationState::Registered { .. } => "в сети".to_owned(),
        RegistrationState::Unregistered => "не в сети".to_owned(),
        RegistrationState::Failed(reason) => {
            let reason = if reason.starts_with("wrong user name or password") {
                "неверный номер или пароль".to_owned()
            } else if reason.starts_with("no response") {
                "сервер не отвечает".to_owned()
            } else if reason.contains("certificate") {
                "сертификат сервера не прошёл проверку (TLS)".to_owned()
            } else if reason.starts_with("cannot connect") {
                "не удалось соединиться с сервером, пробую снова".to_owned()
            } else if reason.starts_with("the connection to the SIP server was lost") {
                "соединение с сервером прервалось, подключаюсь снова".to_owned()
            } else {
                reason.clone()
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
        self.poll_dialog();
        ctx.request_repaint_after(Duration::from_millis(100));
        let mut actions = Vec::new();
        self.keys_and_tray(ctx, &mut actions);

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
    // Windows starts the phone with this word when it starts by itself.
    let start_hidden = std::env::args().any(|argument| argument == "--minimized");
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([400.0, 640.0])
            .with_min_inner_size([360.0, 560.0]),
        ..Default::default()
    };
    eframe::run_native(
        WINDOW_TITLE,
        options,
        Box::new(move |creation| Ok(Box::new(PhoneWindow::new(&creation.egui_ctx, start_hidden)))),
    )
}
