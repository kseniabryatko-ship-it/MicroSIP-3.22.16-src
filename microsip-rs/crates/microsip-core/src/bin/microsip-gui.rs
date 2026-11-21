#![windows_subsystem = "windows"]

//! The phone's window: dial pad, call list, contacts and settings, in the
//! manner of MicroSIP. The phone connects to its account by itself at start.

use eframe::egui;
use microsip_core::audio::{input_device_names, output_device_names};
use microsip_core::call_media::{AudioControls, AudioMedia};
use microsip_core::call_runner::{
    CallHandle, EndReason, Event, Timers, TransferState, UiCommand, account_key, spawn_lines,
};
use microsip_core::call_view::{CallView, Phase};
use microsip_core::chat::{Chat, ChatMessage};
use microsip_core::codec::Codec;
use microsip_core::contacts::{Contacts, dialable};
use microsip_core::history::{CallRecord, Direction, History, TableWords};
use microsip_core::key_tones::KeyTones;
use microsip_core::links::{self, Instance, Request as LinkRequest};
use microsip_core::live_quality::{Lamp, LiveQuality};
use microsip_core::localtime::{
    DateTime, format_duration, format_timestamp, now_unix, utc_offset_seconds,
};
use microsip_core::presence::{PresenceStatus, RemotePresence};
use microsip_core::recording::recording_file_name;
use microsip_core::registration::{Account, RegistrationState};
use microsip_core::rtp::dtmf::DtmfMethod;
use microsip_core::secret;
use microsip_core::services::{LineState, Voicemail};
use microsip_core::settings::{
    self, AccountSettings, Settings, account_title, default_recordings_dir,
};
use microsip_core::srtp::MediaEncryption;
use microsip_core::stream::SipTransport;
use microsip_core::tones::Melody;
use microsip_core::transport::{local_ip_towards, resolve_socket_address};
use microsip_core::voice::VoiceSettings;
use microsip_core::winshell::{self, Shell, ShellOptions};
use std::collections::{HashMap, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

/// The window speaks English instead of Russian.
static ENGLISH: AtomicBool = AtomicBool::new(false);

fn english() -> bool {
    ENGLISH.load(Ordering::Relaxed)
}

fn set_english(on: bool) {
    ENGLISH.store(on, Ordering::Relaxed);
}

/// The Russian or the English text, by the language of the window. Both
/// are written side by side where the text is used.
macro_rules! tr {
    ($russian:expr, $english:expr $(,)?) => {
        if english() { $english } else { $russian }
    };
}

const LOG_LIMIT: usize = 1_000;
const WINDOW_TITLE: &str = "sip by @enxances x NAVI";
/// How long a number from a link or the command line waits for the phone
/// to get registered before it is only left in the number field.
const LINK_WAIT: Duration = Duration::from_secs(20);
/// Contacts offered under the number while it is typed.
const SUGGESTIONS: usize = 4;
const KEYPAD: [&str; 4] = ["123", "456", "789", "*0#"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Dialpad,
    Calls,
    Contacts,
    Messages,
}

/// What the user asked for in this frame; carried out after drawing.
enum Action {
    Call(String),
    Answer,
    Reject,
    Hangup,
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
    /// Turns the lamp of a contact on or off.
    Watch(String, bool),
    /// Opens the conversation with a number.
    OpenChat(String),
    SendText,
    ClearChat(String),
    /// Dials the voicemail number.
    Voicemail,
    /// Removes the last character of the number.
    Erase,
    /// Removes the whole number.
    ClearNumber,
    /// Opens a file or folder with the program Windows uses for it.
    Open(PathBuf),
    /// Puts a contact's number into the number field.
    Suggest(String),
    /// The ring device or the melody file was changed.
    RingChanged,
    /// Shows a system dialog; its answer is taken when it closes.
    Browse(Dialog),
    Autostart(bool),
    /// Open `tel:`, `sip:` and `callto:` links with this program, or not.
    LinkHandler(bool),
    /// Keep every saved account registered, or only the active one.
    AllAccounts(bool),
    /// How keypad digits are sent.
    DtmfMethod(DtmfMethod),
    /// Statuses for colleagues on or off.
    Statuses(bool),
    /// The window's language: English or not.
    English(bool),
    /// The codecs or their order were changed.
    CodecsChanged,
    /// A box of the "Window" settings was changed.
    WindowChanged,
    /// Makes the saved account with this number (among the others) active.
    SwitchAccount(usize),
    AddAccount,
    RemoveAccount,
}

/// What a system dialog was opened for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dialog {
    RecordingsFolder,
    Ringtone,
    ImportContacts,
    ExportContacts,
    ExportHistory,
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
    /// The extra account an incoming call came to; empty for the main one.
    account: String,
}

/// Number of lines: one call can be held while another is talked on.
const LINES: usize = 2;

/// The state of one line as the window shows it.
#[derive(Default)]
struct LineUi {
    view: CallView,
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
    chat_path: Option<PathBuf>,
    trace_file: Option<File>,
    saved: Settings,
    history: History,
    contacts: Contacts,
    chat: Chat,
    /// The conversation shown, and what is being typed into it.
    chat_with: String,
    chat_draft: String,
    /// Messages received after this have not been seen.
    messages_seen: u64,
    /// The mailbox, once the exchange told.
    voicemail: Option<Voicemail>,
    voicemail_number: String,
    /// What each watched number's line is doing.
    presence: HashMap<String, LineState>,
    /// The statuses colleagues published (online, away, busy).
    statuses: HashMap<String, RemotePresence>,
    /// Publish the user's status and watch colleagues' statuses.
    presence_on: bool,
    /// The user's own status.
    my_status: PresenceStatus,
    /// Why the exchange did not take the status, when it did not.
    status_refused: Option<String>,
    /// How keypad digits are sent.
    dtmf_method: DtmfMethod,
    /// Every saved account is registered, not only the active one.
    all_accounts: bool,
    /// The extra account calls go out through (an index into
    /// `other_accounts`); `None` is the active account.
    call_from: Option<usize>,
    /// What the exchange says about each extra account, by its key.
    extra_states: HashMap<String, RegistrationState>,
    /// The keypad sounds its tones.
    key_tones_on: bool,
    key_tones: KeyTones,
    /// "Exit" was chosen in the menu: closing really closes.
    exiting: bool,
    /// The band at the top shows the picture.
    header_picture: bool,
    /// The picture, once loaded into the window.
    header_texture: Option<egui::TextureHandle>,
    /// When the key sounds' speaker was last asked to be ready.
    key_tones_warmed: Option<Instant>,
    /// The speaker and the microphone switched off with their marks next to
    /// the volumes; not kept: the phone always starts with both on.
    speaker_off: bool,
    /// What is typed in the search of the call list; not kept.
    calls_query: String,
    microphone_off: bool,
    /// Asks the window to look again from other threads: the phone engine
    /// and the system dialogs call it when they have news.
    repaint: egui::Context,
    /// An incoming call shows a small window above everything.
    incoming_popup: bool,
    /// The user closed that window for the call ringing now.
    popup_dismissed: bool,
    /// The phone's window stays above the others.
    always_on_top: bool,
    /// What the window was last told about staying on top.
    on_top_applied: Option<bool>,
    /// How loud the other side and the microphone are, for the level bars;
    /// falls back slowly like a meter's needle.
    levels: (f32, f32),
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
    echo_cancellation: bool,
    noise_suppression: bool,
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
    /// Windows opens call links with this program.
    link_handler: bool,
    /// Takes the numbers that later copies of the program hand over.
    instance: Option<Instance>,
    /// A number from a link waiting for the registration, and until when.
    link_target: Option<(String, Instant)>,
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
    /// A paste was asked for at this moment; it waits for the number field
    /// to have the focus, but not for long.
    paste_requested: Option<Instant>,
    /// The selection in the number field before a right click, which moves
    /// the cursor to where the mouse is; the menu works on this one.
    number_selection: Option<egui::text::CCursorRange>,

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
    /// "Почта" was pressed in the status bar.
    voicemail_requested: bool,
    /// The saved accounts other than the active one.
    other_accounts: Vec<AccountSettings>,
    /// An account was chosen in the status bar.
    account_requested: Option<usize>,
    /// Passwords typed in this run, by account, so switching back and forth
    /// does not ask again; never written anywhere.
    typed_passwords: HashMap<(String, String), String>,
}

impl PhoneWindow {
    fn new(
        ctx: &egui::Context,
        start_hidden: bool,
        instance: Option<Instance>,
        start_target: Option<String>,
    ) -> Self {
        let settings_path = settings::default_path();
        let sibling = |name: &str| settings_path.as_ref().map(|path| path.with_file_name(name));
        let history_path = sibling("history.tsv");
        let contacts_path = sibling("contacts.tsv");
        let chat_path = sibling("messages.tsv");
        let trace_path = sibling("sip-trace.log");

        let mut notes = Vec::new();
        let saved = match settings_path.as_deref().map(Settings::load) {
            Some(Ok(saved)) => saved,
            Some(Err(error)) => {
                notes.push(tr!(
                    format!("Не удалось прочитать настройки: {error}"),
                    format!("Cannot read the settings: {error}"),
                ));
                Settings::default()
            }
            None => {
                notes.push(
                    tr!(
                        "Папка профиля не найдена; настройки не сохраняются.",
                        "The profile folder was not found; settings are not saved."
                    )
                    .to_owned(),
                );
                Settings::default()
            }
        };
        set_english(saved.language == "en");
        let controls = AudioControls::default();
        let history = history_path
            .as_deref()
            .and_then(|path| History::load(path).ok())
            .unwrap_or_default();
        let contacts = contacts_path
            .as_deref()
            .and_then(|path| Contacts::load(path).ok())
            .unwrap_or_default();
        let chat = chat_path
            .as_deref()
            .and_then(|path| Chat::load(path).ok())
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
            notes.push(tr!(
                format!("Файл настроек: {}", path.display()),
                format!("Settings file: {}", path.display()),
            ));
        }
        if let (Some(path), true) = (trace_path.as_ref(), trace_file.is_some()) {
            notes.push(tr!(
                format!("Журнал SIP: {}", path.display()),
                format!("SIP log: {}", path.display()),
            ));
        }

