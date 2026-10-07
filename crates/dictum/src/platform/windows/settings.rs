//! The settings window: a modeless dialog on the main thread for the settings people change most.
//! Saving writes only what changed into config.toml (its comments stay) and restarts Dictum to
//! apply it; everything else is still edited in the file ("Open config file").

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Once};

use windows_sys::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::{
    BST_CHECKED, BST_UNCHECKED, CheckDlgButton, ICC_STANDARD_CLASSES, INITCOMMONCONTROLSEX,
    InitCommonControlsEx, IsDlgButtonChecked,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    BN_CLICKED, BS_AUTOCHECKBOX, BS_DEFPUSHBUTTON, BS_GROUPBOX, BS_PUSHBUTTON, CB_ADDSTRING, CB_GETCURSEL,
    CB_RESETCONTENT, CB_SETCURSEL, CBN_SELCHANGE, CBS_AUTOHSCROLL, CBS_DROPDOWN, CBS_DROPDOWNLIST,
    CreateDialogIndirectParamW, DS_CENTER, DS_MODALFRAME, DS_SETFONT, DestroyWindow, ES_AUTOVSCROLL,
    ES_MULTILINE, ES_WANTRETURN, GetDlgItem, GetDlgItemTextW, GetSystemMetrics, GetWindowTextLengthW,
    ICON_BIG, ICON_SMALL, IDCANCEL, IDOK, IMAGE_ICON, IsDialogMessageW, LR_SHARED, LoadImageW, MSG,
    PostMessageW, SM_CXICON, SM_CXSMICON, SM_CYICON, SM_CYSMICON, SW_RESTORE, SW_SHOW, SendDlgItemMessageW,
    SendMessageW, SetDlgItemTextW, SetForegroundWindow, ShowWindow, WM_APP, WM_COMMAND, WM_INITDIALOG,
    WM_NCDESTROY, WM_NEXTDLGCTL, WM_SETICON, WS_CAPTION, WS_CHILD, WS_EX_APPWINDOW, WS_EX_CLIENTEDGE,
    WS_POPUP, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
};

use super::{autostart, wide};
use crate::config::{self, Config, InsertMethod};
use crate::settings;

pub(super) const ID_HOTKEY: i32 = 100;
pub(super) const ID_MICROPHONE: i32 = 101;
pub(super) const ID_SOUNDS: i32 = 102;
pub(super) const ID_AUTOSTART: i32 = 103;
pub(super) const ID_INSERT: i32 = 104;
pub(super) const ID_RESTORE: i32 = 105;
pub(super) const ID_FILLERS: i32 = 106;
pub(super) const ID_COMMANDS: i32 = 107;
pub(super) const ID_SPACE: i32 = 108;
pub(super) const ID_VOCABULARY: i32 = 109;
pub(super) const ID_REPLACEMENTS: i32 = 110;
pub(super) const ID_OPEN_FILE: i32 = 111;

/// Posted by the thread that lists the microphones.
const WM_MICROPHONES: u32 = WM_APP + 10;

const TITLE: &str = "Dictum settings";

struct Dialog {
    hwnd: HWND,
    config_file: PathBuf,
    /// The file's settings when the window opened: only fields that differ get written.
    original: Config,
    autostart: bool,
    /// The `microphone` setting behind each drop-down entry.
    microphones: Vec<String>,
    found_microphones: Arc<Mutex<Option<Vec<String>>>>,
}

thread_local! {
    static DIALOG: RefCell<Option<Dialog>> = const { RefCell::new(None) };
    static SAVED: Cell<Option<bool>> = const { Cell::new(None) };
}

