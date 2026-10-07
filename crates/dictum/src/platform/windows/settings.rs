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
    CB_RESETCONTENT, CB_SETCURSEL, CBN_EDITCHANGE, CBN_SELCHANGE, CBS_AUTOHSCROLL, CBS_DROPDOWN,
    CBS_DROPDOWNLIST, CreateDialogIndirectParamW, DS_CENTER, DS_MODALFRAME, DS_SETFONT, DestroyWindow,
    ES_AUTOVSCROLL, ES_MULTILINE, ES_WANTRETURN, GetDlgItem, GetDlgItemTextW, GetSystemMetrics,
    GetWindowTextLengthW, ICON_BIG, ICON_SMALL, IDCANCEL, IDOK, IMAGE_ICON, IsDialogMessageW, LR_SHARED,
    LoadImageW, MSG, PostMessageW, SM_CXICON, SM_CXSMICON, SM_CYICON, SM_CYSMICON, SW_RESTORE, SW_SHOW,
    SendDlgItemMessageW, SendMessageW, SetDlgItemTextW, SetForegroundWindow, ShowWindow, WM_APP, WM_COMMAND,
    WM_INITDIALOG, WM_NCDESTROY, WM_NEXTDLGCTL, WM_SETICON, WS_CAPTION, WS_CHILD, WS_EX_APPWINDOW,
    WS_EX_CLIENTEDGE, WS_POPUP, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
};

use super::{autostart, wide};
use crate::config::Config;
use crate::settings::{self, Control, Field, Form, Section, Value};

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
pub(super) const ID_INDICATOR: i32 = 112;
pub(super) const ID_STUTTERS: i32 = 113;
/// The "Controls" lines, one per `settings::controls_help` line.
pub(super) const ID_CONTROLS: [i32; 3] = [114, 115, 116];

/// Posted by the thread that lists the microphones.
const WM_MICROPHONES: u32 = WM_APP + 10;

/// The control of each field. Every field must be placed in [`template`] too.
fn id(field: Field) -> i32 {
    match field {
        Field::Hotkey => ID_HOTKEY,
        Field::Microphone => ID_MICROPHONE,
        Field::Indicator => ID_INDICATOR,
        Field::Sounds => ID_SOUNDS,
        Field::Autostart => ID_AUTOSTART,
        Field::InsertMethod => ID_INSERT,
        Field::RestoreClipboard => ID_RESTORE,
        Field::RemoveFillers => ID_FILLERS,
        Field::RemoveStutters => ID_STUTTERS,
        Field::VoiceCommands => ID_COMMANDS,
        Field::TrailingSpace => ID_SPACE,
        Field::Vocabulary => ID_VOCABULARY,
        Field::Replacements => ID_REPLACEMENTS,
    }
}

fn fields() -> impl Iterator<Item = Field> {
    Section::ALL.into_iter().flat_map(|s| s.fields().iter().copied())
}