        let password = if saved.save_password && !saved.password_protected.is_empty() {
            secret::unprotect(&saved.password_protected).unwrap_or_else(|| {
                notes.push(
                    tr!(
                        "Сохранённый пароль не подходит этому пользователю Windows.",
                        "The saved password does not fit this Windows user."
                    )
                    .to_owned(),
                );
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
                    menu_open: tr!("Открыть", "Open").to_owned(),
                    menu_exit: tr!("Выход", "Exit").to_owned(),
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
            link_handler: winshell::link_handler_enabled(),
            instance,
            link_target: start_target.map(|target| (target.clone(), Instant::now() + LINK_WAIT)),
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
            echo_cancellation: saved.echo_cancellation,
            noise_suppression: saved.noise_suppression,
            microphones: input_device_names(),
            speakers: output_device_names(),
            controls: controls.clone(),
            record_calls: saved.record_calls,
            recordings_dir: saved.recordings_dir.clone(),
            do_not_disturb: saved.do_not_disturb,
            auto_answer: saved.auto_answer,
            auto_answer_delay: saved.auto_answer_delay,
            transfer_to: String::new(),
            pending_dnd: None,
            number_changed: false,
            focus_number: true,
            paste_requested: None,
            number_selection: None,
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
            voicemail_requested: false,
            other_accounts: saved.accounts.clone(),
            account_requested: None,
            typed_passwords: HashMap::new(),
            messages_seen: now_unix(),
            chat_with: String::new(),
            chat_draft: String::new(),
            voicemail: None,
            voicemail_number: saved.voicemail_number.clone(),
            presence: HashMap::new(),
            statuses: HashMap::new(),
            presence_on: saved.presence,
            my_status: PresenceStatus::from_name(&saved.presence_status),
            status_refused: None,
            dtmf_method: DtmfMethod::from_name(&saved.dtmf_method),
            all_accounts: saved.all_accounts,
            call_from: None,
            extra_states: HashMap::new(),
            key_tones_on: saved.key_tones,
            key_tones: KeyTones::new(controls.clone()),
            key_tones_warmed: None,
            repaint: ctx.clone(),
            speaker_off: false,
            calls_query: String::new(),
            microphone_off: false,
            exiting: false,
            header_picture: saved.header_picture,
            header_texture: None,
            incoming_popup: saved.incoming_popup,
            popup_dismissed: false,
            always_on_top: saved.always_on_top,
            on_top_applied: None,
            levels: (0.0, 0.0),
            settings_path,
            history_path,
            contacts_path,
            chat_path,
            chat,
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
        self.log(tr!(format!("ОШИБКА: {text}"), format!("ERROR: {text}"),));
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
            self.log(tr!(
                "Не удалось записать журнал SIP; запись остановлена.",
                "Cannot write the SIP log; logging stopped."
            ));
        }
    }

    fn apply_audio_settings(&mut self) {
        // Switched off with its mark: nothing is heard or sent, and the
        // chosen volume comes back when it is switched on again.
        let speaker = if self.speaker_off {
            0
        } else {
            self.speaker_volume
        };
        let microphone = if self.microphone_off {
            0
        } else {
            self.microphone_volume
        };
        self.controls.set_speaker_volume(speaker);
        // Incoming calls ring even with the speaker switched off.
        self.controls.set_ring_volume(self.speaker_volume);
        self.controls.set_microphone_volume(microphone);
        self.controls
            .set_input_device(Some(self.microphone.clone()));
        self.controls.set_output_device(Some(self.speaker.clone()));
        self.controls.set_voice_settings(VoiceSettings {
            echo_cancellation: self.echo_cancellation,
            noise_suppression: self.noise_suppression,
        });
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
                self.error(tr!(
                    format!("Мелодия звонка не подходит: {reason}"),
                    format!("The ringtone cannot be used: {reason}"),
                ));
            }
        }
    }

    fn apply_theme(&self, ctx: &egui::Context) {
        let colors = palette(self.light_theme);
        let mut visuals = if self.light_theme {
            egui::Visuals::light()
        } else {
            egui::Visuals::dark()
        };
        visuals.panel_fill = colors.page;
        visuals.window_fill = colors.card;
        visuals.extreme_bg_color = colors.key;
        visuals.faint_bg_color = colors.key;
        visuals.selection.bg_fill = colors.accent;
        visuals.selection.stroke.color = colors.on_call;
        if self.light_theme {
            // Black on white would hide the selected text.
            visuals.selection.stroke.color = egui::Color32::WHITE;
        }
        visuals.hyperlink_color = colors.accent;
        visuals.window_corner_radius = egui::CornerRadius::same(14);
        visuals.menu_corner_radius = egui::CornerRadius::same(10);
        visuals.window_stroke = egui::Stroke::NONE;
        let widgets = &mut visuals.widgets;
        for state in [
            &mut widgets.noninteractive,
            &mut widgets.inactive,
            &mut widgets.hovered,
            &mut widgets.active,
            &mut widgets.open,
        ] {
            state.corner_radius = egui::CornerRadius::same(10);
        }
        widgets.inactive.weak_bg_fill = colors.key;
        widgets.inactive.bg_fill = colors.key;
        widgets.inactive.bg_stroke = egui::Stroke::new(1.0_f32, colors.key_line);
        widgets.inactive.fg_stroke.color = colors.text;
        widgets.hovered.weak_bg_fill = colors.key_hover;
        widgets.hovered.bg_fill = colors.key_hover;
        widgets.hovered.bg_stroke = egui::Stroke::new(1.0_f32, colors.accent);
        widgets.active.weak_bg_fill = colors.key_hover;
        widgets.active.bg_fill = colors.accent;
        widgets.noninteractive.fg_stroke.color = colors.text;
        widgets.noninteractive.bg_stroke.color = colors.line;
        ctx.set_visuals(visuals);
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
        let password_protected = self.protected_password();
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
            echo_cancellation: self.echo_cancellation,
            noise_suppression: self.noise_suppression,
            local_port: self.port(),
            save_password: self.save_password,
            password_protected,
            record_calls: self.record_calls,
            recordings_dir: self.recordings_dir.trim().to_owned(),
            do_not_disturb: self.do_not_disturb,
            auto_answer: self.auto_answer,
            auto_answer_delay: self.auto_answer_delay,
            voicemail_number: self.voicemail_number.trim().to_owned(),
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
            dtmf_method: self.dtmf_method.name().to_owned(),
            presence: self.presence_on,
            presence_status: self.my_status.name().to_owned(),
            all_accounts: self.all_accounts,
            language: if english() { "en" } else { "ru" }.to_owned(),
            key_tones: self.key_tones_on,
            incoming_popup: self.incoming_popup,
            always_on_top: self.always_on_top,
            header_picture: self.header_picture,
            accounts: self.other_accounts.clone(),
        };
        if current == self.saved {
            return;
        }
        let Some(path) = self.settings_path.clone() else {
            return;
        };
        match current.save(&path) {
            Ok(()) => self.saved = current,
            Err(error) => self.log(tr!(
                format!("Не удалось сохранить настройки: {error}"),
                format!("Cannot save the settings: {error}"),
            )),
        }
    }

    fn save_history(&mut self) {
        if let Some(path) = self.history_path.clone()
            && let Err(error) = self.history.save(&path)
        {
            self.log(tr!(
                format!("Не удалось сохранить список звонков: {error}"),
                format!("Cannot save the call list: {error}"),
            ));
        }
    }

    fn save_contacts(&mut self) {
        if let Some(path) = self.contacts_path.clone()
            && let Err(error) = self.contacts.save(&path)
        {
            self.log(tr!(
                format!("Не удалось сохранить контакты: {error}"),
                format!("Cannot save the contacts: {error}"),
            ));
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
            return self.error(tr!(
                "Укажите сервер и номер в настройках",
                "Enter the server and the number in the settings"
            ));
        }
        self.save_settings();
        let account = Account::new(self.server.trim(), self.user.trim(), self.password.trim())
            .with_domain(self.domain.trim())
            .with_auth_username(self.auth_user.trim())
            .with_transport(self.transport)
            .with_proxy(self.proxy.trim())
            .with_tls_verify(self.tls_verify);
        let server = match account.connect_host_port().and_then(|(host, port)| {
            resolve_socket_address(&host, port).map_err(|error| {
                tr!(
                    format!("сервер {host} не найден: {error}"),
                    format!("server {host} not found: {error}"),
                )
            })
        }) {
            Ok(server) => server,
            Err(error) => {
                return self.error(tr!(
                    format!("Не удалось подключиться: {error}"),
                    format!("Cannot connect: {error}"),
                ));
            }
        };
        let ip = match local_ip_towards(server) {
            Ok(ip) if !ip.is_unspecified() => ip,
            Ok(_) | Err(_) => {
                return self.error(tr!(
                    "Нет сети: сервер недоступен с этого компьютера",
                    "No network: the server cannot be reached from this computer"
                ));
            }
        };
        let local = SocketAddr::new(ip, self.port());
        let controls = self.controls.clone();
        let make_media = move || AudioMedia::with_controls(controls.clone());
        let started = spawn_lines(local, "phone", Timers::default(), LINES, make_media);
        let handle = match started {
            Ok(handle) => handle,
            Err(error) => {
                return self.error(tr!(
                    format!("Не удалось открыть порт {local}: {error}"),
                    format!("Cannot open port {local}: {error}"),
                ));
            }
        };
        // The window is drawn when something happens, not on a timer: the
        // engine wakes it with each piece of news.
        let repaint = self.repaint.clone();
        handle.set_wake(move || repaint.request_repaint());
        let local = handle.local_address();
        self.log(tr!(
            format!("Телефон запущен на {local}"),
            format!("Phone started on {local}"),
        ));
        handle.send(UiCommand::Trace(self.trace_file.is_some()));
        handle.send(UiCommand::DoNotDisturb(self.do_not_disturb));
        handle.send(UiCommand::Forward(self.forward_target()));
        handle.send(UiCommand::Codecs(self.codecs.clone()));
        handle.send(UiCommand::Encryption(self.encryption));
        handle.send(UiCommand::Stun(Some(self.stun.trim().to_owned())));
        handle.send(UiCommand::DtmfMethod(self.dtmf_method));
        handle.send(UiCommand::Presence(
            self.presence_on.then_some(self.my_status),
        ));
        handle.send(UiCommand::Watch(self.contacts.watched()));
        handle.send(UiCommand::Register(account));
        if self.all_accounts {
            handle.send(UiCommand::ExtraAccounts(self.extra_accounts()));
        }
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
            self.log(tr!("Телефон отключён", "Phone stopped"));
        }
        self.local = None;
        self.registration = None;
        self.extra_states.clear();
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
            self.error(tr!(
                "Телефон не подключён. Нажмите «Сохранить и подключить» в настройках.",
                "The phone is not connected. Press \"Save and connect\" in the settings."
            ));
        }
    }

    /// Numbers from links and the command line, and requests of later
    /// copies of the program to come to the front.
    fn take_links(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let requests = self
            .instance
            .as_ref()
            .map(Instance::requests)
            .unwrap_or_default();
        for request in requests {
            self.bring_forward(ctx);
            if let LinkRequest::Dial(target) = request {
                self.log(tr!(
                    format!("Номер из ссылки: {target}"),
                    format!("Number from a link: {target}"),
                ));
                self.link_target = Some((target, Instant::now() + LINK_WAIT));
            }
        }
        let Some((target, deadline)) = self.link_target.take() else {
            return;
        };
        // The number is shown at once; it is dialled as soon as the phone
        // can, as MicroSIP does.
        if self.number != target && self.lines[self.at].view.can_dial() {
            self.number = target.clone();
            self.number_changed = true;
            self.tab = Tab::Dialpad;
        }
        let direct = target.parse::<SocketAddr>().is_ok();
        if (self.is_online() || direct) && self.handle.is_some() {
            actions.push(Action::Call(target));
        } else if Instant::now() < deadline {
            self.link_target = Some((target, deadline));
        } else {
            self.log(tr!(
                "Номер из ссылки не набран: нет регистрации на сервере",
                "The number from the link was not dialled: not registered on the server"
            ));
        }
    }

    /// Shows the window, also from the tray, on top of the others.
    fn bring_forward(&mut self, ctx: &egui::Context) {
        self.start_hidden = false;
        if let Some(shell) = self.shell.as_ref() {
            shell.show_window();
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    }

    /// How soon the window must look again even if nothing happens. News
    /// from the phone, keys, the mouse, the tray and the dialogs wake it by
    /// themselves; this is only for what changes with time alone. A window
    /// at rest is drawn rarely, so it takes next to no processor time.
    fn next_look(&self, ctx: &egui::Context) -> Duration {
        if self.paste_requested.is_some() {
            return Duration::from_millis(100);
        }
        // A number from a link waiting for the registration, a countdown
        // to auto-answer.
        if self.link_target.is_some() || self.lines.iter().any(|line| line.answer_at.is_some()) {
            return Duration::from_millis(250);
        }
        // The call's clock counts seconds.
        if self
            .lines
            .iter()
            .any(|line| line.view.phase() != Phase::Idle)
        {
            return Duration::from_secs(1);
        }
        // Someone is at the window: the key sounds' speaker is kept ready.
        if ctx.input(|input| input.focused) {
            return Duration::from_secs(10);
        }
        // The greeting and "today" in the lists follow the clock.
        Duration::from_secs(30)
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
            return self.error(tr!("Телефон не подключён", "The phone is not connected"));
        }
        let direct = target.parse::<SocketAddr>().is_ok();
        if !direct && !self.is_online() {
            return self.error(tr!(
                "Нет регистрации на сервере — позвонить на номер нельзя",
                "Not registered on the server: numbers cannot be called"
            ));
        }
        self.last_error = None;
        self.number = target.clone();
        self.last_dialled = target.clone();
        self.save_settings();
        let name = self.contacts.name_of(&target).unwrap_or("").to_owned();
        self.log(tr!(format!("Вызов {target}"), format!("Calling {target}"),));
        self.lines[self.at].current = Some(CurrentCall {
            incoming: false,
            number: target.clone(),
            name,
            started: now_unix(),
            connected: None,
            recording: String::new(),
            account: String::new(),
        });
        self.lines[self.at].view.dialing_target(&target);
        match self.call_from_key() {
            Some(account) => self.send(UiCommand::DialFrom { account, target }),
            None => self.send(UiCommand::DialTarget(target)),
        }
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

    /// The microphone off or on again, for every call, until switched back.
    fn toggle_mute(&mut self) {
        self.microphone_off = !self.microphone_off;
        self.apply_audio_settings();
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
            return self.error(tr!(
                "Не задана папка для записей",
                "No folder for recordings is set"
            ));
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
            return self.error(tr!(
                format!("Не найдено: {}", path.display()),
                format!("Not found: {}", path.display()),
            ));
        }
        let program = if cfg!(windows) {
            "explorer"
        } else {
            "xdg-open"
        };
        if let Err(error) = std::process::Command::new(program).arg(&path).spawn() {
            self.error(tr!(
                format!("Не удалось открыть {}: {error}", path.display()),
                format!("Cannot open {}: {error}", path.display()),
            ));
        }
    }

    /// A keypad press: a tone during a call, a digit of the number otherwise.
    fn digit(&mut self, digit: char) {
        if self.key_tones_on {
            self.key_tones.play(digit);
        }
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
        self.lines[self.at].answer_at = None;
        self.transfer_to.clear();
        // The phone finishes the file itself when the call ends.
        self.lines[self.at].recording = None;
        let Some(call) = self.lines[self.at].current.take() else {
            return;
        };
        // The field showed the number called; the call is over, so is it.
        // ("Позвонить" on the empty field brings it back.)
        if !call.incoming && dialable(&self.number) == dialable(&call.number) {
            self.number.clear();
            self.number_changed = true;
        }
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
            result: result_code(reason, call.incoming, call.connected.is_some()),
            ended: now_unix(),
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
                    self.log(tr!(
                        format!("Входящий вызов: {number} {name}"),
                        format!("Incoming call: {number} {name}"),
                    ));
                    self.lines[self.at].current = Some(CurrentCall {
                        incoming: true,
                        number,
                        name,
                        started: now_unix(),
                        connected: None,
                        recording: String::new(),
                        account: String::new(),
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
                Event::Ringing => self.log(tr!("У абонента звонит телефон", "Ringing")),
                Event::Established => {
                    self.log(tr!("Соединено", "Connected"));
                    if let Some(call) = self.lines[self.at].current.as_mut() {
                        call.connected = Some(Instant::now());
                    }
                    if self.record_calls {
                        self.start_recording();
                    }
                }
                Event::Recording(Some(path)) => {
                    self.log(tr!(
                        format!("Запись разговора: {}", path.display()),
                        format!("Recording the call: {}", path.display()),
                    ));
                    if let Some(call) = self.lines[self.at].current.as_mut() {
                        call.recording = path.display().to_string();
                    }
                    self.lines[self.at].recording = Some(path);
                }
                Event::Recording(None) => {
                    self.log(tr!("Запись остановлена", "Recording stopped"));
                    self.lines[self.at].recording = None;
                }
                Event::Transfer(TransferState::Accepted) => self.log(tr!(
                    "Перевод: вызываем номер…",
                    "Transfer: calling the number…"
                )),
                Event::Transfer(TransferState::Completed) => {
                    self.log(tr!("Перевод выполнен", "Transfer done"))
                }
                Event::Transfer(TransferState::Failed(status)) => {
                    self.error(tr!(
                        format!("Перевод не удался (код {status}); разговор продолжается"),
                        format!("The transfer failed (code {status}); the call goes on"),
                    ));
                }
                Event::Declined { number, name } => {
                    // Refused by do-not-disturb: it still belongs in the list.
                    let name = self
                        .contacts
                        .name_of(&number)
                        .map(str::to_owned)
                        .unwrap_or(name);
                    self.log(tr!(
                        format!("Не беспокоить: отклонён вызов {number} {name}"),
                        format!("Do not disturb: call from {number} {name} declined"),
                    ));
                    self.history.add(CallRecord {
                        time: now_unix(),
                        direction: Direction::Missed,
                        seconds: 0,
                        number,
                        name,
                        recording: String::new(),
                        result: "dnd".to_owned(),
                        ended: now_unix(),
                    });
                    self.save_history();
                }
                Event::MediaStats { sent, received } => {
                    self.log(tr!(
                        format!("RTP: отправлено {sent}, принято {received}"),
                        format!("RTP: sent {sent}, received {received}"),
                    ));
                }
                Event::AudioQuality(quality) => {
                    self.log(tr!(format!("Звук: {quality}"), format!("Audio: {quality}"),));
                    // The other side's report, in plain words: how much of
                    // the user's voice did not get there.
                    if let Some(remote) = quality.remote {
                        let mut text = tr!(
                            format!(
                                "У собеседника потерялось {}% вашего звука (пакетов: {}), \
                             неравномерность {} мс",
                                remote.lost_percent, remote.lost_total, remote.jitter_ms
                            ),
                            format!(
                                "The other side lost {}% of your audio ({} packets), jitter {} ms",
                                remote.lost_percent, remote.lost_total, remote.jitter_ms
                            ),
                        );
                        if let Some(round_trip) = remote.round_trip_ms {
                            text.push_str(&tr!(
                                format!(", задержка туда-обратно {round_trip} мс"),
                                format!(", round trip {round_trip} ms"),
                            ));
                        }
                        self.log(text);
                    }
                }
                Event::Hold(true) => self.log(tr!("Вызов на удержании", "Call on hold")),
                Event::Hold(false) => self.log(tr!("Вызов возобновлён", "Call resumed")),
                Event::RemoteHold(true) => self.log(tr!(
                    "Собеседник поставил вызов на удержание",
                    "The other side put the call on hold"
                )),
                Event::RemoteHold(false) => self.log(tr!(
                    "Собеседник возобновил вызов",
                    "The other side resumed the call"
                )),
                Event::Ended(reason) => {
                    self.log(tr!(
                        format!("Вызов завершён: {}", end_reason_text(reason)),
                        format!("Call ended: {}", end_reason_text(reason)),
                    ));
                    self.finish_call(reason, true);
                }
                Event::Registration(state) => {
                    self.log(tr!(
                        format!("Аккаунт: {}", registration_text(&state)),
                        format!("Account: {}", registration_text(&state)),
                    ));
                    self.registration = Some(state);
                }
                Event::LocalAddress(address) => {
                    self.local = Some(address);
                    self.log(tr!(
                        format!("Сеть изменилась: телефон теперь на {address}"),
                        format!("The network changed: the phone is now on {address}"),
                    ));
                }
                Event::Trace {
                    outgoing,
                    peer,
                    text,
                } => self.trace(outgoing, peer, &text),
                // The call view keeps the lamp; the window draws it.
                Event::Line(_) | Event::Codec(_) | Event::Encrypted(_) | Event::LiveQuality(_) => {}
                Event::StatusPublished(Ok(status)) => {
                    if self.status_refused.take().is_some() || status != self.my_status {
                        self.log(tr!(
                            format!("Статус для коллег: {}", status_text(status)),
                            format!("Status for colleagues: {}", status_text(status)),
                        ));
                    }
                }
                Event::StatusPublished(Err(why)) => {
                    self.log(tr!(
format!(
                        "АТС не принимает статус ({why}): коллеги видят только, \
                         свободен ли номер"
                    ),
format!("The exchange does not take the status ({why}): colleagues see only whether the number is free"
                    ),
));
                    self.status_refused = Some(why);
                }
                Event::Status { number, presence } => {
                    self.statuses.insert(number, presence);
                }
                Event::AccountRegistration { account, state } => {
                    let changed = self.extra_states.get(&account).is_none_or(|known| {
                        std::mem::discriminant(known) != std::mem::discriminant(&state)
                    });
                    if changed {
                        self.log(tr!(
                            format!("Аккаунт {account}: {}", registration_text(&state)),
                            format!("Account {account}: {}", registration_text(&state)),
                        ));
                    }
                    self.extra_states.insert(account, state);
                }
                Event::CalledAccount(account) => {
                    let number = account.split('@').next().unwrap_or(&account).to_owned();
                    self.log(tr!(
                        format!("Вызов на номер {number}"),
                        format!("The call is for {number}"),
                    ));
                    if let Some(call) = self.lines[self.at].current.as_mut() {
                        call.account = number;
                    }
                }
                Event::Voicemail(voicemail) => {
                    if voicemail.new > self.voicemail.map_or(0, |known| known.new) {
                        self.log(tr!(
                            format!("Новых голосовых сообщений: {}", voicemail.new),
                            format!("New voicemail messages: {}", voicemail.new),
                        ));
                    }
                    self.voicemail = Some(voicemail);
                }
                Event::Presence { number, state } => {
                    self.presence.insert(number, state);
                }
                Event::Message { number, name, text } => {
                    let who = if name.is_empty() {
                        number.clone()
                    } else {
                        format!("{name} ({number})")
                    };
                    self.log(tr!(
                        format!("Сообщение от {who}"),
                        format!("Message from {who}"),
                    ));
                    self.chat.add(ChatMessage {
                        time: now_unix(),
                        incoming: true,
                        number: number.clone(),
                        name,
                        text,
                    });
                    self.save_chat();
                    // The first conversation opens by itself.
                    if self.chat_with.trim().is_empty() {
                        self.chat_with = number;
                    }
                    // Like a ringing call, a message brings the window up.
                    if let Some(shell) = self.shell.as_ref() {
                        shell.show_window();
                    }
                }
                Event::MessageSent { to, error } => match error {
                    None => self.log(tr!(
                        format!("Сообщение для {to} доставлено на сервер"),
                        format!("Message to {to} delivered to the server"),
                    )),
                    Some(error) => {
                        self.error(tr!(
                            format!("Сообщение для {to} не доставлено: {error}"),
                            format!("Message to {to} not delivered: {error}"),
                        ));
                    }
                },
                Event::Selected(line) => self.selected = line.min(LINES - 1),
                Event::Conference(on) => {
                    self.conference = on;
                    self.log(if on {
                        tr!(
                            "Конференция: все линии соединены",
                            "Conference: all lines joined"
                        )
                    } else {
                        tr!("Конференция завершена", "Conference ended")
                    });
                }
                Event::Forwarded { number, name, to } => {
                    let name = self
                        .contacts
                        .name_of(&number)
                        .map(str::to_owned)
                        .unwrap_or(name);
                    self.log(tr!(
                        format!("Переадресован на {to}: {number} {name}"),
                        format!("Forwarded to {to}: {number} {name}"),
                    ));
                    self.history.add(CallRecord {
                        time: now_unix(),
                        direction: Direction::Missed,
                        seconds: 0,
                        number,
                        name,
                        recording: String::new(),
                        result: format!("forwarded {to}"),
                        ended: now_unix(),
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
                    self.error(tr!(
                        "Укажите имя и номер контакта",
                        "Enter the contact's name and number"
                    ));
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
                    self.error(tr!(
                        "Введите номер, на который перевести",
                        "Enter the number to transfer to"
                    ));
                } else if self.lines[self.at].view.phase() == Phase::Connected {
                    self.last_error = None;
                    self.log(tr!(
                        format!("Перевод на {}", target.trim()),
                        format!("Transfer to {}", target.trim()),
                    ));
                    self.send(UiCommand::Transfer(target.trim().to_owned()));
                }
            }
            Action::Paste => {
                // The field is focused and the system is asked to paste; the
                // text arrives in the next frame like a Ctrl+V would.
                self.focus_number = true;
                self.paste_requested = Some(Instant::now());
            }
            Action::Erase => {
                self.number.pop();
                self.number_changed = true;
            }
            Action::ClearNumber => {
                self.number.clear();
                self.number_changed = true;
                self.focus_number = true;
            }
            Action::SwitchLine(line) => self.switch_line(line),
            Action::JoinLines => {
                if let Some(other) = self.other_line() {
                    self.last_error = None;
                    self.log(tr!(
                        "Соединяю двух собеседников между собой",
                        "Joining the two callers with each other"
                    ));
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
                    self.error(tr!(
                        format!("Не удалось изменить автозапуск: {error}"),
                        format!("Cannot change starting with Windows: {error}"),
                    ));
                }
            },
            Action::AllAccounts(on) => {
                self.all_accounts = on;
                self.update_extra_accounts();
                self.save_settings();
            }
            Action::DtmfMethod(method) => {
                self.dtmf_method = method;
                self.send(UiCommand::DtmfMethod(method));
                self.save_settings();
            }
            Action::Statuses(on) => {
                self.presence_on = on;
                if !on {
                    self.statuses.clear();
                    self.status_refused = None;
                }
                self.send(UiCommand::Presence(on.then_some(self.my_status)));
                self.save_settings();
            }
            Action::English(on) => {
                set_english(on);
                self.save_settings();
            }
            Action::LinkHandler(on) => match winshell::set_link_handler(on) {
                Ok(()) => self.link_handler = winshell::link_handler_enabled(),
                Err(error) => {
                    self.link_handler = winshell::link_handler_enabled();
                    self.error(tr!(
                        format!("Не удалось настроить открытие ссылок: {error}"),
                        format!("Cannot set up opening links: {error}"),
                    ));
                }
            },
            Action::WindowChanged => self.save_settings(),
            Action::SwitchAccount(index) => self.switch_account(index),
            Action::AddAccount => self.add_account(),
            Action::RemoveAccount => self.remove_account(),
            Action::Watch(number, on) => {
                if self.contacts.set_watched(&number, on) {
                    self.save_contacts();
                    if !on {
                        self.presence.remove(&number);
                    }
                    self.send_watch_list();
                }
            }
            Action::OpenChat(number) => {
                self.chat_with = number;
                self.tab = Tab::Messages;
            }
            Action::SendText => self.send_text(),
            Action::ClearChat(number) => {
                self.chat.remove(&number);
                self.save_chat();
            }
            Action::Voicemail => {
                let number = self.voicemail_number.trim().to_owned();
                if number.is_empty() {
                    self.settings_open = true;
                    self.error(tr!(
                        "Укажите номер голосовой почты в настройках",
                        "Enter the voicemail number in the settings"
                    ));
                } else {
                    self.call(number);
                }
            }
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
                (Dialog::ExportHistory, Some(path)) => {
                    self.export_history(&path.with_file_name(history_file_name()))
                }
                _ => self.error(tr!(
                    "Выбор в окне доступен только в Windows — впишите путь вручную",
                    "Choosing in a dialog works on Windows only; type the path"
                )),
            };
        }
        let (sender, receiver) = mpsc::channel();
        let repaint = self.repaint.clone();
        std::thread::spawn(move || {
            let chosen = match dialog {
                Dialog::RecordingsFolder => winshell::pick_folder(),
                Dialog::Ringtone => winshell::pick_open_file("wav"),
                Dialog::ImportContacts => winshell::pick_open_file("csv"),
                Dialog::ExportContacts => winshell::pick_save_file("contacts.csv", "csv"),
                Dialog::ExportHistory => winshell::pick_save_file(&history_file_name(), "csv"),
            };
            let _ = sender.send(chosen);
            repaint.request_repaint();
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
            Dialog::ExportHistory => self.export_history(&path),
        }
    }

    fn import_contacts(&mut self, path: &std::path::Path) {
        match std::fs::read(path) {
            Ok(bytes) => {
                let taken = self.contacts.import_csv(&bytes);
                if taken > 0 {
                    self.save_contacts();
                    self.last_error = None;
                    self.log(tr!(
                        format!("Импорт контактов: {taken} из {}", path.display()),
                        format!("Contacts imported: {taken} of {}", path.display()),
                    ));
                } else {
                    self.error(tr!(
                        format!("В файле {} контактов не найдено", path.display()),
                        format!("No contacts found in {}", path.display()),
                    ));
                }
            }
            Err(error) => self.error(tr!(
                format!("Не удалось прочитать {}: {error}", path.display()),
                format!("Cannot read {}: {error}", path.display()),
            )),
        }
    }

    fn export_contacts(&mut self, path: &std::path::Path) {
        match std::fs::write(path, self.contacts.to_csv()) {
            Ok(()) => {
                self.last_error = None;
                self.log(tr!(
                    format!("Контакты сохранены в {}", path.display()),
                    format!("Contacts saved to {}", path.display()),
                ));
            }
            Err(error) => self.error(tr!(
                format!("Не удалось записать {}: {error}", path.display()),
                format!("Cannot write {}: {error}", path.display()),
            )),
        }
    }

    /// Writes every call into a table for Excel.
    fn export_history(&mut self, path: &std::path::Path) {
        let reason = |code: &str| result_text(code);
        let words = TableWords {
            headings: [
                tr!("Дата", "Date"),
                tr!("Время", "Time"),
                tr!("Окончание", "Ended"),
                tr!("Направление", "Direction"),
                tr!("Номер", "Number"),
                tr!("Имя", "Name"),
                tr!("Длительность разговора", "Conversation length"),
                tr!("Статус", "Status"),
                tr!("Причина", "Reason"),
            ],
            directions: [
                tr!("Исходящий", "Outgoing"),
                tr!("Входящий", "Incoming"),
                tr!("Пропущенный", "Missed"),
            ],
            outcomes: [
                tr!("Дозвон", "Answered"),
                tr!("Недозвон", "Not answered"),
                tr!("Ошибка", "Error"),
            ],
            reason: &reason,
        };
        let table = self.history.to_csv(self.utc_offset, &words);
        match std::fs::write(path, table) {
            Ok(()) => {
                self.last_error = None;
                self.log(tr!(
                    format!(
                        "История звонков ({}) сохранена в {}",
                        self.history.records().len(),
                        path.display()
                    ),
                    format!(
                        "Call history ({}) saved to {}",
                        self.history.records().len(),
                        path.display()
                    ),
                ));
            }
            Err(error) => self.error(tr!(
                format!("Не удалось записать {}: {error}", path.display()),
                format!("Cannot write {}: {error}", path.display()),
            )),
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
            && !self.exiting
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

    // ----- voicemail, busy lamps, messages -----------------------------------

    fn save_chat(&mut self) {
        if let Some(path) = self.chat_path.clone()
            && let Err(error) = self.chat.save(&path)
        {
            self.log(tr!(
                format!("Не удалось сохранить сообщения: {error}"),
                format!("Cannot save the messages: {error}"),
            ));
        }
    }

    /// Tells the phone which contacts to show the line state of.
    fn send_watch_list(&mut self) {
        if self.handle.is_some() {
            self.send(UiCommand::Watch(self.contacts.watched()));
        }
    }

    /// The lamp of a number: what its line is doing, or `None` when the
    /// contact is not watched.
    fn lamp(&self, number: &str) -> Option<LineState> {
        self.contacts
            .list()
            .iter()
            .find(|contact| contact.watched && contact.number == number)
            .map(|_| {
                self.presence
                    .get(number)
                    .copied()
                    .unwrap_or(LineState::Unknown)
            })
    }

    /// A watched colleague's published status, in a few words; nothing for
    /// a plain "online", which the lamp already shows.
    fn status_of(&self, number: &str) -> Option<String> {
        let presence = self.statuses.get(number)?;
        if !presence.online {
            return Some(tr!("не в сети", "offline").to_owned());
        }
        match presence.status {
            Some(PresenceStatus::Online) | None => None,
            Some(status) => Some(status_text(status).to_lowercase()),
        }
    }

    /// Who a number is, for the conversation list.
    fn who(&self, number: &str, name: &str) -> String {
        match self.contacts.name_of(number) {
            Some(saved) => saved.to_owned(),
            None if !name.is_empty() => name.to_owned(),
            None => number.to_owned(),
        }
    }

    fn send_text(&mut self) {
        let to = dialable(&self.chat_with);
        let text = self.chat_draft.trim().to_owned();
        if to.is_empty() || text.is_empty() {
            return;
        }
        if self.handle.is_none() || !self.is_online() {
            return self.error(tr!(
                "Нет регистрации на сервере — сообщение не отправить",
                "Not registered on the server: the message cannot be sent"
            ));
        }
        self.send(UiCommand::SendMessage {
            to: to.clone(),
            text: text.clone(),
        });
        let name = self.contacts.name_of(&to).unwrap_or("").to_owned();
        self.chat.add(ChatMessage {
            time: now_unix(),
            incoming: false,
            number: to.clone(),
            name,
            text,
        });
        self.chat_with = to;
        self.chat_draft.clear();
        self.save_chat();
    }

    fn messages_tab(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        // Opening the tab reads what is shown.
        self.messages_seen = now_unix();
        ui.horizontal(|ui| {
            ui.label(tr!("Кому", "To"));
            ui.add(
                egui::TextEdit::singleline(&mut self.chat_with)
                    .hint_text(tr!("номер", "number"))
                    .desired_width(140.0),
            );
            let partners = self.chat.partners();
            if !partners.is_empty() {
                egui::ComboBox::from_id_salt("chat-partners")
                    .selected_text(tr!("Переписка", "Conversations"))
                    .width(130.0)
                    .show_ui(ui, |ui| {
                        for (number, name) in &partners {
                            let label = format!("{} {number}", self.who(number, name));
                            if ui
                                .selectable_label(self.chat_with == *number, label)
                                .clicked()
                            {
                                actions.push(Action::OpenChat(number.clone()));
                            }
                        }
                    });
            }
        });
        let with = dialable(&self.chat_with);
        if let Some(name) = self.contacts.name_of(&with) {
            ui.weak(name.to_owned());
        }
        ui.separator();

        let input_height = 64.0;
        egui::ScrollArea::vertical()
            .max_height((ui.available_height() - input_height).max(80.0))
            .stick_to_bottom(true)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                let thread = self.chat.with(&with);
                if with.is_empty() {
                    ui.weak(tr!(
                        "Введите номер или выберите переписку.",
                        "Enter a number or choose a conversation."
                    ));
                } else if thread.is_empty() {
                    ui.weak(tr!("Сообщений пока нет.", "No messages yet."));
                }
                for message in thread {
                    let time = format_timestamp(message.time, self.utc_offset);
                    let (who, color) = if message.incoming {
                        (
                            self.who(&message.number, &message.name),
                            self.tint(
                                egui::Color32::LIGHT_GREEN,
                                egui::Color32::from_rgb(30, 130, 50),
                            ),
                        )
                    } else {
                        (
                            tr!("Вы", "You").to_owned(),
                            self.tint(
                                egui::Color32::LIGHT_BLUE,
                                egui::Color32::from_rgb(30, 90, 170),
                            ),
                        )
                    };
                    ui.horizontal(|ui| {
                        ui.colored_label(color, who);
                        ui.weak(time);
                    });
                    ui.label(&message.text);
                    ui.add_space(4.0);
                }
            });
        ui.separator();
        ui.horizontal(|ui| {
            let field = egui::TextEdit::singleline(&mut self.chat_draft)
                .hint_text(tr!("Сообщение", "Message"))
                .desired_width(ui.available_width() - 100.0);
            let response = ui.add(field);
            let enter =
                response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
            let can_send = self.is_online() && !with.is_empty();
            if ui
                .add_enabled(can_send, egui::Button::new(tr!("Отправить", "Send")))
                .clicked()
                || (enter && can_send)
            {
                actions.push(Action::SendText);
                response.request_focus();
            }
        });
        if !with.is_empty()
            && !self.chat.with(&with).is_empty()
            && ui
                .small_button(tr!("Удалить переписку", "Delete the conversation"))
                .clicked()
        {
            actions.push(Action::ClearChat(with));
        }
    }

    // ----- several accounts -------------------------------------------------

    /// The stored form of the password typed: kept as it is while the
    /// password is unchanged, so the file is not rewritten at every start.
    fn protected_password(&self) -> String {
        if !self.save_password || self.password.is_empty() {
            return String::new();
        }
        match secret::unprotect(&self.saved.password_protected) {
            Some(stored) if stored == self.password.trim() => self.saved.password_protected.clone(),
            _ => secret::protect(self.password.trim()).unwrap_or_default(),
        }
    }

    /// The active account as it is typed in the settings.
    fn current_account(&self) -> AccountSettings {
        AccountSettings {
            server: self.server.trim().to_owned(),
            domain: self.domain.trim().to_owned(),
            user: self.user.trim().to_owned(),
            auth_user: self.auth_user.trim().to_owned(),
            save_password: self.save_password,
            password_protected: self.protected_password(),
            transport: self.transport.name().to_owned(),
            proxy: self.proxy.trim().to_owned(),
            tls_verify: self.tls_verify,
            media_encryption: self.encryption.name().to_owned(),
            stun: self.stun.trim().to_owned(),
            voicemail_number: self.voicemail_number.trim().to_owned(),
        }
    }

    /// The password known for a saved account: stored encrypted, or typed
    /// while the program runs.
    fn password_of(&self, account: &AccountSettings) -> Option<String> {
        let saved = if account.save_password && !account.password_protected.is_empty() {
            secret::unprotect(&account.password_protected)
        } else {
            None
        };
        saved
            .or_else(|| {
                self.typed_passwords
                    .get(&(account.server.clone(), account.user.clone()))
                    .cloned()
            })
            .filter(|password| !password.is_empty())
    }

    /// The account the phone registers for a saved one.
    fn account_of(account: &AccountSettings, password: &str) -> Account {
        Account::new(account.server.trim(), account.user.trim(), password.trim())
            .with_domain(account.domain.trim())
            .with_auth_username(account.auth_user.trim())
            .with_transport(SipTransport::from_name(&account.transport))
            .with_proxy(account.proxy.trim())
            .with_tls_verify(account.tls_verify)
    }

    /// The saved accounts besides the active one that can be registered
    /// alongside it; those without a known password are left out.
    fn extra_accounts(&mut self) -> Vec<Account> {
        let mut accounts = Vec::new();
        let mut missing = Vec::new();
        for saved in &self.other_accounts {
            if saved.server.trim().is_empty() || saved.user.trim().is_empty() {
                continue;
            }
            match self.password_of(saved) {
                Some(password) => accounts.push(Self::account_of(saved, &password)),
                None => missing.push(shown_title(saved.title())),
            }
        }
        for title in missing {
            self.log(tr!(
format!(
                "Нет пароля для {title}: снимите «Все аккаунты в сети», выберите его \
                 в верхней строке и введите пароль"
            ),
format!("No password for {title}: switch off \"All accounts online\", choose it in the top row and enter the password"
            ),
));
        }
        accounts
    }

    /// The key of the extra account calls go out through, if one is chosen.
    fn call_from_key(&self) -> Option<String> {
        if !self.all_accounts {
            return None;
        }
        let saved = self.other_accounts.get(self.call_from?)?;
        let password = self.password_of(saved).unwrap_or_default();
        Some(account_key(&Self::account_of(saved, &password)))
    }

    /// Sends the extra accounts again after a change of the list or the mode.
    fn update_extra_accounts(&mut self) {
        let accounts = if self.all_accounts {
            self.extra_accounts()
        } else {
            Vec::new()
        };
        if !self.all_accounts {
            self.call_from = None;
            self.extra_states.clear();
        }
        self.send(UiCommand::ExtraAccounts(accounts));
    }

    /// Makes `account` the one the settings show and the phone uses.
    fn load_account(&mut self, account: &AccountSettings) {
        // The password of the account being left stays at hand.
        if !self.password.is_empty() {
            self.typed_passwords.insert(
                (self.server.trim().to_owned(), self.user.trim().to_owned()),
                self.password.clone(),
            );
        }
        self.server = account.server.clone();
        self.domain = account.domain.clone();
        self.user = account.user.clone();
        self.auth_user = account.auth_user.clone();
        self.save_password = account.save_password;
        let saved = if account.save_password && !account.password_protected.is_empty() {
            secret::unprotect(&account.password_protected)
        } else {
            None
        };
        self.password = saved
            .or_else(|| {
                self.typed_passwords
                    .get(&(account.server.clone(), account.user.clone()))
                    .cloned()
            })
            .unwrap_or_default();
        // What is stored is this account's form of the password now.
        self.saved.password_protected = account.password_protected.clone();
        self.transport = SipTransport::from_name(&account.transport);
        self.proxy = account.proxy.clone();
        self.tls_verify = account.tls_verify;
        self.encryption = MediaEncryption::from_name(&account.media_encryption);
        self.stun = account.stun.clone();
        self.voicemail_number = account.voicemail_number.clone();
        // What the previous account's exchange said does not apply.
        self.voicemail = None;
        self.presence.clear();
        self.statuses.clear();
        self.status_refused = None;
    }

    /// Connects the account just made active, or asks for what is missing.
    fn start_account(&mut self) {
        self.save_settings();
        if self.account_is_set() && !self.password.is_empty() {
            self.log(tr!(
                format!(
                    "Аккаунт: {}",
                    shown_title(account_title(&self.user, &self.server))
                ),
                format!(
                    "Account: {}",
                    shown_title(account_title(&self.user, &self.server))
                ),
            ));
            self.connect();
        } else {
            self.disconnect();
            self.settings_open = true;
        }
    }

    fn switch_account(&mut self, index: usize) {
        if index >= self.other_accounts.len() {
            return;
        }
        // With every account online the list chooses whose number calls
        // go out with; nothing is registered anew.
        if self.all_accounts {
            self.call_from = Some(index);
            return;
        }
        if self
            .lines
            .iter()
            .any(|line| line.view.phase() != Phase::Idle)
        {
            return self.error(tr!(
                "Сменить аккаунт можно, когда нет звонков",
                "The account can be changed when there are no calls"
            ));
        }
        let current = self.current_account();
        let chosen = self.other_accounts.remove(index);
        self.other_accounts.push(current);
        self.load_account(&chosen);
        self.start_account();
    }

    fn add_account(&mut self) {
        if self
            .lines
            .iter()
            .any(|line| line.view.phase() != Phase::Idle)
        {
            return self.error(tr!(
                "Добавить аккаунт можно, когда нет звонков",
                "An account can be added when there are no calls"
            ));
        }
        let current = self.current_account();
        self.other_accounts.push(current);
        self.load_account(&AccountSettings::default());
        self.disconnect();
        self.save_settings();
        self.settings_open = true;
        self.log(tr!(
            "Новый аккаунт: укажите сервер, номер и пароль",
            "New account: enter the server, the number and the password"
        ));
    }

    /// Forgets the active account; the next saved one takes its place.
    fn remove_account(&mut self) {
        if self.other_accounts.is_empty() {
            return;
        }
        if self
            .lines
            .iter()
            .any(|line| line.view.phase() != Phase::Idle)
        {
            return self.error(tr!(
                "Удалить аккаунт можно, когда нет звонков",
                "An account can be removed when there are no calls"
            ));
        }
        let removed = shown_title(account_title(&self.user, &self.server));
        self.call_from = None;
        let next = self.other_accounts.remove(0);
        self.load_account(&next);
        self.log(tr!(
            format!("Аккаунт удалён: {removed}"),
            format!("Account removed: {removed}"),
        ));
        self.start_account();
    }

    // ----- drawing -------------------------------------------------------

    /// With several accounts the active one is chosen here.
    fn account_choice(&mut self, ui: &mut egui::Ui) {
        if self.other_accounts.is_empty() {
            return;
        }
        let current = shown_title(account_title(&self.user, &self.server));
        if self.all_accounts {
            return self.call_from_choice(ui, &current);
        }
        // Closed it shows the number only: the bar is narrow.
        let short = if self.user.trim().is_empty() {
            tr!("аккаунт", "account").to_owned()
        } else {
            self.user.trim().to_owned()
        };
        egui::ComboBox::from_id_salt("account-choice")
            .selected_text(egui::RichText::new(short).strong())
            .width(64.0)
            .show_ui(ui, |ui| {
                ui.add_enabled(false, egui::SelectableLabel::new(true, &current));
                for (index, account) in self.other_accounts.iter().enumerate() {
                    if ui
                        .selectable_label(false, shown_title(account.title()))
                        .clicked()
                    {
                        self.account_requested = Some(index);
                    }
                }
            });
    }

    /// With every account online: whose number calls go out with, and how
    /// each account is doing.
    fn call_from_choice(&mut self, ui: &mut egui::Ui, current: &str) {
        let chosen = self
            .call_from
            .and_then(|index| self.other_accounts.get(index))
            .map_or_else(
                || self.user.trim().to_owned(),
                |account| account.user.trim().to_owned(),
            );
        let main_state = registration_short(self.registration.as_ref());
        let mut rows = vec![(None, format!("{current} — {main_state}"))];
        for (index, account) in self.other_accounts.iter().enumerate() {
            let password = self.password_of(account).unwrap_or_default();
            let key = account_key(&Self::account_of(account, &password));
            let state = registration_short(self.extra_states.get(&key));
            rows.push((
                Some(index),
                format!("{} — {state}", shown_title(account.title())),
            ));
        }
        egui::ComboBox::from_id_salt("call-from")
            .selected_text(egui::RichText::new(chosen).small())
            .width(64.0)
            .show_ui(ui, |ui| {
                ui.weak(tr!("Звонить с номера:", "Call from:"));
                for (index, text) in rows {
                    if ui.selectable_label(self.call_from == index, text).clicked() {
                        self.call_from = index;
                    }
                }
            })
            .response
            .on_hover_text(tr!(
                "Все аккаунты в сети; здесь — с какого номера звонить",
                "All accounts are online; choose here which number calls go out from"
            ));
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
            Phase::Idle => tr!("свободна", "free").to_owned(),
            Phase::Incoming => tr!(format!("входящий {who}"), format!("incoming {who}"),),
            Phase::Connected if state.view.is_held() => {
                tr!(format!("удержание {who}"), format!("hold {who}"),)
            }
            Phase::Connected => tr!(format!("разговор {who}"), format!("talking {who}"),),
            _ => tr!(format!("вызов {who}"), format!("calling {who}"),),
        };
        tr!(
            format!("Линия {}: {what}", line + 1),
            format!("Line {}: {what}", line + 1),
        )
    }

    fn lines_bar(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        ui.horizontal_wrapped(|ui| {
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
                ui.colored_label(egui::Color32::YELLOW, tr!("Второй вызов:", "Second call:"));
                if ui.button(tr!("Ответить", "Answer")).clicked() {
                    actions.push(Action::Answer);
                }
                if ui.button(tr!("Показать", "Show")).clicked() {
                    actions.push(Action::SwitchLine(other));
                }
            });
        }
        // With two calls connected they can be joined.
        let both = (0..LINES).all(|line| self.lines[line].view.phase() == Phase::Connected);
        if both {
            ui.horizontal(|ui| {
                if ui
                    .button(tr!("Соединить собеседников", "Join the callers"))
                    .clicked()
                {
                    actions.push(Action::JoinLines);
                }
                let label = if self.conference {
                    tr!("Завершить конференцию", "End the conference")
                } else {
                    tr!("Конференция", "Conference")
                };
                if ui.button(label).clicked() {
                    actions.push(Action::Conference(!self.conference));
                }
            });
        }
        ui.separator();
    }

    fn dialpad_tab(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        // The keypad is in sight and the user is at the window: its sounds'
        // speaker is kept awake, so the first key is heard whole (Windows
        // needs a moment to wake one). Away from the window the sound card
        // is let go after a minute; calls have their own sound and are not
        // affected.
        let at_window = ui
            .ctx()
            .input(|input| input.focused || input.pointer.has_pointer());
        if self.key_tones_on
            && at_window
            && self
                .key_tones_warmed
                .is_none_or(|at| at.elapsed() >= Duration::from_secs(10))
        {
            self.key_tones.warm();
            self.key_tones_warmed = Some(Instant::now());
        }
        // Everything is sized from the room the card has, so the window can
        // be made any size; when it is too small for even the smallest keys,
        // the card scrolls instead of being cut off.
        let room = ui.available_size();
        let gap = ui.spacing().item_spacing;
        // A ringing call has one row of buttons under the keys, like idle.
        let idle = matches!(
            self.lines[self.at].view.phase(),
            Phase::Idle | Phase::Incoming
        );
        let calls = self
            .lines
            .iter()
            .any(|line| line.view.phase() != Phase::Idle);
        // Above the keys: the number (or who is on the line) and a line of
        // hints, and the lines when there are calls; below them one row of
        // buttons when idle, about four in a call.
        let dialling = self.lines[self.at].view.phase() == Phase::Idle;
        let above = if dialling { 40.0 } else { 70.0 } + if calls { 34.0 } else { 0.0 };
        // Rows under the keys, in key heights and in fixed pixels: the
        // call button (0.95 of a key); in a call also two smaller rows and
        // the transfer field.
        let (below, fixed) = if idle {
            (0.95, 5.0 * gap.y)
        } else {
            (0.95 + 0.71, 6.0 * gap.y + 30.0)
        };
        // The buttons under the keys stop growing at a comfortable height;
        // what a large window has left goes to the keys.
        let estimate = ((room.y - above - 14.0 - fixed) / (4.0 + below)).clamp(22.0, 150.0);
        let row = (estimate * 0.95).clamp(26.0, 64.0);
        let rows = row * below / 0.95;
        let key_height = ((room.y - above - 14.0 - fixed - rows) / 4.0).clamp(22.0, 150.0);
        let sizes = PadSizes {
            key: egui::vec2(((room.x - 2.0 * gap.x) / 3.0).max(36.0), key_height),
            key_font: (key_height * 0.42).clamp(13.0, 40.0),
            row,
            row_font: (row * 0.32).clamp(13.0, 20.0),
        };
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                self.dialpad_content(ui, actions, &sizes);
            });
    }

    /// The twelve keys: the digit large, its letters small under it.
    fn keypad(
        &mut self,
        ui: &mut egui::Ui,
        actions: &mut Vec<Action>,
        sizes: &PadSizes,
        colors: &Palette,
    ) {
        for row in KEYPAD {
            ui.horizontal(|ui| {
                for digit in row.chars() {
                    let mut text = egui::text::LayoutJob {
                        halign: egui::Align::Center,
                        ..Default::default()
                    };
                    text.append(
                        &digit.to_string(),
                        0.0,
                        egui::TextFormat {
                            font_id: egui::FontId::proportional(sizes.key_font),
                            color: colors.text,
                            ..Default::default()
                        },
                    );
                    let letters = key_letters(digit);
                    if !letters.is_empty() && sizes.key.y >= 40.0 {
                        text.append(
                            &format!("\n{letters}"),
                            0.0,
                            egui::TextFormat {
                                font_id: egui::FontId::proportional(
                                    (sizes.key_font * 0.36).max(9.0),
                                ),
                                color: colors.weak,
                                extra_letter_spacing: 1.5,
                                ..Default::default()
                            },
                        );
                    }
                    let key = egui::Button::new(text)
                        .fill(colors.key)
                        .stroke(egui::Stroke::new(1.0_f32, colors.key_line))
                        .corner_radius(14);
                    if ui.add_sized(sizes.key, key).clicked() {
                        actions.push(Action::Digit(digit));
                    }
                }
            });
        }
    }

    /// Call and erase when idle; answer and decline while ringing; hang up,
    /// mute, hold, record and transfer in a call.
    fn call_buttons(
        &mut self,
        ui: &mut egui::Ui,
        actions: &mut Vec<Action>,
        sizes: &PadSizes,
        phase: Phase,
        colors: &Palette,
    ) {
        let spacing = ui.spacing().item_spacing.x;
        let wide = egui::vec2(ui.available_width(), sizes.row);
        let half = egui::vec2((ui.available_width() - spacing) / 2.0, sizes.row);
        let small_row = egui::vec2(ui.available_width(), (sizes.row * 0.75).max(22.0));
        let small_half = egui::vec2(half.x, small_row.y);
        let colored = |text: &str, fill: egui::Color32, ink: egui::Color32| {
            egui::Button::new(
                egui::RichText::new(text)
                    .size(sizes.row_font + 1.0)
                    .strong()
                    .color(ink),
            )
            .fill(fill)
            .stroke(egui::Stroke::NONE)
            .corner_radius(14)
        };
        let plain = |text: &str| {
            egui::Button::new(egui::RichText::new(text).color(colors.text))
                .fill(colors.key)
                .stroke(egui::Stroke::new(1.0_f32, colors.key_line))
                .corner_radius(12)
        };
        let white = egui::Color32::WHITE;
        match phase {
            Phase::Idle => {
                ui.horizontal(|ui| {
                    let erase_width = (sizes.key.x * 0.55).clamp(48.0, 90.0);
                    let call_width = ui.available_width() - erase_width - spacing;
                    let call = colored(tr!("✆  Позвонить", "✆  Call"), colors.call, colors.on_call);
                    if ui
                        .add_sized([call_width, sizes.row], call)
                        .on_hover_text(tr!(
                            "На пустом поле — вернуть последний набранный номер",
                            "With an empty field: bring back the last number dialled"
                        ))
                        .clicked()
                    {
                        actions.push(Action::Call(self.number.clone()));
                    }
                    let has_number = !self.number.is_empty();
                    let erase = ui
                        .add_enabled(
                            has_number,
                            egui::Button::new("")
                                .fill(colors.key)
                                .stroke(egui::Stroke::new(1.0_f32, colors.key_line))
                                .corner_radius(14)
                                .min_size(egui::vec2(erase_width, sizes.row)),
                        )
                        .on_hover_text(tr!(
                            "Стереть цифру. Правый клик — стереть весь номер",
                            "Erase a digit. Right click: erase the whole number"
                        ));
                    let ink = if has_number { colors.text } else { colors.weak };
                    paint_backspace(ui.painter(), erase.rect, ink);
                    if erase.clicked() {
                        actions.push(Action::Erase);
                    }
                    if erase.secondary_clicked() {
                        actions.push(Action::ClearNumber);
                    }
                });
            }
            Phase::Incoming => {
                ui.horizontal(|ui| {
                    if ui
                        .add_sized(
                            half,
                            colored(tr!("Ответить", "Answer"), colors.call, colors.on_call),
                        )
                        .clicked()
                    {
                        actions.push(Action::Answer);
                    }
                    if ui
                        .add_sized(
                            half,
                            colored(tr!("Отклонить", "Decline"), colors.red, white),
                        )
                        .clicked()
                    {
                        actions.push(Action::Reject);
                    }
                });
            }
            _ => {
                let pressed = ui
                    .add_enabled_ui(self.lines[self.at].view.can_hangup(), |ui| {
                        ui.add_sized(
                            wide,
                            colored(tr!("Завершить", "Hang up"), colors.red, white),
                        )
                    })
                    .inner
                    .clicked();
                if pressed {
                    actions.push(Action::Hangup);
                }
                let connected = phase == Phase::Connected;
                let hold = if self.lines[self.at].view.is_held() {
                    tr!("Продолжить", "Resume")
                } else {
                    tr!("Удержание", "Hold")
                };
                let record = if self.lines[self.at].recording.is_some() {
                    tr!("Стоп запись", "Stop recording")
                } else {
                    tr!("Записать", "Record")
                };
                // The microphone is switched off with its mark next to the
                // volumes, for every call.
                ui.add_enabled_ui(connected, |ui| {
                    ui.horizontal(|ui| {
                        if ui.add_sized(small_half, plain(hold)).clicked() {
                            actions.push(Action::ToggleHold);
                        }
                        if ui
                            .add_sized(small_half, plain(record))
                            .on_hover_text(tr!(
                                "Записать разговор в файл",
                                "Record the call to a file"
                            ))
                            .clicked()
                        {
                            actions.push(Action::ToggleRecording);
                        }
                    });
                    ui.horizontal(|ui| {
                        let field = egui::TextEdit::singleline(&mut self.transfer_to)
                            .hint_text(tr!("Перевести на номер", "Transfer to number"))
                            .desired_width((ui.available_width() - 100.0).max(80.0));
                        let typed = ui.add(field);
                        let enter = typed.lost_focus()
                            && ui.input(|input| input.key_pressed(egui::Key::Enter));
                        if ui.add(plain(tr!("Перевести", "Transfer"))).clicked() || enter {
                            actions.push(Action::Transfer(self.transfer_to.clone()));
                        }
                    });
                });
            }
        }
    }

    /// The whole window: the account, NAVI and the menu at the top; a card
    /// with the current page; tiles for the pages; the volumes at the bottom
    /// of the dial page.
    fn main_screen(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let colors = palette(self.light_theme);
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(colors.page))
            .show(ctx, |ui| {
                let full = ui.max_rect();
                let margin = 16.0;
                let head = egui::Rect::from_min_max(
                    full.min + egui::vec2(margin, 12.0),
                    egui::pos2(full.right() - margin, full.top() + 12.0 + 36.0),
                );
                ui.allocate_new_ui(egui::UiBuilder::new().max_rect(head), |ui| {
                    self.header(ui, &colors);
                });

                // A low window: lower tiles and volumes and smaller gaps, so
                // that the card keeps room for the keys and the call buttons.
                let low = full.height() < 600.0;
                let gap = if low { 8.0 } else { 10.0 };
                let bottom = full.bottom() - if low { 10.0 } else { 14.0 };
                let volumes = match (self.tab == Tab::Dialpad, low) {
                    (false, _) => 0.0,
                    (true, true) => 52.0,
                    (true, false) => 70.0,
                };
                let tiles_height = if low { 36.0 } else { 64.0 };
                let volume_rect = egui::Rect::from_min_max(
                    egui::pos2(full.left() + margin, bottom - volumes),
                    egui::pos2(full.right() - margin, bottom),
                );
                let tiles_bottom = if volumes > 0.0 {
                    volume_rect.top() - gap
                } else {
                    bottom
                };
                let tiles_rect = egui::Rect::from_min_max(
                    egui::pos2(full.left() + margin, tiles_bottom - tiles_height),
                    egui::pos2(full.right() - margin, tiles_bottom),
                );
                let card_rect = egui::Rect::from_min_max(
                    egui::pos2(full.left() + margin, head.bottom() + gap),
                    egui::pos2(full.right() - margin, tiles_rect.top() - gap),
                );
                paint_card(ui.painter(), card_rect, 20, &colors);
                let inside = card_rect.shrink(if low { 10.0 } else { 14.0 });
                ui.allocate_new_ui(egui::UiBuilder::new().max_rect(inside), |ui| {
                    ui.set_clip_rect(card_rect.shrink(2.0));
                    match self.tab {
                        Tab::Dialpad => self.dialpad_tab(ui, actions),
                        Tab::Calls => self.calls_tab(ui, actions),
                        Tab::Contacts => self.contacts_tab(ui, actions),
                        Tab::Messages => self.messages_tab(ui, actions),
                    }
                });
                self.tiles(ui, tiles_rect, &colors);
                if volumes > 0.0 {
                    paint_card(ui.painter(), volume_rect, 18, &colors);
                    let inside =
                        volume_rect.shrink2(egui::vec2(16.0, if low { 4.0 } else { 10.0 }));
                    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(inside), |ui| {
                        ui.spacing_mut().item_spacing.y = if low { 2.0 } else { 8.0 };
                        self.volume_sliders(ui, &colors);
                    });
                }
            });
    }

    /// The letters NAVI, loaded once from the program itself.
    fn header_texture(&mut self, ctx: &egui::Context) -> Option<egui::TextureHandle> {
        if self.header_texture.is_none() {
            let decoded =
                image::load_from_memory_with_format(HEADER_PICTURE, image::ImageFormat::Png)
                    .ok()?
                    .to_rgba8();
            let size = [decoded.width() as usize, decoded.height() as usize];
            let picture = egui::ColorImage::from_rgba_unmultiplied(size, decoded.as_raw());
            self.header_texture = Some(ctx.load_texture(
                "header-picture",
                picture,
                // Shown much smaller than drawn: smoothed at every size.
                egui::TextureOptions::LINEAR.with_mipmap_mode(Some(egui::TextureFilter::Linear)),
            ));
        }
        self.header_texture.clone()
    }

    /// The top row: the account and its state in a pill, NAVI and the menu.
    fn header(&mut self, ui: &mut egui::Ui, colors: &Palette) {
        {
            let visuals = ui.visuals_mut();
            for state in [
                &mut visuals.widgets.inactive,
                &mut visuals.widgets.hovered,
                &mut visuals.widgets.active,
                &mut visuals.widgets.open,
            ] {
                state.weak_bg_fill = colors.chip;
                state.bg_fill = colors.chip;
                state.bg_stroke = egui::Stroke::NONE;
                state.fg_stroke.color = colors.text;
            }
            visuals.widgets.hovered.weak_bg_fill = colors.key_hover;
        }
        let (dot, text) = match self.registration.as_ref() {
            Some(RegistrationState::Registered { .. }) => {
                (colors.green, tr!("в сети", "online").to_owned())
            }
            Some(RegistrationState::Registering) => (
                egui::Color32::from_rgb(245, 180, 0),
                tr!("подключение…", "connecting…").to_owned(),
            ),
            Some(state @ RegistrationState::Failed(_)) => (colors.red, registration_text(state)),
            Some(RegistrationState::Unregistered) | None => {
                (colors.weak, tr!("не в сети", "offline").to_owned())
            }
        };
        let letters = if self.header_picture {
            self.header_texture(ui.ctx())
        } else {
            None
        };
        ui.horizontal_centered(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().button_padding = egui::vec2(9.0, 6.0);
                let menu = ui
                    .menu_button(
                        egui::RichText::new("☰").size(18.0).color(colors.text),
                        |ui| {
                            self.main_menu(ui);
                        },
                    )
                    .response;
                // News in the pages of the menu: a red dot on its button.
                let unread = self.chat.received_since(self.messages_seen);
                let new_mail = self.voicemail.map_or(0, |voicemail| voicemail.new);
                if unread > 0 || new_mail > 0 {
                    ui.painter().circle_filled(
                        menu.rect.right_top() + egui::vec2(-3.0, 3.0),
                        4.5,
                        colors.red,
                    );
                }
                if let Some(letters) = letters.as_ref() {
                    let [width, height] = letters.size();
                    let tall = 24.0;
                    let size = egui::vec2(tall * width as f32 / height as f32, tall);
                    let (place, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                    ui.painter().image(
                        letters.id(),
                        place,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        colors.accent,
                    );
                }
                if self.do_not_disturb {
                    ui.label(
                        egui::RichText::new(tr!("Не беспокоить", "Do not disturb"))
                            .color(colors.red)
                            .small(),
                    );
                }
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    egui::Frame::NONE
                        .fill(colors.chip)
                        .corner_radius(16)
                        .inner_margin(egui::Margin {
                            left: 10,
                            right: 12,
                            top: 5,
                            bottom: 5,
                        })
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            let (spot, _) = ui
                                .allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                            ui.painter().circle_filled(
                                spot.center(),
                                6.0,
                                dot.gamma_multiply(0.25),
                            );
                            ui.painter().circle_filled(spot.center(), 4.0, dot);
                            self.account_choice(ui);
                            let user = self.user.trim();
                            if !user.is_empty() && self.other_accounts.is_empty() {
                                ui.label(egui::RichText::new(user).strong().color(colors.text));
                            }
                            ui.add(
                                egui::Label::new(egui::RichText::new(text).color(colors.weak))
                                    .truncate(),
                            );
                        });
                });
            });
        });
    }

    /// The window's menu: settings, the log and the switches that used to
    /// fill the top row.
    fn main_menu(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(200.0);
        // The pages that have no tile.
        let unread = self.chat.received_since(self.messages_seen);
        let new_mail = self.voicemail.map_or(0, |voicemail| voicemail.new);
        if ui.button(tr!("Контакты", "Contacts")).clicked() {
            self.tab = Tab::Contacts;
            self.settings_open = false;
            ui.close_menu();
        }
        let chat = if unread > 0 {
            tr!(
                format!("Чат (новых: {unread})"),
                format!("Chat ({unread} new)")
            )
        } else {
            tr!("Чат", "Chat").to_owned()
        };
        if ui.button(chat).clicked() {
            self.tab = Tab::Messages;
            self.settings_open = false;
            ui.close_menu();
        }
        let mail = if new_mail > 0 {
            tr!(
                format!("Голосовая почта (новых: {new_mail})"),
                format!("Voicemail ({new_mail} new)")
            )
        } else {
            tr!("Голосовая почта", "Voicemail").to_owned()
        };
        if ui.button(mail).clicked() {
            self.voicemail_requested = true;
            ui.close_menu();
        }
        ui.separator();
        if ui.button(tr!("Настройки", "Settings")).clicked() {
            self.settings_open = !self.settings_open;
            ui.close_menu();
        }
        if ui.button(tr!("Журнал", "Log")).clicked() {
            self.log_open = !self.log_open;
            ui.close_menu();
        }
        ui.separator();
        let mut dnd = self.do_not_disturb;
        if ui
            .checkbox(&mut dnd, tr!("Не беспокоить", "Do not disturb"))
            .changed()
        {
            self.pending_dnd = Some(dnd);
        }
        if self.presence_on {
            ui.label(egui::RichText::new(tr!("Мой статус", "My status")).weak());
            let before = self.my_status;
            for status in PresenceStatus::ALL {
                ui.radio_value(&mut self.my_status, status, status_text(status));
            }
            if self.my_status != before {
                self.send(UiCommand::Presence(Some(self.my_status)));
                self.save_settings();
            }
            if let Some(why) = self.status_refused.as_ref() {
                ui.weak(tr!(
                    format!("АТС не принимает статус ({why})"),
                    format!("The exchange does not take the status ({why})"),
                ));
            }
        }
        ui.separator();
        if ui
            .checkbox(
                &mut self.always_on_top,
                tr!("Поверх всех окон", "Always on top"),
            )
            .changed()
        {
            self.save_settings();
        }
        if ui
            .checkbox(&mut self.key_tones_on, tr!("Звук кнопок", "Keypad sounds"))
            .changed()
        {
            self.save_settings();
        }
        ui.separator();
        // Closes the program, not only the window: calls end and the
        // account goes off the exchange.
        if ui
            .button(tr!("Выход", "Exit"))
            .on_hover_text(tr!(
                "Закрыть программу полностью, а не свернуть в трей",
                "Close the program completely, not to the tray"
            ))
            .clicked()
        {
            self.exiting = true;
            self.save_settings();
            ui.close_menu();
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    /// The two tiles under the card, dial and calls, the page shown in the
    /// accent colour and the missed calls counted in red; the other pages
    /// are in the menu.
    fn tiles(&mut self, ui: &mut egui::Ui, rect: egui::Rect, colors: &Palette) {
        let missed = self.history.missed_since(self.missed_seen);
        let tiles = [
            (Tab::Dialpad, Icon::Keypad, tr!("Набор", "Dial"), 0),
            (Tab::Calls, Icon::Clock, tr!("Звонки", "Calls"), missed),
        ];
        let gap = 8.0;
        let width = (rect.width() - gap) / 2.0;
        for (index, (tab, icon, label, count)) in tiles.into_iter().enumerate() {
            let tile = egui::Rect::from_min_size(
                rect.min + egui::vec2(index as f32 * (width + gap), 0.0),
                egui::vec2(width, rect.height()),
            );
            let response = ui.interact(tile, ui.id().with(("tile", index)), egui::Sense::click());
            let shown = tab == self.tab;
            let fill = if response.hovered() {
                colors.key_hover
            } else {
                colors.card
            };
            let painter = ui.painter();
            painter.rect(
                tile,
                14,
                fill,
                egui::Stroke::new(1.0_f32, colors.card_line),
                egui::StrokeKind::Inside,
            );
            let ink = if shown { colors.accent } else { colors.weak };
            // The mark and the word side by side, together in the middle.
            let word =
                painter.layout_no_wrap(label.to_owned(), egui::FontId::proportional(14.0), ink);
            let badge = (count > 0).then(|| {
                let text = if count > 99 {
                    "99+".to_owned()
                } else {
                    count.to_string()
                };
                painter.layout_no_wrap(text, egui::FontId::proportional(10.0), egui::Color32::WHITE)
            });
            let badge_width = badge.as_ref().map_or(0.0, |badge| badge.size().x + 18.0);
            let total = 22.0 + 10.0 + word.size().x + badge_width;
            let left = tile.center().x - total / 2.0;
            paint_icon(painter, icon, egui::pos2(left + 11.0, tile.center().y), ink);
            let word_left = left + 32.0;
            let word_width = word.size().x;
            painter.galley(
                egui::pos2(word_left, tile.center().y - word.size().y / 2.0),
                word,
                ink,
            );
            if let Some(badge) = badge {
                let size = egui::vec2(badge.size().x + 12.0, 17.0);
                let pill = egui::Rect::from_min_size(
                    egui::pos2(word_left + word_width + 6.0, tile.center().y - size.y / 2.0),
                    size,
                );
                painter.rect_filled(pill, 9, colors.red);
                painter.galley(
                    pill.center() - badge.size() / 2.0,
                    badge,
                    egui::Color32::WHITE,
                );
            }
            if response.clicked() {
                self.tab = tab;
                self.settings_open = false;
            }
        }
    }

    fn volume_sliders(&mut self, ui: &mut egui::Ui, colors: &Palette) {
        let before = (self.speaker_volume, self.microphone_volume);
        // During a call a thin bar under each slider shows how loud the
        // other side is and how loud the microphone is, as in MicroSIP.
        let talking = self
            .lines
            .iter()
            .any(|line| line.view.phase() == Phase::Connected);
        let (heard, spoken) = self.controls.take_levels();
        let fall = |old: f32, new: f32| if talking { new.max(old * 0.85) } else { 0.0 };
        self.levels = (fall(self.levels.0, heard), fall(self.levels.1, spoken));
        if talking {
            ui.ctx().request_repaint_after(Duration::from_millis(50));
        }
        let (heard_level, spoken_level) = self.levels;
        let rows = [
            (
                Icon::Speaker,
                tr!("Динамик", "Speaker"),
                tr!(
                    "Полоска — как громко говорит собеседник",
                    "The bar shows how loud the other side speaks"
                ),
                heard_level,
            ),
            (
                Icon::Microphone,
                tr!("Микрофон", "Microphone"),
                tr!(
                    "Полоска — как громко вас слышно",
                    "The bar shows how loud you are"
                ),
                spoken_level,
            ),
        ];
        let mut toggled = None;
        for (index, (icon, name, hint, level)) in rows.into_iter().enumerate() {
            let off = if index == 0 {
                self.speaker_off
            } else {
                self.microphone_off
            };
            let value = if index == 0 {
                &mut self.speaker_volume
            } else {
                &mut self.microphone_volume
            };
            ui.horizontal(|ui| {
                // The mark switches it off and on; off, it is crossed out
                // in red and the slider is dimmed.
                let (spot, mark) =
                    ui.allocate_exact_size(egui::vec2(24.0, 22.0), egui::Sense::click());
                if mark.hovered() {
                    ui.painter().rect_filled(spot, 6, colors.key_hover);
                }
                let ink = if off { colors.red } else { colors.weak };
                paint_icon(ui.painter(), icon, spot.center(), ink);
                if off {
                    let center = spot.center();
                    ui.painter().line_segment(
                        [
                            center + egui::vec2(-8.0, -8.0),
                            center + egui::vec2(8.0, 8.0),
                        ],
                        egui::Stroke::new(2.0_f32, colors.red),
                    );
                }
                let switch = match (index, off) {
                    (0, false) => tr!("Выключить динамик", "Turn the speaker off"),
                    (0, true) => tr!("Включить динамик", "Turn the speaker on"),
                    (_, false) => tr!("Выключить микрофон", "Mute the microphone"),
                    (_, true) => tr!("Включить микрофон", "Unmute the microphone"),
                };
                if mark.on_hover_text(format!("{name}: {switch}")).clicked() {
                    toggled = Some(index);
                }
                let width = (ui.available_width() - 46.0).max(60.0);
                let dimmed = if off {
                    Palette {
                        accent: colors.weak,
                        knob: colors.weak,
                        ..*colors
                    }
                } else {
                    *colors
                };
                level_slider(ui, value, 200, width, &dimmed, talking.then_some(level))
                    .on_hover_text(hint);
                let shown = if off {
                    tr!("выкл", "off").to_owned()
                } else {
                    format!("{value}%")
                };
                ui.label(egui::RichText::new(shown).color(ink).size(12.5));
            });
        }
        match toggled {
            Some(0) => {
                self.speaker_off = !self.speaker_off;
                self.apply_audio_settings();
            }
            Some(_) => self.toggle_mute(),
            None => {}
        }
        if before != (self.speaker_volume, self.microphone_volume) {
            // Volumes act at once; they are saved when the window closes.
            self.apply_audio_settings();
        }
    }

    /// Undo, cut, copy, paste, delete and select all for the number field.
    fn number_menu(&mut self, ui: &mut egui::Ui, id: egui::Id, actions: &mut Vec<Action>) {
        let ctx = ui.ctx().clone();
        let mut state = egui::TextEdit::load_state(&ctx, id).unwrap_or_default();
        let length = self.number.chars().count();
        let range = self
            .number_selection
            .or_else(|| state.cursor.char_range())
            .unwrap_or_else(|| egui::text::CCursorRange::one(egui::text::CCursor::new(length)));
        // The right click moved the cursor; the selection is shown again.
        if state.cursor.char_range() != Some(range) {
            state.cursor.set_char_range(Some(range));
            state.clone().store(&ctx, id);
        }
        let [from, to] = range.sorted();
        let (from, to) = (from.index.min(length), to.index.min(length));
        let selected: String = self.number.chars().skip(from).take(to - from).collect();
        let has_selection = from < to;
        let mut undoer = state.undoer();
        let current = (range, self.number.clone());
        let mut done = false;

        let item = |ui: &mut egui::Ui, enabled: bool, text: &str, keys: &str| {
            ui.add_enabled(enabled, egui::Button::new(text).shortcut_text(keys))
                .clicked()
        };
        if item(
            ui,
            undoer.has_undo(&current),
            tr!("Отменить", "Undo"),
            "Ctrl+Z",
        ) {
            if let Some((cursor, text)) = undoer.undo(&current).cloned() {
                self.number = text;
                state.cursor.set_char_range(Some(cursor));
            }
            done = true;
        }
        ui.separator();
        let cut = item(ui, has_selection, tr!("Вырезать", "Cut"), "Ctrl+X");
        if item(ui, has_selection, tr!("Копировать", "Copy"), "Ctrl+C") || cut {
            ctx.copy_text(selected.clone());
            done = true;
        }
        if item(ui, true, tr!("Вставить", "Paste"), "Ctrl+V") {
            // The text goes to the field with the focus: this one.
            ctx.memory_mut(|memory| memory.request_focus(id));
            actions.push(Action::Paste);
            done = true;
        }
        if item(ui, has_selection, tr!("Удалить", "Delete"), "Del") || (cut && has_selection)
        {
            // Undo brings the deleted part back.
            undoer.add_undo(&current);
            let kept: String = self
                .number
                .chars()
                .take(from)
                .chain(self.number.chars().skip(to))
                .collect();
            self.number = kept;
            let at = egui::text::CCursor::new(from);
            state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::one(at)));
            done = true;
        }
        ui.separator();
        if item(ui, length > 0, tr!("Выделить всё", "Select all"), "Ctrl+A") {
            state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::two(
                    egui::text::CCursor::new(0),
                    egui::text::CCursor::new(length),
                )));
            done = true;
        }
        if done {
            self.number_selection = state.cursor.char_range();
            state.set_undoer(undoer);
            state.store(&ctx, id);
            self.focus_number = true;
            ui.close_menu();
        }
    }

    fn dialpad_content(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>, sizes: &PadSizes) {
        let colors = palette(self.light_theme);
        // The lines only matter once there is a call.
        if self
            .lines
            .iter()
            .any(|line| line.view.phase() != Phase::Idle)
        {
            self.lines_bar(ui, actions);
        }
        let phase = self.lines[self.at].view.phase();
        let idle = phase == Phase::Idle;
        if idle {
            self.number_field(ui, actions, &colors);
        } else {
            self.call_info(ui, phase);
        }
        ui.add_space(6.0);
        self.keypad(ui, actions, sizes, &colors);
        ui.add_space(8.0);
        self.call_buttons(ui, actions, sizes, phase, &colors);
        if let Some(error) = self.last_error.as_ref() {
            ui.add_space(4.0);
            ui.colored_label(egui::Color32::RED, error);
        }
    }

    /// The number being dialled: large, in the middle of the card, with
    /// the right-click menu and the contacts that fit under it.
    fn number_field(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>, colors: &Palette) {
        let idle = true;
        let number_id = egui::Id::new("number-field");
        let size = (ui.available_width() / 11.0).clamp(20.0, 30.0);
        let field = egui::TextEdit::singleline(&mut self.number)
            .id(number_id)
            .hint_text(egui::RichText::new(tr!("Номер", "Number")).color(colors.weak))
            .font(egui::FontId::proportional(size))
            .text_color(colors.text)
            .frame(false)
            .horizontal_align(egui::Align::Center)
            .desired_width(f32::INFINITY);
        // The selection as it is before this frame's clicks, kept while the
        // menu is open.
        let right_click = ui.input(|input| {
            input.pointer.button_pressed(egui::PointerButton::Secondary)
                || input.pointer.button_down(egui::PointerButton::Secondary)
        });
        let menu_open = ui.memory(|memory| memory.any_popup_open());
        if !right_click && !menu_open {
            self.number_selection = egui::TextEdit::load_state(ui.ctx(), number_id)
                .and_then(|state| state.cursor.char_range());
        }
        let response = ui.add_enabled(idle, field);
        // The right-click menu of a Windows text field, as in MicroSIP.
        response.context_menu(|ui| self.number_menu(ui, number_id, actions));
        // Keys typed on the keyboard sound like the keypad's.
        if response.changed() && self.key_tones_on {
            let typed: Vec<char> = ui.input(|input| {
                input
                    .events
                    .iter()
                    .filter_map(|event| match event {
                        egui::Event::Text(text) => Some(text.chars().collect::<Vec<_>>()),
                        _ => None,
                    })
                    .flatten()
                    .collect()
            });
            if let [digit] = typed.as_slice() {
                self.key_tones.play(*digit);
            }
        }
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
        // A paste that could not happen at once is forgotten: it must not
        // turn up later by surprise.
        if self
            .paste_requested
            .is_some_and(|at| at.elapsed() > Duration::from_secs(1))
        {
            self.paste_requested = None;
        }
        if self.paste_requested.is_some() && response.has_focus() {
            self.paste_requested = None;
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::RequestPaste);
            // The text comes as an event: the window looks again at once
            // instead of at the next mouse move.
            ui.ctx().request_repaint();
        } else if self.paste_requested.is_some() {
            // The field takes the focus in this frame; paste in the next.
            ui.ctx().request_repaint();
        }
        if idle && let Some(name) = self.contacts.name_of(&self.number) {
            ui.vertical_centered(|ui| {
                ui.label(egui::RichText::new(name.to_owned()).color(colors.weak));
            });
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
    }

    /// Who is on the line and how the call is going, in place of the
    /// number while there is a call.
    fn call_info(&mut self, ui: &mut egui::Ui, phase: Phase) {
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
                        tr!(" · шифруется", " · encrypted")
                    } else {
                        ""
                    };
                    match self.lines[self.at].view.codec() {
                        Some(codec) => tr!(
                            format!("Разговор {time} · {codec}{secure}"),
                            format!("Talking {time} · {codec}{secure}"),
                        ),
                        None => tr!(
                            format!("Разговор {time}{secure}"),
                            format!("Talking {time}{secure}"),
                        ),
                    }
                }
                _ => phase_text(phase).to_owned(),
            };
            ui.vertical_centered(|ui| {
                ui.label(egui::RichText::new(who).size(22.0));
            });
            let live = self.lines[self.at]
                .view
                .live_quality()
                .filter(|_| phase == Phase::Connected);
            ui.horizontal(|ui| {
                // In the middle of the card: the lamp and the state together.
                let text_width = ui.fonts(|fonts| {
                    fonts
                        .layout_no_wrap(
                            state.clone(),
                            egui::TextStyle::Body.resolve(ui.style()),
                            egui::Color32::WHITE,
                        )
                        .size()
                        .x
                });
                let lamp_width = if live.is_some() { 18.0 } else { 0.0 };
                ui.add_space(((ui.available_width() - text_width - lamp_width) / 2.0).max(0.0));
                // The quality lamp, as in MicroSIP: green, yellow or red,
                // with the numbers under the mouse.
                if let Some(live) = live {
                    let (color, meaning) = quality_look(live.lamp);
                    let (dot, response) =
                        ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                    ui.painter().circle_filled(dot.center(), 6.0, color);
                    response.on_hover_text(quality_details(&live, meaning));
                }
                ui.label(state);
            });
            if let Some(at) = self.lines[self.at].answer_at {
                let left = at.saturating_duration_since(Instant::now()).as_secs() + 1;
                ui.label(tr!(
                    format!("Автоответ через {left} с"),
                    format!("Auto answer in {left} s"),
                ));
            }
            if self.lines[self.at].recording.is_some() {
                ui.colored_label(
                    egui::Color32::RED,
                    tr!("Идёт запись разговора", "The call is being recorded"),
                );
            }
            if self.microphone_off {
                ui.colored_label(
                    egui::Color32::YELLOW,
                    tr!("Микрофон выключен", "Microphone muted"),
                );
            }
            if self.lines[self.at].view.is_held() {
                ui.colored_label(
                    egui::Color32::YELLOW,
                    tr!("Вызов на удержании", "Call on hold"),
                );
            }
            if self.lines[self.at].view.is_remote_held() {
                ui.colored_label(
                    egui::Color32::YELLOW,
                    tr!(
                        "Собеседник поставил на удержание",
                        "Put on hold by the other side"
                    ),
                );
            }
        }
    }

    fn calls_tab(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        self.missed_seen = now_unix();
        let colors = palette(self.light_theme);
        // The buttons give way to the number in a narrow window: with words
        // when there is room, as small marks in less, and none at all in a
        // narrow one; the right click on a call offers all of them anyway.
        let room = ui.available_width();
        let buttons = if room >= 330.0 {
            RowButtons::Words
        } else if room >= 250.0 {
            RowButtons::Marks
        } else {
            RowButtons::None
        };
        let free = self.dialog.is_none();
        let has_calls = !self.history.is_empty();
        let download_hint = tr!(
            "Сохранить все звонки в файл CSV для Excel: дата, время, номер, \
             длительность разговора и статус",
            "Save every call to a CSV file for Excel: date, time, number, \
             length of the conversation and status"
        );
        let total = self.history.records().len();
        let searching = !self.calls_query.trim().is_empty();
        let shown = if searching {
            self.history
                .records()
                .iter()
                .filter(|record| {
                    let name = self
                        .contacts
                        .name_of(&record.number)
                        .unwrap_or(&record.name);
                    call_matches(&record.number, name, &self.calls_query)
                })
                .count()
        } else {
            total
        };
        ui.horizontal(|ui| {
            ui.label(if searching {
                tr!(
                    format!("Найдено: {shown} из {total}"),
                    format!("Found: {shown} of {total}"),
                )
            } else {
                tr!(format!("Звонков: {total}"), format!("Calls: {total}"))
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let download = match buttons {
                    RowButtons::Words => Some(tr!("Скачать историю…", "Download the history…")),
                    RowButtons::Marks => Some(tr!("Скачать…", "Download…")),
                    RowButtons::None => None,
                };
                if let Some(download) = download {
                    if ui
                        .add_enabled(free && has_calls, egui::Button::new(download))
                        .on_hover_text(download_hint)
                        .clicked()
                    {
                        actions.push(Action::Browse(Dialog::ExportHistory));
                    }
                    if has_calls && ui.button(tr!("Очистить", "Clear")).clicked() {
                        actions.push(Action::ClearHistory);
                    }
                } else {
                    ui.menu_button("•••", |ui| {
                        if ui
                            .add_enabled(
                                free && has_calls,
                                egui::Button::new(tr!("Скачать историю…", "Download the history…")),
                            )
                            .on_hover_text(download_hint)
                            .clicked()
                        {
                            actions.push(Action::Browse(Dialog::ExportHistory));
                            ui.close_menu();
                        }
                        if ui
                            .add_enabled(has_calls, egui::Button::new(tr!("Очистить", "Clear")))
                            .clicked()
                        {
                            actions.push(Action::ClearHistory);
                            ui.close_menu();
                        }
                    });
                }
            });
        });
        // Search by number (however it is written) or by name.
        ui.horizontal(|ui| {
            let clear = !self.calls_query.is_empty();
            // Room left for the cross and the gap before it, and for the
            // field's own margins, so the row does not grow past the card.
            let margins = 2.0 * ui.spacing().button_padding.x;
            let cross_room = 24.0 + ui.spacing().item_spacing.x;
            let width = ui.available_width() - margins - if clear { cross_room } else { 0.0 };
            ui.add(
                egui::TextEdit::singleline(&mut self.calls_query)
                    .hint_text(tr!(
                        "🔍 Поиск по номеру или имени",
                        "🔍 Search by number or name"
                    ))
                    .desired_width(width),
            );
            if clear {
                let cross = ui
                    .add_sized([24.0, 20.0], egui::Button::new(""))
                    .on_hover_text(tr!("Очистить поиск", "Clear the search"));
                // Drawn: the fonts have no cross.
                let center = cross.rect.center();
                let stroke = egui::Stroke::new(1.6_f32, colors.weak);
                for (from, to) in [((-4.0, -4.0), (4.0, 4.0)), ((-4.0, 4.0), (4.0, -4.0))] {
                    ui.painter().line_segment(
                        [
                            center + egui::vec2(from.0, from.1),
                            center + egui::vec2(to.0, to.1),
                        ],
                        stroke,
                    );
                }
                if cross.clicked() {
                    self.calls_query.clear();
                }
            }
        });
        ui.separator();
        let can_dial = self.lines[self.at].view.can_dial();
        let mut to_contacts = None;
        let query = self.calls_query.clone();
        let mut found = 0;
        egui::ScrollArea::vertical().show(ui, |ui| {
            for (index, record) in self.history.records().iter().enumerate() {
                // A contact saved later names old calls too.
                let name = self
                    .contacts
                    .name_of(&record.number)
                    .unwrap_or(&record.name);
                if !call_matches(&record.number, name, &query) {
                    continue;
                }
                found += 1;
                let (mark, color) = match record.direction {
                    Direction::Outgoing => (
                        tr!("Исх.", "Out"),
                        self.tint(
                            egui::Color32::from_rgb(76, 141, 255),
                            egui::Color32::from_rgb(30, 90, 170),
                        ),
                    ),
                    Direction::Incoming => (
                        tr!("Вх.", "In"),
                        self.tint(
                            egui::Color32::from_rgb(34, 197, 94),
                            egui::Color32::from_rgb(30, 130, 50),
                        ),
                    ),
                    Direction::Missed => (tr!("Проп.", "Missed"), colors.red),
                };
                let who = if name.is_empty() {
                    record.number.clone()
                } else {
                    format!("{name} ({})", record.number)
                };
                let recorded = !record.recording.is_empty();
                let mut chosen = None;
                ui.push_id(index, |ui| {
                    ui.horizontal(|ui| {
                        ui.colored_label(color, mark);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            match buttons {
                                RowButtons::Words => {
                                    if ui
                                        .add_enabled(
                                            can_dial,
                                            egui::Button::new(tr!("Позвонить", "Call")),
                                        )
                                        .clicked()
                                    {
                                        chosen = Some(RowChoice::Call);
                                    }
                                    if name.is_empty()
                                        && ui.button(tr!("В контакты", "To contacts")).clicked()
                                    {
                                        chosen = Some(RowChoice::ToContacts);
                                    }
                                    if recorded && ui.button(tr!("Запись", "Recording")).clicked()
                                    {
                                        chosen = Some(RowChoice::Recording);
                                    }
                                }
                                RowButtons::Marks => {
                                    if mark_button(ui, Icon::Phone, can_dial, &colors)
                                        .on_hover_text(tr!("Позвонить", "Call"))
                                        .clicked()
                                    {
                                        chosen = Some(RowChoice::Call);
                                    }
                                    if name.is_empty()
                                        && mark_button(ui, Icon::PersonAdd, true, &colors)
                                            .on_hover_text(tr!("В контакты", "To contacts"))
                                            .clicked()
                                    {
                                        chosen = Some(RowChoice::ToContacts);
                                    }
                                }
                                RowButtons::None => {}
                            }
                            // The name takes the room the buttons leave.
                            ui.with_layout(
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| {
                                    // Selectable with the mouse; a right click
                                    // offers to copy, save and call.
                                    let label = ui
                                        .add(egui::Label::new(&who).truncate().selectable(true))
                                        .on_hover_text(&who);
                                    label.context_menu(|ui| {
                                        call_menu(
                                            ui,
                                            &record.number,
                                            name,
                                            can_dial,
                                            recorded,
                                            &mut chosen,
                                        );
                                    });
                                },
                            );
                        });
                    });
                    let length = if record.seconds > 0 {
                        format!(", {}", format_duration(record.seconds))
                    } else {
                        String::new()
                    };
                    ui.horizontal_wrapped(|ui| {
                        let when = ui.add(
                            egui::Label::new(
                                egui::RichText::new(format!(
                                    "{}{}{length}",
                                    format_timestamp(record.time, self.utc_offset),
                                    end_text(record.time, record.ended, self.utc_offset)
                                ))
                                .weak(),
                            )
                            .sense(egui::Sense::click()),
                        );
                        when.context_menu(|ui| {
                            call_menu(ui, &record.number, name, can_dial, recorded, &mut chosen);
                        });
                        // Why the call did not take place, as MicroSIP shows it.
                        if let Some(why) = result_text(&record.result) {
                            // A long reason is cut at the window's edge; the
                            // whole of it shows under the mouse.
                            let text = egui::RichText::new(why.clone()).color(colors.red);
                            ui.add(egui::Label::new(text).truncate()).on_hover_text(why);
                        }
                    });
                    ui.separator();
                });
                match chosen {
                    Some(RowChoice::Call) => actions.push(Action::Call(record.number.clone())),
                    Some(RowChoice::ToContacts) => to_contacts = Some(record.number.clone()),
                    Some(RowChoice::Recording) => {
                        actions.push(Action::Open(PathBuf::from(&record.recording)));
                    }
                    None => {}
                }
            }
            if found == 0 && !query.trim().is_empty() {
                ui.add_space(8.0);
                ui.weak(tr!(
                    "Звонков с таким номером нет",
                    "No calls with such a number"
                ));
            }
        });
        if let Some(number) = to_contacts {
            self.new_contact_number = number;
            self.tab = Tab::Contacts;
        }
    }

    fn contacts_tab(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        ui.horizontal(|ui| {
            ui.label(tr!("Имя", "Name"));
            ui.add(egui::TextEdit::singleline(&mut self.new_contact_name).desired_width(120.0));
            ui.label(tr!("Номер", "Number"));
            ui.add(egui::TextEdit::singleline(&mut self.new_contact_number).desired_width(110.0));
        });
        ui.horizontal(|ui| {
            if ui
                .button(tr!("Сохранить контакт", "Save the contact"))
                .clicked()
            {
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
                        egui::Button::new(tr!("Экспорт…", "Export…")),
                    )
                    .on_hover_text(tr!(
                        "Сохранить контакты в файл CSV (открывается в Excel)",
                        "Save the contacts to a CSV file (opens in Excel)"
                    ))
                    .clicked()
                {
                    actions.push(Action::Browse(Dialog::ExportContacts));
                }
                if ui
                    .add_enabled(free, egui::Button::new(tr!("Импорт…", "Import…")))
                    .on_hover_text(tr!(
                        "Добавить контакты из файла CSV: имя и номер",
                        "Add contacts from a CSV file: name and number"
                    ))
                    .clicked()
                {
                    actions.push(Action::Browse(Dialog::ImportContacts));
                }
            });
        });
        ui.separator();
        ui.add(
            egui::TextEdit::singleline(&mut self.contact_search)
                .hint_text(tr!("Поиск", "Search"))
                .desired_width(f32::INFINITY),
        );
        let can_dial = self.lines[self.at].view.can_dial();
        egui::ScrollArea::vertical().show(ui, |ui| {
            let found = self.contacts.matching(&self.contact_search);
            if found.is_empty() {
                ui.weak(tr!("Контактов нет", "No contacts"));
            }
            for (index, contact) in found.into_iter().enumerate() {
                ui.push_id(index, |ui| {
                    ui.horizontal(|ui| {
                        if let Some(state) = self.lamp(&contact.number) {
                            let (color, hint) = lamp_look(state);
                            let (dot, response) = ui
                                .allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
                            ui.painter().circle_filled(dot.center(), 5.0, color);
                            response.on_hover_text(hint);
                        }
                        if let Some(text) = self.status_of(&contact.number) {
                            ui.label(egui::RichText::new(text).small().weak());
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button(tr!("Удалить", "Delete")).clicked() {
                                actions.push(Action::RemoveContact(contact.number.clone()));
                            }
                            let watch = if contact.watched {
                                tr!("Не следить", "Unwatch")
                            } else {
                                tr!("Следить", "Watch")
                            };
                            if ui
                                .small_button(watch)
                                .on_hover_text(tr!(
                                    "Показывать, занят ли этот номер (BLF)",
                                    "Show whether this number is busy (BLF)"
                                ))
                                .clicked()
                            {
                                actions
                                    .push(Action::Watch(contact.number.clone(), !contact.watched));
                            }
                            if ui.small_button(tr!("Написать", "Message")).clicked() {
                                actions.push(Action::OpenChat(contact.number.clone()));
                            }
                            if ui
                                .add_enabled(can_dial, egui::Button::new(tr!("Позвонить", "Call")))
                                .clicked()
                            {
                                actions.push(Action::Call(contact.number.clone()));
                            }
                            // Name and number take the room the buttons
                            // leave, and are cut short rather than run under
                            // them.
                            ui.with_layout(
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| {
                                    let mut text = egui::text::LayoutJob::default();
                                    let style = ui.style();
                                    egui::RichText::new(&contact.name).append_to(
                                        &mut text,
                                        style,
                                        egui::FontSelection::Default,
                                        egui::Align::Center,
                                    );
                                    egui::RichText::new(format!("  {}", contact.number))
                                        .weak()
                                        .append_to(
                                            &mut text,
                                            style,
                                            egui::FontSelection::Default,
                                            egui::Align::Center,
                                        );
                                    let label =
                                        ui.add(egui::Label::new(text).truncate().selectable(true));
                                    label.context_menu(|ui| {
                                        copy_menu(ui, &contact.number, &contact.name);
                                    });
                                    label.on_hover_text(format!(
                                        "{} {}",
                                        contact.name, contact.number
                                    ));
                                },
                            );
                        });
                    });
                    ui.separator();
                });
            }
        });
    }

    fn settings_window(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let mut open = self.settings_open;
        // Never larger than the phone's window, whatever its size; what
        // does not fit scrolls.
        let screen = ctx.screen_rect();
        egui::Window::new(tr!("Настройки", "Settings"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .constrain(true)
            .max_width((screen.width() - 16.0).max(200.0))
            .min_width((screen.width() - 16.0).clamp(200.0, 400.0))
            .show(ctx, |ui| {
                let room = (screen.height() - 70.0).max(150.0);
                egui::ScrollArea::both()
                    .max_height(room)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.heading(tr!("Аккаунт", "Account"));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui
                                    .add_enabled(
                                        !self.other_accounts.is_empty(),
                                        egui::Button::new(tr!("Удалить этот", "Remove this one")),
                                    )
                                    .on_hover_text(tr!("Забыть этот аккаунт; станет активным следующий", "Forget this account; the next one becomes active"))
                                    .clicked()
                                {
                                    actions.push(Action::RemoveAccount);
                                }
                                if ui
                                    .button(tr!("Добавить", "Add"))
                                    .on_hover_text(tr!("Ещё один аккаунт; переключение — в верхней строке окна", "One more account; switch in the top row of the window"))
                                    .clicked()
                                {
                                    actions.push(Action::AddAccount);
                                }
                            });
                        });
                        if !self.other_accounts.is_empty() && self.all_accounts {
                            ui.weak(tr!(
                                format!(
                                    "Аккаунтов: {}. Все в сети; здесь — настройки основного.",
                                    self.other_accounts.len() + 1
                                ),
                                format!(
                                    "Accounts: {}. All are online; these are the main one's settings.",
                                    self.other_accounts.len() + 1
                                ),
                            ));
                        } else if !self.other_accounts.is_empty() {
                            ui.weak(tr!(
format!(
                                "Аккаунтов: {}. Работает один — выбранный в верхней строке окна.",
                                self.other_accounts.len() + 1
                            ),
format!("Accounts: {}. One works: the one chosen in the top row of the window.",
                                self.other_accounts.len() + 1
                            ),
));
                        }
                        egui::Grid::new("account").num_columns(2).show(ui, |ui| {
                            ui.label(tr!("SIP-сервер", "SIP server"));
                            ui.text_edit_singleline(&mut self.server);
                            ui.end_row();
                            ui.label(tr!("Номер (пользователь)", "Number (user)"));
                            ui.text_edit_singleline(&mut self.user);
                            ui.end_row();
                            ui.label(tr!("Пароль", "Password"));
                            ui.horizontal(|ui| {
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.password)
                                        .password(!self.show_password)
                                        .desired_width(150.0),
                                );
                                ui.checkbox(&mut self.show_password, tr!("Показать", "Show"));
                            });
                            ui.end_row();
                            ui.label(tr!("Домен (если отличается)", "Domain (if different)"));
                            ui.text_edit_singleline(&mut self.domain);
                            ui.end_row();
                            ui.label(tr!("Логин (если отличается)", "Login (if different)"));
                            ui.text_edit_singleline(&mut self.auth_user);
                            ui.end_row();
ui.label(tr!("Транспорт", "Transport"));
ui.horizontal(|ui| {
    for transport in SipTransport::ALL {
        ui.radio_value(&mut self.transport, transport, transport.name());
    }
});
ui.end_row();
ui.label(tr!("STUN-сервер", "STUN server"));
ui.add(
    egui::TextEdit::singleline(&mut self.stun)
        .hint_text(tr!("не используется", "not used"))
        .desired_width(170.0),
);
ui.end_row();
ui.label(tr!("Шифрование (SRTP)", "Encryption (SRTP)"));
egui::ComboBox::from_id_salt("media-encryption")
    .selected_text(encryption_text(self.encryption))
    .width(170.0)
    .show_ui(ui, |ui| {
        for mode in MediaEncryption::ALL {
            ui.selectable_value(&mut self.encryption, mode, encryption_text(mode));
        }
    });
ui.end_row();
ui.label(tr!("Прокси (если есть)", "Proxy (if any)"));
ui.add(
    egui::TextEdit::singleline(&mut self.proxy)
        .hint_text(tr!("не используется", "not used"))
        .desired_width(170.0),
);
ui.end_row();
                            ui.label(tr!("Локальный порт", "Local port"));
                            ui.add(
                                egui::TextEdit::singleline(&mut self.local_port)
                                    .hint_text(tr!("автоматически", "automatic"))
                                    .desired_width(100.0),
                            );
                            ui.end_row();
                        });