/// Opens the settings window, or brings it forward if it is already open.
pub fn open(config_file: &Path) {
    if let Some(hwnd) = window() {
        unsafe {
            ShowWindow(hwnd, SW_RESTORE);
            SetForegroundWindow(hwnd);
        }
        return;
    }
    let original = match Config::load_or_create(config_file) {
        Ok(config) => config,
        Err(e) => {
            log::error!("{e:#}");
            message(std::ptr::null_mut(), &format!("{e:#}\n\nOpening the file so you can fix it."));
            super::open(config_file);
            return;
        }
    };
    let (choices, _) = settings::microphone_choices(&original.microphone, None);
    DIALOG.with(|d| {
        *d.borrow_mut() = Some(Dialog {
            hwnd: std::ptr::null_mut(),
            config_file: config_file.to_path_buf(),
            original,
            autostart: autostart::is_enabled(),
            microphones: choices.into_iter().map(|(_, setting)| setting).collect(),
            found_microphones: Arc::default(),
        })
    });

    static INIT_CONTROLS: Once = Once::new();
    INIT_CONTROLS.call_once(|| {
        let icc = INITCOMMONCONTROLSEX {
            dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_STANDARD_CLASSES,
        };
        unsafe { InitCommonControlsEx(&icc) };
    });
    let template = template();
    let hwnd = unsafe {
        CreateDialogIndirectParamW(
            GetModuleHandleW(std::ptr::null()),
            template.as_ptr().cast(),
            std::ptr::null_mut(),
            Some(dialog_proc),
            0,
        )
    };
    if hwnd.is_null() {
        log::error!("failed to create the settings window; opening the file instead");
        DIALOG.with(|d| d.borrow_mut().take());
        super::open(config_file);
        return;
    }
    DIALOG.with(|d| {
        if let Some(dialog) = d.borrow_mut().as_mut() {
            dialog.hwnd = hwnd;
        }
    });
    unsafe {
        ShowWindow(hwnd, SW_SHOW);
        SetForegroundWindow(hwnd);
    }
}

/// The open settings window, if any.
pub fn window() -> Option<HWND> {
    DIALOG.with(|d| d.borrow().as_ref().map(|d| d.hwnd)).filter(|hwnd| !hwnd.is_null())
}

/// Lets the settings window handle keyboard navigation (Tab, Enter, Esc). Returns `true` if it
/// took the message, which must then not be dispatched again.
pub fn dialog_message(msg: &MSG) -> bool {
    window().is_some_and(|hwnd| unsafe { IsDialogMessageW(hwnd, msg) } != 0)
}

/// After the user saved: `Some(restart)`, once.
pub fn take_saved() -> Option<bool> {
    SAVED.with(Cell::take)
}

unsafe extern "system" fn dialog_proc(hwnd: HWND, msg: u32, wparam: WPARAM, _lparam: LPARAM) -> isize {
    // No `DIALOG` borrow may be held across calls that send messages back here.
    match msg {
        WM_INITDIALOG => {
            init(hwnd);
            1 // focus the first control
        }
        WM_MICROPHONES => {
            show_microphones(hwnd);
            1
        }
        WM_COMMAND => {
            let id = (wparam & 0xFFFF) as i32;
            let code = ((wparam >> 16) & 0xFFFF) as u32;
            match (id, code) {
                (IDOK, BN_CLICKED) => save(hwnd),
                (IDCANCEL, BN_CLICKED) => unsafe {
                    DestroyWindow(hwnd);
                },
                (ID_OPEN_FILE, BN_CLICKED) => {
                    if let Some(path) = DIALOG.with(|d| d.borrow().as_ref().map(|d| d.config_file.clone())) {
                        super::open(&path);
                    }
                }
                (ID_INSERT, CBN_SELCHANGE) => update_restore_enabled(hwnd),
                _ => return 0,
            }
            1
        }
        WM_NCDESTROY => {
            DIALOG.with(|d| d.borrow_mut().take());
            0
        }
        _ => 0,
    }
}

fn init(hwnd: HWND) {
    let Some((config, autostart, found)) = DIALOG.with(|d| {
        d.borrow().as_ref().map(|d| (d.original.clone(), d.autostart, d.found_microphones.clone()))
    }) else {
        return;
    };
    set_icons(hwnd);

    let presets: Vec<String> =
        settings::HOTKEY_PRESETS.iter().map(|spec| settings::hotkey_label(spec)).collect();
    fill_combo(hwnd, ID_HOTKEY, &presets, None);
    set_text(hwnd, ID_HOTKEY, &settings::hotkey_label(&config.hotkey));
    let (choices, selected) = settings::microphone_choices(&config.microphone, None);
    let labels: Vec<String> = choices.into_iter().map(|(label, _)| label).collect();
    fill_combo(hwnd, ID_MICROPHONE, &labels, Some(selected));
    check(hwnd, ID_SOUNDS, config.sounds);
    check(hwnd, ID_AUTOSTART, autostart);

    let methods = ["Pasting (fast; your clipboard is kept)", "Typing (never touches the clipboard)"];
    let methods: Vec<String> = methods.map(String::from).to_vec();
    let method = match config.insert_method {
        InsertMethod::Paste => 0,
        InsertMethod::Type => 1,
    };
    fill_combo(hwnd, ID_INSERT, &methods, Some(method));
    check(hwnd, ID_RESTORE, config.restore_clipboard);
    update_restore_enabled(hwnd);
    check(hwnd, ID_FILLERS, config.remove_fillers);
    check(hwnd, ID_COMMANDS, config.voice_commands);
    check(hwnd, ID_SPACE, config.trailing_space);
    set_text(hwnd, ID_VOCABULARY, &settings::vocabulary_text(&config.vocabulary));
    set_text(hwnd, ID_REPLACEMENTS, &settings::replacements_text(&config.replacements));

    // Listing devices can take a moment: keep it off the thread that runs the keyboard hook.
    let target = hwnd as usize;
    crate::audio::list_microphones(move |names| {
        *found.lock().unwrap() = Some(names);
        unsafe { PostMessageW(target as HWND, WM_MICROPHONES, 0, 0) };
    });
}

