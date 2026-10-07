//! The settings window, drawn from the shared form in `crate::settings`: sections become columns
//! of labelled controls, so a field added there shows up here without changes. Lives on the main
//! thread. Saving writes only what changed into config.toml (its comments stay) and restarts
//! Dictum to apply it.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject, Sel};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAlert, NSAlertStyle, NSApplication, NSBackingStoreType, NSBorderType, NSButton, NSColor, NSComboBox,
    NSComboBoxDelegate, NSControlStateValueOff, NSControlStateValueOn, NSControlTextEditingDelegate, NSFont,
    NSPopUpButton, NSTextAlignment, NSTextField, NSTextFieldDelegate, NSTextView, NSView, NSWindow,
    NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{NSArray, NSNotification, NSPoint, NSRect, NSSize, NSString};

use super::autostart;
use crate::config::Config;
use crate::settings::{self, Control, Field, Form, Section, Value, plain};

const MARGIN: f64 = 20.0;
const COLUMN: f64 = 390.0;
const COLUMN_GAP: f64 = 32.0;
const LABEL_WIDTH: f64 = 100.0;
const SECTION_GAP: f64 = 22.0;
const MIN_TEXT_HEIGHT: f64 = 90.0;

enum Widget {
    Combo(Retained<NSComboBox>),
    Popup(Retained<NSPopUpButton>),
    Check(Retained<NSButton>),
    Text(Retained<NSTextView>),
}

impl Widget {
    fn value(&self) -> Value {
        match self {
            Widget::Combo(combo) => Value::Text(combo.stringValue().to_string()),
            Widget::Popup(popup) => Value::Choice(usize::try_from(popup.indexOfSelectedItem()).ok()),
            Widget::Check(check) => Value::Check(check.state() == NSControlStateValueOn),
            Widget::Text(text) => Value::Text(text.string().to_string()),
        }
    }

    fn show(&self, value: &Value) {
        match (self, value) {
            (Widget::Combo(combo), Value::Text(text)) => combo.setStringValue(&NSString::from_str(text)),
            (Widget::Popup(popup), Value::Choice(Some(i))) => popup.selectItemAtIndex(*i as isize),
            (Widget::Check(check), Value::Check(on)) => {
                check.setState(if *on { NSControlStateValueOn } else { NSControlStateValueOff })
            }
            (Widget::Text(view), Value::Text(text)) => view.setString(&NSString::from_str(text)),
            _ => {}
        }
    }

    fn set_options(&self, options: &[String]) {
        let titles: Vec<Retained<NSString>> = options.iter().map(|o| NSString::from_str(o)).collect();
        match self {
            Widget::Combo(combo) => {
                combo.removeAllItems();
                for title in &titles {
                    // SAFETY: combo boxes show string values as they are.
                    unsafe { combo.addItemWithObjectValue(title) };
                }
            }
            Widget::Popup(popup) => {
                popup.removeAllItems();
                popup.addItemsWithTitles(&NSArray::from_retained_slice(&titles));
            }
            _ => {}
        }
    }

    fn set_enabled(&self, enabled: bool) {
        match self {
            Widget::Combo(combo) => combo.setEnabled(enabled),
            Widget::Popup(popup) => popup.setEnabled(enabled),
            Widget::Check(check) => check.setEnabled(enabled),
            Widget::Text(text) => text.setEditable(enabled),
        }
    }

    /// The view that takes keyboard focus.
    fn view(&self) -> &NSView {
        match self {
            Widget::Combo(combo) => combo,
            Widget::Popup(popup) => popup,
            Widget::Check(check) => check,
            Widget::Text(text) => text,
        }
    }
}

struct SettingsWindow {
    window: Retained<NSWindow>,
    _controller: Retained<Controller>,
    widgets: Vec<(Field, Widget)>,
    controls: Vec<Retained<NSTextField>>,
    config_file: PathBuf,
    form: Form,
}

impl SettingsWindow {
    fn widget(&self, field: Field) -> &Widget {
        &self.widgets.iter().find(|(f, _)| *f == field).expect("every field has a widget").1
    }

    fn value(&self, field: Field) -> Value {
        self.widget(field).value()
    }
}