if self.transport == SipTransport::Tls {
    ui.checkbox(&mut self.tls_verify, tr!("Проверять сертификат сервера (TLS)", "Check the server certificate (TLS)"));
    if !self.tls_verify {
        ui.weak(tr!("Без проверки связь шифруется, но сервер может оказаться подменённым.", "Without the check the link is encrypted, but the server could be an impostor."));
    }
}
                        if secret::is_available() {
                            ui.checkbox(&mut self.save_password, tr!("Запомнить пароль", "Remember the password"));
                            ui.weak(
                                tr!("Пароль хранится зашифрованным для этого пользователя Windows.", "The password is kept encrypted for this Windows user."),
                            );
                        } else {
                            ui.weak(tr!("На этой системе пароль не сохраняется.", "On this system the password is not saved."));
                        }
                        ui.horizontal(|ui| {
                            if ui.button(tr!("Сохранить и подключить", "Save and connect")).clicked() {
                                actions.push(Action::Connect);
                            }
                            if ui
                                .add_enabled(self.handle.is_some(), egui::Button::new(tr!("Отключить", "Disconnect")))
                                .clicked()
                            {
                                actions.push(Action::Disconnect);
                            }
                        });
                        if let Some(local) = self.local {
                            ui.weak(tr!(
format!("Адрес телефона: {local}"),
format!("Phone address: {local}"),
));
                        }
                        let mut all = self.all_accounts;
                        if ui
                            .checkbox(&mut all, tr!("Все аккаунты в сети одновременно", "All accounts online at once"))
                            .on_hover_text(
                                tr!("Звонки на любой из сохранённых аккаунтов приходят сюда; \
                                 с какого номера звонить — в верхней строке окна", "Calls to any saved account ring here; choose the number to call from in the top row of the window"),
                            )
                            .changed()
                        {
                            actions.push(Action::AllAccounts(all));
                        }
                        if self.all_accounts {
                            ui.weak(
                                tr!("Дополнительные аккаунты работают по UDP; почта, лампы и \
                                 статусы — у основного.", "Additional accounts work over UDP; voicemail, lamps and statuses stay with the main one."),
                            );
                        }
                        ui.horizontal(|ui| {
                            ui.label(tr!("Сигналы кнопок (DTMF)", "Keypad digits (DTMF)"));
                            let before = self.dtmf_method;
                            egui::ComboBox::from_id_salt("dtmf-method")
                                .selected_text(dtmf_text(self.dtmf_method))
                                .width(150.0)
                                .show_ui(ui, |ui| {
                                    for method in DtmfMethod::ALL {
                                        ui.selectable_value(
                                            &mut self.dtmf_method,
                                            method,
                                            dtmf_text(method),
                                        );
                                    }
                                });
                            if self.dtmf_method != before {
                                actions.push(Action::DtmfMethod(self.dtmf_method));
                            }
                        });
                        let mut statuses = self.presence_on;
                        if ui
                            .checkbox(&mut statuses, tr!("Статусы: показывать коллегам свой и видеть их", "Statuses: show mine to colleagues and see theirs"))
                            .on_hover_text(
                                tr!("Свой статус выбирается в верхней строке окна. Не все АТС \
                                 его принимают; Asterisk и FreePBX — нет.", "Your status is chosen in the top row of the window. Not every exchange takes it; Asterisk and FreePBX do not."),
                            )
                            .changed()
                        {
                            actions.push(Action::Statuses(statuses));
                        }

                        ui.separator();
                        ui.heading(tr!("Звук", "Audio"));
                        let before = (self.microphone.clone(), self.speaker.clone());
                        device_picker(ui, tr!("Микрофон", "Microphone"), &mut self.microphone, &self.microphones);
                        device_picker(ui, tr!("Динамик", "Speaker"), &mut self.speaker, &self.speakers);
                        if ui.button(tr!("Обновить список устройств", "Refresh the device list")).clicked() {
                            self.microphones = input_device_names();
                            self.speakers = output_device_names();
                        }
                        if before != (self.microphone.clone(), self.speaker.clone()) {
                            actions.push(Action::AudioChanged);
                        }
                        ui.weak(tr!("Новое устройство используется со следующего звонка.", "A new device is used from the next call on."));
                        let before = (self.echo_cancellation, self.noise_suppression);
                        ui.checkbox(&mut self.echo_cancellation, tr!("Подавлять эхо", "Cancel echo"));
                        ui.checkbox(&mut self.noise_suppression, tr!("Подавлять шум", "Suppress noise"));
                        if before != (self.echo_cancellation, self.noise_suppression) {
                            actions.push(Action::AudioChanged);
                        }
                        ui.weak(tr!("Эхо — ваш собеседник слышит сам себя из ваших колонок. Действует со следующего звонка.", "Echo: the other side hears itself from your speakers. Takes effect from the next call."));
                        let before = (self.ring_speaker.clone(), self.ringtone.clone());
                        device_picker(ui, tr!("Звонок", "Ringing"), &mut self.ring_speaker, &self.speakers);
                        ui.horizontal(|ui| {
                            ui.label(tr!("Мелодия", "Ringtone"));
                            let field = egui::TextEdit::singleline(&mut self.ringtone)
                                .hint_text(tr!("встроенный звонок", "built-in ring"))
                                .desired_width(170.0);
                            let typed = ui.add(field).lost_focus();
                            if ui
                                .add_enabled(self.dialog.is_none(), egui::Button::new(tr!("Обзор…", "Browse…")))
                                .clicked()
                            {
                                actions.push(Action::Browse(Dialog::Ringtone));
                            }
                            if ui.button(tr!("Сбросить", "Reset")).clicked() {
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
                    tr!("Мелодия — файл WAV. «Звонок» на «По умолчанию» звучит там же, где разговор.", "The ringtone is a WAV file. \"Ringing\" on \"Default\" sounds where calls do."),
                );

                        ui.separator();
                        ui.heading(tr!("Кодеки", "Codecs"));
                        if self.codecs_editor(ui) {
                            actions.push(Action::CodecsChanged);
                        }
                        ui.weak(
                            tr!("Сверху — предпочитаемый. Изменения действуют со следующего звонка. \
     Если АТС выбирает кодек сама, решает её порядок.", "The top one is preferred. Changes take effect from the next call. If the exchange chooses the codec itself, its order decides."),
                        );

                        ui.separator();
                        ui.heading(tr!("Входящие вызовы", "Incoming calls"));
                        ui.horizontal(|ui| {
                            ui.label(tr!("Номер голосовой почты", "Voicemail number"));
                            if ui
                                .add(egui::TextEdit::singleline(&mut self.voicemail_number).desired_width(80.0))
                                .lost_focus()
                            {
                                actions.push(Action::WindowChanged);
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut self.auto_answer, tr!("Автоответ через", "Auto answer after"));
                            ui.add(egui::DragValue::new(&mut self.auto_answer_delay).range(0..=60));
                            ui.label(tr!("с", "s"));
                        });
                        ui.weak(tr!("«Не беспокоить» включается галочкой в верхней строке окна.", "\"Do not disturb\" is switched on in the top row of the window."));
                        let before = (self.forward, self.forward_to.clone());
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut self.forward, tr!("Переадресация на номер", "Forward to number"));
                            ui.add(
                                egui::TextEdit::singleline(&mut self.forward_to)
                                    .desired_width(140.0),
                            );
                        });
                        if before != (self.forward, self.forward_to.clone()) {
                            actions.push(Action::ForwardChanged);
                        }

                        ui.separator();
                        ui.heading(tr!("Запись разговоров", "Call recording"));
                        ui.checkbox(&mut self.record_calls, tr!("Записывать все разговоры", "Record all calls"));
                        let default = default_recordings_dir()
                            .map(|folder| folder.display().to_string())
                            .unwrap_or_default();
                        ui.horizontal(|ui| {
                            ui.label(tr!("Папка", "Folder"));
                            ui.add(
                                egui::TextEdit::singleline(&mut self.recordings_dir)
                                    .hint_text(default)
                                    .desired_width(200.0),
                            );
                            if ui
                                .add_enabled(self.dialog.is_none(), egui::Button::new(tr!("Обзор…", "Browse…")))
                                .clicked()
                            {
                                actions.push(Action::Browse(Dialog::RecordingsFolder));
                            }
                        });
                        if ui.button(tr!("Открыть папку с записями", "Open the recordings folder")).clicked()
                            && let Some(folder) = self.recordings_folder()
                        {
                            // The folder appears with the first recording; make it
                            // now so that there is something to open.
                            let _ = std::fs::create_dir_all(&folder);
                            actions.push(Action::Open(folder));
                        }
                        ui.weak(tr!("Файлы WAV: слева ваш голос, справа собеседник.", "WAV files: your voice on the left, the other side on the right."));

                        ui.separator();
                        ui.heading(tr!("Окно", "Window"));
                        ui.horizontal(|ui| {
                            ui.label("Язык / Language");
                            let mut english_now = english();
                            ui.radio_value(&mut english_now, false, "Русский");
                            ui.radio_value(&mut english_now, true, "English");
                            if english_now != english() {
                                actions.push(Action::English(english_now));
                            }
                        });
                        let before = (self.close_to_tray, self.hotkeys, self.light_theme);
                        ui.checkbox(&mut self.light_theme, tr!("Светлая тема", "Light theme"));
                        if ui
                            .checkbox(
                                &mut self.header_picture,
                                tr!("Надпись NAVI в шапке", "NAVI at the top"),
                            )
                            .changed()
                        {
                            self.save_settings();
                        }
                        let before_window =
                            (self.always_on_top, self.incoming_popup, self.key_tones_on);
                        ui.checkbox(
                            &mut self.always_on_top,
                            tr!("Поверх всех окон", "Always on top"),
                        );
                        ui.checkbox(
                            &mut self.incoming_popup,
                            tr!(
                                "Окно входящего звонка с кнопками «Ответить» и «Отклонить»",
                                "A window for incoming calls with Answer and Decline"
                            ),
                        );
                        ui.checkbox(
                            &mut self.key_tones_on,
                            tr!("Звук кнопок при наборе", "Keypad sounds when dialling"),
                        );
                        if before_window
                            != (self.always_on_top, self.incoming_popup, self.key_tones_on)
                        {
                            self.save_settings();
                        }
                        if self.shell.is_some() {
                            ui.checkbox(
                                &mut self.close_to_tray,
                                tr!("При закрытии сворачивать к часам (в трей)", "Closing hides to the notification area (tray)"),
                            );
                            let mut autostart = self.autostart;
                            if ui
                                .checkbox(&mut autostart, tr!("Запускать вместе с Windows", "Start with Windows"))
                                .changed()
                            {
                                actions.push(Action::Autostart(autostart));
                            }
                            let mut links = self.link_handler;
                            if ui
                                .checkbox(&mut links, tr!("Открывать ссылки tel:, sip: и callto:", "Open tel:, sip: and callto: links"))
                                .changed()
                            {
                                actions.push(Action::LinkHandler(links));
                            }
                            ui.horizontal_wrapped(|ui| {
                                ui.weak(
                                    tr!("Если по ссылке открывается другая программа, выберите \
                                     MicroSIP-Rust для TEL и SIP в «Приложениях по умолчанию».", "If a link opens another program, choose MicroSIP-Rust for TEL and SIP in \"Default apps\"."),
                                );
                                if ui.small_button(tr!("Открыть", "Open")).clicked() {
                                    winshell::open_default_apps();
                                }
                            });
                            ui.checkbox(&mut self.hotkeys, tr!("Горячие клавиши в любой программе", "Hotkeys in any program"));
                            ui.weak(
                                tr!("Ctrl+Alt+PageUp — ответить, Ctrl+Alt+PageDown — завершить или \
                         отклонить, Ctrl+Alt+Insert — номер из буфера обмена.\n\
                         Галочка клавиш действует после перезапуска программы.", "Ctrl+Alt+PageUp answers, Ctrl+Alt+PageDown hangs up or declines, Ctrl+Alt+Insert takes the number from the clipboard.\nThe hotkeys box takes effect after a restart."),
                            );
                        } else {
                            ui.weak(
                                tr!("Значок у часов, автозапуск и общие клавиши есть только в Windows.", "The tray icon, autostart and global hotkeys are available on Windows only."),
                            );
                        }
                        ui.weak(tr!("В окне: Enter — ответить, Esc — отклонить входящий.", "In the window: Enter answers, Esc declines an incoming call."));
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
                        if ui
                            .add_enabled(down, egui::Button::new(tr!("Ниже", "Down")))
                            .clicked()
                        {
                            self.codecs.swap(index, index + 1);
                            changed = true;
                        }
                        let up = on && index > 0;
                        if ui
                            .add_enabled(up, egui::Button::new(tr!("Выше", "Up")))
                            .clicked()
                        {
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
        egui::Window::new(tr!("Журнал", "Log"))
            .open(&mut open)
            .default_size([520.0, 320.0])
            .show(ctx, |ui| {
                if ui.button(tr!("Очистить", "Clear")).clicked() {
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
        tr!("По умолчанию", "Default").to_owned()
    } else {
        chosen.clone()
    };
    egui::ComboBox::from_label(label)
        .selected_text(shown)
        .width(280.0)
        .show_ui(ui, |ui| {
            ui.selectable_value(chosen, String::new(), tr!("По умолчанию", "Default"));
            for name in names {
                ui.selectable_value(chosen, name.clone(), name);
            }
        });
}

fn phase_text(phase: Phase) -> &'static str {
    match phase {
        Phase::Idle => tr!("Готов", "Ready"),
        Phase::Calling => tr!("Вызов…", "Calling…"),
        Phase::Ringing => tr!("У абонента звонит телефон", "Ringing"),
        Phase::Incoming => tr!("Входящий вызов", "Incoming call"),
        Phase::Answering => tr!("Соединение…", "Connecting…"),
        Phase::Connected => tr!("Разговор", "Talking"),
    }
}

fn end_reason_text(reason: EndReason) -> String {
    match reason {
        EndReason::Completed => tr!("разговор окончен", "call ended").to_owned(),
        EndReason::Rejected(status) => match sip_status_text(status) {
            // What the code means, then the code with its standard name,
            // as MicroSIP shows it.
            Some((russian, phrase)) => tr!(
                format!("{russian} ({status} {phrase})"),
                format!("{phrase} ({status})"),
            ),
            None => tr!(
                format!("вызов не прошёл (код {status})"),
                format!("call failed (code {status})"),
            ),
        },
        EndReason::Cancelled => tr!("отменён", "cancelled").to_owned(),
        EndReason::Timeout => tr!("нет ответа", "no answer").to_owned(),
        EndReason::Failed => tr!("не удалось позвонить", "could not call").to_owned(),
    }
}

/// What a SIP answer code means: in Russian words, and its standard name
/// (RFC 3261 and later), which MicroSIP shows.
fn sip_status_text(status: u16) -> Option<(&'static str, &'static str)> {
    Some(match status {
        400 => ("неверный запрос", "Bad Request"),
        401 => ("нужен пароль", "Unauthorized"),
        402 => ("нужна оплата", "Payment Required"),
        403 => ("запрещено АТС", "Forbidden"),
        404 => ("номер не найден", "Not Found"),
        405 => ("действие не разрешено", "Method Not Allowed"),
        406 => ("неприемлемо", "Not Acceptable"),
        407 => ("нужен пароль для прокси", "Proxy Authentication Required"),
        408 => ("нет ответа", "Request Timeout"),
        410 => ("номера больше нет", "Gone"),
        413 => ("слишком большой запрос", "Request Entity Too Large"),
        414 => ("слишком длинный адрес", "Request-URI Too Long"),
        415 => ("формат не поддерживается", "Unsupported Media Type"),
        416 => ("адрес не поддерживается", "Unsupported URI Scheme"),
        420 => ("неизвестное расширение SIP", "Bad Extension"),
        421 => ("нужно расширение SIP", "Extension Required"),
        423 => ("слишком короткий срок", "Interval Too Brief"),
        480 => ("абонент недоступен", "Temporarily Unavailable"),
        481 => ("такого вызова нет", "Call/Transaction Does Not Exist"),
        482 => ("вызов зациклился", "Loop Detected"),
        483 => ("слишком много переходов", "Too Many Hops"),
        484 => ("номер набран не полностью", "Address Incomplete"),
        485 => ("неоднозначный номер", "Ambiguous"),
        486 => ("занято", "Busy Here"),
        487 => ("вызов отменён", "Request Terminated"),
        488 => ("не подошли кодеки или шифрование", "Not Acceptable Here"),
        489 => ("событие не поддерживается", "Bad Event"),
        491 => ("запрос уже обрабатывается", "Request Pending"),
        493 => ("не удалось расшифровать", "Undecipherable"),
        500 => ("внутренняя ошибка АТС", "Server Internal Error"),
        501 => ("АТС этого не умеет", "Not Implemented"),
        502 => ("ошибка шлюза", "Bad Gateway"),
        503 => ("сервис недоступен", "Service Unavailable"),
        504 => ("АТС не дождалась ответа", "Server Time-out"),
        505 => ("версия SIP не поддерживается", "Version Not Supported"),
        513 => ("слишком большое сообщение", "Message Too Large"),
        580 => ("условия вызова не выполнены", "Precondition Failure"),
        600 => ("занято везде", "Busy Everywhere"),
        603 => ("вызов отклонён", "Decline"),
        604 => ("номер нигде не существует", "Does Not Exist Anywhere"),
        606 => ("вызов не принят", "Not Acceptable"),
        _ => return None,
    })
}

/// Messages of the phone's core, in Russian where they are known. The text
/// after the first colon (a system detail) is kept as it is.
fn error_text(message: &str) -> String {
    let known: [(&str, &str); 14] = [
        (
            "Cannot play the ringing sound",
            tr!("Не удалось воспроизвести звонок", "Cannot play the ring"),
        ),
        (
            "Cannot start audio",
            tr!("Не удалось включить звук", "Cannot start the audio"),
        ),
        ("Audio failed", tr!("Сбой звука", "Audio failure")),
        (
            "Hold did not work",
            tr!("Удержание не сработало", "Hold did not work"),
        ),
        (
            "Resume did not work",
            tr!("Возобновление не сработало", "Resume did not work"),
        ),
        (
            "Transfer did not work",
            tr!("Перевод не удался", "The transfer failed"),
        ),
        (
            "both lines must have a connected call",
            tr!(
                "Соединить можно только два идущих разговора",
                "Only two ongoing calls can be joined"
            ),
        ),
        (
            "only a connected call can be recorded",
            tr!(
                "Записывать можно только во время разговора",
                "Recording is possible only during a call"
            ),
        ),
        (
            "register on a SIP server before calling a number",
            tr!(
                "Нет регистрации на сервере — позвонить на номер нельзя",
                "Not registered on the server: numbers cannot be called"
            ),
        ),
        (
            "invalid number or SIP address",
            tr!("Неверный номер", "Invalid number"),
        ),
        ("STUN server", tr!("STUN-сервер", "STUN server")),
        (
            "register on a SIP server before sending messages",
            tr!(
                "Нет регистрации на сервере — сообщение не отправить",
                "Not registered on the server: the message cannot be sent"
            ),
        ),
        (
            "a call is already in progress",
            tr!("Уже идёт вызов", "A call is already in progress"),
        ),
        (
            "digits can be sent only during a connected call",
            tr!(
                "Цифры можно отправлять только во время разговора",
                "Digits can be sent only during a call"
            ),
        ),
    ];
    for (english, russian) in known {
        if let Some(rest) = message.strip_prefix(english) {
            return format!("{russian}{rest}");
        }
    }
    message.to_owned()
}

/// Sizes of the dial pad, worked out from the window's size.
struct PadSizes {
    key: egui::Vec2,
    key_font: f32,
    /// Height of the rows of buttons under the keys.
    row: f32,
    row_font: f32,
}

/// The code kept in the call list for how a call ended; empty for a call
/// that went normally.
fn result_code(reason: EndReason, incoming: bool, connected: bool) -> String {
    match reason {
        EndReason::Completed if connected => String::new(),
        EndReason::Completed => "failed".to_owned(),
        EndReason::Rejected(_) if incoming => "declined".to_owned(),
        EndReason::Rejected(status) => status.to_string(),
        // An incoming call given up by the caller is simply missed.
        EndReason::Cancelled if incoming => String::new(),
        EndReason::Cancelled => "cancelled".to_owned(),
        EndReason::Timeout => "timeout".to_owned(),
        EndReason::Failed => "failed".to_owned(),
    }
}

/// What the call list says about such a code.
fn result_text(code: &str) -> Option<String> {
    let code = code.trim();
    if code.is_empty() {
        return None;
    }
    if let Some(to) = code.strip_prefix("forwarded ") {
        return Some(tr!(
            format!("переадресован на {to}"),
            format!("forwarded to {to}"),
        ));
    }
    Some(match code {
        "cancelled" => tr!("отменён", "cancelled").to_owned(),
        "timeout" => tr!("нет ответа", "no answer").to_owned(),
        "failed" => tr!("не удалось позвонить", "could not call").to_owned(),
        "declined" => tr!("отклонён", "declined").to_owned(),
        "dnd" => tr!("отклонён: «Не беспокоить»", "declined: \"Do not disturb\"").to_owned(),
        other => match other.parse::<u16>() {
            Ok(status) => end_reason_text(EndReason::Rejected(status)),
            Err(_) => other.to_owned(),
        },
    })
}

/// The colours of the window, light or dark. One layout for both: the
/// dark one in NAVI black and yellow, the light one white and black with
/// the yellow kept for the call button.
#[derive(Clone, Copy)]
struct Palette {
    page: egui::Color32,
    card: egui::Color32,
    /// The thin frame of the cards and tiles.
    card_line: egui::Color32,
    key: egui::Color32,
    key_hover: egui::Color32,
    /// The frame of a key; transparent where the keys stand out by colour.
    key_line: egui::Color32,
    /// The account's pill and the menu button.
    chip: egui::Color32,
    text: egui::Color32,
    weak: egui::Color32,
    line: egui::Color32,
    /// NAVI, the page shown, the filled part of the sliders.
    accent: egui::Color32,
    call: egui::Color32,
    on_call: egui::Color32,
    track: egui::Color32,
    knob: egui::Color32,
    green: egui::Color32,
    red: egui::Color32,
}

fn palette(light: bool) -> Palette {
    let rgb = egui::Color32::from_rgb;
    if light {
        Palette {
            page: rgb(255, 255, 255),
            card: rgb(255, 255, 255),
            card_line: rgb(229, 229, 229),
            key: rgb(255, 255, 255),
            key_hover: rgb(245, 245, 245),
            key_line: rgb(229, 229, 229),
            chip: rgb(243, 243, 243),
            text: rgb(10, 10, 10),
            weak: rgb(115, 115, 115),
            line: rgb(229, 229, 229),
            accent: rgb(10, 10, 10),
            call: rgb(10, 10, 10),
            on_call: rgb(255, 229, 0),
            track: rgb(229, 229, 229),
            knob: rgb(10, 10, 10),
            green: rgb(34, 197, 94),
            red: rgb(225, 29, 72),
        }
    } else {
        Palette {
            page: rgb(11, 11, 12),
            card: rgb(20, 20, 22),
            card_line: rgb(34, 34, 38),
            key: rgb(31, 31, 35),
            key_hover: rgb(42, 42, 47),
            key_line: egui::Color32::TRANSPARENT,
            chip: rgb(28, 28, 31),
            text: rgb(245, 245, 245),
            weak: rgb(138, 141, 147),
            line: rgb(34, 34, 38),
            accent: rgb(255, 229, 0),
            call: rgb(255, 229, 0),
            on_call: rgb(11, 11, 12),
            track: rgb(42, 42, 47),
            knob: rgb(255, 255, 255),
            green: rgb(34, 197, 94),
            red: rgb(255, 69, 58),
        }
    }
}

/// ` – 14:35` after the start of a call in the list: when it ended, with
/// the date when that was another day; nothing when it was not noted.
fn end_text(started: u64, ended: u64, offset: i64) -> String {
    if ended == 0 {
        return String::new();
    }
    let start = format_timestamp(started, offset);
    let end = format_timestamp(ended, offset);
    match (start.split_once(' '), end.split_once(' ')) {
        (Some((start_day, _)), Some((end_day, clock))) if start_day == end_day => {
            format!(" – {clock}")
        }
        _ => format!(" – {end}"),
    }
}

/// The letters NAVI at the top, built into the program: white on nothing,
/// painted in the theme's accent colour.
const HEADER_PICTURE: &[u8] = include_bytes!("../../assets/header-letters.png");

/// The windows' own icon: NAVI on black. Small, so it costs next to
/// nothing; without it the window library unpacks a large one of its own.
const WINDOW_ICON: &[u8] = include_bytes!("../../assets/icon-64.png");

fn window_icon() -> Arc<egui::IconData> {
    static ICON: OnceLock<Arc<egui::IconData>> = OnceLock::new();
    ICON.get_or_init(|| {
        let icon = image::load_from_memory_with_format(WINDOW_ICON, image::ImageFormat::Png)
            .map(|decoded| decoded.to_rgba8())
            .map(|decoded| egui::IconData {
                width: decoded.width(),
                height: decoded.height(),
                rgba: decoded.into_raw(),
            })
            .unwrap_or_default();
        Arc::new(icon)
    })
    .clone()
}

/// A card: filled, with a thin frame.
fn paint_card(painter: &egui::Painter, rect: egui::Rect, radius: u8, colors: &Palette) {
    painter.rect(
        rect,
        radius,
        colors.card,
        egui::Stroke::new(1.0_f32, colors.card_line),
        egui::StrokeKind::Inside,
    );
}

/// The marks of the tiles and the volumes, drawn in one thin line so that
/// they look alike (the fonts' symbols differ from each other).
#[derive(Clone, Copy)]
enum Icon {
    Keypad,
    Clock,
    Speaker,
    Microphone,
    Phone,
    PersonAdd,
}

fn paint_icon(painter: &egui::Painter, icon: Icon, center: egui::Pos2, ink: egui::Color32) {
    paint_icon_scaled(painter, icon, center, ink, 1.0);
}

/// A mark, `scale` times its usual size.
fn paint_icon_scaled(
    painter: &egui::Painter,
    icon: Icon,
    center: egui::Pos2,
    ink: egui::Color32,
    scale: f32,
) {
    let stroke = egui::Stroke::new(1.6_f32, ink);
    let at = |x: f32, y: f32| center + egui::vec2(x, y) * scale;
    match icon {
        Icon::Keypad => {
            for row in 0..3 {
                for column in 0..3 {
                    let spot = at(column as f32 * 5.5 - 5.5, row as f32 * 5.0 - 7.0);
                    painter.circle_filled(spot, 1.7 * scale, ink);
                }
            }
            painter.circle_filled(at(0.0, 8.0), 1.7 * scale, ink);
        }
        Icon::Clock => {
            painter.circle_stroke(center, 8.5 * scale, stroke);
            painter.line_segment([at(0.0, -5.0), at(0.0, 0.0)], stroke);
            painter.line_segment([at(0.0, 0.0), at(3.5, 2.5)], stroke);
        }
        Icon::Speaker => {
            let cone = vec![
                at(-7.0, -2.5),
                at(-3.5, -2.5),
                at(1.0, -6.5),
                at(1.0, 6.5),
                at(-3.5, 2.5),
                at(-7.0, 2.5),
            ];
            painter.add(egui::Shape::closed_line(cone, stroke));
            let wave: Vec<egui::Pos2> = (0..=8)
                .map(|step| {
                    let angle = -0.9 + 1.8 * step as f32 / 8.0;
                    at(1.5 + 5.5 * angle.cos(), 5.5 * angle.sin())
                })
                .collect();
            painter.add(egui::Shape::line(wave, stroke));
        }
        Icon::Microphone => {
            let body = egui::Rect::from_center_size(at(0.0, -2.5), egui::vec2(6.0, 10.0) * scale);
            painter.rect_stroke(body, 3, stroke, egui::StrokeKind::Middle);
            let cup: Vec<egui::Pos2> = (0..=10)
                .map(|step| {
                    let angle = std::f32::consts::PI * step as f32 / 10.0;
                    at(6.0 * angle.cos(), 0.5 + 5.0 * angle.sin())
                })
                .collect();
            painter.add(egui::Shape::line(cup, stroke));
            painter.line_segment([at(0.0, 5.5), at(0.0, 8.5)], stroke);
        }
        Icon::Phone => {
            // A handset: two ear pieces joined by a curved grip.
            let grip: Vec<egui::Pos2> = (0..=12)
                .map(|step| {
                    let angle = std::f32::consts::PI * (1.0 + 0.5 * step as f32 / 12.0);
                    at(5.0 + 11.0 * angle.cos(), -5.0 - 11.0 * angle.sin())
                })
                .collect();
            let first = grip[0];
            let last = grip[grip.len() - 1];
            painter.add(egui::Shape::line(grip, egui::Stroke::new(3.0 * scale, ink)));
            painter.circle_filled(first, 2.6 * scale, ink);
            painter.circle_filled(last, 2.6 * scale, ink);
        }
        Icon::PersonAdd => {
            painter.circle_stroke(at(-2.5, -4.0), 3.6 * scale, stroke);
            let shoulders: Vec<egui::Pos2> = (0..=12)
                .map(|step| {
                    let angle = std::f32::consts::PI * (1.0 + step as f32 / 12.0);
                    at(-2.5 + 7.0 * angle.cos(), 9.0 + 6.5 * angle.sin())
                })
                .collect();
            painter.add(egui::Shape::line(shoulders, stroke));
            painter.line_segment([at(7.0, -3.0), at(7.0, 3.0)], stroke);
            painter.line_segment([at(4.0, 0.0), at(10.0, 0.0)], stroke);
        }
    }
}

/// A volume slider in the window's colours: a thin rail filled up to a round
/// knob; `level`, during a call, a thin bar under it showing how loud the
/// sound is.
fn level_slider(
    ui: &mut egui::Ui,
    value: &mut u32,
    max: u32,
    width: f32,
    colors: &Palette,
    level: Option<f32>,
) -> egui::Response {
    let (rect, mut response) =
        ui.allocate_exact_size(egui::vec2(width, 20.0), egui::Sense::click_and_drag());
    let knob_radius = 7.0;
    let left = rect.left() + knob_radius;
    let span = (rect.width() - 2.0 * knob_radius).max(1.0);
    if let Some(pointer) = response.interact_pointer_pos() {
        let share = ((pointer.x - left) / span).clamp(0.0, 1.0);
        let chosen = (share * max as f32).round() as u32;
        if chosen != *value {
            *value = chosen;
            response.mark_changed();
        }
    }
    // The wheel moves it in steps of five.
    if response.hovered() {
        let scroll = ui.input(|input| input.smooth_scroll_delta.y + input.smooth_scroll_delta.x);
        if scroll.abs() > 0.5 {
            let step = if scroll > 0.0 { 5 } else { -5 };
            *value = (*value as i32 + step).clamp(0, max as i32) as u32;
            response.mark_changed();
        }
    }
    let share = (*value).min(max) as f32 / max as f32;
    let y = rect.center().y;
    let knob = egui::pos2(left + span * share, y);
    let painter = ui.painter();
    let rail =
        egui::Rect::from_min_max(egui::pos2(left, y - 2.5), egui::pos2(left + span, y + 2.5));
    painter.rect_filled(rail, 3, colors.track);
    let mut filled = rail;
    filled.set_right(knob.x);
    painter.rect_filled(filled, 3, colors.accent);
    if let Some(level) = level {
        // A level on a sound scale: quiet speech is still seen.
        let shown = (1.0 + (level.max(0.000_1)).log10() / 3.0).clamp(0.0, 1.0);
        let color = if level > 0.9 {
            colors.red
        } else {
            colors.green
        };
        let bar = egui::Rect::from_min_max(
            egui::pos2(left, y + 6.0),
            egui::pos2(left + span * shown, y + 8.0),
        );
        painter.rect_filled(bar, 1, color);
    }
    painter.circle_filled(
        knob + egui::vec2(0.0, 1.0),
        knob_radius,
        egui::Color32::from_black_alpha(60),
    );
    painter.circle_filled(knob, knob_radius, colors.knob);
    response
}

/// The backspace mark (the fonts have no glyph for it): a key pointing
/// left with a cross in it.
fn paint_backspace(painter: &egui::Painter, rect: egui::Rect, ink: egui::Color32) {
    let center = rect.center();
    let (width, height) = (22.0, 16.0);
    let left = center.x - width / 2.0;
    let right = center.x + width / 2.0;
    let top = center.y - height / 2.0;
    let bottom = center.y + height / 2.0;
    let outline = vec![
        egui::pos2(left, center.y),
        egui::pos2(left + 7.0, top),
        egui::pos2(right, top),
        egui::pos2(right, bottom),
        egui::pos2(left + 7.0, bottom),
    ];
    let stroke = egui::Stroke::new(1.6_f32, ink);
    painter.add(egui::Shape::closed_line(outline, stroke));
    let cross = egui::pos2(center.x + 3.5, center.y);
    let arm = 3.5;
    painter.line_segment(
        [cross + egui::vec2(-arm, -arm), cross + egui::vec2(arm, arm)],
        stroke,
    );
    painter.line_segment(
        [cross + egui::vec2(-arm, arm), cross + egui::vec2(arm, -arm)],
        stroke,
    );
}

/// The letters of a key, as on a telephone.
fn key_letters(digit: char) -> &'static str {
    match digit {
        '2' => "ABC",
        '3' => "DEF",
        '4' => "GHI",
        '5' => "JKL",
        '6' => "MNO",
        '7' => "PQRS",
        '8' => "TUV",
        '9' => "WXYZ",
        '0' => "+",
        _ => "",
    }
}