fn show_microphones(hwnd: HWND) {
    let selected = selection(hwnd, ID_MICROPHONE);
    let update = DIALOG.with(|d| {
        let mut d = d.borrow_mut();
        let dialog = d.as_mut()?;
        let devices = dialog.found_microphones.lock().unwrap().take()?;
        let current = selected.and_then(|i| dialog.microphones.get(i)).cloned().unwrap_or_default();
        let (choices, selected) = settings::microphone_choices(&current, Some(&devices));
        let (labels, values): (Vec<String>, Vec<String>) = choices.into_iter().unzip();
        dialog.microphones = values;
        Some((labels, selected))
    });
    if let Some((labels, selected)) = update {
        fill_combo(hwnd, ID_MICROPHONE, &labels, Some(selected));
    }
}

fn update_restore_enabled(hwnd: HWND) {
    let paste = selection(hwnd, ID_INSERT) == Some(0);
    unsafe { EnableWindow(GetDlgItem(hwnd, ID_RESTORE), paste.into()) };
}

fn save(hwnd: HWND) {
    let Some((path, original, autostart_was, microphones)) = DIALOG.with(|d| {
        d.borrow()
            .as_ref()
            .map(|d| (d.config_file.clone(), d.original.clone(), d.autostart, d.microphones.clone()))
    }) else {
        return;
    };
    let mut config = original.clone();
    config.hotkey = match settings::hotkey_spec(&text(hwnd, ID_HOTKEY), &original.hotkey) {
        Ok(spec) => spec,
        Err(e) => {
            let hint = "Pick one from the list, or type keys joined by “+”, like ctrl+shift+space.";
            return complain(hwnd, ID_HOTKEY, &format!("{e:#}\n\n{hint}"));
        }
    };
    config.microphone =
        selection(hwnd, ID_MICROPHONE).and_then(|i| microphones.get(i)).cloned().unwrap_or_default();
    config.sounds = checked(hwnd, ID_SOUNDS);
    config.insert_method =
        if selection(hwnd, ID_INSERT) == Some(1) { InsertMethod::Type } else { InsertMethod::Paste };
    config.restore_clipboard = checked(hwnd, ID_RESTORE);
    config.remove_fillers = checked(hwnd, ID_FILLERS);
    config.voice_commands = checked(hwnd, ID_COMMANDS);
    config.trailing_space = checked(hwnd, ID_SPACE);
    config.vocabulary = settings::parse_vocabulary(&text(hwnd, ID_VOCABULARY));
    config.replacements = match settings::parse_replacements(&text(hwnd, ID_REPLACEMENTS)) {
        Ok(replacements) => replacements,
        Err(e) => return complain(hwnd, ID_REPLACEMENTS, &format!("{e:#}")),
    };

    let autostart = checked(hwnd, ID_AUTOSTART);
    if autostart != autostart_was {
        if let Err(e) = autostart::set(autostart) {
            log::error!("{e:#}");
            return complain(hwnd, ID_AUTOSTART, &format!("Could not change “Start with Windows”: {e:#}"));
        }
        DIALOG.with(|d| {
            if let Some(dialog) = d.borrow_mut().as_mut() {
                dialog.autostart = autostart;
            }
        });
    }
    let restart = match config::save_changes(&path, &original, &config) {
        Ok(changed) => changed,
        Err(e) => {
            log::error!("{e:#}");
            return complain(hwnd, IDOK, &format!("Could not save the settings: {e:#}"));
        }
    };
    if restart {
        log::info!("settings saved");
    }
    SAVED.with(|s| s.set(Some(restart)));
    unsafe { DestroyWindow(hwnd) };
}

/// Explains what's wrong and puts the cursor on the field to fix.
fn complain(hwnd: HWND, id: i32, text: &str) {
    message(hwnd, text);
    unsafe { SendMessageW(hwnd, WM_NEXTDLGCTL, GetDlgItem(hwnd, id) as WPARAM, 1) };
}