thread_local! {
    static OPEN: RefCell<Option<SettingsWindow>> = const { RefCell::new(None) };
}

define_class!(
    /// Receives the window's events: button actions, field changes and closing.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "DictumSettingsController"]
    struct Controller;

    unsafe impl NSObjectProtocol for Controller {}

    unsafe impl NSWindowDelegate for Controller {
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _notification: &NSNotification) {
            // Let AppKit finish closing before the window is released.
            super::main_async(|| {
                OPEN.with(|o| o.borrow_mut().take());
            });
        }
    }

    unsafe impl NSControlTextEditingDelegate for Controller {
        #[unsafe(method(controlTextDidChange:))]
        fn text_did_change(&self, _notification: &NSNotification) {
            refresh(None);
        }
    }

    unsafe impl NSTextFieldDelegate for Controller {}

    unsafe impl NSComboBoxDelegate for Controller {
        // The text isn't updated yet when the selection changes: use the preset.
        #[unsafe(method(comboBoxSelectionDidChange:))]
        fn combo_selection_did_change(&self, notification: &NSNotification) {
            let combo = notification.object().and_then(|o| o.downcast::<NSComboBox>().ok());
            let preset = combo
                .and_then(|c| usize::try_from(c.indexOfSelectedItem()).ok())
                .and_then(|i| settings::HOTKEY_PRESETS.get(i));
            if let Some(preset) = preset {
                refresh(Some(&settings::hotkey_label(preset)));
            }
        }
    }

    impl Controller {
        #[unsafe(method(dictumChanged:))]
        fn changed(&self, _sender: Option<&AnyObject>) {
            refresh(None);
        }

        #[unsafe(method(dictumSave:))]
        fn save(&self, _sender: Option<&AnyObject>) {
            save();
        }

        #[unsafe(method(dictumCancel:))]
        fn cancel(&self, _sender: Option<&AnyObject>) {
            close();
        }

        #[unsafe(method(dictumOpenFile:))]
        fn open_file(&self, _sender: Option<&AnyObject>) {
            if let Some(path) = OPEN.with(|o| o.borrow().as_ref().map(|s| s.config_file.clone())) {
                super::open(&path, true);
            }
        }
    }
);

impl Controller {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        unsafe { msg_send![super(this), init] }
    }
}

define_class!(
    /// A view with the origin at the top left, so the layout reads top to bottom.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "DictumFlippedView"]
    struct FlippedView;

    impl FlippedView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }
    }
);

impl FlippedView {
    fn new(mtm: MainThreadMarker, frame: NSRect) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }
}

/// Opens the settings window, or brings it forward if it is already open.
pub fn open(config_file: &Path) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    activate(mtm);
    if let Some(window) = OPEN.with(|o| o.borrow().as_ref().map(|s| s.window.clone())) {
        window.makeKeyAndOrderFront(None);
        return;
    }
    let original = match Config::load_or_create(config_file) {
        Ok(config) => config,
        Err(e) => {
            log::error!("{e:#}");
            alert(mtm, &format!("{e:#}\n\nOpening the file so you can fix it."));
            super::open(config_file, true);
            return;
        }
    };
    let form = Form::new(original, autostart::is_enabled());
    let settings = build(mtm, form, config_file.to_path_buf());
    let window = settings.window.clone();
    OPEN.with(|o| *o.borrow_mut() = Some(settings));
    refresh(None);

    // Listing devices can take a moment: keep it off the main thread.
    crate::audio::list_microphones(|names| super::main_async(move || show_microphones(&names)));
    window.center();
    window.makeKeyAndOrderFront(None);
}

fn activate(mtm: MainThreadMarker) {
    let app = NSApplication::sharedApplication(mtm);
    if app.respondsToSelector(sel!(activate)) {
        app.activate();
    } else {
        #[allow(deprecated)]
        app.activateIgnoringOtherApps(true);
    }
}

fn close() {
    if let Some(window) = OPEN.with(|o| o.borrow().as_ref().map(|s| s.window.clone())) {
        window.close();
    }
}