/// The suggested name of a saved call history: `calls-2026-11-19.csv`.
fn history_file_name() -> String {
    let today = DateTime::from_unix(now_unix(), utc_offset_seconds());
    format!(
        "calls-{}-{:02}-{:02}.csv",
        today.year, today.month, today.day
    )
}

/// "Copy the number" (and the name, when there is one) for a right click on
/// a call or a contact.
/// Whether a call fits the search of the call list. Digits are looked for
/// among the number's digits however either is written, so `901 075`
/// finds `+7 (901) 075-06-27`, and a Russian number found with 8 is found
/// with 7 as well; with letters the name and the number are searched,
/// ignoring case.
fn call_matches(number: &str, name: &str, query: &str) -> bool {
    let query = query.trim();
    if query.is_empty() {
        return true;
    }
    let digits: String = query.chars().filter(char::is_ascii_digit).collect();
    if !digits.is_empty() && !query.chars().any(char::is_alphabetic) {
        let number_digits: String = number.chars().filter(char::is_ascii_digit).collect();
        if number_digits.contains(&digits) {
            return true;
        }
        // 8 or +7 at the start: the same number.
        let other = match digits.as_bytes() {
            [b'8', ..] => Some(format!("7{}", &digits[1..])),
            [b'7', ..] => Some(format!("8{}", &digits[1..])),
            _ => None,
        };
        return digits.len() >= 4 && other.is_some_and(|other| number_digits.starts_with(&other));
    }
    let query = query.to_lowercase();
    name.to_lowercase().contains(&query) || number.to_lowercase().contains(&query)
}

