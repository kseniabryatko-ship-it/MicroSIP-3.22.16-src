//! What the phone needs from Windows itself, outside its own window: an icon
//! in the notification area, coming to the front when a call rings, global
//! keys to answer and hang up, starting with Windows, and the system's
//! dialogs for choosing a folder or a file.
//!
//! The icon, the keys and the watching for a ringing call run on a small
//! thread of their own with its own hidden window, so they keep working
//! while the phone's window is minimized or hidden and its drawing loop is
//! asleep. On other systems everything here does nothing.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

/// Requests from the notification icon and the global keys, picked up by
/// the window when it next draws.
#[derive(Default)]
pub struct ShellSignals {
    /// The answer key was pressed.
    pub answer: AtomicBool,
    /// The hang-up key was pressed.
    pub hangup: AtomicBool,
    /// The key that brings the number from the clipboard was pressed.
    pub paste: AtomicBool,
    /// "Exit" was chosen from the icon's menu: closing must really close.
    pub exit: AtomicBool,
    /// Set by the phone while a call is ringing; the shell watches it.
    pub ringing: Mutex<Option<Arc<AtomicBool>>>,
}

pub struct ShellOptions {
    /// Title of the phone's window, by which it is found.
    pub window_title: String,
    /// Text shown when the mouse rests on the icon.
    pub tip: String,
    /// Menu texts: open the window, exit the program.
    pub menu_open: String,
    pub menu_exit: String,
    /// Register Ctrl+Alt+PageUp (answer), Ctrl+Alt+PageDown (hang up) and
    /// Ctrl+Alt+Insert (number from the clipboard).
    pub hotkeys: bool,
}

/// The running shell integration. Dropping it removes the icon.
pub struct Shell {
    signals: Arc<ShellSignals>,
    window_title: String,
    #[cfg(windows)]
    hidden_window: Arc<std::sync::atomic::AtomicIsize>,
}

impl Shell {
    /// Starts the integration. `wake` is called (from another thread) when
    /// the window should look at the signals. `None` where unsupported.
    pub fn start<F>(options: ShellOptions, wake: F) -> Option<Self>
    where
        F: Fn() + Send + Sync + 'static,
    {
        platform::start(options, Box::new(wake))
    }

    pub fn signals(&self) -> &Arc<ShellSignals> {
        &self.signals
    }

    /// Tells the shell which flag says "a call is ringing" (it changes when
    /// the phone reconnects).
    pub fn watch_ringing(&self, flag: Option<Arc<AtomicBool>>) {
        if let Ok(mut ringing) = self.signals.ringing.lock() {
            *ringing = flag;
        }
    }

    /// Hides the phone's window; the icon brings it back.
    pub fn hide_window(&self) {
        platform::show_window(&self.window_title, false);
    }

    pub fn show_window(&self) {
        platform::show_window(&self.window_title, true);
    }
}

impl Drop for Shell {
    fn drop(&mut self) {
        #[cfg(windows)]
        platform::stop(&self.hidden_window);
    }
}

/// True where the notification icon and the rest are available.
pub fn is_available() -> bool {
    cfg!(windows)
}

/// Makes the program start with Windows (minimized), or stops that.
pub fn set_autostart(enabled: bool) -> Result<(), String> {
    platform::set_autostart(enabled)
}

pub fn autostart_enabled() -> bool {
    platform::autostart_enabled()
}

/// Makes Windows open `tel:`, `sip:` and `callto:` links with this
/// program, or stops that (only where they still open it).
pub fn set_link_handler(enabled: bool) -> Result<(), String> {
    platform::set_link_handler(enabled)
}

/// True when the links are registered to open this program.
pub fn link_handler_enabled() -> bool {
    platform::link_handler_enabled()
}

/// Opens the "Default apps" page of the Windows settings, where the user
/// picks the program for each kind of link.
pub fn open_default_apps() {
    platform::open_default_apps();
}

/// Shows the system's "choose a folder" dialog. Blocks until it is closed.
pub fn pick_folder() -> Option<PathBuf> {
    platform::dialog(
        "$d = New-Object System.Windows.Forms.FolderBrowserDialog; \
         if ($d.ShowDialog() -eq 'OK') { $d.SelectedPath }",
    )
}