fn show_microphones(devices: &[String]) {
    OPEN.with(|o| {
        let mut o = o.borrow_mut();
        let Some(settings) = o.as_mut() else { return };
        let selected = settings.value(Field::Microphone);
        let selected = if let Value::Choice(selected) = selected { selected } else { None };
        let (labels, selected) = settings.form.devices_found(devices, selected);
        let widget = settings.widget(Field::Microphone);
        widget.set_options(&labels);
        widget.show(&Value::Choice(Some(selected)));
    });
}

/// Updates what depends on other fields: which can be changed, and the "Controls" lines (left as
/// they are while the hotkey is still being typed). `hotkey` stands in for the hotkey field's
/// text, which isn't updated yet while its selection changes.
fn refresh(hotkey: Option<&str>) {
    OPEN.with(|o| {
        let Ok(o) = o.try_borrow() else { return };
        let Some(settings) = o.as_ref() else { return };
        for (field, widget) in &settings.widgets {
            widget.set_enabled(Form::enabled(*field, |f| settings.value(f)));
        }
        let hotkey = hotkey.map_or_else(|| settings.value(Field::Hotkey), |h| Value::Text(h.to_string()));
        let Value::Text(hotkey) = hotkey else { return };
        if let Some(lines) = settings.form.controls(&hotkey) {
            for (label, line) in settings.controls.iter().zip(lines) {
                label.setStringValue(&NSString::from_str(&line));
            }
        }
    });
}

fn save() {
    let Some(mtm) = MainThreadMarker::new() else { return };
    let saved = OPEN.with(|o| {
        let mut o = o.borrow_mut();
        let settings = o.as_mut()?;
        let values: Vec<(Field, Value)> = settings.widgets.iter().map(|(f, w)| (*f, w.value())).collect();
        let value = |field: Field| values.iter().find(|(f, _)| *f == field).map(|(_, v)| v.clone()).unwrap();
        let result = settings
            .form
            .read(value)
            .and_then(|changes| settings.form.save(&settings.config_file, &changes, autostart::set));
        Some((result, settings.window.clone()))
    });
    let Some((result, window)) = saved else { return };
    match result {
        Ok(restart) => {
            window.close();
            super::settings_saved(restart);
        }
        // No borrow is held here: the alert runs a modal loop that may call back into us.
        Err(problem) => {
            alert(mtm, &problem.message);
            let focus = problem.field.and_then(|field| {
                OPEN.with(|o| o.borrow().as_ref().map(|s| Retained::from(s.widget(field).view())))
            });
            if let Some(view) = focus {
                window.makeFirstResponder(Some(&view));
            }
        }
    }
}

fn alert(mtm: MainThreadMarker, message: &str) {
    let (title, details) = message.split_once("\n\n").unwrap_or((message, ""));
    let alert = NSAlert::new(mtm);
    alert.setAlertStyle(NSAlertStyle::Warning);
    alert.setMessageText(&NSString::from_str(title));
    alert.setInformativeText(&NSString::from_str(details));
    alert.runModal();
}

/// Lays out labels and controls top to bottom in a flipped view.
struct Builder<'a> {
    mtm: MainThreadMarker,
    view: &'a NSView,
    target: &'a Controller,
}