/// How the buttons of a call in the list are shown, by the room there is.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RowButtons {
    Words,
    Marks,
    None,
}

/// What was chosen for a call in the list, with a button or its menu.
#[derive(Clone, Copy)]
enum RowChoice {
    Call,
    ToContacts,
    Recording,
}

/// The right click on a call in the list: copy, save to the contacts, call,
/// and open the recording when there is one.
fn call_menu(
    ui: &mut egui::Ui,
    number: &str,
    name: &str,
    can_dial: bool,
    recorded: bool,
    chosen: &mut Option<RowChoice>,
) {
    copy_menu(ui, number, name);
    ui.separator();
    if name.is_empty() && ui.button(tr!("В контакты", "To contacts")).clicked() {
        *chosen = Some(RowChoice::ToContacts);
        ui.close_menu();
    }
    if ui
        .add_enabled(can_dial, egui::Button::new(tr!("Позвонить", "Call")))
        .clicked()
    {
        *chosen = Some(RowChoice::Call);
        ui.close_menu();
    }
    if recorded
        && ui
            .button(tr!("Открыть запись", "Open the recording"))
            .clicked()
    {
        *chosen = Some(RowChoice::Recording);
        ui.close_menu();
    }
}

/// A small square button with a drawn mark instead of words.
fn mark_button(ui: &mut egui::Ui, icon: Icon, enabled: bool, colors: &Palette) -> egui::Response {
    let button = egui::Button::new("")
        .fill(colors.key)
        .stroke(egui::Stroke::new(1.0_f32, colors.key_line))
        .corner_radius(8)
        .min_size(egui::vec2(28.0, 22.0));
    let response = ui.add_enabled(enabled, button);
    let ink = if enabled {
        colors.text
    } else {
        colors.weak.gamma_multiply(0.6)
    };
    let painter = ui.painter().with_clip_rect(response.rect);
    paint_icon_scaled(&painter, icon, response.rect.center(), ink, 0.75);
    response
}