struct Dialog {
    hwnd: HWND,
    config_file: PathBuf,
    form: Form,
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
    DIALOG.with(|d| {
        *d.borrow_mut() = Some(Dialog {
            hwnd: std::ptr::null_mut(),
            config_file: config_file.to_path_buf(),
            form: Form::new(original, autostart::is_enabled()),
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
            let control = (wparam & 0xFFFF) as i32;
            let code = ((wparam >> 16) & 0xFFFF) as u32;
            match (control, code) {
                (IDOK, BN_CLICKED) => save(hwnd),
                (IDCANCEL, BN_CLICKED) => unsafe {
                    DestroyWindow(hwnd);
                },
                (ID_OPEN_FILE, BN_CLICKED) => {
                    if let Some(path) = DIALOG.with(|d| d.borrow().as_ref().map(|d| d.config_file.clone())) {
                        super::open(&path);
                    }
                }
                // The edit text isn't updated yet when the selection changes: use the preset.
                (ID_HOTKEY, CBN_SELCHANGE) => {
                    let preset = selection(hwnd, ID_HOTKEY).and_then(|i| settings::HOTKEY_PRESETS.get(i));
                    if let Some(preset) = preset {
                        refresh(hwnd, Some(&settings::hotkey_label(preset)));
                    }
                }
                (_, CBN_SELCHANGE | CBN_EDITCHANGE | BN_CLICKED) if fields().any(|f| id(f) == control) => {
                    refresh(hwnd, None)
                }
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
    let Some((initial, found)) = DIALOG.with(|d| {
        d.borrow().as_ref().map(|d| {
            let initial: Vec<_> = fields().map(|f| (f, d.form.options(f), d.form.initial(f))).collect();
            (initial, d.found_microphones.clone())
        })
    }) else {
        return;
    };
    set_icons(hwnd);
    for (field, options, value) in initial {
        if let Control::Choice { .. } = field.control() {
            fill_combo(hwnd, id(field), &options, None);
        }
        show(hwnd, field, &value);
    }
    refresh(hwnd, None);

    // Listing devices can take a moment: keep it off the thread that runs the keyboard hook.
    let target = hwnd as usize;
    crate::audio::list_microphones(move |names| {
        *found.lock().unwrap() = Some(names);
        unsafe { PostMessageW(target as HWND, WM_MICROPHONES, 0, 0) };
    });
}

/// What a field's control holds.
fn value(hwnd: HWND, field: Field) -> Value {
    match field.control() {
        Control::Choice { editable: true, .. } | Control::Lines { .. } => Value::Text(text(hwnd, id(field))),
        Control::Choice { editable: false, .. } => Value::Choice(selection(hwnd, id(field))),
        Control::Check { .. } => Value::Check(checked(hwnd, id(field))),
    }
}

fn show(hwnd: HWND, field: Field, value: &Value) {
    match value {
        Value::Text(text) => set_text(hwnd, id(field), text),
        Value::Choice(selected) => {
            let index = selected.unwrap_or(usize::MAX); // CB_SETCURSEL with -1 clears it
            unsafe { SendDlgItemMessageW(hwnd, id(field), CB_SETCURSEL, index, 0) };
        }
        Value::Check(on) => check(hwnd, id(field), *on),
    }
}

fn show_microphones(hwnd: HWND) {
    let selected = selection(hwnd, ID_MICROPHONE);
    let update = DIALOG.with(|d| {
        let mut d = d.borrow_mut();
        let dialog = d.as_mut()?;
        let devices = dialog.found_microphones.lock().unwrap().take()?;
        Some(dialog.form.devices_found(&devices, selected))
    });
    if let Some((labels, selected)) = update {
        fill_combo(hwnd, ID_MICROPHONE, &labels, Some(selected));
    }
}

/// Updates what depends on other fields: which can be changed, and the "Controls" lines (left as
/// they are while the hotkey is still being typed). `hotkey` stands in for the hotkey field's
/// text, which isn't updated yet while its selection changes.
fn refresh(hwnd: HWND, hotkey: Option<&str>) {
    for field in fields() {
        let enabled = Form::enabled(field, |f| value(hwnd, f));
        unsafe { EnableWindow(GetDlgItem(hwnd, id(field)), enabled.into()) };
    }
    let hotkey = hotkey.map_or_else(|| text(hwnd, ID_HOTKEY), String::from);
    let lines = DIALOG.with(|d| d.borrow().as_ref().and_then(|d| d.form.controls(&hotkey)));
    for (id, line) in ID_CONTROLS.into_iter().zip(lines.into_iter().flatten()) {
        set_text(hwnd, id, &line);
    }
}

fn save(hwnd: HWND) {
    // Read the controls before borrowing `DIALOG`: no borrow may be held while messages are sent.
    let values: Vec<(Field, Value)> = fields().map(|f| (f, value(hwnd, f))).collect();
    let value = |field: Field| values.iter().find(|(f, _)| *f == field).map(|(_, v)| v.clone()).unwrap();
    let saved = DIALOG.with(|d| {
        let mut d = d.borrow_mut();
        let dialog = d.as_mut()?;
        Some(
            dialog
                .form
                .read(value)
                .and_then(|changes| dialog.form.save(&dialog.config_file, &changes, autostart::set)),
        )
    });
    match saved {
        None => {}
        Some(Err(problem)) => complain(hwnd, problem.field.map_or(IDOK, id), &problem.message),
        Some(Ok(restart)) => {
            SAVED.with(|s| s.set(Some(restart)));
            unsafe { DestroyWindow(hwnd) };
        }
    }
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
    let mut t = Template::new(settings::TITLE, 440, 306);

    for section in Section::ALL {
        let group = match section {
            Section::Dictation => (7, 7, 206, 94),
            Section::Text => (7, 108, 206, 103),
            Section::Vocabulary => (220, 7, 213, 204),
        };
        t.group(section.title(), group);
        for &field in section.fields() {
            let (label, control) = place(field);
            match field.control() {
                Control::Choice { label: text, editable } => {
                    t.label(text, label);
                    let style = if editable { CBS_DROPDOWN | CBS_AUTOHSCROLL } else { CBS_DROPDOWNLIST };
                    t.combo(id(field), style, control);
                }
                Control::Check { label: text } => t.check(id(field), text, control),
                Control::Lines { label: text } => {
                    t.label(text, label);
                    t.edit(id(field), control);
                }
            }
        }
    }

    t.group(settings::CONTROLS_TITLE, (7, 218, 426, 58));
    for (id, y) in ID_CONTROLS.into_iter().zip([231, 242, 253]) {
        t.text(id, "", (14, y, 412, 8));
    }
    t.label(settings::CONTROLS_NOTE, (14, 264, 412, 8));

    t.button(ID_OPEN_FILE, settings::OPEN_FILE, (7, 285, 70, 14), false);
    t.label(settings::SAVE_NOTE, (84, 288, 200, 8));
    t.button(IDOK, settings::SAVE, (329, 285, 50, 14), true);
    t.button(IDCANCEL, settings::CANCEL, (383, 285, 50, 14), false);
    t.finish()
}

/// Where each field goes: its label and its control. Checkboxes carry their own text, so their
/// label rectangle is unused.
fn place(field: Field) -> (Rect, Rect) {
    let check = |y| ((0, 0, 0, 0), (14, y, 192, 10));
    let choice = |y, drop_down| ((14, y + 2, 56, 8), (74, y, 132, drop_down));
    match field {
        Field::Hotkey => choice(19, 100),
        Field::Microphone => choice(37, 120),
        Field::Indicator => check(56),
        Field::Sounds => check(70),
        Field::Autostart => check(84),
        Field::InsertMethod => choice(120, 60),
        Field::RestoreClipboard => check(139),
        Field::RemoveFillers => check(153),
        Field::RemoveStutters => check(167),
        Field::VoiceCommands => check(181),
        Field::TrailingSpace => check(195),
        Field::Vocabulary => ((227, 19, 199, 16), (227, 37, 199, 79)),
        Field::Replacements => ((227, 123, 199, 8), (227, 134, 199, 70)),
    }
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
        self.text(-1, text, rect);
    }

    /// A label the code fills in or changes later.
    fn text(&mut self, id: i32, text: &str, rect: Rect) {
        self.item(Self::STATIC, text, id, 0, 0, rect);
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