#[cfg(not(test))]
fn message(owner: HWND, text: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONWARNING, MessageBoxW};
    unsafe { MessageBoxW(owner, wide(text).as_ptr(), wide("Dictum").as_ptr(), MB_ICONWARNING) };
}

// Tests can't click a modal message box away reliably: record the warnings instead.
#[cfg(test)]
thread_local! {
    pub(super) static WARNINGS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

#[cfg(test)]
fn message(_owner: HWND, text: &str) {
    WARNINGS.with(|w| w.borrow_mut().push(text.to_string()));
}

fn set_icons(hwnd: HWND) {
    let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
    for (kind, cx, cy) in [(ICON_BIG, SM_CXICON, SM_CYICON), (ICON_SMALL, SM_CXSMICON, SM_CYSMICON)] {
        // Resource 1 is the app icon (build.rs); test binaries don't have it.
        let icon = unsafe {
            LoadImageW(instance, 1 as _, IMAGE_ICON, GetSystemMetrics(cx), GetSystemMetrics(cy), LR_SHARED)
        };
        if !icon.is_null() {
            unsafe { SendMessageW(hwnd, WM_SETICON, kind as WPARAM, icon as LPARAM) };
        }
    }
}

pub(super) fn check(hwnd: HWND, id: i32, on: bool) {
    unsafe { CheckDlgButton(hwnd, id, if on { BST_CHECKED } else { BST_UNCHECKED }) };
}

pub(super) fn checked(hwnd: HWND, id: i32) -> bool {
    unsafe { IsDlgButtonChecked(hwnd, id) == BST_CHECKED }
}

pub(super) fn set_text(hwnd: HWND, id: i32, text: &str) {
    unsafe { SetDlgItemTextW(hwnd, id, wide(text).as_ptr()) };
}

pub(super) fn text(hwnd: HWND, id: i32) -> String {
    let len = unsafe { GetWindowTextLengthW(GetDlgItem(hwnd, id)) }.max(0) as usize;
    let mut buf = vec![0u16; len + 1];
    let copied = unsafe { GetDlgItemTextW(hwnd, id, buf.as_mut_ptr(), buf.len() as i32) } as usize;
    String::from_utf16_lossy(&buf[..copied.min(len)])
}

fn fill_combo(hwnd: HWND, id: i32, items: &[String], selected: Option<usize>) {
    unsafe {
        SendDlgItemMessageW(hwnd, id, CB_RESETCONTENT, 0, 0);
        for item in items {
            SendDlgItemMessageW(hwnd, id, CB_ADDSTRING, 0, wide(item).as_ptr() as LPARAM);
        }
        if let Some(index) = selected {
            SendDlgItemMessageW(hwnd, id, CB_SETCURSEL, index, 0);
        }
    }
}

pub(super) fn selection(hwnd: HWND, id: i32) -> Option<usize> {
    usize::try_from(unsafe { SendDlgItemMessageW(hwnd, id, CB_GETCURSEL, 0, 0) }).ok()
}

/// The layout, in dialog units (they scale with the font and the display's DPI).
fn template() -> Vec<u32> {
    let mut t = Template::new(TITLE, 440, 213);

    t.group("Dictation", (7, 7, 206, 80));
    t.label("&Hotkey:", (14, 21, 56, 8));
    t.combo(ID_HOTKEY, CBS_DROPDOWN | CBS_AUTOHSCROLL, (74, 19, 132, 100));
    t.label("&Microphone:", (14, 39, 56, 8));
    t.combo(ID_MICROPHONE, CBS_DROPDOWNLIST, (74, 37, 132, 120));
    t.check(ID_SOUNDS, "Play a sound when recording starts and stops", (14, 56, 192, 10));
    t.check(ID_AUTOSTART, "Start with Windows", (14, 70, 192, 10));

    t.group("Text", (7, 94, 206, 89));
    t.label("&Insert text by:", (14, 108, 56, 8));
    t.combo(ID_INSERT, CBS_DROPDOWNLIST, (74, 106, 132, 60));
    t.check(ID_RESTORE, "Put the clipboard back after pasting", (14, 125, 192, 10));
    t.check(ID_FILLERS, "Remove filler words (um, uh)", (14, 139, 192, 10));
    t.check(ID_COMMANDS, "Voice commands: “new line”, “new paragraph”", (14, 153, 192, 10));
    t.check(ID_SPACE, "Add a space after each dictation", (14, 167, 192, 10));

    t.group("Vocabulary", (220, 7, 213, 176));
    t.label(
        "&Words and names recognition tends to get wrong, one per line, spelled the way you want them:",
        (227, 19, 199, 16),
    );
    t.edit(ID_VOCABULARY, (227, 37, 199, 72));
    t.label("&Replacements for anything else, one per line:  heard = written", (227, 116, 199, 8));
    t.edit(ID_REPLACEMENTS, (227, 127, 199, 49));

    t.button(ID_OPEN_FILE, "&Open config file", (7, 192, 70, 14), false);
    t.label("Saving restarts Dictum to apply the changes.", (84, 195, 200, 8));
    t.button(IDOK, "Save", (329, 192, 50, 14), true);
    t.button(IDCANCEL, "Cancel", (383, 192, 50, 14), false);
    t.finish()
}

type Rect = (i16, i16, i16, i16);

/// An in-memory DLGTEMPLATEEX.
struct Template {
    words: Vec<u16>,
    items: u16,
}

impl Template {
    const BUTTON: u16 = 0x0080;
    const EDIT: u16 = 0x0081;
    const STATIC: u16 = 0x0082;
    const COMBOBOX: u16 = 0x0085;

    fn new(title: &str, cx: i16, cy: i16) -> Self {
        let style = WS_POPUP | WS_CAPTION | WS_SYSMENU | (DS_MODALFRAME | DS_SETFONT | DS_CENTER) as u32;
        let mut t = Self { words: vec![1, 0xFFFF], items: 0 }; // version, signature
        t.dword(0); // help ID
        t.dword(WS_EX_APPWINDOW);
        t.dword(style);
        t.words.push(0); // item count, filled in by `finish`
        t.words.extend([0, 0, cx as u16, cy as u16]);
        t.words.extend([0, 0]); // no menu, default window class
        t.string(title);
        t.words.extend([9, 400, 1 << 8]); // 9 pt, normal weight, not italic, DEFAULT_CHARSET
        t.string("Segoe UI");
        t
    }

    fn group(&mut self, text: &str, rect: Rect) {
        self.item(Self::BUTTON, text, -1, BS_GROUPBOX as u32, 0, rect);
    }

    fn label(&mut self, text: &str, rect: Rect) {
        self.item(Self::STATIC, text, -1, 0, 0, rect);
    }

    fn combo(&mut self, id: i32, style: i32, rect: Rect) {
        self.item(Self::COMBOBOX, "", id, style as u32 | WS_VSCROLL | WS_TABSTOP, 0, rect);
    }

    fn check(&mut self, id: i32, text: &str, rect: Rect) {
        self.item(Self::BUTTON, text, id, BS_AUTOCHECKBOX as u32 | WS_TABSTOP, 0, rect);
    }

    fn edit(&mut self, id: i32, rect: Rect) {
        let style = (ES_MULTILINE | ES_AUTOVSCROLL | ES_WANTRETURN) as u32 | WS_VSCROLL | WS_TABSTOP;
        self.item(Self::EDIT, "", id, style, WS_EX_CLIENTEDGE, rect);
    }

    fn button(&mut self, id: i32, text: &str, rect: Rect, default: bool) {
        let style = if default { BS_DEFPUSHBUTTON } else { BS_PUSHBUTTON } as u32 | WS_TABSTOP;
        self.item(Self::BUTTON, text, id, style, 0, rect);
    }

    fn item(&mut self, class: u16, text: &str, id: i32, style: u32, ex_style: u32, (x, y, cx, cy): Rect) {
        if self.words.len() % 2 == 1 {
            self.words.push(0); // items start on a DWORD boundary
        }
        self.dword(0); // help ID
        self.dword(ex_style);
        self.dword(WS_CHILD | WS_VISIBLE | style);
        self.words.extend([x as u16, y as u16, cx as u16, cy as u16]);
        self.dword(id as u32);
        self.words.extend([0xFFFF, class]);
        self.string(text);
        self.words.push(0); // no creation data
        self.items += 1;
    }

    fn dword(&mut self, value: u32) {
        self.words.extend([value as u16, (value >> 16) as u16]);
    }

    fn string(&mut self, s: &str) {
        self.words.extend(s.encode_utf16().chain(Some(0)));
    }

    /// The template in a DWORD-aligned buffer, as `CreateDialogIndirectParamW` requires.
    fn finish(mut self) -> Vec<u32> {
        self.words[8] = self.items;
        self.words
            .chunks(2)
            .map(|pair| u32::from(pair[0]) | u32::from(pair.get(1).copied().unwrap_or(0)) << 16)
            .collect()
    }
}