fn copy_menu(ui: &mut egui::Ui, number: &str, name: &str) {
    if ui
        .button(tr!("Копировать номер", "Copy the number"))
        .clicked()
    {
        ui.ctx().copy_text(number.to_owned());
        ui.close_menu();
    }
    if !name.is_empty() && ui.button(tr!("Копировать имя", "Copy the name")).clicked()
    {
        ui.ctx().copy_text(name.to_owned());
        ui.close_menu();
    }
    if !name.is_empty()
        && ui
            .button(tr!("Копировать имя и номер", "Copy the name and number"))
            .clicked()
    {
        ui.ctx().copy_text(format!("{name} {number}"));
        ui.close_menu();
    }
}

/// An account's title in the window's language: the settings name an
/// account without number and server in Russian.
fn shown_title(title: String) -> String {
    if english() && title == "новый аккаунт" {
        "new account".to_owned()
    } else {
        title
    }
}

/// The user's status as the window names it.
fn status_text(status: PresenceStatus) -> &'static str {
    match status {
        PresenceStatus::Online => tr!("В сети", "Online"),
        PresenceStatus::Away => tr!("Нет на месте", "Away"),
        PresenceStatus::Busy => tr!("Занят", "Busy"),
    }
}

/// The ways of sending keypad digits as the settings list them.
fn dtmf_text(method: DtmfMethod) -> &'static str {
    match method {
        DtmfMethod::Auto => tr!("Авто", "Auto"),
        DtmfMethod::Rfc2833 => "RFC 2833",
        DtmfMethod::InBand => tr!("В звуке (in-band)", "In-band"),
        DtmfMethod::SipInfo => "SIP INFO",
    }
}