/// Shows the system's "open file" dialog for `extension` (such as `csv`).
pub fn pick_open_file(extension: &str) -> Option<PathBuf> {
    platform::dialog(&format!(
        "$d = New-Object System.Windows.Forms.OpenFileDialog; \
         $d.Filter = '{0}|*.{0}|*.*|*.*'; \
         if ($d.ShowDialog() -eq 'OK') {{ $d.FileName }}",
        safe_extension(extension)
    ))
}

/// Shows the system's "save file" dialog with a suggested name.
pub fn pick_save_file(suggested_name: &str, extension: &str) -> Option<PathBuf> {
    let name: String = suggested_name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .collect();
    platform::dialog(&format!(
        "$d = New-Object System.Windows.Forms.SaveFileDialog; \
         $d.FileName = '{name}'; $d.Filter = '{0}|*.{0}'; \
         if ($d.ShowDialog() -eq 'OK') {{ $d.FileName }}",
        safe_extension(extension)
    ))
}

/// Only letters and digits may go into the dialog's script.
fn safe_extension(extension: &str) -> String {
    let cleaned: String = extension
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect();
    if cleaned.is_empty() {
        "txt".to_owned()
    } else {
        cleaned
    }
}

#[cfg(windows)]
mod platform {
    use super::{Shell, ShellOptions, ShellSignals};
    use std::os::windows::process::CommandExt;
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
    use std::sync::{Arc, OnceLock};
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, SetProcessWorkingSetSize};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        MOD_ALT, MOD_CONTROL, RegisterHotKey, VK_INSERT, VK_NEXT, VK_PRIOR,
    };
    use windows_sys::Win32::UI::Shell::{
        NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW, Shell_NotifyIconW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreateIconFromResourceEx, CreatePopupMenu, CreateWindowExW, DefWindowProcW,
        DestroyMenu, DispatchMessageW, FindWindowW, FlashWindow, GetCursorPos, GetMessageW,
        GetSystemMetrics, HICON, HWND_MESSAGE, HWND_NOTOPMOST, HWND_TOPMOST, IDI_APPLICATION,
        IsIconic, IsWindowVisible, LR_DEFAULTCOLOR, LoadIconW, MF_STRING, MSG, PostMessageW,
        PostQuitMessage, RegisterClassW, SM_CXSMICON, SW_HIDE, SW_RESTORE, SW_SHOW, SWP_NOMOVE,
        SWP_NOSIZE, SWP_SHOWWINDOW, SetForegroundWindow, SetTimer, SetWindowPos, ShowWindow,
        TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, WM_APP, WM_CLOSE, WM_COMMAND,
        WM_DESTROY, WM_HOTKEY, WM_LBUTTONUP, WM_RBUTTONUP, WM_TIMER, WNDCLASSW,
    };

    /// Do not open a console window for helper programs.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
    const RUN_VALUE: &str = "MicroSIP-Rust";

    const TRAY_MESSAGE: u32 = WM_APP + 1;
    const MENU_OPEN: usize = 1;
    const MENU_EXIT: usize = 2;
    const HOTKEY_ANSWER: i32 = 1;
    const HOTKEY_HANGUP: i32 = 2;
    const HOTKEY_PASTE: i32 = 3;

    /// What the hidden window's procedure needs; there is one per program.
    struct State {
        signals: Arc<ShellSignals>,
        title: Vec<u16>,
        menu_open: Vec<u16>,
        menu_exit: Vec<u16>,
        wake: Box<dyn Fn() + Send + Sync>,
        was_ringing: AtomicBool,
    }

    static STATE: OnceLock<State> = OnceLock::new();

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// The phone's own window, found by its title.
    fn main_window(title: &[u16]) -> HWND {
        // SAFETY: `title` is a NUL-terminated UTF-16 string that outlives
        // the call; a null class name means "any class".
        unsafe { FindWindowW(std::ptr::null(), title.as_ptr()) }
    }

    fn show(title: &[u16], visible: bool) {
        let window = main_window(title);
        if window.is_null() {
            return;
        }
        // SAFETY: `window` is a window handle just obtained from the system;
        // these calls only change how it is shown.
        unsafe {
            if !visible {
                ShowWindow(window, SW_HIDE);
                // In the tray the window is not drawn: the memory the
                // drawing held goes back to Windows, as MicroSIP's does.
                // It is taken again, a little at a time, when needed.
                SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX);
            } else {
                let command = if IsIconic(window) != 0 {
                    SW_RESTORE
                } else {
                    SW_SHOW
                };
                ShowWindow(window, command);
                SetForegroundWindow(window);
            }
        }
    }

    pub fn show_window(title: &str, visible: bool) {
        show(&wide(title), visible);
    }

    /// Keeps the window above the others while a call rings.
    fn set_topmost(title: &[u16], topmost: bool) {
        let window = main_window(title);
        if window.is_null() {
            return;
        }
        let place = if topmost {
            HWND_TOPMOST
        } else {
            HWND_NOTOPMOST
        };
        let flags = if topmost {
            SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW
        } else {
            SWP_NOMOVE | SWP_NOSIZE
        };
        // SAFETY: as in `show`; position and size are left unchanged.
        unsafe {
            SetWindowPos(window, place, 0, 0, 0, 0, flags);
            if topmost {
                FlashWindow(window, 1);
            }
        }
    }

    fn on_timer(state: &State) {
        let ringing = state
            .signals
            .ringing
            .lock()
            .ok()
            .and_then(|flag| flag.as_ref().map(|flag| flag.load(Ordering::Relaxed)))
            .unwrap_or(false);
        let was = state.was_ringing.swap(ringing, Ordering::Relaxed);
        if ringing && !was {
            show(&state.title, true);
            set_topmost(&state.title, true);
            (state.wake)();
        } else if !ringing && was {
            set_topmost(&state.title, false);
        }
    }

    fn toggle(state: &State) {
        let window = main_window(&state.title);
        if window.is_null() {
            return;
        }
        // SAFETY: only asks how the window is shown.
        let shown = unsafe { IsWindowVisible(window) != 0 && IsIconic(window) == 0 };
        show(&state.title, !shown);
        (state.wake)();
    }

    fn menu(hidden: HWND, state: &State) {
        // SAFETY: the menu is created, shown and destroyed here; the strings
        // are NUL-terminated and live in `state`; `hidden` is our window.
        unsafe {
            let menu = CreatePopupMenu();
            if menu.is_null() {
                return;
            }
            AppendMenuW(menu, MF_STRING, MENU_OPEN, state.menu_open.as_ptr());
            AppendMenuW(menu, MF_STRING, MENU_EXIT, state.menu_exit.as_ptr());
            let mut point = POINT { x: 0, y: 0 };
            GetCursorPos(&mut point);
            // Needed for the menu to close when the user clicks elsewhere.
            SetForegroundWindow(hidden);
            TrackPopupMenu(
                menu,
                TPM_RIGHTBUTTON,
                point.x,
                point.y,
                0,
                hidden,
                std::ptr::null(),
            );
            DestroyMenu(menu);
        }
    }

    unsafe extern "system" fn window_procedure(
        window: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        let Some(state) = STATE.get() else {
            // SAFETY: default handling of a message for our own window.
            return unsafe { DefWindowProcW(window, message, wparam, lparam) };
        };
        match message {
            WM_TIMER => on_timer(state),
            TRAY_MESSAGE => match lparam as u32 {
                WM_LBUTTONUP => toggle(state),
                WM_RBUTTONUP => menu(window, state),
                _ => {}
            },
            WM_COMMAND => match wparam & 0xFFFF {
                MENU_OPEN => {
                    show(&state.title, true);
                    (state.wake)();
                }
                MENU_EXIT => {
                    state.signals.exit.store(true, Ordering::Relaxed);
                    // A hidden window is not drawn and would never notice.
                    show(&state.title, true);
                    let main = main_window(&state.title);
                    if !main.is_null() {
                        // SAFETY: asks the phone's window to close.
                        unsafe { PostMessageW(main, WM_CLOSE, 0, 0) };
                    }
                    (state.wake)();
                }
                _ => {}
            },
            WM_HOTKEY => {
                match wparam as i32 {
                    HOTKEY_ANSWER => state.signals.answer.store(true, Ordering::Relaxed),
                    HOTKEY_HANGUP => state.signals.hangup.store(true, Ordering::Relaxed),
                    HOTKEY_PASTE => state.signals.paste.store(true, Ordering::Relaxed),
                    _ => {}
                }
                // A hidden or minimized window is not drawn, and it is the
                // drawing that acts on the key; a number is typed into a
                // window in front.
                let main = main_window(&state.title);
                // SAFETY: only asks how the window is shown.
                let asleep =
                    !main.is_null() && unsafe { IsWindowVisible(main) == 0 || IsIconic(main) != 0 };
                if asleep || wparam as i32 == HOTKEY_PASTE {
                    show(&state.title, true);
                }
                (state.wake)();
            }
            WM_DESTROY => {
                // SAFETY: ends this thread's message loop.
                unsafe { PostQuitMessage(0) };
            }
            // SAFETY: default handling of a message for our own window.
            _ => return unsafe { DefWindowProcW(window, message, wparam, lparam) },
        }
        0
    }

    pub fn start(options: ShellOptions, wake: Box<dyn Fn() + Send + Sync>) -> Option<Shell> {
        let signals = Arc::new(ShellSignals::default());
        let state = State {
            signals: Arc::clone(&signals),
            title: wide(&options.window_title),
            menu_open: wide(&options.menu_open),
            menu_exit: wide(&options.menu_exit),
            wake,
            was_ringing: AtomicBool::new(false),
        };
        // One shell per program: a second start is refused.
        STATE.set(state).ok()?;

        let hidden_window = Arc::new(AtomicIsize::new(0));
        let for_thread = Arc::clone(&hidden_window);
        let tip = options.tip;
        let hotkeys = options.hotkeys;
        std::thread::spawn(move || run(&for_thread, &tip, hotkeys));
        Some(Shell {
            signals,
            window_title: options.window_title,
            hidden_window,
        })
    }

    /// The shell thread: a hidden window, the icon, the keys, the timer.
    fn run(handle: &AtomicIsize, tip: &str, hotkeys: bool) {
        let class_name = wide("MicroSipRustShell");
        // SAFETY: plain Win32 window creation. Every pointer passed is to a
        // NUL-terminated buffer or structure that lives until the call
        // returns; the window procedure is the function above.
        unsafe {
            let instance = GetModuleHandleW(std::ptr::null());
            let class = WNDCLASSW {
                style: 0,
                lpfnWndProc: Some(window_procedure),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: instance,
                hIcon: std::ptr::null_mut(),
                hCursor: std::ptr::null_mut(),
                hbrBackground: std::ptr::null_mut(),
                lpszMenuName: std::ptr::null(),
                lpszClassName: class_name.as_ptr(),
            };
            if RegisterClassW(&class) == 0 {
                return;
            }
            let window = CreateWindowExW(
                0,
                class_name.as_ptr(),
                class_name.as_ptr(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            );
            if window.is_null() {
                return;
            }
            handle.store(window as isize, Ordering::Release);

            let mut icon: NOTIFYICONDATAW = std::mem::zeroed();
            icon.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
            icon.hWnd = window;
            icon.uID = 1;
            icon.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
            icon.uCallbackMessage = TRAY_MESSAGE;
            icon.hIcon = tray_icon();
            if icon.hIcon.is_null() {
                icon.hIcon = LoadIconW(std::ptr::null_mut(), IDI_APPLICATION);
            }
            for (slot, unit) in icon.szTip.iter_mut().zip(tip.encode_utf16().take(127)) {
                *slot = unit;
            }
            Shell_NotifyIconW(NIM_ADD, &icon);

            if hotkeys {
                // A key some other program already holds is simply not ours.
                let modifiers = MOD_CONTROL | MOD_ALT;
                RegisterHotKey(window, HOTKEY_ANSWER, modifiers, u32::from(VK_PRIOR));
                RegisterHotKey(window, HOTKEY_HANGUP, modifiers, u32::from(VK_NEXT));
                RegisterHotKey(window, HOTKEY_PASTE, modifiers, u32::from(VK_INSERT));
            }
            SetTimer(window, 1, 200, None);

            let mut message: MSG = std::mem::zeroed();
            while GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) > 0 {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
            Shell_NotifyIconW(NIM_DELETE, &icon);
        }
    }

    /// The tray icon, NAVI on black, in the size the tray uses here.
    fn tray_icon() -> HICON {
        const SMALL: &[u8] = include_bytes!("../assets/tray-16.png");
        const LARGE: &[u8] = include_bytes!("../assets/tray-32.png");
        // SAFETY: only reads a system setting.
        let wanted = unsafe { GetSystemMetrics(SM_CXSMICON) };
        let png = if wanted <= 16 { SMALL } else { LARGE };
        let Ok(picture) = image::load_from_memory_with_format(png, image::ImageFormat::Png) else {
            return std::ptr::null_mut();
        };
        let picture = picture.to_rgba8();
        let (width, height) = (picture.width(), picture.height());
        // An icon as Windows keeps it in a program's resources: a header,
        // the colours from the bottom row up (blue, green, red, alpha) and
        // an empty mask, the transparency being in the alpha.
        let mask_row = width.div_ceil(32) * 4;
        let mut resource = Vec::new();
        for value in [40, width, height * 2] {
            resource.extend(value.to_le_bytes());
        }
        resource.extend(1_u16.to_le_bytes());
        resource.extend(32_u16.to_le_bytes());
        resource.extend(0_u32.to_le_bytes());
        resource.extend((width * height * 4 + mask_row * height).to_le_bytes());
        resource.extend([0; 16]);
        for row in picture.rows().rev() {
            for pixel in row {
                let [red, green, blue, alpha] = pixel.0;
                resource.extend([blue, green, red, alpha]);
            }
        }
        resource.resize(resource.len() + (mask_row * height) as usize, 0);
        // SAFETY: the buffer is a complete icon image as described above
        // and lives until the call returns; the system copies it.
        unsafe {
            CreateIconFromResourceEx(
                resource.as_ptr(),
                resource.len() as u32,
                1,
                0x0003_0000,
                width as i32,
                height as i32,
                LR_DEFAULTCOLOR,
            )
        }
    }

    pub fn stop(handle: &AtomicIsize) {
        let window = handle.load(Ordering::Acquire) as HWND;
        if !window.is_null() {
            // SAFETY: our own hidden window. A window can only be destroyed
            // by its own thread, so it is asked to close itself.
            unsafe { PostMessageW(window, WM_CLOSE, 0, 0) };
        }
    }

    fn hidden(program: &str) -> Command {
        let mut command = Command::new(program);
        command.creation_flags(CREATE_NO_WINDOW);
        command
    }

    pub fn set_autostart(enabled: bool) -> Result<(), String> {
        let mut command = hidden("reg");
        if enabled {
            let program = std::env::current_exe().map_err(|error| error.to_string())?;
            let line = format!("\"{}\" --minimized", program.display());
            command.args(["add", RUN_KEY, "/v", RUN_VALUE, "/t", "REG_SZ", "/d"]);
            command.arg(line).arg("/f");
        } else {
            command.args(["delete", RUN_KEY, "/v", RUN_VALUE, "/f"]);
        }
        let status = command.status().map_err(|error| error.to_string())?;
        // Deleting what is not there is not a failure for the user.
        if status.success() || !enabled {
            Ok(())
        } else {
            Err(format!("reg.exe finished with {status}"))
        }
    }

    pub fn autostart_enabled() -> bool {
        hidden("reg")
            .args(["query", RUN_KEY, "/v", RUN_VALUE])
            .output()
            .is_ok_and(|output| output.status.success())
    }

    fn reg(arguments: &[String]) -> Result<(), String> {
        let status = hidden("reg")
            .args(arguments)
            .status()
            .map_err(|error| error.to_string())?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("reg.exe finished with {status}"))
        }
    }

    fn open_command(key: &str) -> Option<String> {
        let output = hidden("reg")
            .args(["query", &format!("{key}\\shell\\open\\command"), "/ve"])
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
    }

    pub fn set_link_handler(enabled: bool) -> Result<(), String> {
        let program = std::env::current_exe().map_err(|error| error.to_string())?;
        let program = program.display().to_string();
        if enabled {
            for command in crate::links::registry_commands(&program, true) {
                reg(&command)?;
            }
            return Ok(());
        }
        // Another program's links are left alone.
        for scheme in crate::links::SCHEMES {
            let key = crate::links::scheme_key(scheme);
            if open_command(&key).is_some_and(|text| crate::links::owns_scheme(&text, &program)) {
                let _ = reg(&["delete".to_owned(), key, "/f".to_owned()]);
            }
        }
        // Deleting what is not there is not a failure for the user.
        for command in crate::links::registry_commands(&program, false) {
            let _ = reg(&command);
        }
        Ok(())
    }

    pub fn link_handler_enabled() -> bool {
        let Ok(program) = std::env::current_exe() else {
            return false;
        };
        let program = program.display().to_string();
        open_command(&crate::links::scheme_key("tel"))
            .is_some_and(|text| crate::links::owns_scheme(&text, &program))
    }

    pub fn open_default_apps() {
        let _ = hidden("cmd")
            .args(["/c", "start", "", "ms-settings:defaultapps"])
            .spawn();
    }

    /// Runs a Windows Forms dialog and returns the path it printed.
    pub fn dialog(script: &str) -> Option<PathBuf> {
        let script = format!(
            "[Console]::OutputEncoding = [Text.Encoding]::UTF8; \
             Add-Type -AssemblyName System.Windows.Forms; {script}"
        );
        let output = hidden("powershell")
            .args(["-NoProfile", "-STA", "-Command", &script])
            .output()
            .ok()?;
        let path = String::from_utf8(output.stdout).ok()?;
        let path = path.trim().trim_start_matches('\u{feff}');
        (!path.is_empty()).then(|| PathBuf::from(path))
    }
}