impl Builder<'_> {
    fn add(&self, view: &NSView, frame: NSRect) {
        view.setFrame(frame);
        self.view.addSubview(view);
    }

    fn label(&self, text: &str, frame: NSRect) -> Retained<NSTextField> {
        let label = NSTextField::labelWithString(&NSString::from_str(text), self.mtm);
        self.add(&label, frame);
        label
    }

    /// A label wrapped to the column; returns its height.
    fn wrapping_label(&self, text: &str, origin: NSPoint) -> f64 {
        let label = NSTextField::wrappingLabelWithString(&NSString::from_str(text), self.mtm);
        label.setPreferredMaxLayoutWidth(COLUMN);
        let height = label.fittingSize().height.ceil();
        self.add(&label, NSRect::new(origin, NSSize::new(COLUMN, height)));
        height
    }

    fn heading(&self, text: &str, x: f64, y: f64, width: f64) {
        let label = self.label(text, rect(x, y, width, 17.0));
        label.setFont(Some(&NSFont::boldSystemFontOfSize(13.0)));
    }

    fn note(&self, text: &str, frame: NSRect) -> Retained<NSTextField> {
        let label = self.label(text, frame);
        label.setFont(Some(&NSFont::systemFontOfSize(NSFont::smallSystemFontSize())));
        label.setTextColor(Some(&NSColor::secondaryLabelColor()));
        label
    }

    fn button(&self, title: &str, action: Sel) -> Retained<NSButton> {
        let target: &AnyObject = self.target.as_ref();
        unsafe {
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str(title),
                Some(target),
                Some(action),
                self.mtm,
            )
        }
    }

    /// Places one field at `y` in the column at `x`; returns its height. `text_height` is the
    /// height for multi-line text.
    fn field(&self, field: Field, x: f64, y: f64, text_height: f64) -> (Widget, f64) {
        let target: &AnyObject = self.target.as_ref();
        match field.control() {
            Control::Choice { label, editable } => {
                let text = self.label(&plain(label), rect(x, y + 4.0, LABEL_WIDTH - 8.0, 17.0));
                text.setAlignment(NSTextAlignment::Right);
                let frame = rect(x + LABEL_WIDTH, y, COLUMN - LABEL_WIDTH, 25.0);
                let widget = if editable {
                    let combo = NSComboBox::initWithFrame(NSComboBox::alloc(self.mtm), frame);
                    combo.setCompletes(false);
                    combo.setNumberOfVisibleItems(8);
                    unsafe { combo.setDelegate(Some(ProtocolObject::from_ref(self.target))) };
                    self.view.addSubview(&combo);
                    Widget::Combo(combo)
                } else {
                    let popup =
                        NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(self.mtm), frame, false);
                    unsafe {
                        popup.setTarget(Some(target));
                        popup.setAction(Some(sel!(dictumChanged:)));
                    }
                    self.view.addSubview(&popup);
                    Widget::Popup(popup)
                };
                (widget, 32.0)
            }
            Control::Check { label } => {
                let check = unsafe {
                    NSButton::checkboxWithTitle_target_action(
                        &NSString::from_str(&plain(label)),
                        Some(target),
                        Some(sel!(dictumChanged:)),
                        self.mtm,
                    )
                };
                self.add(&check, rect(x, y, COLUMN, 18.0));
                (Widget::Check(check), 24.0)
            }
            Control::Lines { label } => {
                let label_height = self.wrapping_label(&plain(label), NSPoint::new(x, y));
                let scroll = NSTextView::scrollableTextView(self.mtm);
                scroll.setBorderType(NSBorderType::BezelBorder);
                self.add(&scroll, rect(x, y + label_height + 6.0, COLUMN, text_height));
                let text = scroll
                    .documentView()
                    .and_then(|v| v.downcast::<NSTextView>().ok())
                    .expect("a scrollable text view holds a text view");
                // Plain text as typed: no smart quotes, dashes or corrections in terms and rules.
                text.setRichText(false);
                text.setFont(Some(&NSFont::systemFontOfSize(13.0)));
                text.setAutomaticQuoteSubstitutionEnabled(false);
                text.setAutomaticDashSubstitutionEnabled(false);
                text.setAutomaticTextReplacementEnabled(false);
                text.setAutomaticSpellingCorrectionEnabled(false);
                (Widget::Text(text), label_height + 6.0 + text_height + 14.0)
            }
        }
    }
}

fn rect(x: f64, y: f64, width: f64, height: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(width, height))
}

/// Heights of the fields in a section, given the height of each multi-line text.
fn section_height(mtm: MainThreadMarker, section: Section, text_height: f64) -> f64 {
    let fields: f64 = section
        .fields()
        .iter()
        .map(|f| match f.control() {
            Control::Choice { .. } => 32.0,
            Control::Check { .. } => 24.0,
            Control::Lines { label } => wrapped_height(mtm, &plain(label)) + 6.0 + text_height + 14.0,
        })
        .sum();
    25.0 + fields
}

/// Height of `text` wrapped to a column.
fn wrapped_height(mtm: MainThreadMarker, text: &str) -> f64 {
    let label = NSTextField::wrappingLabelWithString(&NSString::from_str(text), mtm);
    label.setPreferredMaxLayoutWidth(COLUMN);
    label.fittingSize().height.ceil()
}