/// A registration in two words, for the account list.
fn registration_short(state: Option<&RegistrationState>) -> &'static str {
    match state {
        Some(RegistrationState::Registered { .. }) => tr!("в сети", "online"),
        Some(RegistrationState::Registering) => tr!("подключение…", "connecting…"),
        Some(RegistrationState::Failed(_)) => tr!("ошибка", "error"),
        Some(RegistrationState::Unregistered) | None => tr!("не в сети", "offline"),
    }
}

/// The colour of the call-quality lamp and what it means.
fn quality_look(lamp: Lamp) -> (egui::Color32, &'static str) {
    match lamp {
        Lamp::Good => (
            egui::Color32::from_rgb(60, 180, 75),
            tr!("Связь хорошая", "Good connection"),
        ),
        Lamp::Fair => (
            egui::Color32::from_rgb(230, 160, 0),
            tr!("Связь с помехами", "Connection with glitches"),
        ),
        Lamp::Poor => (
            egui::Color32::from_rgb(210, 50, 50),
            tr!("Связь плохая", "Poor connection"),
        ),
    }
}

/// The numbers behind the quality lamp, for its hover text.
fn quality_details(live: &LiveQuality, meaning: &str) -> String {
    let mut text = tr!(
        format!(
            "{meaning}\nПотеряно входящих пакетов: {} %",
            live.incoming_loss_percent
        ),
        format!(
            "{meaning}\nIncoming packets lost: {} %",
            live.incoming_loss_percent
        ),
    );
    if live.received == 0 {
        text.push_str(tr!(
            "\nОт собеседника не приходит звук",
            "\nNo audio comes from the other side"
        ));
    }
    if live.gaps > 0 {
        text.push_str(&tr!(
            format!("\nПерерывов в звуке: {}", live.gaps),
            format!("\nAudio gaps: {}", live.gaps),
        ));
    }
    if let Some(remote) = live.remote {
        text.push_str(&tr!(
            format!(
                "\nУ собеседника потеряно: {} %\nДжиттер: {} мс",
                remote.lost_percent, remote.jitter_ms
            ),
            format!(
                "\nLost at the other side: {} %\nJitter: {} ms",
                remote.lost_percent, remote.jitter_ms
            ),
        ));
        if let Some(round_trip) = remote.round_trip_ms {
            text.push_str(&tr!(
                format!("\nЗадержка туда и обратно: {round_trip} мс"),
                format!("\nRound trip: {round_trip} ms"),
            ));
        }
    }
    text
}