#[cfg(not(windows))]
mod platform {
    use super::{Shell, ShellOptions};
    use std::path::PathBuf;

    pub fn start(_options: ShellOptions, _wake: Box<dyn Fn() + Send + Sync>) -> Option<Shell> {
        None
    }

    pub fn show_window(_title: &str, _visible: bool) {}

    pub fn set_autostart(_enabled: bool) -> Result<(), String> {
        Err("starting with the system is available on Windows only".to_owned())
    }

    pub fn autostart_enabled() -> bool {
        false
    }

    pub fn dialog(_script: &str) -> Option<PathBuf> {
        None
    }

    pub fn set_link_handler(_enabled: bool) -> Result<(), String> {
        Err("opening links is set up on Windows only".to_owned())
    }

    pub fn link_handler_enabled() -> bool {
        false
    }

    pub fn open_default_apps() {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialog_scripts_take_only_plain_extensions() {
        assert_eq!(safe_extension("csv"), "csv");
        assert_eq!(safe_extension("c'; Remove-Item x; '"), "cRemoveItemx");
        assert_eq!(safe_extension("*.*"), "txt");
    }

    #[cfg(not(windows))]
    #[test]
    fn without_windows_nothing_is_started_or_changed() {
        assert!(!is_available());
        let options = ShellOptions {
            window_title: "x".to_owned(),
            tip: "x".to_owned(),
            menu_open: "x".to_owned(),
            menu_exit: "x".to_owned(),
            hotkeys: true,
        };
        assert!(Shell::start(options, || {}).is_none());
        assert!(!autostart_enabled());
        assert!(set_autostart(true).is_err());
        assert_eq!(pick_folder(), None);
        assert_eq!(pick_open_file("csv"), None);
        assert_eq!(pick_save_file("contacts.csv", "csv"), None);
    }
}