/// Sections with multi-line text go in the right column, the others stack on the left.
fn has_text(section: Section) -> bool {
    section.fields().iter().any(|f| matches!(f.control(), Control::Lines { .. }))
}

fn build(mtm: MainThreadMarker, form: Form, config_file: PathBuf) -> SettingsWindow {
    let left: Vec<Section> = Section::ALL.into_iter().filter(|s| !has_text(*s)).collect();
    let right: Vec<Section> = Section::ALL.into_iter().filter(|s| has_text(*s)).collect();
    let column_height = |sections: &[Section], text_height: f64| -> f64 {
        sections.iter().map(|s| section_height(mtm, *s, text_height)).sum::<f64>()
            + SECTION_GAP * sections.len().saturating_sub(1) as f64
    };
    // Multi-line texts share whatever height the left column leaves them.
    let texts = right
        .iter()
        .flat_map(|s| s.fields())
        .filter(|f| matches!(f.control(), Control::Lines { .. }))
        .count();
    let fixed_right = column_height(&right, 0.0);
    let text_height = ((column_height(&left, 0.0) - fixed_right) / texts.max(1) as f64).max(MIN_TEXT_HEIGHT);
    let columns_height = column_height(&left, 0.0).max(column_height(&right, text_height));

    let width = MARGIN * 2.0 + COLUMN * 2.0 + COLUMN_GAP;
    let controls_top = MARGIN + columns_height + SECTION_GAP;
    let buttons_top = controls_top + 25.0 + 3.0 * 20.0 + 18.0 + SECTION_GAP;
    let height = buttons_top + 32.0 + MARGIN;

    let content = FlippedView::new(mtm, rect(0.0, 0.0, width, height));
    let controller = Controller::new(mtm);
    let b = Builder { mtm, view: &content, target: &controller };

    let mut widgets = Vec::new();
    for (column, sections) in [(0.0, &left), (1.0, &right)] {
        let x = MARGIN + column * (COLUMN + COLUMN_GAP);
        let mut y = MARGIN;
        for section in sections.iter() {
            b.heading(section.title(), x, y, COLUMN);
            y += 25.0;
            for &field in section.fields() {
                let (widget, used) = b.field(field, x, y, text_height);
                if let Control::Choice { .. } = field.control() {
                    widget.set_options(&form.options(field));
                }
                widget.show(&form.initial(field));
                widgets.push((field, widget));
                y += used;
            }
            y += SECTION_GAP;
        }
    }

    let full = width - 2.0 * MARGIN;
    b.heading(settings::CONTROLS_TITLE, MARGIN, controls_top, full);
    let controls = (0..3)
        .map(|i| b.label("", rect(MARGIN, controls_top + 25.0 + 20.0 * f64::from(i), full, 17.0)))
        .collect();
    b.note(settings::CONTROLS_NOTE, rect(MARGIN, controls_top + 25.0 + 60.0, full, 16.0));

    let open_file = b.button(&plain(settings::OPEN_FILE), sel!(dictumOpenFile:));
    open_file.sizeToFit();
    let open_width = open_file.frame().size.width;
    b.add(&open_file, rect(MARGIN, buttons_top, open_width, 32.0));
    b.note(settings::SAVE_NOTE, rect(MARGIN + open_width + 10.0, buttons_top + 8.0, 300.0, 16.0));
    let save = b.button(settings::SAVE, sel!(dictumSave:));
    save.setKeyEquivalent(&NSString::from_str("\r"));
    let cancel = b.button(settings::CANCEL, sel!(dictumCancel:));
    cancel.setKeyEquivalent(&NSString::from_str("\u{1b}"));
    let button_width = 96.0;
    b.add(&save, rect(width - MARGIN - button_width, buttons_top, button_width, 32.0));
    b.add(&cancel, rect(width - MARGIN - 2.0 * button_width - 10.0, buttons_top, button_width, 32.0));

    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            rect(0.0, 0.0, width, height),
            NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Miniaturizable,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(&NSString::from_str(settings::TITLE));
    window.setContentView(Some(&content));
    window.setDelegate(Some(ProtocolObject::from_ref(&*controller)));
    window.setAutorecalculatesKeyViewLoop(true);

    SettingsWindow { window, _controller: controller, widgets, controls, config_file, form }
}