/// The colour of a busy lamp and what it means.
fn lamp_look(state: LineState) -> (egui::Color32, &'static str) {
    match state {
        LineState::Idle => (
            egui::Color32::from_rgb(60, 180, 75),
            tr!("свободен", "free"),
        ),
        LineState::Ringing => (
            egui::Color32::from_rgb(230, 160, 0),
            tr!("ему звонят", "ringing"),
        ),
        LineState::Busy => (
            egui::Color32::from_rgb(210, 50, 50),
            tr!("разговаривает", "talking"),
        ),
        LineState::Unknown => (
            egui::Color32::GRAY,
            tr!("состояние неизвестно", "state unknown"),
        ),
    }
}

/// The encryption modes as the settings list them.
fn encryption_text(mode: MediaEncryption) -> &'static str {
    match mode {
        MediaEncryption::Off => tr!("выключено", "off"),
        MediaEncryption::Optional => tr!("если АТС умеет", "if the exchange can"),
        MediaEncryption::Required => tr!("обязательно", "required"),
    }
}

/// A few words on what a codec is good for.
fn codec_note(codec: Codec) -> &'static str {
    match codec {
        Codec::Pcmu | Codec::Pcma => tr!(
            "обычная телефония, есть везде",
            "usual telephony, available everywhere"
        ),
        Codec::G722 => tr!("чистый широкополосный звук", "clear wideband audio"),
        Codec::Opus => tr!(
            "широкополосный, стойкий к потерям",
            "wideband, robust against loss"
        ),
        Codec::G729 => tr!("для медленных каналов", "for slow links"),
        Codec::Gsm => tr!("старые АТС и GSM-шлюзы", "old exchanges and GSM gateways"),
        Codec::Speex => tr!("экономный, 15 кбит/с", "thrifty, 15 kbit/s"),
        Codec::SpeexWide => tr!("широкополосный Speex", "wideband Speex"),
    }
}

/// Why a melody file was refused, in the user's language.
fn melody_error_text(error: &str) -> String {
    match error {
        "not a WAV file" => tr!("это не файл WAV", "this is not a WAV file").to_owned(),
        "the file is larger than 20 MB" => {
            tr!("файл больше 20 МБ", "the file is larger than 20 MB").to_owned()
        }
        "the WAV file has no sound in it" | "the WAV file has no format" => {
            tr!("в файле нет звука", "the file has no audio").to_owned()
        }
        other if other.starts_with("this kind of WAV") => tr!(
            "сжатый WAV не поддерживается — сохраните его как обычный PCM",
            "compressed WAV is not supported; save it as plain PCM"
        )
        .to_owned(),
        other => tr!(
            format!("файл не читается ({other})"),
            format!("the file cannot be read ({other})"),
        ),
    }
}

fn registration_text(state: &RegistrationState) -> String {
    match state {
        RegistrationState::Registering => tr!("подключение…", "connecting…").to_owned(),
        RegistrationState::Registered { .. } => tr!("в сети", "online").to_owned(),
        RegistrationState::Unregistered => tr!("не в сети", "offline").to_owned(),
        RegistrationState::Failed(reason) => {
            let reason = if reason.starts_with("wrong user name or password") {
                tr!("неверный номер или пароль", "wrong number or password").to_owned()
            } else if reason.starts_with("no response") {
                tr!("сервер не отвечает", "the server does not answer").to_owned()
            } else if reason.contains("certificate") {
                tr!(
                    "сертификат сервера не прошёл проверку (TLS)",
                    "the server certificate failed the check (TLS)"
                )
                .to_owned()
            } else if reason.starts_with("cannot connect") {
                tr!(
                    "не удалось соединиться с сервером, пробую снова",
                    "cannot connect to the server, trying again"
                )
                .to_owned()
            } else if reason.starts_with("the connection to the SIP server was lost") {
                tr!(
                    "соединение с сервером прервалось, подключаюсь снова",
                    "the connection to the server broke, reconnecting"
                )
                .to_owned()
            } else {
                reason.clone()
            };
            tr!(format!("Ошибка: {reason}"), format!("Error: {reason}"),)
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
        ctx.request_repaint_after(self.next_look(ctx));
        let mut actions = Vec::new();
        self.keys_and_tray(ctx, &mut actions);
        self.take_links(ctx, &mut actions);

        self.main_screen(ctx, &mut actions);

        if self.settings_open {
            self.settings_window(ctx, &mut actions);
        }
        if self.log_open {
            self.log_window(ctx);
        }
        if let Some(index) = self.account_requested.take() {
            actions.push(Action::SwitchAccount(index));
        }
        if std::mem::take(&mut self.voicemail_requested) {
            actions.push(Action::Voicemail);
        }
        if let Some(on) = self.pending_dnd.take() {
            actions.push(Action::DoNotDisturb(on));
        }
        self.incoming_window(ctx, &mut actions);
        if self.on_top_applied != Some(self.always_on_top) {
            self.on_top_applied = Some(self.always_on_top);
            ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(if self.always_on_top {
                egui::WindowLevel::AlwaysOnTop
            } else {
                egui::WindowLevel::Normal
            }));
        }
        for action in actions {
            self.apply(action);
        }
    }
}

impl PhoneWindow {
    /// A small window above all others while a call rings, with the caller
    /// and the buttons to answer or decline, as MicroSIP shows.
    fn incoming_window(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let ringing = (0..LINES).find(|line| self.lines[*line].view.phase() == Phase::Incoming);
        let Some(line) = ringing.filter(|_| self.incoming_popup) else {
            self.popup_dismissed = false;
            return;
        };
        if self.popup_dismissed {
            return;
        }
        let (who, number, account) = match self.lines[line].current.as_ref() {
            Some(call) if !call.name.is_empty() => {
                (call.name.clone(), call.number.clone(), call.account.clone())
            }
            Some(call) => (call.number.clone(), String::new(), call.account.clone()),
            None => (
                tr!("Входящий вызов", "Incoming call").to_owned(),
                String::new(),
                String::new(),
            ),
        };
        // Bottom right of the screen, where notifications appear.
        let size = egui::vec2(320.0, 128.0);
        let screen = ctx.input(|input| input.viewport().monitor_size);
        let mut builder = egui::ViewportBuilder::default()
            .with_title(tr!("Входящий вызов", "Incoming call"))
            .with_inner_size(size)
            .with_resizable(false)
            .with_icon(window_icon())
            .with_always_on_top();
        if let Some(screen) = screen {
            builder = builder.with_position(egui::pos2(
                (screen.x - size.x - 24.0).max(0.0),
                (screen.y - size.y - 72.0).max(0.0),
            ));
        }
        let colors = palette(self.light_theme);
        let mut answer = false;
        let mut decline = false;
        let mut closed = false;
        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("incoming-call"),
            builder,
            |ctx, _| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.label(tr!("Входящий вызов", "Incoming call"));
                    ui.label(egui::RichText::new(&who).size(20.0).strong());
                    if !number.is_empty() {
                        ui.label(&number);
                    }
                    if !account.is_empty() {
                        ui.weak(tr!(format!("на номер {account}"), format!("to {account}"),));
                    }
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        let half = egui::vec2((ui.available_width() - 8.0) / 2.0, 34.0);
                        let green = egui::Button::new(
                            egui::RichText::new(tr!("Ответить", "Answer"))
                                .strong()
                                .color(colors.on_call),
                        )
                        .fill(colors.call)
                        .stroke(egui::Stroke::NONE)
                        .corner_radius(12);
                        if ui.add_sized(half, green).clicked() {
                            answer = true;
                        }
                        let red = egui::Button::new(
                            egui::RichText::new(tr!("Отклонить", "Decline"))
                                .strong()
                                .color(egui::Color32::WHITE),
                        )
                        .fill(colors.red)
                        .stroke(egui::Stroke::NONE)
                        .corner_radius(12);
                        if ui.add_sized(half, red).clicked() {
                            decline = true;
                        }
                    });
                });
                if ctx.input(|input| input.viewport().close_requested()) {
                    closed = true;
                }
                // Answered or declined here: the little window goes at once,
                // without waiting for the main window to be drawn again
                // (Windows does not draw a window that is minimized).
                if answer || decline {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                }
            },
        );
        if answer {
            actions.push(Action::Answer);
        }
        if decline {
            actions.push(Action::Reject);
        }
        // Closed with its cross: the call rings on in the main window.
        if closed {
            self.popup_dismissed = true;
        }
    }
}

fn main() -> eframe::Result<()> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    // Windows starts the phone with this word when it starts by itself.
    let start_hidden = arguments.iter().any(|argument| argument == "--minimized");
    // A link (`tel:2205`) or a number to call.
    let start_target = arguments
        .iter()
        .find_map(|argument| links::target_from_argument(argument));
    // Only one copy runs: a later one hands its number over and exits.
    let window_context: Arc<OnceLock<egui::Context>> = Arc::default();
    let wake = {
        let window_context = Arc::clone(&window_context);
        move || {
            if let Some(context) = window_context.get() {
                context.request_repaint();
            }
        }
    };
    let settings_dir = settings::default_path().and_then(|path| path.parent().map(Path::to_owned));
    let instance = match settings_dir {
        Some(dir) => match links::start(&dir, start_target.clone().map(LinkRequest::Dial), wake) {
            links::Start::Handed => return Ok(()),
            links::Start::First(instance) => Some(instance),
        },
        None => None,
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_icon(window_icon())
            .with_inner_size([400.0, 640.0])
            // Any size up to full screen; the window adapts. Below this the
            // buttons of a call would no longer fit under the keys.
            .with_min_inner_size([280.0, 480.0]),
        ..Default::default()
    };
    eframe::run_native(
        WINDOW_TITLE,
        options,
        Box::new(move |creation| {
            let _ = window_context.set(creation.egui_ctx.clone());
            Ok(Box::new(PhoneWindow::new(
                &creation.egui_ctx,
                start_hidden,
                instance,
                start_target,
            )))
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calls_are_found_by_any_part_of_the_number_or_the_name() {
        let number = "+7 (901) 075-06-27";
        for query in [
            "",
            "  ",
            "901",
            "075-06",
            "79010750627",
            "8 901 075",
            "0627",
        ] {
            assert!(call_matches(number, "", query), "{query}");
        }
        for query in ["902", "8902", "12345"] {
            assert!(!call_matches(number, "", query), "{query}");
        }
        // Written with 8, found with +7 too.
        assert!(call_matches("89010750627", "", "+7 901"));
        assert!(call_matches("4010", "Анна Смирнова", "анна"));
        assert!(call_matches("4010", "Анна Смирнова", "СМИР"));
        assert!(call_matches("4010", "Анна Смирнова", "401"));
        assert!(!call_matches("4010", "Анна Смирнова", "Иван"));
        // Letters typed into a short number are no match.
        assert!(!call_matches("4010", "", "4010a"));
    }

    #[test]
    fn the_end_of_a_call_follows_its_start() {
        // 19.11.2026 14:30 in Moscow.
        let start = 1_795_087_800;
        assert_eq!(end_text(start, 0, 3 * 3_600), "");
        assert_eq!(end_text(start, start + 302, 3 * 3_600), " – 14:35");
        assert_eq!(
            end_text(start, start + 10 * 3_600, 3 * 3_600),
            " – 20.11.2026 00:30"
        );
    }

    #[test]
    fn failed_calls_say_what_the_code_means() {
        set_english(false);
        assert_eq!(
            end_reason_text(EndReason::Rejected(503)),
            "сервис недоступен (503 Service Unavailable)"
        );
        assert_eq!(
            result_text("486").as_deref(),
            Some("занято (486 Busy Here)")
        );
        assert_eq!(
            end_reason_text(EndReason::Rejected(499)),
            "вызов не прошёл (код 499)"
        );
        set_english(true);
        assert_eq!(end_reason_text(EndReason::Rejected(404)), "Not Found (404)");
        set_english(false);
    }
}
