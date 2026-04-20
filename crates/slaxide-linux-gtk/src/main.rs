mod auth;

use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use auth::{
    CLIENT_ID_ENV, CLIENT_SECRET_ENV, DEFAULT_REDIRECT_URI, DEFAULT_WORKSPACE_KEY,
    PendingSlackLogin, REDIRECT_URI_ENV, SlackAuthController, SlackAuthStatus,
    SlackOAuthEnvironment, StoredSlackSession, USER_SCOPES_ENV,
};
use chrono::{Datelike, Local, Timelike, Utc};
use emojis::get_by_shortcode;
use gtk::{
    Application, ApplicationWindow, Box as GtkBox, Button, ComboBoxText, Entry, Grid, Image, Label,
    ListView, MenuButton, Orientation, Overflow, ScrolledWindow, SearchEntry, TextBuffer, TextView,
    WrapMode, gdk, gio, glib, prelude::*,
};
use reqwest::{Client as HttpClient, header};
use slaxide_core::{
    AppSettings, AppSettingsPatch, AttachmentKind, AttachmentSummary, AuthorProfile,
    ChannelPermission, ChannelProfile, KeywordProfile, NotificationAction, NotificationRule,
    OfficeSettings, PatternMatcher, ProfileMode, QuietHours, RankedTimelineItem, ReactionSummary,
    ReplyState, SearchProfile, SectionProfile, ShortcutBindings, SlackConfigSettings, ThemeId,
    TimelineItem, TimelineMode, TimelineRanker, WorkspaceProfile,
};
use slaxide_platform::{
    KeyringSecretStore, NotificationBackend, NotificationRequest, NotifyRustBackend,
};
use slaxide_slack::{
    SlackClient, SlackConversation, SlackFile, SlackHistoryMessage, SlackMessageChangedEvent,
    SlackMessageDeletedEvent, SlackMessageEvent, SlackReaction, SlackSocketEvent, SlackUser,
    SocketModeSession,
};
use slaxide_store::StoreHandle;
use tokio::runtime::Runtime;
use uuid::Uuid;

const APP_ID: &str = "dev.slaxide.Slaxide";
const APP_DIR_NAME: &str = "slaxide";
const SLACK_APP_TOKEN_ENV: &str = "SLAXIDE_SLACK_APP_TOKEN";
const TIMELINE_LIMIT: usize = 1_500;
const TIMELINE_PAGE_SIZE: usize = 60;
const TIMELINE_SCROLL_PREFETCH_PX: f64 = 960.0;
const INITIAL_CHANNEL_PAGE_LIMIT: usize = 200;
const INITIAL_CHANNEL_SYNC_LIMIT: usize = 24;
const INITIAL_MESSAGES_PER_CHANNEL: usize = 25;
const LIVE_SYNC_RECONNECT_DELAY: Duration = Duration::from_secs(5);
const LIVE_RECONCILE_INTERVAL: Duration = Duration::from_secs(30);
const LIVE_RECONCILE_CHANNEL_LIMIT: usize = 8;
const LIVE_RECONCILE_MESSAGES_PER_CHANNEL: usize = 50;
const INITIAL_AUTH_LOOKUP_TIMEOUT: Duration = Duration::from_millis(1200);
const AVATAR_SIZE_PX: i32 = 44;
const SHARED_PREVIEW_AVATAR_SIZE_PX: i32 = 30;
const TIMELINE_INSERT_ANIMATION_MS: f64 = 380.0;
const TIMELINE_ATTACHMENT_WIDTH_PX: i32 = 404;
const THREAD_ATTACHMENT_WIDTH_PX: i32 = 328;
const TYPING_INDICATOR_TTL: Duration = Duration::from_secs(6);
const OFFICE_ACTIVITY_WINDOW_HOURS: i64 = 8;
const OFFICE_BUBBLE_BODY_LIMIT: usize = 160;
const OFFICE_SCENE_WIDTH_PX: i32 = 1400;
const OFFICE_SCENE_HEIGHT_PX: i32 = 1100;
const OFFICE_WORKSTATION_WIDTH_PX: i32 = 240;
const OFFICE_WORKSTATION_HEIGHT_PX: i32 = 238;
const OFFICE_PIXEL_AVATAR_SIZE_PX: i32 = 72;
const OFFICE_SLOT_COORDS: [(f64, f64); 16] = [
    (72.0, 128.0),
    (344.0, 128.0),
    (888.0, 128.0),
    (1160.0, 128.0),
    (72.0, 358.0),
    (344.0, 358.0),
    (888.0, 358.0),
    (1160.0, 358.0),
    (72.0, 588.0),
    (344.0, 588.0),
    (888.0, 588.0),
    (1160.0, 588.0),
    (72.0, 818.0),
    (344.0, 818.0),
    (888.0, 818.0),
    (1160.0, 818.0),
];
const QUICK_REACTION_NAMES: [&str; 8] = [
    "+1",
    "heart",
    "tada",
    "rocket",
    "eyes",
    "white_check_mark",
    "thinking_face",
    "fire",
];

fn main() {
    load_development_dotenv();
    configure_startup_environment();
    let app = Application::builder().application_id(APP_ID).build();
    app.connect_activate(build_ui);
    app.run();
}

fn load_development_dotenv() {
    let Ok(cwd) = env::current_dir() else {
        return;
    };
    let path = cwd.join(".env");
    if !path.exists() {
        return;
    }

    let raw = match fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(error) => {
            eprintln!(
                "[slaxide] failed to read .env from {}: {error}",
                path.display()
            );
            return;
        }
    };

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((key, value)) = trimmed.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() || env::var_os(key).is_some() {
            continue;
        }
        let value = parse_dotenv_value(value);
        // SAFETY: this runs during process startup on the main thread.
        unsafe {
            env::set_var(key, value);
        }
    }
}

fn parse_dotenv_value(raw: &str) -> String {
    let value = raw.trim();
    if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        value[1..value.len() - 1].to_string()
    } else {
        value.to_string()
    }
}

fn slack_env_keys() -> [&'static str; 5] {
    [
        CLIENT_ID_ENV,
        CLIENT_SECRET_ENV,
        REDIRECT_URI_ENV,
        USER_SCOPES_ENV,
        SLACK_APP_TOKEN_ENV,
    ]
}

fn externally_provided_env_keys() -> BTreeSet<String> {
    slack_env_keys()
        .into_iter()
        .filter(|key| env::var_os(key).is_some())
        .map(str::to_string)
        .collect()
}

fn sync_settings_env(settings: &AppSettings, locked_keys: &BTreeSet<String>) {
    sync_setting_env_value(
        CLIENT_ID_ENV,
        settings.slack.client_id.as_deref(),
        locked_keys,
    );
    sync_setting_env_value(
        CLIENT_SECRET_ENV,
        settings.slack.client_secret.as_deref(),
        locked_keys,
    );
    sync_setting_env_value(
        REDIRECT_URI_ENV,
        settings.slack.redirect_uri.as_deref(),
        locked_keys,
    );
    sync_setting_env_value(
        USER_SCOPES_ENV,
        settings.slack.user_scopes.as_deref(),
        locked_keys,
    );
    sync_setting_env_value(
        SLACK_APP_TOKEN_ENV,
        settings.slack.app_token.as_deref(),
        locked_keys,
    );
}

fn sync_setting_env_value(key: &str, value: Option<&str>, locked_keys: &BTreeSet<String>) {
    if locked_keys.contains(key) {
        return;
    }

    match value.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => {
            // SAFETY: UI writes happen on the GTK main thread.
            unsafe {
                env::set_var(key, value);
            }
        }
        None => {
            // SAFETY: UI writes happen on the GTK main thread.
            unsafe {
                env::remove_var(key);
            }
        }
    }
}

fn resolved_slack_config_value(env_key: &str, stored_value: Option<&str>) -> String {
    env::var(env_key)
        .ok()
        .or_else(|| {
            stored_value
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_default()
}

fn normalized_entry_value(value: String) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

#[derive(Clone)]
struct MultiSelectPicker {
    button: MenuButton,
    summary_label: Label,
    options: Rc<Vec<(String, String, gtk::CheckButton)>>,
    placeholder: String,
    empty_label: String,
}

impl MultiSelectPicker {
    fn new(placeholder: &str, empty_label: &str, options: &[(String, String)]) -> Self {
        let button = MenuButton::new();
        button.set_always_show_arrow(false);
        button.set_hexpand(true);
        button.set_halign(gtk::Align::Fill);

        let summary_label = Label::new(None);
        summary_label.set_xalign(0.0);
        summary_label.set_hexpand(true);
        summary_label.add_css_class("meta");

        let chevron = Image::builder()
            .icon_name("pan-down-symbolic")
            .pixel_size(14)
            .build();

        let button_row = GtkBox::new(Orientation::Horizontal, 8);
        button_row.append(&summary_label);
        button_row.append(&chevron);
        button.set_child(Some(&button_row));

        let popover = gtk::Popover::new();
        popover.set_has_arrow(false);

        let content = GtkBox::new(Orientation::Vertical, 8);
        content.set_margin_top(8);
        content.set_margin_bottom(8);
        content.set_margin_start(8);
        content.set_margin_end(8);

        let options = options
            .iter()
            .map(|(id, label)| {
                let check = gtk::CheckButton::with_label(label);
                check.set_halign(gtk::Align::Start);
                (id.clone(), label.clone(), check)
            })
            .collect::<Vec<_>>();

        if options.is_empty() {
            let empty = Label::new(Some(empty_label));
            empty.add_css_class("meta");
            empty.set_wrap(true);
            empty.set_xalign(0.0);
            content.append(&empty);
            button.set_sensitive(false);
        } else {
            let actions = GtkBox::new(Orientation::Horizontal, 8);
            let clear_button = Button::with_label("Clear");
            let select_all_button = Button::with_label("Select all");
            actions.append(&clear_button);
            actions.append(&select_all_button);
            content.append(&actions);

            let options_box = GtkBox::new(Orientation::Vertical, 6);
            for (_, _, check) in &options {
                options_box.append(check);
            }
            let scroll = ScrolledWindow::new();
            scroll.set_min_content_height(196);
            scroll.set_child(Some(&options_box));
            content.append(&scroll);

            let option_handles = Rc::new(options.clone());
            {
                let option_handles = option_handles.clone();
                clear_button.connect_clicked(move |_| {
                    for (_, _, check) in option_handles.iter() {
                        check.set_active(false);
                    }
                });
            }
            {
                let option_handles = option_handles.clone();
                select_all_button.connect_clicked(move |_| {
                    for (_, _, check) in option_handles.iter() {
                        check.set_active(true);
                    }
                });
            }
        }

        popover.set_child(Some(&content));
        button.set_popover(Some(&popover));

        let picker = Self {
            button,
            summary_label,
            options: Rc::new(options),
            placeholder: placeholder.to_string(),
            empty_label: empty_label.to_string(),
        };

        for (_, _, check) in picker.options.iter() {
            let picker = picker.clone();
            check.connect_toggled(move |_| {
                picker.refresh_summary();
            });
        }
        picker.refresh_summary();
        picker
    }

    fn widget(&self) -> MenuButton {
        self.button.clone()
    }

    fn selected_ids(&self) -> Vec<String> {
        self.options
            .iter()
            .filter(|(_, _, check)| check.is_active())
            .map(|(id, _, _)| id.clone())
            .collect()
    }

    fn selected_set(&self) -> BTreeSet<String> {
        self.selected_ids().into_iter().collect()
    }

    fn set_selected_ids<I, S>(&self, values: I)
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let selected = values
            .into_iter()
            .map(|value| value.as_ref().to_string())
            .collect::<BTreeSet<_>>();
        for (id, _, check) in self.options.iter() {
            let active = selected.contains(id);
            if check.is_active() != active {
                check.set_active(active);
            }
        }
        self.refresh_summary();
    }

    fn refresh_summary(&self) {
        if self.options.is_empty() {
            self.summary_label.set_text(&self.empty_label);
            self.button.set_tooltip_text(Some(&self.empty_label));
            return;
        }

        let selected = self
            .options
            .iter()
            .filter(|(_, _, check)| check.is_active())
            .map(|(_, label, _)| label.clone())
            .collect::<Vec<_>>();

        let summary = match selected.len() {
            0 => self.placeholder.clone(),
            1 => selected[0].clone(),
            2 => format!("{}, {}", selected[0], selected[1]),
            count => format!("{}, {} +{}", selected[0], selected[1], count - 2),
        };
        self.summary_label.set_text(&summary);
        let tooltip = if selected.is_empty() {
            self.placeholder.clone()
        } else {
            selected.join("\n")
        };
        self.button.set_tooltip_text(Some(&tooltip));
    }
}

#[derive(Clone)]
struct PatternMatcherRow {
    container: GtkBox,
    kind_picker: ComboBoxText,
    expression_entry: Entry,
}

#[derive(Clone)]
struct PatternMatcherEditor {
    root: GtkBox,
    rows_box: GtkBox,
    rows: Rc<RefCell<Vec<PatternMatcherRow>>>,
    expression_placeholder: String,
}

impl PatternMatcherEditor {
    fn new(expression_placeholder: &str) -> Self {
        let root = GtkBox::new(Orientation::Vertical, 8);
        let rows_box = GtkBox::new(Orientation::Vertical, 8);
        root.append(&rows_box);

        let add_button = Button::with_label("Add matcher");
        add_button.add_css_class("flat");
        root.append(&add_button);

        let editor = Self {
            root,
            rows_box,
            rows: Rc::new(RefCell::new(Vec::new())),
            expression_placeholder: expression_placeholder.to_string(),
        };

        {
            let editor = editor.clone();
            add_button.connect_clicked(move |_| {
                editor.add_empty_row();
            });
        }

        editor.add_empty_row();
        editor
    }

    fn widget(&self) -> GtkBox {
        self.root.clone()
    }

    fn set_matchers(&self, matchers: &[PatternMatcher]) {
        while let Some(child) = self.rows_box.first_child() {
            self.rows_box.remove(&child);
        }
        self.rows.borrow_mut().clear();

        if matchers.is_empty() {
            self.add_empty_row();
            return;
        }

        for matcher in matchers {
            self.add_row(Some(matcher.clone()));
        }
    }

    fn matchers(&self) -> Result<Vec<PatternMatcher>, String> {
        let mut matchers = Vec::new();
        for row in self.rows.borrow().iter() {
            let expression = row.expression_entry.text().trim().to_string();
            if expression.is_empty() {
                continue;
            }

            let matcher = match row.kind_picker.active_id().as_deref() {
                Some("regex") => {
                    regex::Regex::new(&expression)
                        .map_err(|error| format!("Invalid regex `{expression}`: {error}"))?;
                    PatternMatcher::Regex(expression)
                }
                _ => PatternMatcher::Text(expression),
            };
            matchers.push(matcher);
        }
        Ok(matchers)
    }

    fn add_empty_row(&self) {
        self.add_row(None);
    }

    fn add_row(&self, matcher: Option<PatternMatcher>) {
        let row_box = GtkBox::new(Orientation::Horizontal, 8);

        let kind_picker = ComboBoxText::new();
        kind_picker.append(Some("text"), "Text");
        kind_picker.append(Some("regex"), "Regex");
        kind_picker.set_active_id(Some("text"));

        let expression_entry = Entry::builder()
            .hexpand(true)
            .placeholder_text(&self.expression_placeholder)
            .build();

        let remove_button = build_icon_button("user-trash-symbolic", "Remove matcher");
        remove_button.remove_css_class("nav-button");
        remove_button.add_css_class("close-button");

        row_box.append(&kind_picker);
        row_box.append(&expression_entry);
        row_box.append(&remove_button);

        if let Some(matcher) = matcher {
            match matcher {
                PatternMatcher::Text(text) => {
                    kind_picker.set_active_id(Some("text"));
                    expression_entry.set_text(&text);
                }
                PatternMatcher::Regex(pattern) => {
                    kind_picker.set_active_id(Some("regex"));
                    expression_entry.set_text(&pattern);
                }
            }
        }

        self.rows_box.append(&row_box);
        self.rows.borrow_mut().push(PatternMatcherRow {
            container: row_box.clone(),
            kind_picker,
            expression_entry,
        });

        let editor = self.clone();
        remove_button.connect_clicked(move |_| {
            editor.rows_box.remove(&row_box);
            editor
                .rows
                .borrow_mut()
                .retain(|row| row.container.as_ptr() != row_box.as_ptr());
            if editor.rows.borrow().is_empty() {
                editor.add_empty_row();
            }
        });
    }
}

fn build_profile_mode_picker() -> ComboBoxText {
    let picker = ComboBoxText::new();
    picker.append(Some("allow"), "Allow");
    picker.append(Some("deny"), "Deny");
    picker.set_active_id(Some("allow"));
    picker
}

fn set_profile_mode_picker(picker: &ComboBoxText, mode: ProfileMode) {
    let id = match mode {
        ProfileMode::Allow => "allow",
        ProfileMode::Deny => "deny",
    };
    picker.set_active_id(Some(id));
}

fn profile_mode_from_picker(picker: &ComboBoxText) -> ProfileMode {
    match picker.active_id().as_deref() {
        Some("deny") => ProfileMode::Deny,
        _ => ProfileMode::Allow,
    }
}

fn build_notification_action_picker() -> ComboBoxText {
    let picker = ComboBoxText::new();
    picker.append(Some("notify"), "Notify");
    picker.append(Some("silent"), "Silent");
    picker.append(Some("critical"), "Critical");
    picker.set_active_id(Some("notify"));
    picker
}

fn set_notification_action_picker(picker: &ComboBoxText, action: NotificationAction) {
    let id = match action {
        NotificationAction::Notify => "notify",
        NotificationAction::Silent => "silent",
        NotificationAction::Critical => "critical",
    };
    picker.set_active_id(Some(id));
}

fn notification_action_from_picker(picker: &ComboBoxText) -> NotificationAction {
    match picker.active_id().as_deref() {
        Some("silent") => NotificationAction::Silent,
        Some("critical") => NotificationAction::Critical,
        _ => NotificationAction::Notify,
    }
}

fn build_channel_permission_picker() -> ComboBoxText {
    let picker = ComboBoxText::new();
    picker.append(Some("read_write"), "Read / write");
    picker.append(Some("read_only"), "Read only");
    picker.append(Some("hidden"), "Hidden");
    picker.set_active_id(Some("read_write"));
    picker
}

fn set_channel_permission_picker(picker: &ComboBoxText, permission: ChannelPermission) {
    let id = match permission {
        ChannelPermission::ReadWrite => "read_write",
        ChannelPermission::ReadOnly => "read_only",
        ChannelPermission::Hidden => "hidden",
    };
    picker.set_active_id(Some(id));
}

fn channel_permission_from_picker(picker: &ComboBoxText) -> ChannelPermission {
    match picker.active_id().as_deref() {
        Some("read_only") => ChannelPermission::ReadOnly,
        Some("hidden") => ChannelPermission::Hidden,
        _ => ChannelPermission::ReadWrite,
    }
}

fn build_hour_picker(placeholder: &str) -> ComboBoxText {
    let picker = ComboBoxText::new();
    picker.append(Some(""), placeholder);
    for hour in 0..24u8 {
        picker.append(Some(&hour.to_string()), &format!("{hour:02}:00"));
    }
    picker.set_active_id(Some(""));
    picker
}

fn set_hour_picker(picker: &ComboBoxText, hour: Option<u8>) {
    let id = hour.map(|hour| hour.to_string()).unwrap_or_default();
    picker.set_active_id(Some(&id));
}

fn hour_from_picker(picker: &ComboBoxText) -> Option<u8> {
    picker
        .active_id()
        .and_then(|value| if value.is_empty() { None } else { Some(value) })
        .and_then(|value| value.parse::<u8>().ok())
}

fn sync_quiet_hours_inputs(
    enabled_check: &gtk::CheckButton,
    start_picker: &ComboBoxText,
    end_picker: &ComboBoxText,
) {
    let enabled = enabled_check.is_active();
    start_picker.set_sensitive(enabled);
    end_picker.set_sensitive(enabled);
}

fn parse_quiet_hours(
    enabled: bool,
    start_hour: Option<u8>,
    end_hour: Option<u8>,
) -> Result<Option<QuietHours>, String> {
    if !enabled {
        return Ok(None);
    }

    match (start_hour, end_hour) {
        (Some(start_hour), Some(end_hour)) => Ok(Some(QuietHours {
            start_hour,
            end_hour,
        })),
        _ => Err("Quiet hours require both start and end time.".to_string()),
    }
}

fn apply_keyword_profile_form(
    profile: Option<&KeywordProfile>,
    id_entry: &Entry,
    label_entry: &Entry,
    mode_picker: &ComboBoxText,
    matchers_editor: &PatternMatcherEditor,
) {
    if let Some(profile) = profile {
        id_entry.set_text(&profile.id);
        label_entry.set_text(&profile.label);
        set_profile_mode_picker(mode_picker, profile.mode);
        matchers_editor.set_matchers(&profile.matchers);
    } else {
        id_entry.set_text("");
        label_entry.set_text("");
        set_profile_mode_picker(mode_picker, ProfileMode::Allow);
        matchers_editor.set_matchers(&[]);
    }
}

fn apply_channel_profile_form(
    profile: Option<&ChannelProfile>,
    id_entry: &Entry,
    label_entry: &Entry,
    mode_picker: &ComboBoxText,
    channels_picker: &MultiSelectPicker,
    matchers_editor: &PatternMatcherEditor,
) {
    if let Some(profile) = profile {
        id_entry.set_text(&profile.id);
        label_entry.set_text(&profile.label);
        set_profile_mode_picker(mode_picker, profile.mode);
        channels_picker.set_selected_ids(profile.channels.iter());
        matchers_editor.set_matchers(&profile.channel_name_matchers);
    } else {
        id_entry.set_text("");
        label_entry.set_text("");
        set_profile_mode_picker(mode_picker, ProfileMode::Allow);
        channels_picker.set_selected_ids(std::iter::empty::<&str>());
        matchers_editor.set_matchers(&[]);
    }
}

fn apply_section_profile_form(
    profile: Option<&SectionProfile>,
    id_entry: &Entry,
    label_entry: &Entry,
    channels_picker: &MultiSelectPicker,
    matchers_editor: &PatternMatcherEditor,
) {
    if let Some(profile) = profile {
        id_entry.set_text(&profile.id);
        label_entry.set_text(&profile.label);
        channels_picker.set_selected_ids(profile.channels.iter());
        matchers_editor.set_matchers(&profile.channel_name_matchers);
    } else {
        id_entry.set_text("");
        label_entry.set_text("");
        channels_picker.set_selected_ids(std::iter::empty::<&str>());
        matchers_editor.set_matchers(&[]);
    }
}

fn apply_author_profile_form(
    profile: Option<&AuthorProfile>,
    id_entry: &Entry,
    label_entry: &Entry,
    mode_picker: &ComboBoxText,
    authors_picker: &MultiSelectPicker,
) {
    if let Some(profile) = profile {
        id_entry.set_text(&profile.id);
        label_entry.set_text(&profile.label);
        set_profile_mode_picker(mode_picker, profile.mode);
        authors_picker.set_selected_ids(profile.authors.iter());
    } else {
        id_entry.set_text("");
        label_entry.set_text("");
        set_profile_mode_picker(mode_picker, ProfileMode::Allow);
        authors_picker.set_selected_ids(std::iter::empty::<&str>());
    }
}

fn apply_search_profile_form(
    profile: Option<&SearchProfile>,
    id_entry: &Entry,
    label_entry: &Entry,
    query_entry: &Entry,
    keyword_profiles_picker: &MultiSelectPicker,
    section_profiles_picker: &MultiSelectPicker,
    channel_profiles_picker: &MultiSelectPicker,
    author_profiles_picker: &MultiSelectPicker,
) {
    if let Some(profile) = profile {
        id_entry.set_text(&profile.id);
        label_entry.set_text(&profile.label);
        query_entry.set_text(profile.query.as_deref().unwrap_or(""));
        keyword_profiles_picker.set_selected_ids(profile.keyword_profiles.iter());
        section_profiles_picker.set_selected_ids(profile.section_profiles.iter());
        channel_profiles_picker.set_selected_ids(profile.channel_profiles.iter());
        author_profiles_picker.set_selected_ids(profile.author_profiles.iter());
    } else {
        id_entry.set_text("");
        label_entry.set_text("");
        query_entry.set_text("");
        keyword_profiles_picker.set_selected_ids(std::iter::empty::<&str>());
        section_profiles_picker.set_selected_ids(std::iter::empty::<&str>());
        channel_profiles_picker.set_selected_ids(std::iter::empty::<&str>());
        author_profiles_picker.set_selected_ids(std::iter::empty::<&str>());
    }
}

fn apply_notification_rule_form(
    rule: Option<&NotificationRule>,
    label_entry: &Entry,
    enabled_check: &gtk::CheckButton,
    action_picker: &ComboBoxText,
    keyword_profiles_picker: &MultiSelectPicker,
    section_profiles_picker: &MultiSelectPicker,
    channel_profiles_picker: &MultiSelectPicker,
    author_profiles_picker: &MultiSelectPicker,
    search_profiles_picker: &MultiSelectPicker,
    thread_only_check: &gtk::CheckButton,
    quiet_enabled_check: &gtk::CheckButton,
    quiet_start_picker: &ComboBoxText,
    quiet_end_picker: &ComboBoxText,
    rule_id_label: &Label,
) {
    if let Some(rule) = rule {
        rule_id_label.set_text(&format!("Rule ID: {}", rule.id));
        label_entry.set_text(&rule.label);
        enabled_check.set_active(rule.enabled);
        set_notification_action_picker(action_picker, rule.action.clone());
        keyword_profiles_picker.set_selected_ids(rule.keyword_profile_ids.iter());
        section_profiles_picker.set_selected_ids(rule.section_profile_ids.iter());
        channel_profiles_picker.set_selected_ids(rule.channel_profile_ids.iter());
        author_profiles_picker.set_selected_ids(rule.author_profile_ids.iter());
        search_profiles_picker.set_selected_ids(rule.search_profile_ids.iter());
        thread_only_check.set_active(rule.thread_participation_only);
        quiet_enabled_check.set_active(rule.quiet_hours.is_some());
        set_hour_picker(
            quiet_start_picker,
            rule.quiet_hours.as_ref().map(|hours| hours.start_hour),
        );
        set_hour_picker(
            quiet_end_picker,
            rule.quiet_hours.as_ref().map(|hours| hours.end_hour),
        );
    } else {
        rule_id_label.set_text("Rule ID: new");
        label_entry.set_text("");
        enabled_check.set_active(true);
        set_notification_action_picker(action_picker, NotificationAction::Notify);
        keyword_profiles_picker.set_selected_ids(std::iter::empty::<&str>());
        section_profiles_picker.set_selected_ids(std::iter::empty::<&str>());
        channel_profiles_picker.set_selected_ids(std::iter::empty::<&str>());
        author_profiles_picker.set_selected_ids(std::iter::empty::<&str>());
        search_profiles_picker.set_selected_ids(std::iter::empty::<&str>());
        thread_only_check.set_active(false);
        quiet_enabled_check.set_active(false);
        set_hour_picker(quiet_start_picker, None);
        set_hour_picker(quiet_end_picker, None);
    }
    sync_quiet_hours_inputs(quiet_enabled_check, quiet_start_picker, quiet_end_picker);
}

const PROFILE_SUMMARY_CHANNEL_CAP: usize = 10;

fn capped_count_label(count: usize, cap: usize) -> String {
    if count > cap {
        format!("{cap}+")
    } else {
        count.to_string()
    }
}

fn format_match_summary(messages: usize, threads: usize, channels: usize) -> String {
    format!(
        "{messages} cached messages across {threads} threads in {} channels.",
        capped_count_label(channels, PROFILE_SUMMARY_CHANNEL_CAP)
    )
}

fn summarize_matching_items<F>(
    state: &UiState,
    mut predicate: F,
) -> Result<(usize, usize, usize), String>
where
    F: FnMut(&TimelineItem) -> Result<bool, String>,
{
    let mut messages = 0usize;
    let mut thread_hits = BTreeSet::new();
    let mut channel_hits = BTreeSet::new();

    for item in &state.source_items {
        if !state.settings.can_read_channel(&item.channel_id) {
            continue;
        }
        if predicate(item)? {
            messages += 1;
            thread_hits.insert(item.thread_ts.clone());
            channel_hits.insert(item.channel_id.clone());
        }
    }

    Ok((messages, thread_hits.len(), channel_hits.len()))
}

fn matcher_matches_channel_name(
    matcher: &PatternMatcher,
    channel_name: &str,
) -> Result<bool, String> {
    matcher
        .matches(channel_name)
        .map_err(|error| error.to_string())
        .and_then(|matched| {
            if matched {
                Ok(true)
            } else {
                let raw = channel_name.trim_start_matches('#');
                if raw == channel_name {
                    Ok(false)
                } else {
                    matcher.matches(raw).map_err(|error| error.to_string())
                }
            }
        })
}

fn section_profile_matches_channel(
    profile: &SectionProfile,
    channel_id: &str,
    channel_name: &str,
) -> Result<bool, String> {
    if profile.channels.contains(channel_id) {
        return Ok(true);
    }
    for matcher in &profile.channel_name_matchers {
        if matcher_matches_channel_name(matcher, channel_name)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn channel_profile_matches_channel(
    profile: &ChannelProfile,
    channel_id: &str,
    channel_name: &str,
) -> Result<bool, String> {
    if profile.channels.contains(channel_id) {
        return Ok(true);
    }
    for matcher in &profile.channel_name_matchers {
        if matcher_matches_channel_name(matcher, channel_name)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn keyword_profile_preview_summary(state: &UiState, profile: Option<&KeywordProfile>) -> String {
    let Some(profile) = profile else {
        return "Pick a keyword profile to preview cached hits.".to_string();
    };
    if profile.matchers.is_empty() {
        return "No matcher configured yet.".to_string();
    }
    match summarize_matching_items(state, |item| {
        profile.matchers.iter().try_fold(false, |matched, matcher| {
            if matched {
                Ok(true)
            } else {
                matcher
                    .matches(&item.body)
                    .map_err(|error| error.to_string())
            }
        })
    }) {
        Ok((messages, threads, channels)) => format!(
            "{} profile hits {}",
            match profile.mode {
                ProfileMode::Allow => "Allow",
                ProfileMode::Deny => "Deny",
            },
            format_match_summary(messages, threads, channels)
        ),
        Err(error) => format!("Matcher error: {error}"),
    }
}

fn section_profile_preview_summary(state: &UiState, profile: Option<&SectionProfile>) -> String {
    let Some(profile) = profile else {
        return "Pick a section to preview cached hits.".to_string();
    };
    if profile.channels.is_empty() && profile.channel_name_matchers.is_empty() {
        return "No channel target configured yet.".to_string();
    }
    let covered_channels = state
        .available_channels()
        .into_iter()
        .filter(|(channel_id, channel_name)| {
            section_profile_matches_channel(profile, channel_id, channel_name).unwrap_or(false)
        })
        .count();
    match summarize_matching_items(state, |item| {
        section_profile_matches_channel(profile, &item.channel_id, &item.channel_name)
    }) {
        Ok((messages, threads, channels)) => format!(
            "Section covers {} channels and hits {}",
            capped_count_label(covered_channels, PROFILE_SUMMARY_CHANNEL_CAP),
            format_match_summary(messages, threads, channels)
        ),
        Err(error) => format!("Matcher error: {error}"),
    }
}

fn channel_profile_preview_summary(state: &UiState, profile: Option<&ChannelProfile>) -> String {
    let Some(profile) = profile else {
        return "Pick a channel profile to preview cached hits.".to_string();
    };
    if profile.channels.is_empty() && profile.channel_name_matchers.is_empty() {
        return "No channel target configured yet.".to_string();
    }
    let covered_channels = state
        .available_channels()
        .into_iter()
        .filter(|(channel_id, channel_name)| {
            channel_profile_matches_channel(profile, channel_id, channel_name).unwrap_or(false)
        })
        .count();
    match summarize_matching_items(state, |item| {
        channel_profile_matches_channel(profile, &item.channel_id, &item.channel_name)
    }) {
        Ok((messages, threads, channels)) => format!(
            "{} profile covers {} channels and hits {}",
            match profile.mode {
                ProfileMode::Allow => "Allow",
                ProfileMode::Deny => "Deny",
            },
            capped_count_label(covered_channels, PROFILE_SUMMARY_CHANNEL_CAP),
            format_match_summary(messages, threads, channels)
        ),
        Err(error) => format!("Matcher error: {error}"),
    }
}

fn author_profile_preview_summary(state: &UiState, profile: Option<&AuthorProfile>) -> String {
    let Some(profile) = profile else {
        return "Pick an author profile to preview cached hits.".to_string();
    };
    if profile.authors.is_empty() {
        return "No author configured yet.".to_string();
    }
    match summarize_matching_items(state, |item| Ok(profile.authors.contains(&item.author_id))) {
        Ok((messages, threads, channels)) => format!(
            "{} profile targets {} authors and hits {}",
            match profile.mode {
                ProfileMode::Allow => "Allow",
                ProfileMode::Deny => "Deny",
            },
            profile.authors.len(),
            format_match_summary(messages, threads, channels)
        ),
        Err(error) => format!("Matcher error: {error}"),
    }
}

fn search_profile_preview_summary(state: &UiState, profile: Option<&SearchProfile>) -> String {
    let Some(profile) = profile else {
        return "Pick a search profile to preview cached hits.".to_string();
    };
    match summarize_matching_items(state, |item| {
        state
            .settings
            .search_profile_matches(&profile.id, item)
            .map_err(|error| error.to_string())
    }) {
        Ok((messages, threads, channels)) => {
            format!(
                "Search profile hits {}",
                format_match_summary(messages, threads, channels)
            )
        }
        Err(error) => format!("Profile error: {error}"),
    }
}

fn configure_startup_environment() {
    let has_wayland = env::var_os("WAYLAND_DISPLAY").is_some();
    let on_hyprland = env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some();

    if has_wayland && env::var_os("GDK_BACKEND").is_none() {
        // SAFETY: this runs before the GTK application and worker threads start.
        unsafe {
            env::set_var("GDK_BACKEND", "wayland");
        }
    }

    if on_hyprland && env::var_os("GSK_RENDERER").is_none() {
        // SAFETY: this runs before the GTK application and worker threads start.
        unsafe {
            env::set_var("GSK_RENDERER", "cairo");
        }
    }

    eprintln!(
        "[slaxide] startup env: XDG_SESSION_TYPE={:?} GDK_BACKEND={:?} GSK_RENDERER={:?}",
        env::var("XDG_SESSION_TYPE").ok(),
        env::var("GDK_BACKEND").ok(),
        env::var("GSK_RENDERER").ok()
    );
}

fn fallback_auth_status() -> SlackAuthStatus {
    match SlackOAuthEnvironment::from_env() {
        Ok(env) => SlackAuthStatus::Disconnected {
            scopes: env.user_scopes().to_vec(),
            redirect_uri: env.redirect_uri().to_string(),
        },
        Err(error) => SlackAuthStatus::MissingConfig(error.to_string()),
    }
}

fn load_initial_auth_status(
    auth_controller: Arc<SlackAuthController<KeyringSecretStore>>,
    workspace_key: &str,
) -> SlackAuthStatus {
    let (status_tx, status_rx) = mpsc::channel();
    let workspace_key = workspace_key.to_string();
    std::thread::spawn(move || {
        let _ = status_tx.send(auth_controller.initial_status_for(&workspace_key));
    });

    match status_rx.recv_timeout(INITIAL_AUTH_LOOKUP_TIMEOUT) {
        Ok(status) => {
            eprintln!("[slaxide] auth: initial session ready");
            status
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            eprintln!(
                "[slaxide] auth: initial session lookup timed out, continuing without keyring"
            );
            fallback_auth_status()
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            eprintln!("[slaxide] auth: initial session lookup failed");
            fallback_auth_status()
        }
    }
}

#[derive(Clone, Debug)]
struct AppPaths {
    config_dir: PathBuf,
    data_dir: PathBuf,
    cache_dir: PathBuf,
    db_path: PathBuf,
}

impl AppPaths {
    fn discover() -> Result<Self> {
        let config_dir = xdg_dir("XDG_CONFIG_HOME", ".config")?.join(APP_DIR_NAME);
        let data_dir = xdg_dir("XDG_DATA_HOME", ".local/share")?.join(APP_DIR_NAME);
        let cache_dir = xdg_dir("XDG_CACHE_HOME", ".cache")?.join(APP_DIR_NAME);
        let db_path = data_dir.join("slaxide.db");

        Ok(Self {
            config_dir,
            data_dir,
            cache_dir,
            db_path,
        })
    }

    fn create_dirs(&self) -> Result<()> {
        fs::create_dir_all(&self.config_dir).context("failed to create config dir")?;
        fs::create_dir_all(&self.data_dir).context("failed to create data dir")?;
        fs::create_dir_all(&self.cache_dir).context("failed to create cache dir")?;
        Ok(())
    }

    fn config_file_path(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }
}

fn xdg_dir(env_key: &str, fallback_suffix: &str) -> Result<PathBuf> {
    if let Some(path) = env::var_os(env_key).filter(|path| !path.is_empty()) {
        return Ok(PathBuf::from(path));
    }

    Ok(home_dir()?.join(fallback_suffix))
}

fn home_dir() -> Result<PathBuf> {
    env::var_os("HOME")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .context("HOME is not set")
}

fn load_settings_patch(path: &Path) -> Result<Option<AppSettingsPatch>> {
    if !path.exists() {
        return Ok(None);
    }

    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed to read TOML config from {}", path.display()))?;
    let patch = toml::from_str::<AppSettingsPatch>(&raw)
        .with_context(|| format!("failed to parse TOML config from {}", path.display()))?;
    Ok(Some(patch))
}

#[derive(Clone)]
struct AppBootstrap {
    paths: Option<AppPaths>,
    runtime: Option<Rc<Runtime>>,
    store: Option<StoreHandle>,
    startup_message: String,
}

impl AppBootstrap {
    fn initialize() -> Self {
        match Self::try_initialize() {
            Ok(bootstrap) => bootstrap,
            Err(error) => Self {
                paths: None,
                runtime: None,
                store: None,
                startup_message: format!("Memory-only fallback.\n{error:#}"),
            },
        }
    }

    fn try_initialize() -> Result<Self> {
        eprintln!("[slaxide] bootstrap: discovering app paths");
        let paths = AppPaths::discover()?;
        paths.create_dirs()?;

        eprintln!("[slaxide] bootstrap: creating tokio runtime");
        let runtime = Rc::new(Runtime::new().context("failed to create tokio runtime")?);
        eprintln!("[slaxide] bootstrap: starting store actor");
        let store =
            StoreHandle::spawn(paths.db_path.clone()).context("failed to start store actor")?;
        let startup_message = format!(
            "DB: {}\nConfig: {}\nCache: {}",
            paths.db_path.display(),
            paths.config_dir.display(),
            paths.cache_dir.display()
        );

        Ok(Self {
            paths: Some(paths),
            runtime: Some(runtime),
            store: Some(store),
            startup_message,
        })
    }

    fn load_state(&self) -> (UiState, String) {
        eprintln!("[slaxide] bootstrap: loading state");
        let mut settings = AppSettings::default();
        let mut items = Vec::new();
        let mut issues = Vec::new();
        let mut removed_legacy_count = 0;
        let mut seeded_notification_rules = false;
        let mut loaded_config_path = None::<PathBuf>;

        if let (Some(runtime), Some(store)) = (self.runtime.as_ref(), self.store.as_ref()) {
            eprintln!("[slaxide] bootstrap: reading saved settings");
            match runtime.block_on(store.load_settings()) {
                Ok(Some(saved)) => settings = saved,
                Ok(None) => {}
                Err(error) => issues.push(format!("settings read failed: {error}")),
            }

            if let Some(paths) = self.paths.as_ref() {
                let config_path = paths.config_file_path();
                match load_settings_patch(&config_path) {
                    Ok(Some(patch)) => {
                        settings.apply_patch(patch);
                        loaded_config_path = Some(config_path);
                    }
                    Ok(None) => {}
                    Err(error) => issues.push(format!("config.toml read failed: {error}")),
                }
            }

            let workspace_key = normalized_active_workspace_key(&settings);
            eprintln!(
                "[slaxide] bootstrap: reading cached timeline for workspace={} limit={TIMELINE_LIMIT}",
                workspace_key
            );
            match runtime.block_on(store.list_timeline_items(workspace_key.clone(), TIMELINE_LIMIT))
            {
                Ok(saved) => {
                    eprintln!(
                        "[slaxide] bootstrap: loaded {} cached items before cleanup",
                        saved.len()
                    );
                    removed_legacy_count = saved
                        .iter()
                        .filter(|item| is_legacy_local_only_item(item))
                        .count();
                    items = saved
                        .into_iter()
                        .filter(|item| !is_legacy_local_only_item(item))
                        .collect();
                    if removed_legacy_count > 0
                        && let Err(error) = runtime
                            .block_on(store.replace_timeline_items(workspace_key, items.clone()))
                    {
                        issues.push(format!("timeline cleanup failed: {error}"));
                    }
                }
                Err(error) => issues.push(format!("timeline read failed: {error}")),
            }

            if seed_default_notification_rules(&mut settings) {
                seeded_notification_rules = true;
                if let Err(error) = runtime.block_on(store.save_settings(settings.clone())) {
                    issues.push(format!("notification rule seed failed: {error}"));
                }
            }
        }

        let mut status = if !issues.is_empty() {
            format!(
                "Store fallback: {}\n{}",
                issues.join(" | "),
                self.startup_message
            )
        } else if items.is_empty() {
            format!("Local cache is empty.\n{}", self.startup_message)
        } else if let Some(paths) = self.paths.as_ref() {
            format!(
                "Loaded {} cached items from {}.",
                items.len(),
                paths.db_path.display()
            )
        } else {
            self.startup_message.clone()
        };

        if removed_legacy_count > 0 {
            status = format!("Removed {removed_legacy_count} legacy local replies.\n{status}");
        }
        if seeded_notification_rules {
            status = format!("Seeded default notification rules.\n{status}");
        }
        if let Some(config_path) = loaded_config_path {
            status = format!(
                "Loaded config overlay from {}.\n{status}",
                config_path.display()
            );
        }

        (
            UiState::new(
                settings,
                items,
                BTreeMap::new(),
                BTreeMap::new(),
                BTreeMap::new(),
                BTreeMap::new(),
            ),
            status,
        )
    }

    fn save_settings(&self, settings: &AppSettings) {
        if let (Some(runtime), Some(store)) = (self.runtime.as_ref(), self.store.as_ref())
            && let Err(error) = runtime.block_on(store.save_settings(settings.clone()))
        {
            eprintln!("failed to save settings: {error:#}");
        }
    }

    fn replace_timeline_items(&self, workspace_key: &str, items: &[TimelineItem]) {
        if let (Some(runtime), Some(store)) = (self.runtime.as_ref(), self.store.as_ref())
            && let Err(error) = runtime
                .block_on(store.replace_timeline_items(workspace_key.to_string(), items.to_vec()))
        {
            eprintln!("failed to save timeline items: {error:#}");
        }
    }

    fn load_timeline_items(&self, workspace_key: &str) -> Result<Vec<TimelineItem>> {
        if let (Some(runtime), Some(store)) = (self.runtime.as_ref(), self.store.as_ref()) {
            return runtime
                .block_on(store.list_timeline_items(workspace_key.to_string(), TIMELINE_LIMIT));
        }
        Ok(Vec::new())
    }

    fn avatar_cache_dir(&self) -> Option<PathBuf> {
        self.paths
            .as_ref()
            .map(|paths| paths.cache_dir.join("avatars"))
    }

    fn image_cache_dir(&self) -> Option<PathBuf> {
        self.paths
            .as_ref()
            .map(|paths| paths.cache_dir.join("images"))
    }
}

fn is_legacy_local_only_item(item: &TimelineItem) -> bool {
    item.author_id == "U-me"
}

fn normalized_active_workspace_key(settings: &AppSettings) -> String {
    settings
        .active_workspace_key
        .as_deref()
        .filter(|key| !key.is_empty())
        .unwrap_or(DEFAULT_WORKSPACE_KEY)
        .to_string()
}

fn workspace_profile_from_session(
    workspace_key: &str,
    session: &StoredSlackSession,
) -> WorkspaceProfile {
    let label = session
        .team_name
        .clone()
        .or(session.team_id.clone())
        .unwrap_or_else(|| "Slack room".to_string());
    WorkspaceProfile {
        key: workspace_key.to_string(),
        label,
        team_id: session.team_id.clone(),
        team_name: session.team_name.clone(),
        app_id: session.app_id.clone(),
    }
}

fn merge_user_names(
    source_items: &[TimelineItem],
    mut user_names: BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    for item in source_items {
        if item.author_name != item.author_id && item.author_name != "you" {
            user_names
                .entry(item.author_id.clone())
                .or_insert_with(|| item.author_name.clone());
        }
    }
    user_names
}

fn merge_user_avatar_paths(
    source_items: &[TimelineItem],
    mut user_avatar_paths: BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    for item in source_items {
        if let Some(avatar_path) = item.author_avatar_path.as_ref() {
            user_avatar_paths
                .entry(item.author_id.clone())
                .or_insert_with(|| avatar_path.clone());
        }
    }
    user_avatar_paths
}

fn merge_known_channels(
    source_items: &[TimelineItem],
    mut known_channels: BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    for item in source_items {
        known_channels
            .entry(item.channel_id.clone())
            .or_insert_with(|| item.channel_name.clone());
    }
    known_channels
}

fn merge_known_channel_creators(
    source_items: &[TimelineItem],
    mut known_channel_creators: BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    for item in source_items {
        known_channel_creators
            .entry(item.channel_id.clone())
            .or_insert_with(|| item.author_id.clone());
    }
    known_channel_creators
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MainView {
    Timeline,
    Office,
}

#[derive(Clone, Debug)]
struct OfficePresence {
    channel_name: String,
    thread_ts: String,
    speaker_name: String,
    speaker_avatar_path: Option<String>,
    latest_body: String,
    activity_count: usize,
}

#[derive(Clone)]
struct UiState {
    settings: AppSettings,
    mode: TimelineMode,
    main_view: MainView,
    user_names: BTreeMap<String, String>,
    user_avatar_paths: BTreeMap<String, String>,
    known_channels: BTreeMap<String, String>,
    known_channel_creators: BTreeMap<String, String>,
    typing_by_channel: BTreeMap<String, Vec<TypingIndicator>>,
    source_items: Vec<TimelineItem>,
    ranked_items: Vec<RankedTimelineItem>,
    filtered_ranked_items: Vec<RankedTimelineItem>,
    loaded_timeline_items: usize,
    selected_message_ts: Option<String>,
    highlighted_timeline_message_ts: Option<String>,
    focused_thread_message_ts: Option<String>,
    search_query: String,
    section_filter: Option<String>,
    channel_filter: Option<String>,
    author_filter: Option<String>,
}

impl UiState {
    fn new(
        settings: AppSettings,
        source_items: Vec<TimelineItem>,
        user_names: BTreeMap<String, String>,
        user_avatar_paths: BTreeMap<String, String>,
        known_channels: BTreeMap<String, String>,
        known_channel_creators: BTreeMap<String, String>,
    ) -> Self {
        let mut state = Self {
            settings,
            main_view: MainView::Timeline,
            user_names: merge_user_names(&source_items, user_names),
            user_avatar_paths: merge_user_avatar_paths(&source_items, user_avatar_paths),
            known_channels: merge_known_channels(&source_items, known_channels),
            known_channel_creators: merge_known_channel_creators(
                &source_items,
                known_channel_creators,
            ),
            typing_by_channel: BTreeMap::new(),
            mode: TimelineMode::Recent,
            source_items,
            ranked_items: Vec::new(),
            filtered_ranked_items: Vec::new(),
            loaded_timeline_items: TIMELINE_PAGE_SIZE,
            selected_message_ts: None,
            highlighted_timeline_message_ts: None,
            focused_thread_message_ts: None,
            search_query: String::new(),
            section_filter: None,
            channel_filter: None,
            author_filter: None,
        };
        state.rerank(true);
        state
    }

    fn theme_id(&self) -> ThemeId {
        self.settings.theme_id
    }

    fn active_workspace_key(&self) -> &str {
        self.settings
            .active_workspace_key
            .as_deref()
            .filter(|key| !key.is_empty())
            .unwrap_or(DEFAULT_WORKSPACE_KEY)
    }

    fn workspace_profiles(&self) -> &Vec<WorkspaceProfile> {
        &self.settings.workspaces
    }

    fn active_search_profile_id(&self) -> Option<&str> {
        self.settings
            .active_search_profile_id
            .as_deref()
            .filter(|profile_id| self.settings.search_profile(profile_id).is_some())
    }

    fn main_view(&self) -> MainView {
        self.main_view
    }

    fn set_main_view(&mut self, main_view: MainView) -> bool {
        if self.main_view == main_view {
            return false;
        }
        self.main_view = main_view;
        true
    }

    fn available_sections(&self) -> Vec<(String, String)> {
        let mut profiles = self
            .settings
            .section_profiles
            .iter()
            .map(|profile| (profile.id.clone(), profile.label.clone()))
            .collect::<Vec<_>>();
        profiles.sort_by(|left, right| left.1.cmp(&right.1).then(left.0.cmp(&right.0)));
        profiles
    }

    fn search_profiles(&self) -> Vec<(String, String)> {
        let mut profiles = self
            .settings
            .search_profiles
            .iter()
            .map(|profile| (profile.id.clone(), profile.label.clone()))
            .collect::<Vec<_>>();
        profiles.sort_by(|left, right| left.1.cmp(&right.1));
        profiles
    }

    fn office_channel_profile_id(&self) -> Option<&str> {
        self.settings
            .office
            .channel_profile_id
            .as_deref()
            .filter(|profile_id| {
                self.settings
                    .channel_profiles
                    .iter()
                    .any(|profile| profile.id == *profile_id)
            })
            .or_else(|| {
                self.settings
                    .channel_profiles
                    .iter()
                    .find(|profile| profile.id == "times_channels")
                    .map(|profile| profile.id.as_str())
            })
    }

    fn office_channel_profile(&self) -> Option<&ChannelProfile> {
        let profile_id = self.office_channel_profile_id()?;
        self.settings
            .channel_profiles
            .iter()
            .find(|profile| profile.id == profile_id)
    }

    fn office_presence_items(&self) -> Vec<OfficePresence> {
        let Some(profile) = self.office_channel_profile() else {
            return Vec::new();
        };

        let mut latest_by_channel = BTreeMap::<String, TimelineItem>::new();
        let mut activity_counts = BTreeMap::<String, usize>::new();
        let activity_threshold = Utc::now() - chrono::Duration::hours(OFFICE_ACTIVITY_WINDOW_HOURS);

        for item in &self.source_items {
            if !self.settings.can_read_channel(&item.channel_id) {
                continue;
            }
            if !channel_profile_matches_channel(profile, &item.channel_id, &item.channel_name)
                .unwrap_or(false)
            {
                continue;
            }

            latest_by_channel
                .entry(item.channel_id.clone())
                .and_modify(|existing| {
                    if item.message_ts > existing.message_ts {
                        *existing = item.clone();
                    }
                })
                .or_insert_with(|| item.clone());

            if item.last_activity_at >= activity_threshold {
                *activity_counts.entry(item.channel_id.clone()).or_insert(0) += 1;
            }
        }

        let mut presence = latest_by_channel
            .into_values()
            .map(|item| {
                let speaker_id = self
                    .known_channel_creators
                    .get(&item.channel_id)
                    .cloned()
                    .unwrap_or_else(|| item.author_id.clone());
                let speaker_name = self
                    .user_names
                    .get(&speaker_id)
                    .cloned()
                    .filter(|name| !name.is_empty())
                    .unwrap_or_else(|| item.author_name.clone());
                let latest_body = office_message_preview(&item);
                OfficePresence {
                    channel_name: self
                        .channel_name_for(&item.channel_id)
                        .unwrap_or_else(|| item.channel_name.clone()),
                    thread_ts: item.thread_ts.clone(),
                    speaker_name,
                    speaker_avatar_path: self
                        .user_avatar_paths
                        .get(&speaker_id)
                        .cloned()
                        .or_else(|| item.author_avatar_path.clone()),
                    latest_body,
                    activity_count: activity_counts.get(&item.channel_id).copied().unwrap_or(0),
                }
            })
            .collect::<Vec<_>>();

        presence.sort_by(|left, right| {
            right
                .activity_count
                .cmp(&left.activity_count)
                .then(left.channel_name.cmp(&right.channel_name))
        });
        presence
    }

    fn set_active_workspace(&mut self, workspace_key: String) -> bool {
        if self.active_workspace_key() == workspace_key {
            return false;
        }
        self.settings.active_workspace_key = Some(workspace_key);
        self.search_query.clear();
        self.section_filter = None;
        self.channel_filter = None;
        self.author_filter = None;
        self.selected_message_ts = None;
        self.highlighted_timeline_message_ts = None;
        self.focused_thread_message_ts = None;
        true
    }

    fn set_active_search_profile(&mut self, profile_id: Option<String>) -> bool {
        let next_profile = profile_id
            .map(|profile_id| profile_id.trim().to_string())
            .filter(|profile_id| {
                !profile_id.is_empty() && self.settings.search_profile(profile_id).is_some()
            });
        if self.settings.active_search_profile_id == next_profile {
            return false;
        }
        self.settings.active_search_profile_id = next_profile;
        self.apply_filters(true);
        true
    }

    fn upsert_workspace_profile(&mut self, profile: WorkspaceProfile, make_active: bool) -> bool {
        let mut changed = false;
        if let Some(existing) = self
            .settings
            .workspaces
            .iter_mut()
            .find(|existing| existing.key == profile.key)
        {
            if *existing != profile {
                *existing = profile.clone();
                changed = true;
            }
        } else {
            self.settings.workspaces.push(profile.clone());
            changed = true;
        }
        if make_active
            && self.settings.active_workspace_key.as_deref() != Some(profile.key.as_str())
        {
            self.settings.active_workspace_key = Some(profile.key);
            changed = true;
        }
        changed
    }

    fn remove_workspace_profile(&mut self, workspace_key: &str) -> bool {
        let before = self.settings.workspaces.len();
        self.settings
            .workspaces
            .retain(|profile| profile.key != workspace_key);
        let removed = self.settings.workspaces.len() != before;
        if removed && self.active_workspace_key() == workspace_key {
            self.settings.active_workspace_key = self
                .settings
                .workspaces
                .first()
                .map(|profile| profile.key.clone());
        }
        removed
    }

    fn visible_items(&self) -> &[RankedTimelineItem] {
        &self.filtered_ranked_items
    }

    fn loaded_visible_items(&self) -> &[RankedTimelineItem] {
        &self.filtered_ranked_items[..self
            .loaded_timeline_items
            .min(self.filtered_ranked_items.len())]
    }

    fn set_theme(&mut self, theme_id: ThemeId) -> bool {
        if self.settings.theme_id == theme_id {
            return false;
        }

        self.settings.theme_id = theme_id;
        true
    }

    fn replace_items(
        &mut self,
        items: Vec<TimelineItem>,
        user_names: BTreeMap<String, String>,
        user_avatar_paths: BTreeMap<String, String>,
        known_channels: BTreeMap<String, String>,
        known_channel_creators: BTreeMap<String, String>,
    ) {
        self.user_names = merge_user_names(&items, user_names);
        self.user_avatar_paths = merge_user_avatar_paths(&items, user_avatar_paths);
        self.known_channels = merge_known_channels(&items, known_channels);
        self.known_channel_creators = merge_known_channel_creators(&items, known_channel_creators);
        self.typing_by_channel.clear();
        self.source_items = items;
        if self
            .channel_filter
            .as_ref()
            .is_some_and(|channel_id| !self.known_channels.contains_key(channel_id))
        {
            self.channel_filter = None;
        }
        if self.author_filter.as_ref().is_some_and(|author_id| {
            !self
                .source_items
                .iter()
                .any(|item| &item.author_id == author_id)
        }) {
            self.author_filter = None;
        }
        if self
            .section_filter
            .as_ref()
            .is_some_and(|section_id| self.settings.section_profile(section_id).is_none())
        {
            self.section_filter = None;
        }
        if self
            .settings
            .active_search_profile_id
            .as_ref()
            .is_some_and(|profile_id| self.settings.search_profile(profile_id).is_none())
        {
            self.settings.active_search_profile_id = None;
        }
        self.rerank(true);
    }

    fn search_query(&self) -> &str {
        &self.search_query
    }

    fn section_filter(&self) -> Option<&str> {
        self.section_filter.as_deref()
    }

    fn channel_filter(&self) -> Option<&str> {
        self.channel_filter.as_deref()
    }

    fn author_filter(&self) -> Option<&str> {
        self.author_filter.as_deref()
    }

    fn set_search_query(&mut self, search_query: String) {
        self.search_query = search_query;
        self.apply_filters(true);
    }

    fn set_section_filter(&mut self, section_id: Option<String>) {
        self.section_filter = section_id
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty() && self.settings.section_profile(value).is_some());
        self.apply_filters(true);
    }

    fn set_channel_filter(&mut self, channel_id: Option<String>) {
        self.channel_filter = channel_id.filter(|value| !value.is_empty());
        self.apply_filters(true);
    }

    fn set_author_filter(&mut self, author_id: Option<String>) {
        self.author_filter = author_id.filter(|value| !value.is_empty());
        self.apply_filters(true);
    }

    fn has_cached_items(&self) -> bool {
        !self.source_items.is_empty()
    }

    fn needs_user_directory_refresh(&self) -> bool {
        !self.source_items.is_empty()
            && self.source_items.iter().any(|item| {
                (item.author_name == item.author_id && item.author_name != "you")
                    || item.author_avatar_path.is_none()
                    || item.attachments.iter().any(attachment_image_needs_refresh)
            })
    }

    fn needs_channel_directory_refresh(&self) -> bool {
        !self.source_items.is_empty()
            && self.source_items.iter().any(|item| {
                item.channel_name.trim().is_empty() || item.channel_name == item.channel_id
            })
    }

    fn load_more_timeline_items(&mut self) -> bool {
        if self.loaded_timeline_items >= self.filtered_ranked_items.len() {
            return false;
        }

        self.loaded_timeline_items =
            (self.loaded_timeline_items + TIMELINE_PAGE_SIZE).min(self.filtered_ranked_items.len());
        true
    }

    fn rerank(&mut self, reset_paging: bool) {
        let ranker = TimelineRanker::new(Utc::now());
        self.ranked_items = ranker.visible_items(
            self.mode,
            &self.settings.timeline,
            self.source_items
                .iter()
                .filter(|item| self.settings.can_read_channel(&item.channel_id))
                .filter(|item| item.message_ts == item.thread_ts)
                .cloned(),
        );

        self.apply_filters(reset_paging);
    }

    fn apply_filters(&mut self, reset_paging: bool) {
        let search_query = self.search_query.trim().to_lowercase();
        let active_search_profile_id = self.active_search_profile_id().map(str::to_string);
        self.filtered_ranked_items = self
            .ranked_items
            .iter()
            .filter(|ranked| {
                self.section_filter.as_ref().is_none_or(|section_id| {
                    self.settings
                        .section_profile(section_id)
                        .is_some_and(|section| section.channels.contains(&ranked.item.channel_id))
                }) && self
                    .channel_filter
                    .as_ref()
                    .is_none_or(|channel_id| &ranked.item.channel_id == channel_id)
                    && self
                        .author_filter
                        .as_ref()
                        .is_none_or(|author_id| &ranked.item.author_id == author_id)
                    && active_search_profile_id.as_ref().is_none_or(|profile_id| {
                        match self
                            .settings
                            .search_profile_matches(profile_id.as_str(), &ranked.item)
                        {
                            Ok(matches) => matches,
                            Err(error) => {
                                eprintln!(
                                    "[slaxide] search profile `{}` failed: {}",
                                    profile_id, error
                                );
                                false
                            }
                        }
                    })
                    && (search_query.is_empty()
                        || ranked.item.body.to_lowercase().contains(&search_query)
                        || ranked
                            .item
                            .author_name
                            .to_lowercase()
                            .contains(&search_query)
                        || ranked
                            .item
                            .channel_name
                            .to_lowercase()
                            .contains(&search_query))
            })
            .cloned()
            .collect();

        let target_visible = if reset_paging {
            TIMELINE_PAGE_SIZE
        } else {
            self.loaded_timeline_items.max(TIMELINE_PAGE_SIZE)
        };
        self.loaded_timeline_items = self.filtered_ranked_items.len().min(target_visible);

        let selected_still_visible = self.selected_message_ts.as_ref().is_some_and(|message_ts| {
            self.filtered_ranked_items
                .iter()
                .any(|item| &item.item.message_ts == message_ts)
        });

        if !selected_still_visible {
            self.selected_message_ts = self
                .filtered_ranked_items
                .first()
                .map(|item| item.item.message_ts.clone());
        }
        if self
            .highlighted_timeline_message_ts
            .as_ref()
            .is_some_and(|message_ts| {
                !self
                    .filtered_ranked_items
                    .iter()
                    .any(|item| &item.item.message_ts == message_ts)
            })
        {
            self.highlighted_timeline_message_ts = None;
        }
        if self
            .focused_thread_message_ts
            .as_ref()
            .is_some_and(|message_ts| {
                !self
                    .source_items
                    .iter()
                    .any(|item| &item.message_ts == message_ts)
            })
        {
            self.focused_thread_message_ts = self.selected_message_ts.clone();
        }
    }

    fn select(&mut self, message_ts: Option<String>) {
        self.selected_message_ts = message_ts.filter(|selected| {
            self.ranked_items
                .iter()
                .any(|item| item.item.message_ts == *selected)
        });
        self.highlighted_timeline_message_ts = None;
        self.focused_thread_message_ts = self.selected_message_ts.clone();
    }

    fn available_channels(&self) -> Vec<(String, String)> {
        let mut channels = self
            .known_channels
            .iter()
            .filter(|(channel_id, _)| self.settings.can_read_channel(channel_id))
            .map(|(channel_id, channel_name)| (channel_id.clone(), channel_name.clone()))
            .into_iter()
            .collect::<Vec<_>>();
        channels.sort_by(|left, right| left.1.cmp(&right.1));
        channels
    }

    fn available_post_channels(&self) -> Vec<(String, String)> {
        self.available_channels()
            .into_iter()
            .filter(|(channel_id, _)| self.settings.can_write_channel(channel_id))
            .collect()
    }

    fn background_refresh_channels(&self, limit: usize) -> Vec<(String, String)> {
        let mut channels = self
            .loaded_visible_items()
            .iter()
            .map(|ranked| {
                (
                    ranked.item.channel_id.clone(),
                    ranked.item.channel_name.clone(),
                )
            })
            .collect::<BTreeMap<_, _>>()
            .into_iter()
            .collect::<Vec<_>>();
        if let Some(profile) = self.office_channel_profile() {
            for item in &self.source_items {
                if !self.settings.can_read_channel(&item.channel_id) {
                    continue;
                }
                if !channel_profile_matches_channel(profile, &item.channel_id, &item.channel_name)
                    .unwrap_or(false)
                {
                    continue;
                }
                if channels
                    .iter()
                    .any(|(channel_id, _)| channel_id == &item.channel_id)
                {
                    continue;
                }
                channels.push((item.channel_id.clone(), item.channel_name.clone()));
            }
        }
        if channels.is_empty() {
            channels = self.available_channels();
        }
        channels.truncate(limit);
        channels
    }

    fn available_authors(&self) -> Vec<(String, String)> {
        let mut authors = self
            .source_items
            .iter()
            .filter(|item| self.settings.can_read_channel(&item.channel_id))
            .filter(|item| item.message_ts == item.thread_ts)
            .map(|item| (item.author_id.clone(), item.author_name.clone()))
            .collect::<BTreeMap<_, _>>()
            .into_iter()
            .collect::<Vec<_>>();
        authors.sort_by(|left, right| left.1.cmp(&right.1));
        authors
    }

    fn available_members(&self) -> Vec<(String, String)> {
        let mut members = self.user_names.clone();
        for item in &self.source_items {
            if item.author_name != "you" {
                members
                    .entry(item.author_id.clone())
                    .or_insert_with(|| item.author_name.clone());
            }
        }
        let mut members = members.into_iter().collect::<Vec<_>>();
        members.sort_by(|left, right| left.1.cmp(&right.1));
        members
    }

    #[cfg(test)]
    fn selected_item(&self) -> Option<RankedTimelineItem> {
        self.selected_message_ts.as_ref().and_then(|selected| {
            self.ranked_items
                .iter()
                .find(|item| &item.item.message_ts == selected)
                .cloned()
        })
    }

    fn root_item(&self, thread_ts: &str) -> Option<TimelineItem> {
        self.source_items
            .iter()
            .find(|item| item.message_ts == thread_ts)
            .cloned()
    }

    fn contains_message_ts(&self, message_ts: &str) -> bool {
        self.source_items
            .iter()
            .any(|item| item.message_ts == message_ts)
    }

    fn item_by_channel_and_message_ts(
        &self,
        channel_id: &str,
        message_ts: &str,
    ) -> Option<TimelineItem> {
        self.source_items
            .iter()
            .find(|item| item.channel_id == channel_id && item.message_ts == message_ts)
            .cloned()
    }

    fn channel_name_for(&self, channel_id: &str) -> Option<String> {
        self.known_channels.get(channel_id).cloned().or_else(|| {
            self.source_items
                .iter()
                .find(|item| item.channel_id == channel_id)
                .map(|item| item.channel_name.clone())
        })
    }

    fn upsert_known_channel(&mut self, channel_id: String, channel_name: String) {
        if channel_name.trim().is_empty() {
            return;
        }
        self.known_channels.insert(channel_id, channel_name);
    }

    fn rename_known_channel(&mut self, channel_id: &str, channel_name: &str) {
        if channel_name.trim().is_empty() {
            return;
        }
        self.known_channels
            .insert(channel_id.to_string(), channel_name.to_string());
        for item in &mut self.source_items {
            if item.channel_id == channel_id {
                item.channel_name = channel_name.to_string();
            }
        }
        self.rerank(false);
    }

    fn archive_known_channel(&mut self, channel_id: &str) {
        self.known_channels.remove(channel_id);
        self.source_items
            .retain(|item| item.channel_id != channel_id);
        self.typing_by_channel.remove(channel_id);
        if self.channel_filter.as_deref() == Some(channel_id) {
            self.channel_filter = None;
        }
        self.rerank(true);
    }

    fn cleanup_expired_typing(&mut self) -> bool {
        let now = Instant::now();
        let before = self.typing_by_channel.values().map(Vec::len).sum::<usize>();
        self.typing_by_channel.retain(|_, indicators| {
            indicators.retain(|indicator| indicator.expires_at > now);
            !indicators.is_empty()
        });
        let after = self.typing_by_channel.values().map(Vec::len).sum::<usize>();
        before != after
    }

    fn note_user_typing(&mut self, channel_id: &str, user_id: &str) -> bool {
        let indicators = self
            .typing_by_channel
            .entry(channel_id.to_string())
            .or_default();
        let expires_at = Instant::now() + TYPING_INDICATOR_TTL;
        if let Some(indicator) = indicators
            .iter_mut()
            .find(|indicator| indicator.user_id == user_id)
        {
            let changed = indicator.expires_at < expires_at;
            indicator.expires_at = expires_at;
            return changed;
        }

        indicators.push(TypingIndicator {
            user_id: user_id.to_string(),
            expires_at,
        });
        true
    }

    fn typing_summary_for_channel(&self, channel_id: &str) -> Option<String> {
        let indicators = self.typing_by_channel.get(channel_id)?;
        let mut names = indicators
            .iter()
            .map(|indicator| self.author_name_for(&indicator.user_id, false))
            .filter(|name| name != "you")
            .collect::<Vec<_>>();
        names.sort();
        names.dedup();
        match names.as_slice() {
            [] => None,
            [one] => Some(format!("{one} is typing...")),
            [left, right] => Some(format!("{left} and {right} are typing...")),
            [first, second, ..] => Some(format!("{first}, {second}, and others are typing...")),
        }
    }

    fn author_name_for(&self, author_id: &str, participant: bool) -> String {
        if participant {
            return "you".to_string();
        }

        self.user_names
            .get(author_id)
            .cloned()
            .or_else(|| {
                self.source_items
                    .iter()
                    .find(|item| item.author_id == author_id && item.author_name != item.author_id)
                    .map(|item| item.author_name.clone())
            })
            .unwrap_or_else(|| author_id.to_string())
    }

    fn author_avatar_path_for(&self, author_id: &str) -> Option<String> {
        self.user_avatar_paths.get(author_id).cloned().or_else(|| {
            self.source_items
                .iter()
                .find(|item| item.author_id == author_id)
                .and_then(|item| item.author_avatar_path.clone())
        })
    }

    fn ranked_item_for_thread(&self, thread_ts: &str) -> Option<RankedTimelineItem> {
        self.ranked_items
            .iter()
            .find(|item| item.item.thread_ts == thread_ts)
            .cloned()
    }

    fn thread_items(&self, thread_ts: &str) -> Vec<TimelineItem> {
        let mut items = self
            .source_items
            .iter()
            .filter(|item| item.thread_ts == thread_ts)
            .cloned()
            .collect::<Vec<_>>();
        items.sort_by(|left, right| left.message_ts.cmp(&right.message_ts));
        items
    }

    fn thread_reply_count(&self, thread_ts: &str) -> usize {
        self.source_items
            .iter()
            .filter(|item| item.thread_ts == thread_ts && item.message_ts != thread_ts)
            .count()
    }

    fn focused_thread_message_ts(&self) -> Option<&str> {
        self.focused_thread_message_ts.as_deref()
    }

    fn focus_message(&mut self, message_ts: &str) -> Option<TimelineItem> {
        let target = self
            .source_items
            .iter()
            .find(|item| item.message_ts == message_ts)
            .cloned()?;

        self.search_query.clear();
        self.section_filter = None;
        self.channel_filter = None;
        self.author_filter = None;
        self.settings.active_search_profile_id = None;
        self.apply_filters(true);

        self.selected_message_ts = Some(target.thread_ts.clone());
        self.highlighted_timeline_message_ts = Some(target.thread_ts.clone());
        self.focused_thread_message_ts = Some(target.message_ts.clone());
        if let Some(index) = self
            .filtered_ranked_items
            .iter()
            .position(|item| item.item.message_ts == target.thread_ts)
        {
            self.loaded_timeline_items = self.loaded_timeline_items.max(index + 1);
        }

        Some(target)
    }

    fn apply_reply_item(&mut self, reply_item: TimelineItem) {
        let thread_ts = reply_item.thread_ts.clone();
        let last_activity_at = reply_item.last_activity_at;
        self.upsert_known_channel(
            reply_item.channel_id.clone(),
            reply_item.channel_name.clone(),
        );
        if reply_item.author_name != "you" && reply_item.author_name != reply_item.author_id {
            self.user_names
                .entry(reply_item.author_id.clone())
                .or_insert_with(|| reply_item.author_name.clone());
        }
        if let Some(avatar_path) = reply_item.author_avatar_path.as_ref() {
            self.user_avatar_paths
                .entry(reply_item.author_id.clone())
                .or_insert_with(|| avatar_path.clone());
        }
        self.source_items.push(reply_item);
        for item in &mut self.source_items {
            if item.thread_ts == thread_ts {
                item.last_activity_at = last_activity_at;
                item.participant = true;
            }
        }

        self.rerank(false);
    }

    fn apply_incoming_item(&mut self, incoming_item: TimelineItem) -> bool {
        if self.contains_message_ts(&incoming_item.message_ts) {
            return false;
        }

        let channel_id = incoming_item.channel_id.clone();
        let thread_ts = incoming_item.thread_ts.clone();
        let last_activity_at = incoming_item.last_activity_at;
        let direct_mention = incoming_item.direct_mention;
        let mut focus_keyword_hits = incoming_item.focus_keyword_hits.clone();
        let unread = incoming_item.unread;
        let is_root = incoming_item.message_ts == incoming_item.thread_ts;
        if incoming_item.author_name != incoming_item.author_id
            && incoming_item.author_name != "you"
        {
            self.user_names
                .entry(incoming_item.author_id.clone())
                .or_insert_with(|| incoming_item.author_name.clone());
        }
        if let Some(avatar_path) = incoming_item.author_avatar_path.as_ref() {
            self.user_avatar_paths
                .entry(incoming_item.author_id.clone())
                .or_insert_with(|| avatar_path.clone());
        }
        self.upsert_known_channel(channel_id.clone(), incoming_item.channel_name.clone());
        self.source_items.push(incoming_item);
        self.typing_by_channel.remove(&channel_id);

        if !is_root {
            for item in &mut self.source_items {
                if item.message_ts != thread_ts {
                    continue;
                }

                item.last_activity_at = last_activity_at;
                item.unread |= unread;
                item.direct_mention |= direct_mention;
                for keyword in focus_keyword_hits.drain(..) {
                    if !item.focus_keyword_hits.contains(&keyword) {
                        item.focus_keyword_hits.push(keyword);
                    }
                }
                break;
            }
        }

        self.rerank(false);
        true
    }

    fn apply_message_edit(&mut self, message_ts: &str, body: &str) -> bool {
        let mut updated = false;
        for item in &mut self.source_items {
            if item.message_ts == message_ts {
                item.body = body.to_string();
                item.rich_text_blocks.clear();
                updated = true;
            }
        }
        if updated {
            self.rerank(false);
        }
        updated
    }

    fn apply_message_delete(&mut self, message_ts: &str) -> bool {
        let Some(target) = self
            .source_items
            .iter()
            .find(|item| item.message_ts == message_ts)
            .cloned()
        else {
            return false;
        };

        if target.message_ts == target.thread_ts {
            self.source_items
                .retain(|item| item.thread_ts != target.thread_ts);
        } else {
            self.source_items
                .retain(|item| item.message_ts != message_ts);
            let thread_ts = target.thread_ts.clone();
            let latest_activity = self
                .source_items
                .iter()
                .filter(|item| item.thread_ts == thread_ts)
                .map(|item| item.last_activity_at)
                .max()
                .unwrap_or(target.last_activity_at);
            for item in &mut self.source_items {
                if item.thread_ts == thread_ts {
                    item.last_activity_at = latest_activity;
                }
            }
        }

        if self.focused_thread_message_ts.as_deref() == Some(message_ts) {
            self.focused_thread_message_ts = Some(target.thread_ts.clone());
        }
        if self.selected_message_ts.as_deref() == Some(message_ts) {
            self.selected_message_ts = None;
        }
        self.rerank(true);
        true
    }

    fn apply_reaction_added(
        &mut self,
        message_ts: &str,
        reaction_name: &str,
        reactor_user_id: Option<&str>,
        current_user_id: Option<&str>,
    ) -> bool {
        let normalized_name =
            normalize_reaction_name(reaction_name).unwrap_or_else(|| reaction_name.to_string());
        let reactor_is_current_user = reactor_user_id
            .zip(current_user_id)
            .is_some_and(|(left, right)| left == right);
        let mut updated = false;

        for item in &mut self.source_items {
            if item.message_ts != message_ts {
                continue;
            }

            if let Some(existing) = item.reactions.iter_mut().find(|entry| {
                normalize_reaction_name(&entry.name).as_deref() == Some(&normalized_name)
            }) {
                if !(reactor_is_current_user && existing.me) {
                    existing.count = existing.count.saturating_add(1);
                }
                existing.me |= reactor_is_current_user;
                existing.emoji = reaction_emoji(&normalized_name);
            } else {
                item.reactions.push(ReactionSummary {
                    name: normalized_name.clone(),
                    emoji: reaction_emoji(&normalized_name),
                    count: 1,
                    me: reactor_is_current_user,
                });
            }
            item.reactions
                .sort_by(|left, right| right.count.cmp(&left.count));
            updated = true;
        }

        if updated {
            self.rerank(false);
        }

        updated
    }

    fn apply_reaction_removed(
        &mut self,
        message_ts: &str,
        reaction_name: &str,
        reactor_user_id: Option<&str>,
        current_user_id: Option<&str>,
    ) -> bool {
        let normalized_name =
            normalize_reaction_name(reaction_name).unwrap_or_else(|| reaction_name.to_string());
        let reactor_is_current_user = reactor_user_id
            .zip(current_user_id)
            .is_some_and(|(left, right)| left == right);
        let mut updated = false;

        for item in &mut self.source_items {
            if item.message_ts != message_ts {
                continue;
            }

            let Some(index) = item.reactions.iter().position(|entry| {
                normalize_reaction_name(&entry.name).as_deref() == Some(&normalized_name)
            }) else {
                continue;
            };

            let existing = &mut item.reactions[index];
            if reactor_is_current_user {
                existing.me = false;
            }
            existing.count = existing.count.saturating_sub(1);
            if existing.count == 0 {
                if existing.me {
                    existing.count = 1;
                } else {
                    item.reactions.remove(index);
                }
            }
            item.reactions
                .sort_by(|left, right| right.count.cmp(&left.count));
            updated = true;
        }

        if updated {
            self.rerank(false);
        }

        updated
    }

    fn apply_remote_message_snapshot(
        &mut self,
        message_ts: &str,
        body: &str,
        rich_text_blocks: Vec<serde_json::Value>,
        reactions: Vec<ReactionSummary>,
        attachments: Vec<AttachmentSummary>,
    ) -> bool {
        let mut updated = false;
        for item in &mut self.source_items {
            if item.message_ts != message_ts {
                continue;
            }
            item.body = body.to_string();
            item.rich_text_blocks = rich_text_blocks.clone();
            item.reactions = reactions.clone();
            item.attachments = attachments.clone();
            updated = true;
        }

        if updated {
            self.rerank(false);
        }

        updated
    }

    fn apply_background_refresh_items(&mut self, items: Vec<TimelineItem>) -> bool {
        let mut changed = false;
        for item in items {
            if self.contains_message_ts(&item.message_ts) {
                changed |= self.apply_remote_message_snapshot(
                    &item.message_ts,
                    &item.body,
                    item.rich_text_blocks.clone(),
                    item.reactions.clone(),
                    item.attachments.clone(),
                );
            } else {
                changed |= self.apply_incoming_item(item);
            }
        }
        changed
    }
}

enum AuthEvent {
    Completed {
        workspace_key: String,
        result: std::result::Result<StoredSlackSession, String>,
    },
    HistoryLoaded {
        workspace_key: String,
        result: std::result::Result<InitialHistoryLoad, String>,
    },
    ChannelPostSent {
        workspace_key: String,
        result: std::result::Result<TimelineItem, String>,
    },
    ReplySent {
        workspace_key: String,
        result: std::result::Result<TimelineItem, String>,
    },
    ReactionAdded {
        workspace_key: String,
        result: std::result::Result<ReactionUpdate, String>,
    },
    MessageEdited {
        workspace_key: String,
        result: std::result::Result<MessageEditUpdate, String>,
    },
    MessageDeleted {
        workspace_key: String,
        result: std::result::Result<MessageDeleteUpdate, String>,
    },
    AdminActionFinished {
        workspace_key: String,
        result: std::result::Result<AdminActionUpdate, String>,
    },
    ShareLinkResolved {
        workspace_key: String,
        result: std::result::Result<ShareLinkUpdate, String>,
    },
    BackgroundRefreshLoaded {
        workspace_key: String,
        result: std::result::Result<Vec<TimelineItem>, String>,
    },
    NotificationActivated(String),
    LiveEvent(SlackSocketEvent),
    LiveSyncFailed(String),
}

#[derive(Clone, Debug)]
struct ReactionUpdate {
    message_ts: String,
    reaction_name: String,
    reactor_user_id: Option<String>,
}

#[derive(Clone, Debug)]
struct ShareLinkUpdate {
    permalink: String,
}

#[derive(Clone, Debug)]
struct PendingWorkspaceLogin {
    workspace_key: String,
    login: PendingSlackLogin,
}

#[derive(Clone, Debug)]
struct InitialHistoryLoad {
    items: Vec<TimelineItem>,
    channel_count: usize,
    user_names: BTreeMap<String, String>,
    user_avatar_paths: BTreeMap<String, String>,
    known_channels: BTreeMap<String, String>,
    known_channel_creators: BTreeMap<String, String>,
    missing_user_names_scope: bool,
}

#[derive(Clone, Debug, Default)]
struct UserDirectoryLoad {
    user_names: BTreeMap<String, String>,
    user_avatar_paths: BTreeMap<String, String>,
}

#[derive(Clone, Debug)]
struct MessageEditUpdate {
    message_ts: String,
    body: String,
}

#[derive(Clone, Debug)]
struct MessageDeleteUpdate {
    message_ts: String,
}

#[derive(Clone, Debug)]
struct AdminActionUpdate {
    message: String,
    created_channel: Option<(String, String)>,
    renamed_channel: Option<(String, String)>,
    archived_channel_id: Option<String>,
}

#[derive(Clone, Debug)]
struct TypingIndicator {
    user_id: String,
    expires_at: Instant,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum DeckColumn {
    Settings,
    Admin,
    ConfigEditor,
    Thread { thread_ts: String },
}

#[derive(Clone, Debug, Default)]
struct DeckState {
    columns: Vec<DeckColumn>,
}

impl DeckState {
    fn open_settings(&mut self) {
        if !self.columns.contains(&DeckColumn::Settings) {
            self.columns.push(DeckColumn::Settings);
        }
    }

    fn open_thread(&mut self, thread_ts: String) {
        self.columns.push(DeckColumn::Thread { thread_ts });
    }

    fn open_admin(&mut self) {
        if !self.columns.contains(&DeckColumn::Admin) {
            self.columns.push(DeckColumn::Admin);
        }
    }

    fn open_config_editor(&mut self) {
        if !self.columns.contains(&DeckColumn::ConfigEditor) {
            self.columns.push(DeckColumn::ConfigEditor);
        }
    }

    fn close_config_editor(&mut self) {
        if let Some(index) = self
            .columns
            .iter()
            .position(|column| *column == DeckColumn::ConfigEditor)
        {
            self.columns.remove(index);
        }
    }

    fn ensure_thread_open(&mut self, thread_ts: String) {
        if self.columns.iter().any(|column| {
            matches!(column, DeckColumn::Thread { thread_ts: existing } if existing == &thread_ts)
        }) {
            return;
        }
        self.columns.push(DeckColumn::Thread { thread_ts });
    }

    fn close(&mut self, index: usize) {
        if index < self.columns.len() {
            self.columns.remove(index);
        }
    }

    fn has_settings(&self) -> bool {
        self.columns.contains(&DeckColumn::Settings)
    }

    fn has_admin(&self) -> bool {
        self.columns.contains(&DeckColumn::Admin)
    }

    fn has_config_editor(&self) -> bool {
        self.columns.contains(&DeckColumn::ConfigEditor)
    }
}

#[derive(Clone)]
struct UiHandles {
    timeline_store: gio::ListStore,
    deck_columns: GtkBox,
    startup_status: Label,
    timeline_button: Button,
    office_button: Button,
    admin_button: Button,
    settings_button: Button,
    timeline_title: Label,
    filter_bar: GtkBox,
    composer_bar: GtkBox,
    office_summary: Label,
    timeline_scroll: ScrolledWindow,
    office_scene: gtk::Fixed,
    office_scroll: ScrolledWindow,
    search_entry: SearchEntry,
    search_profile_filter: ComboBoxText,
    section_filter: ComboBoxText,
    channel_filter: ComboBoxText,
    author_filter: ComboBoxText,
    composer_channel: ComboBoxText,
    composer_entry: Entry,
    composer_send_button: Button,
    composer_typing_label: Label,
}

#[derive(Clone)]
struct UiRuntime {
    bootstrap: Rc<AppBootstrap>,
    state: Rc<RefCell<UiState>>,
    deck_state: Rc<RefCell<DeckState>>,
    ui_sync_suppressed: Rc<RefCell<bool>>,
    config_editor_widget: Rc<RefCell<Option<GtkBox>>>,
    env_locked_keys: Rc<BTreeSet<String>>,
    auth_controller: Arc<SlackAuthController<KeyringSecretStore>>,
    auth_state: Rc<RefCell<SlackAuthStatus>>,
    pending_login: Rc<RefCell<Option<PendingWorkspaceLogin>>>,
    auth_tx: mpsc::Sender<AuthEvent>,
    provider: gtk::CssProvider,
    notifier: Arc<NotifyRustBackend>,
    live_sync_generation: Arc<AtomicU64>,
    live_sync_started_generation: Rc<RefCell<Option<u64>>>,
    background_refresh_in_flight: Rc<RefCell<bool>>,
    animated_threads: Rc<RefCell<BTreeSet<String>>>,
}

fn build_ui(app: &Application) {
    eprintln!("[slaxide] activate");
    let bootstrap = Rc::new(AppBootstrap::initialize());
    let (initial_state, startup_status) = bootstrap.load_state();
    let state = Rc::new(RefCell::new(initial_state));
    let deck_state = Rc::new(RefCell::new(DeckState::default()));
    let ui_sync_suppressed = Rc::new(RefCell::new(false));
    let config_editor_widget = Rc::new(RefCell::new(None));
    let env_locked_keys = Rc::new(externally_provided_env_keys());
    sync_settings_env(&state.borrow().settings, &env_locked_keys);
    let auth_controller = Arc::new(SlackAuthController::new(KeyringSecretStore::new(APP_ID)));
    let initial_workspace_key = state.borrow().active_workspace_key().to_string();
    eprintln!("[slaxide] auth: loading initial session");
    let auth_state = Rc::new(RefCell::new(load_initial_auth_status(
        auth_controller.clone(),
        &initial_workspace_key,
    )));
    let pending_login = Rc::new(RefCell::new(None::<PendingWorkspaceLogin>));
    let (auth_tx, auth_rx) = mpsc::channel::<AuthEvent>();
    let provider = gtk::CssProvider::new();
    let notifier = Arc::new(NotifyRustBackend::new().with_activation_handler({
        let auth_tx = auth_tx.clone();
        move |message_ts| {
            let _ = auth_tx.send(AuthEvent::NotificationActivated(message_ts));
        }
    }));
    let live_sync_generation = Arc::new(AtomicU64::new(0));
    let live_sync_started_generation = Rc::new(RefCell::new(None));
    let background_refresh_in_flight = Rc::new(RefCell::new(false));
    let animated_threads = Rc::new(RefCell::new(BTreeSet::new()));
    apply_theme(&provider, state.borrow().theme_id());

    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }

    let window = ApplicationWindow::builder()
        .application(app)
        .title("Slaxide")
        .default_width(1640)
        .default_height(920)
        .build();
    if env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some() {
        window.set_decorated(false);
    }

    let root = GtkBox::new(Orientation::Horizontal, 0);
    root.add_css_class("app-root");

    let rail = GtkBox::new(Orientation::Vertical, 10);
    rail.add_css_class("icon-rail");
    rail.set_width_request(72);

    let brand = Label::new(Some("S"));
    brand.add_css_class("title-2");
    brand.add_css_class("brand-mark");
    brand.set_xalign(0.5);
    rail.append(&brand);

    let timeline_button = build_icon_button("view-list-symbolic", "Recent timeline");
    let office_button = build_icon_button("network-workgroup-symbolic", "Virtual office");
    let admin_button = build_icon_button("emblem-system-symbolic", "Open admin tools");
    let settings_button = build_icon_button("preferences-system-symbolic", "Open settings");
    rail.append(&timeline_button);
    rail.append(&office_button);
    rail.append(&admin_button);
    let spacer = GtkBox::new(Orientation::Vertical, 0);
    spacer.set_vexpand(true);
    rail.append(&spacer);
    rail.append(&settings_button);

    let deck_scroll = ScrolledWindow::new();
    deck_scroll.set_hexpand(true);
    deck_scroll.set_vexpand(true);
    deck_scroll.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Never);

    let deck = GtkBox::new(Orientation::Horizontal, 14);

    let timeline_column = GtkBox::new(Orientation::Vertical, 12);
    timeline_column.add_css_class("deck-column");
    timeline_column.add_css_class("timeline-column");
    timeline_column.set_width_request(520);

    let timeline_header_wrap = GtkBox::new(Orientation::Vertical, 10);
    timeline_header_wrap.add_css_class("timeline-head");
    let timeline_header = GtkBox::new(Orientation::Horizontal, 8);
    let timeline_title = Label::new(Some("Recent"));
    timeline_title.add_css_class("title-2");
    timeline_title.set_xalign(0.0);
    let timeline_spacer = GtkBox::new(Orientation::Horizontal, 0);
    timeline_spacer.set_hexpand(true);
    timeline_header.append(&timeline_title);
    timeline_header.append(&timeline_spacer);

    let search = SearchEntry::builder()
        .placeholder_text("Search messages")
        .build();
    search.set_hexpand(true);
    let search_profile_filter = ComboBoxText::new();
    search_profile_filter.add_css_class("filter-select");
    let section_filter = ComboBoxText::new();
    section_filter.add_css_class("filter-select");
    let channel_filter = ComboBoxText::new();
    channel_filter.add_css_class("filter-select");
    let author_filter = ComboBoxText::new();
    author_filter.add_css_class("filter-select");
    let composer_channel = ComboBoxText::new();
    composer_channel.add_css_class("filter-select");
    let composer_entry = Entry::builder()
        .placeholder_text("Post a new message")
        .build();
    composer_entry.set_hexpand(true);
    let composer_send_button = Button::with_label("Send");
    composer_send_button.add_css_class("suggested-action");

    let filter_bar = GtkBox::new(Orientation::Horizontal, 8);
    filter_bar.add_css_class("timeline-filterbar");
    filter_bar.append(&search);
    filter_bar.append(&search_profile_filter);
    filter_bar.append(&section_filter);
    filter_bar.append(&channel_filter);
    filter_bar.append(&author_filter);

    let composer_bar = GtkBox::new(Orientation::Horizontal, 8);
    composer_bar.add_css_class("timeline-filterbar");
    composer_bar.append(&composer_channel);
    composer_bar.append(&composer_entry);
    composer_bar.append(&composer_send_button);
    let composer_typing_label = Label::new(None);
    composer_typing_label.add_css_class("meta");
    composer_typing_label.set_xalign(0.0);
    let office_summary = Label::new(None);
    office_summary.add_css_class("meta");
    office_summary.set_wrap(true);
    office_summary.set_xalign(0.0);
    office_summary.set_visible(false);

    let startup_status = Label::new(Some(&startup_status));
    startup_status.add_css_class("meta");
    startup_status.set_wrap(true);
    startup_status.set_xalign(0.0);

    timeline_header_wrap.append(&timeline_header);
    timeline_header_wrap.append(&filter_bar);
    timeline_header_wrap.append(&composer_bar);
    timeline_header_wrap.append(&composer_typing_label);
    timeline_header_wrap.append(&office_summary);

    let timeline_store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let list_scroll = ScrolledWindow::new();
    list_scroll.set_vexpand(true);
    list_scroll.set_hexpand(true);
    let office_scene = gtk::Fixed::new();
    office_scene.add_css_class("office-scene");
    office_scene.set_size_request(OFFICE_SCENE_WIDTH_PX, OFFICE_SCENE_HEIGHT_PX);
    let office_scroll = ScrolledWindow::new();
    office_scroll.set_vexpand(true);
    office_scroll.set_hexpand(true);
    office_scroll.set_visible(false);
    office_scroll.set_child(Some(&office_scene));

    timeline_column.append(&timeline_header_wrap);
    timeline_column.append(&list_scroll);
    timeline_column.append(&office_scroll);

    let deck_columns = GtkBox::new(Orientation::Horizontal, 14);
    deck.append(&timeline_column);
    deck.append(&deck_columns);
    deck_scroll.set_child(Some(&deck));

    root.append(&rail);
    root.append(&deck_scroll);
    window.set_child(Some(&root));

    let handles = UiHandles {
        timeline_store: timeline_store.clone(),
        deck_columns: deck_columns.clone(),
        startup_status: startup_status.clone(),
        timeline_button: timeline_button.clone(),
        office_button: office_button.clone(),
        admin_button: admin_button.clone(),
        settings_button: settings_button.clone(),
        timeline_title: timeline_title.clone(),
        filter_bar: filter_bar.clone(),
        composer_bar: composer_bar.clone(),
        office_summary: office_summary.clone(),
        timeline_scroll: list_scroll.clone(),
        office_scene: office_scene.clone(),
        office_scroll: office_scroll.clone(),
        search_entry: search.clone(),
        search_profile_filter: search_profile_filter.clone(),
        section_filter: section_filter.clone(),
        channel_filter: channel_filter.clone(),
        author_filter: author_filter.clone(),
        composer_channel: composer_channel.clone(),
        composer_entry: composer_entry.clone(),
        composer_send_button: composer_send_button.clone(),
        composer_typing_label: composer_typing_label.clone(),
    };
    let runtime = UiRuntime {
        bootstrap: bootstrap.clone(),
        state: state.clone(),
        deck_state: deck_state.clone(),
        ui_sync_suppressed: ui_sync_suppressed.clone(),
        config_editor_widget: config_editor_widget.clone(),
        env_locked_keys: env_locked_keys.clone(),
        auth_controller: auth_controller.clone(),
        auth_state: auth_state.clone(),
        pending_login: pending_login.clone(),
        auth_tx: auth_tx.clone(),
        provider: provider.clone(),
        notifier: notifier.clone(),
        live_sync_generation: live_sync_generation.clone(),
        live_sync_started_generation: live_sync_started_generation.clone(),
        background_refresh_in_flight: background_refresh_in_flight.clone(),
        animated_threads: animated_threads.clone(),
    };

    {
        let handles = handles.clone();
        let runtime = runtime.clone();
        let key_controller = gtk::EventControllerKey::new();
        key_controller.connect_key_pressed(move |_, key, _keycode, modifiers| {
            if handle_window_shortcuts(&handles, &runtime, key, modifiers) {
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        window.add_controller(key_controller);
    }

    {
        let handles = handles.clone();
        let runtime = runtime.clone();
        glib::timeout_add_local(Duration::from_millis(700), move || {
            if runtime.state.borrow_mut().cleanup_expired_typing() {
                refresh_ui(&handles, &runtime);
            }
            glib::ControlFlow::Continue
        });
    }

    {
        let handles = handles.clone();
        let runtime = runtime.clone();
        glib::timeout_add_local(LIVE_RECONCILE_INTERVAL, move || {
            maybe_start_background_refresh(&handles, &runtime);
            glib::ControlFlow::Continue
        });
    }

    if let SlackAuthStatus::Connected(session) = &*runtime.auth_state.borrow() {
        let active_workspace_key = runtime.state.borrow().active_workspace_key().to_string();
        let _ = ensure_active_workspace_profile(&runtime, &active_workspace_key, session);
    }

    let selection = gtk::NoSelection::new(Some(timeline_store.clone()));
    let factory = build_timeline_factory(&handles, &runtime);
    let list = ListView::new(Some(selection.clone()), Some(factory.clone()));
    list.add_css_class("timeline-list");
    list.set_vexpand(true);
    list.set_single_click_activate(false);
    list_scroll.set_child(Some(&list));

    eprintln!("[slaxide] preparing initial ui");
    refresh_ui(&handles, &runtime);
    eprintln!("[slaxide] initial ui ready");
    maybe_start_initial_history_load(&bootstrap, &state, &auth_state, &startup_status, &auth_tx);
    maybe_start_live_sync(&runtime);
    if slack_app_token().is_none() {
        startup_status.set_text(&format!(
            "Live notifications are disabled. Set {} and restart to receive other people's posts while the app is open.",
            SLACK_APP_TOKEN_ENV
        ));
    }

    {
        let handles = handles.clone();
        let runtime = runtime.clone();
        list_scroll
            .vadjustment()
            .connect_value_changed(move |adjustment| {
                let remaining = adjustment.upper() - (adjustment.value() + adjustment.page_size());
                if remaining <= TIMELINE_SCROLL_PREFETCH_PX
                    && runtime.state.borrow_mut().load_more_timeline_items()
                {
                    sync_timeline_store(&handles.timeline_store, &runtime);
                }
            });
    }

    {
        let handles = handles.clone();
        let runtime = runtime.clone();
        timeline_button.connect_clicked(move |_| {
            runtime.state.borrow_mut().set_main_view(MainView::Timeline);
            runtime.deck_state.borrow_mut().columns.clear();
            refresh_ui(&handles, &runtime);
        });
    }

    {
        let handles = handles.clone();
        let runtime = runtime.clone();
        office_button.connect_clicked(move |_| {
            runtime.state.borrow_mut().set_main_view(MainView::Office);
            refresh_ui(&handles, &runtime);
        });
    }

    {
        let handles = handles.clone();
        let runtime = runtime.clone();
        admin_button.connect_clicked(move |_| {
            runtime.deck_state.borrow_mut().open_admin();
            refresh_ui(&handles, &runtime);
        });
    }

    {
        let handles = handles.clone();
        let runtime = runtime.clone();
        settings_button.connect_clicked(move |_| {
            runtime.deck_state.borrow_mut().open_settings();
            refresh_ui(&handles, &runtime);
        });
    }

    {
        let handles = handles.clone();
        let runtime = runtime.clone();
        search.connect_search_changed(move |entry| {
            if *runtime.ui_sync_suppressed.borrow() {
                return;
            }
            let next_query = entry.text().to_string();
            let changed = {
                let mut state = runtime.state.borrow_mut();
                if state.search_query() == next_query {
                    false
                } else {
                    state.set_search_query(next_query);
                    true
                }
            };
            if changed {
                refresh_ui(&handles, &runtime);
            }
        });
    }

    {
        let handles = handles.clone();
        let runtime = runtime.clone();
        search_profile_filter.connect_changed(move |picker| {
            if *runtime.ui_sync_suppressed.borrow() {
                return;
            }
            let next_profile = picker
                .active_id()
                .map(|value| value.to_string())
                .filter(|value| !value.is_empty());
            let settings = {
                let mut state = runtime.state.borrow_mut();
                if !state.set_active_search_profile(next_profile) {
                    return;
                }
                state.settings.clone()
            };
            runtime.bootstrap.save_settings(&settings);
            refresh_ui(&handles, &runtime);
        });
    }

    {
        let handles = handles.clone();
        let runtime = runtime.clone();
        section_filter.connect_changed(move |picker| {
            if *runtime.ui_sync_suppressed.borrow() {
                return;
            }
            let next_section = picker
                .active_id()
                .map(|value| value.to_string())
                .filter(|value| !value.is_empty());
            let changed = {
                let mut state = runtime.state.borrow_mut();
                if state.section_filter() == next_section.as_deref() {
                    false
                } else {
                    state.set_section_filter(next_section);
                    true
                }
            };
            if changed {
                refresh_ui(&handles, &runtime);
            }
        });
    }

    {
        let handles = handles.clone();
        let runtime = runtime.clone();
        channel_filter.connect_changed(move |picker| {
            if *runtime.ui_sync_suppressed.borrow() {
                return;
            }
            let next_channel = picker
                .active_id()
                .map(|value| value.to_string())
                .filter(|value| !value.is_empty());
            let changed = {
                let mut state = runtime.state.borrow_mut();
                if state.channel_filter() == next_channel.as_deref() {
                    false
                } else {
                    state.set_channel_filter(next_channel);
                    true
                }
            };
            if changed {
                refresh_ui(&handles, &runtime);
            }
        });
    }

    {
        let handles = handles.clone();
        let runtime = runtime.clone();
        author_filter.connect_changed(move |picker| {
            if *runtime.ui_sync_suppressed.borrow() {
                return;
            }
            let next_author = picker
                .active_id()
                .map(|value| value.to_string())
                .filter(|value| !value.is_empty());
            let changed = {
                let mut state = runtime.state.borrow_mut();
                if state.author_filter() == next_author.as_deref() {
                    false
                } else {
                    state.set_author_filter(next_author);
                    true
                }
            };
            if changed {
                refresh_ui(&handles, &runtime);
            }
        });
    }

    {
        let handles = handles.clone();
        let runtime = runtime.clone();
        composer_send_button.connect_clicked(move |_| {
            start_channel_post(&handles, &runtime);
        });
    }

    {
        let handles = handles.clone();
        let runtime = runtime.clone();
        composer_channel.connect_changed(move |picker| {
            if *runtime.ui_sync_suppressed.borrow() {
                return;
            }
            let can_post = matches!(&*runtime.auth_state.borrow(), SlackAuthStatus::Connected(_))
                && picker.active_id().is_some_and(|value| !value.is_empty());
            handles.composer_send_button.set_sensitive(can_post);
        });
    }

    {
        let handles = handles.clone();
        let runtime = runtime.clone();
        composer_entry.connect_activate(move |_| {
            start_channel_post(&handles, &runtime);
        });
    }

    {
        let handles = handles.clone();
        let runtime = runtime.clone();
        let auth_tx = auth_tx.clone();
        let window = window.clone();
        gtk::glib::timeout_add_local(Duration::from_millis(150), move || {
            while let Ok(event) = auth_rx.try_recv() {
                match event {
                    AuthEvent::Completed {
                        workspace_key,
                        result,
                    } => {
                        stop_live_sync(&runtime);
                        *runtime.pending_login.borrow_mut() = None;
                        match result {
                            Ok(session) => {
                                let cached_items = runtime
                                    .bootstrap
                                    .load_timeline_items(&workspace_key)
                                    .unwrap_or_default();
                                let settings = {
                                    let mut state = runtime.state.borrow_mut();
                                    state.set_active_workspace(workspace_key.clone());
                                    state.upsert_workspace_profile(
                                        workspace_profile_from_session(&workspace_key, &session),
                                        true,
                                    );
                                    state.replace_items(
                                        cached_items,
                                        BTreeMap::new(),
                                        BTreeMap::new(),
                                        BTreeMap::new(),
                                        BTreeMap::new(),
                                    );
                                    state.settings.clone()
                                };
                                runtime.bootstrap.save_settings(&settings);
                                *runtime.auth_state.borrow_mut() =
                                    SlackAuthStatus::Connected(session);
                                maybe_start_initial_history_load(
                                    &runtime.bootstrap,
                                    &runtime.state,
                                    &runtime.auth_state,
                                    &handles.startup_status,
                                    &auth_tx,
                                );
                                maybe_start_live_sync(&runtime);
                                refresh_ui(&handles, &runtime);
                            }
                            Err(error) => {
                                if runtime.state.borrow().active_workspace_key() == workspace_key {
                                    *runtime.auth_state.borrow_mut() =
                                        SlackAuthStatus::Error(error);
                                    refresh_ui(&handles, &runtime);
                                } else {
                                    handles
                                        .startup_status
                                        .set_text("Slack room connect failed.");
                                }
                            }
                        }
                    }
                    AuthEvent::HistoryLoaded {
                        workspace_key,
                        result,
                    } => match result {
                        Ok(load) => {
                            eprintln!(
                                "[slaxide] initial history ingest loaded {} items across {} channels",
                                load.items.len(),
                                load.channel_count
                            );
                            runtime
                                .bootstrap
                                .replace_timeline_items(&workspace_key, &load.items);
                            let saved_message = {
                                let mut state = runtime.state.borrow_mut();
                                let seeded =
                                    seed_watched_channels(&mut state.settings, &load.items);
                                let seeded_notifications =
                                    seed_default_notification_rules(&mut state.settings);
                                if seeded > 0 || seeded_notifications {
                                    runtime.bootstrap.save_settings(&state.settings);
                                }
                                if state.active_workspace_key() == workspace_key {
                                    state.replace_items(
                                        load.items.clone(),
                                        load.user_names.clone(),
                                        load.user_avatar_paths.clone(),
                                        load.known_channels.clone(),
                                        load.known_channel_creators.clone(),
                                    );
                                    let mut message = format!(
                                        "Loaded {} Slack threads from {} channels.",
                                        load.items.len(),
                                        load.channel_count
                                    );
                                    if seeded_notifications {
                                        message.push_str(
                                            " Default notification rules are now enabled.",
                                        );
                                    }
                                    if load.missing_user_names_scope {
                                        message.push_str(
                                            " Reconnect Slack once to grant users:read for display names.",
                                        );
                                    }
                                    message
                                } else {
                                    "Slack history ingest finished for a background room."
                                        .to_string()
                                }
                            };
                            if runtime.state.borrow().active_workspace_key() == workspace_key {
                                handles.startup_status.set_text(&saved_message);
                                refresh_ui(&handles, &runtime);
                            }
                        }
                        Err(error) => {
                            eprintln!("[slaxide] initial history ingest failed: {error}");
                            if runtime.state.borrow().active_workspace_key() == workspace_key {
                                handles
                                    .startup_status
                                    .set_text(&format!("Slack history ingest failed: {error}"));
                                refresh_ui(&handles, &runtime);
                            }
                        }
                    },
                    AuthEvent::ChannelPostSent {
                        workspace_key,
                        result,
                    } => match result {
                        Ok(post_item) => {
                            let items = {
                                let mut state = runtime.state.borrow_mut();
                                if state.active_workspace_key() != workspace_key
                                    || !state.apply_incoming_item(post_item.clone())
                                {
                                    None
                                } else {
                                    state.select(Some(post_item.message_ts.clone()));
                                    Some(state.source_items.clone())
                                }
                            };
                            if let Some(items) = items {
                                runtime
                                    .bootstrap
                                    .replace_timeline_items(&workspace_key, &items);
                            }
                            if runtime.state.borrow().active_workspace_key() == workspace_key {
                                handles.composer_entry.set_text("");
                                handles.startup_status.set_text("Message sent to Slack.");
                                refresh_ui(&handles, &runtime);
                            }
                        }
                        Err(error) => {
                            eprintln!("[slaxide] channel post failed: {error}");
                            if runtime.state.borrow().active_workspace_key() == workspace_key {
                                handles
                                    .startup_status
                                    .set_text(&format!("Slack post failed: {error}"));
                                refresh_ui(&handles, &runtime);
                            }
                        }
                    },
                    AuthEvent::ReplySent {
                        workspace_key,
                        result,
                    } => match result {
                        Ok(reply_item) => {
                            eprintln!(
                                "[slaxide] thread reply sent to {} {}",
                                reply_item.channel_id, reply_item.thread_ts
                            );
                            let items = {
                                let mut state = runtime.state.borrow_mut();
                                if state.active_workspace_key() != workspace_key {
                                    None
                                } else {
                                    state.apply_reply_item(reply_item);
                                    Some(state.source_items.clone())
                                }
                            };
                            if let Some(items) = items {
                                runtime
                                    .bootstrap
                                    .replace_timeline_items(&workspace_key, &items);
                            }
                            if runtime.state.borrow().active_workspace_key() == workspace_key {
                                handles.startup_status.set_text("Reply sent to Slack.");
                                refresh_ui(&handles, &runtime);
                            }
                        }
                        Err(error) => {
                            eprintln!("[slaxide] thread reply failed: {error}");
                            if runtime.state.borrow().active_workspace_key() == workspace_key {
                                handles
                                    .startup_status
                                    .set_text(&format!("Slack reply failed: {error}"));
                                refresh_ui(&handles, &runtime);
                            }
                        }
                    },
                    AuthEvent::ReactionAdded {
                        workspace_key,
                        result,
                    } => match result {
                        Ok(update) => {
                            let current_user_id = match &*runtime.auth_state.borrow() {
                                SlackAuthStatus::Connected(session) => session.user_id.clone(),
                                _ => None,
                            };
                            let items = {
                                let mut state = runtime.state.borrow_mut();
                                if state.active_workspace_key() != workspace_key
                                    || !state.apply_reaction_added(
                                        &update.message_ts,
                                        &update.reaction_name,
                                        update.reactor_user_id.as_deref(),
                                        current_user_id.as_deref(),
                                    )
                                {
                                    None
                                } else {
                                    Some(state.source_items.clone())
                                }
                            };
                            if let Some(items) = items {
                                runtime
                                    .bootstrap
                                    .replace_timeline_items(&workspace_key, &items);
                            }
                            if runtime.state.borrow().active_workspace_key() == workspace_key {
                                handles.startup_status.set_text("Reaction sent to Slack.");
                                refresh_ui(&handles, &runtime);
                            }
                        }
                        Err(error) => {
                            eprintln!("[slaxide] reaction add failed: {error}");
                            if runtime.state.borrow().active_workspace_key() == workspace_key {
                                handles
                                    .startup_status
                                    .set_text(&format!("Slack reaction failed: {error}"));
                                refresh_ui(&handles, &runtime);
                            }
                        }
                    },
                    AuthEvent::MessageEdited {
                        workspace_key,
                        result,
                    } => match result {
                        Ok(update) => {
                            let items = {
                                let mut state = runtime.state.borrow_mut();
                                if state.active_workspace_key() != workspace_key
                                    || !state.apply_message_edit(&update.message_ts, &update.body)
                                {
                                    None
                                } else {
                                    Some(state.source_items.clone())
                                }
                            };
                            if let Some(items) = items {
                                runtime
                                    .bootstrap
                                    .replace_timeline_items(&workspace_key, &items);
                            }
                            if runtime.state.borrow().active_workspace_key() == workspace_key {
                                handles.startup_status.set_text("Slack message updated.");
                                refresh_ui(&handles, &runtime);
                            }
                        }
                        Err(error) => {
                            eprintln!("[slaxide] message edit failed: {error}");
                            if runtime.state.borrow().active_workspace_key() == workspace_key {
                                handles
                                    .startup_status
                                    .set_text(&format!("Slack edit failed: {error}"));
                                refresh_ui(&handles, &runtime);
                            }
                        }
                    },
                    AuthEvent::MessageDeleted {
                        workspace_key,
                        result,
                    } => match result {
                        Ok(update) => {
                            let items = {
                                let mut state = runtime.state.borrow_mut();
                                if state.active_workspace_key() != workspace_key
                                    || !state.apply_message_delete(&update.message_ts)
                                {
                                    None
                                } else {
                                    Some(state.source_items.clone())
                                }
                            };
                            if let Some(items) = items {
                                runtime
                                    .bootstrap
                                    .replace_timeline_items(&workspace_key, &items);
                            }
                            if runtime.state.borrow().active_workspace_key() == workspace_key {
                                handles.startup_status.set_text("Slack message deleted.");
                                refresh_ui(&handles, &runtime);
                            }
                        }
                        Err(error) => {
                            eprintln!("[slaxide] message delete failed: {error}");
                            if runtime.state.borrow().active_workspace_key() == workspace_key {
                                handles
                                    .startup_status
                                    .set_text(&format!("Slack delete failed: {error}"));
                                refresh_ui(&handles, &runtime);
                            }
                        }
                    },
                    AuthEvent::AdminActionFinished {
                        workspace_key,
                        result,
                    } => match result {
                        Ok(update) => {
                            if runtime.state.borrow().active_workspace_key() == workspace_key {
                                {
                                    let mut state = runtime.state.borrow_mut();
                                    if let Some((channel_id, channel_name)) =
                                        update.created_channel.as_ref()
                                    {
                                        state.upsert_known_channel(
                                            channel_id.clone(),
                                            channel_name.clone(),
                                        );
                                    }
                                    if let Some((channel_id, channel_name)) =
                                        update.renamed_channel.as_ref()
                                    {
                                        state.rename_known_channel(channel_id, channel_name);
                                    }
                                    if let Some(channel_id) = update.archived_channel_id.as_deref()
                                    {
                                        state.archive_known_channel(channel_id);
                                    }
                                }
                                handles.startup_status.set_text(&update.message);
                                refresh_ui(&handles, &runtime);
                            }
                        }
                        Err(error) => {
                            eprintln!("[slaxide] admin action failed: {error}");
                            if runtime.state.borrow().active_workspace_key() == workspace_key {
                                handles
                                    .startup_status
                                    .set_text(&format!("Slack admin action failed: {error}"));
                                refresh_ui(&handles, &runtime);
                            }
                        }
                    },
                    AuthEvent::ShareLinkResolved {
                        workspace_key,
                        result,
                    } => match result {
                        Ok(update) => {
                            if runtime.state.borrow().active_workspace_key() == workspace_key {
                                let _ = copy_text_to_clipboard(
                                    &handles,
                                    &update.permalink,
                                    "Copied Slack message URL to clipboard.",
                                );
                            }
                        }
                        Err(error) => {
                            eprintln!("[slaxide] share link failed: {error}");
                            if runtime.state.borrow().active_workspace_key() == workspace_key {
                                handles
                                    .startup_status
                                    .set_text(&format!("Slack share link failed: {error}"));
                            }
                        }
                    },
                    AuthEvent::BackgroundRefreshLoaded {
                        workspace_key,
                        result,
                    } => {
                        *runtime.background_refresh_in_flight.borrow_mut() = false;
                        match result {
                            Ok(items) => {
                                let updated_items = {
                                    let mut state = runtime.state.borrow_mut();
                                    if state.active_workspace_key() != workspace_key
                                        || !state.apply_background_refresh_items(items)
                                    {
                                        None
                                    } else {
                                        Some(state.source_items.clone())
                                    }
                                };
                                if let Some(items) = updated_items {
                                    runtime
                                        .bootstrap
                                        .replace_timeline_items(&workspace_key, &items);
                                    refresh_ui(&handles, &runtime);
                                }
                            }
                            Err(error) => {
                                eprintln!("[slaxide] background refresh failed: {error}");
                            }
                        }
                    }
                    AuthEvent::NotificationActivated(message_ts) => {
                        focus_message_from_notification(&window, &handles, &runtime, &message_ts);
                    }
                    AuthEvent::LiveEvent(event) => {
                        let session = match &*runtime.auth_state.borrow() {
                            SlackAuthStatus::Connected(session) => session.clone(),
                            _ => continue,
                        };
                        match event {
                            SlackSocketEvent::Message(event) => {
                                let image_cache_dir = runtime.bootstrap.image_cache_dir();
                                let incoming = {
                                    let state = runtime.state.borrow();
                                    live_message_to_timeline_item(
                                        &state,
                                        &session,
                                        image_cache_dir,
                                        event,
                                    )
                                };
                                let Some(incoming) = incoming else {
                                    continue;
                                };
                                let animation_target = incoming.thread_ts.clone();
                                eprintln!(
                                    "[slaxide] live message {} {}",
                                    incoming.channel_id, incoming.message_ts
                                );

                                let action = {
                                    let state = runtime.state.borrow();
                                    if should_notify_for_item(&session, &incoming) {
                                        notification_action_for_item(&state.settings, &incoming)
                                    } else {
                                        None
                                    }
                                };

                                let updated_items = {
                                    let mut state = runtime.state.borrow_mut();
                                    if !state.apply_incoming_item(incoming.clone()) {
                                        None
                                    } else {
                                        Some(state.source_items.clone())
                                    }
                                };

                                if let Some(items) = updated_items {
                                    let workspace_key =
                                        runtime.state.borrow().active_workspace_key().to_string();
                                    runtime
                                        .animated_threads
                                        .borrow_mut()
                                        .insert(animation_target);
                                    runtime
                                        .bootstrap
                                        .replace_timeline_items(&workspace_key, &items);
                                    if let Some(action) = action {
                                        notify_about_item(
                                            runtime.notifier.clone(),
                                            &incoming,
                                            action,
                                        );
                                    }
                                    refresh_ui(&handles, &runtime);
                                }
                            }
                            SlackSocketEvent::MessageChanged(event) => {
                                let image_cache_dir = runtime.bootstrap.image_cache_dir();
                                let Some((
                                    message_ts,
                                    body,
                                    rich_text_blocks,
                                    reactions,
                                    attachments,
                                )) = live_message_changed_snapshot(
                                    &session,
                                    image_cache_dir,
                                    &event,
                                )
                                else {
                                    continue;
                                };
                                eprintln!("[slaxide] live message changed {}", message_ts);
                                let workspace_key =
                                    runtime.state.borrow().active_workspace_key().to_string();
                                let updated_items = {
                                    let mut state = runtime.state.borrow_mut();
                                    if !state.apply_remote_message_snapshot(
                                        &message_ts,
                                        &body,
                                        rich_text_blocks,
                                        reactions,
                                        attachments,
                                    ) {
                                        None
                                    } else {
                                        Some(state.source_items.clone())
                                    }
                                };
                                if let Some(items) = updated_items {
                                    runtime
                                        .bootstrap
                                        .replace_timeline_items(&workspace_key, &items);
                                    refresh_ui(&handles, &runtime);
                                }
                            }
                            SlackSocketEvent::MessageDeleted(event) => {
                                let Some(message_ts) = live_message_deleted_ts(&event) else {
                                    continue;
                                };
                                eprintln!("[slaxide] live message deleted {}", message_ts);
                                let workspace_key =
                                    runtime.state.borrow().active_workspace_key().to_string();
                                let updated_items = {
                                    let mut state = runtime.state.borrow_mut();
                                    if !state.apply_message_delete(message_ts) {
                                        None
                                    } else {
                                        Some(state.source_items.clone())
                                    }
                                };
                                if let Some(items) = updated_items {
                                    runtime
                                        .bootstrap
                                        .replace_timeline_items(&workspace_key, &items);
                                    refresh_ui(&handles, &runtime);
                                }
                            }
                            SlackSocketEvent::ReactionAdded(event) => {
                                let Some(message_ts) = event.item.ts.as_deref() else {
                                    continue;
                                };
                                if event.item.kind != "message" {
                                    continue;
                                }
                                eprintln!(
                                    "[slaxide] live reaction added {} {}",
                                    event.reaction, message_ts
                                );
                                let workspace_key =
                                    runtime.state.borrow().active_workspace_key().to_string();
                                let updated_items = {
                                    let mut state = runtime.state.borrow_mut();
                                    if !state.apply_reaction_added(
                                        message_ts,
                                        &event.reaction,
                                        event.user.as_deref(),
                                        session.user_id.as_deref(),
                                    ) {
                                        None
                                    } else {
                                        Some(state.source_items.clone())
                                    }
                                };
                                if let Some(items) = updated_items {
                                    runtime
                                        .bootstrap
                                        .replace_timeline_items(&workspace_key, &items);
                                    refresh_ui(&handles, &runtime);
                                }
                            }
                            SlackSocketEvent::ReactionRemoved(event) => {
                                let Some(message_ts) = event.item.ts.as_deref() else {
                                    continue;
                                };
                                if event.item.kind != "message" {
                                    continue;
                                }
                                eprintln!(
                                    "[slaxide] live reaction removed {} {}",
                                    event.reaction, message_ts
                                );
                                let workspace_key =
                                    runtime.state.borrow().active_workspace_key().to_string();
                                let updated_items = {
                                    let mut state = runtime.state.borrow_mut();
                                    if !state.apply_reaction_removed(
                                        message_ts,
                                        &event.reaction,
                                        event.user.as_deref(),
                                        session.user_id.as_deref(),
                                    ) {
                                        None
                                    } else {
                                        Some(state.source_items.clone())
                                    }
                                };
                                if let Some(items) = updated_items {
                                    runtime
                                        .bootstrap
                                        .replace_timeline_items(&workspace_key, &items);
                                    refresh_ui(&handles, &runtime);
                                }
                            }
                            SlackSocketEvent::UserTyping(event) => {
                                let Some(user_id) = event.user.as_deref() else {
                                    continue;
                                };
                                if session.user_id.as_deref() == Some(user_id)
                                    || session.bot_user_id.as_deref() == Some(user_id)
                                {
                                    continue;
                                }
                                if runtime
                                    .state
                                    .borrow_mut()
                                    .note_user_typing(&event.channel, user_id)
                                {
                                    refresh_ui(&handles, &runtime);
                                }
                            }
                            SlackSocketEvent::Unsupported { kind } => {
                                if let Some(kind) = kind {
                                    eprintln!("[slaxide] ignoring unsupported live event: {kind}");
                                }
                            }
                        }
                    }
                    AuthEvent::LiveSyncFailed(error) => {
                        eprintln!("[slaxide] live sync failed: {error}");
                        handles
                            .startup_status
                            .set_text(&format!("Live sync reconnecting: {error}"));
                    }
                }
            }
            gtk::glib::ControlFlow::Continue
        });
    }

    eprintln!("[slaxide] presenting window");
    window.present();
}

fn maybe_start_initial_history_load(
    bootstrap: &Rc<AppBootstrap>,
    state: &Rc<RefCell<UiState>>,
    auth_state: &Rc<RefCell<SlackAuthStatus>>,
    startup_status: &Label,
    auth_tx: &mpsc::Sender<AuthEvent>,
) {
    let (has_cached_items, needs_user_directory_refresh, needs_channel_directory_refresh) = {
        let state = state.borrow();
        (
            state.has_cached_items(),
            state.needs_user_directory_refresh(),
            state.needs_channel_directory_refresh(),
        )
    };
    if has_cached_items && !needs_user_directory_refresh && !needs_channel_directory_refresh {
        return;
    }

    let session = match &*auth_state.borrow() {
        SlackAuthStatus::Connected(session) => session.clone(),
        _ => return,
    };

    let status = if has_cached_items {
        if needs_channel_directory_refresh {
            "Refreshing Slack channel names..."
        } else {
            "Refreshing Slack display names..."
        }
    } else {
        "Loading recent Slack history..."
    };
    startup_status.set_text(status);
    eprintln!(
        "[slaxide] starting initial history ingest (cached_items={has_cached_items} refresh_names={needs_user_directory_refresh} refresh_channels={needs_channel_directory_refresh})"
    );
    let settings = state.borrow().settings.clone();
    let workspace_key = state.borrow().active_workspace_key().to_string();
    let avatar_cache_dir = bootstrap.avatar_cache_dir();
    let image_cache_dir = bootstrap.image_cache_dir();
    let auth_tx = auth_tx.clone();
    std::thread::spawn(move || {
        let result = load_initial_history(&session, &settings, avatar_cache_dir, image_cache_dir)
            .map_err(|error| error.to_string());
        let _ = auth_tx.send(AuthEvent::HistoryLoaded {
            workspace_key,
            result,
        });
    });
}

fn maybe_start_background_refresh(handles: &UiHandles, runtime: &UiRuntime) {
    if *runtime.background_refresh_in_flight.borrow() {
        return;
    }

    let session = match &*runtime.auth_state.borrow() {
        SlackAuthStatus::Connected(session) => session.clone(),
        _ => return,
    };
    let (channels, settings, user_names, user_avatar_paths) = {
        let state = runtime.state.borrow();
        if state.source_items.is_empty() {
            return;
        }
        (
            state.background_refresh_channels(LIVE_RECONCILE_CHANNEL_LIMIT),
            state.settings.clone(),
            state.user_names.clone(),
            state.user_avatar_paths.clone(),
        )
    };
    if channels.is_empty() {
        return;
    }

    *runtime.background_refresh_in_flight.borrow_mut() = true;
    let image_cache_dir = runtime.bootstrap.image_cache_dir();
    let workspace_key = runtime.state.borrow().active_workspace_key().to_string();
    let auth_tx = runtime.auth_tx.clone();
    let _handles = handles;
    std::thread::spawn(move || {
        let result = load_background_refresh(
            &session,
            &settings,
            user_names,
            user_avatar_paths,
            image_cache_dir,
            channels,
        )
        .map_err(|error| error.to_string());
        let _ = auth_tx.send(AuthEvent::BackgroundRefreshLoaded {
            workspace_key,
            result,
        });
    });
}

fn maybe_start_live_sync(runtime: &UiRuntime) {
    if runtime.live_sync_started_generation.borrow().is_some() {
        return;
    }

    let session = match &*runtime.auth_state.borrow() {
        SlackAuthStatus::Connected(session) => session.clone(),
        _ => return,
    };
    let Some(app_token) = slack_app_token() else {
        eprintln!(
            "[slaxide] live sync disabled: set {} for desktop notifications",
            SLACK_APP_TOKEN_ENV
        );
        return;
    };

    let generation = runtime.live_sync_generation.fetch_add(1, Ordering::SeqCst) + 1;
    *runtime.live_sync_started_generation.borrow_mut() = Some(generation);
    eprintln!("[slaxide] starting live sync generation {generation}");
    let auth_tx = runtime.auth_tx.clone();
    let live_sync_generation = runtime.live_sync_generation.clone();
    std::thread::spawn(move || {
        run_live_sync_loop(
            generation,
            live_sync_generation,
            app_token,
            session,
            auth_tx,
        );
    });
}

fn stop_live_sync(runtime: &UiRuntime) {
    runtime.live_sync_generation.fetch_add(1, Ordering::SeqCst);
    *runtime.live_sync_started_generation.borrow_mut() = None;
}

fn slack_app_token() -> Option<String> {
    env::var(SLACK_APP_TOKEN_ENV)
        .ok()
        .map(|token| token.trim().to_string())
        .filter(|token| !token.is_empty())
}

fn run_live_sync_loop(
    generation: u64,
    live_sync_generation: Arc<AtomicU64>,
    app_token: String,
    session: StoredSlackSession,
    auth_tx: mpsc::Sender<AuthEvent>,
) {
    let runtime = match Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = auth_tx.send(AuthEvent::LiveSyncFailed(error.to_string()));
            return;
        }
    };

    runtime.block_on(async move {
        let client = match SlackClient::api_only() {
            Ok(client) => client,
            Err(error) => {
                let _ = auth_tx.send(AuthEvent::LiveSyncFailed(error.to_string()));
                return;
            }
        };

        loop {
            if live_sync_generation.load(Ordering::SeqCst) != generation {
                return;
            }

            let mut socket = match SocketModeSession::connect(&client, &app_token).await {
                Ok(socket) => {
                    eprintln!("[slaxide] live sync websocket connected");
                    socket
                }
                Err(error) => {
                    let _ = auth_tx.send(AuthEvent::LiveSyncFailed(error.to_string()));
                    tokio::time::sleep(LIVE_SYNC_RECONNECT_DELAY).await;
                    continue;
                }
            };

            loop {
                if live_sync_generation.load(Ordering::SeqCst) != generation {
                    return;
                }

                let envelope = match socket.next_envelope().await {
                    Ok(Some(envelope)) => envelope,
                    Ok(None) => break,
                    Err(error) => {
                        let _ = auth_tx.send(AuthEvent::LiveSyncFailed(error.to_string()));
                        break;
                    }
                };

                if let Err(error) = socket.ack(&envelope).await {
                    let _ = auth_tx.send(AuthEvent::LiveSyncFailed(error.to_string()));
                    break;
                }

                if envelope.kind != "events_api" {
                    if envelope.kind == "hello" {
                        let live_app_id = envelope
                            .connection_info
                            .as_ref()
                            .and_then(|info| info.app_id.as_deref());
                        eprintln!("[slaxide] live sync hello app_id={live_app_id:?}");
                        if let (Some(expected_app_id), Some(live_app_id)) =
                            (session.app_id.as_deref(), live_app_id)
                            && expected_app_id != live_app_id
                        {
                            let _ = auth_tx.send(AuthEvent::LiveSyncFailed(format!(
                                "Socket Mode app mismatch: OAuth app {expected_app_id}, app token connected to {live_app_id}"
                            )));
                        }
                    }
                    continue;
                }
                let Some(payload) = envelope.payload else {
                    continue;
                };
                if payload.kind != "event_callback" {
                    continue;
                }
                let event = match SlackSocketEvent::parse(payload.event) {
                    Ok(event) => event,
                    Err(error) => {
                        let _ = auth_tx.send(AuthEvent::LiveSyncFailed(error.to_string()));
                        continue;
                    }
                };
                let _ = auth_tx.send(AuthEvent::LiveEvent(event));
            }

            tokio::time::sleep(LIVE_SYNC_RECONNECT_DELAY).await;
        }
    });
}

fn live_message_to_timeline_item(
    state: &UiState,
    session: &StoredSlackSession,
    image_cache_dir: Option<PathBuf>,
    event: SlackMessageEvent,
) -> Option<TimelineItem> {
    match event.subtype.as_deref() {
        Some("message_changed" | "message_deleted" | "channel_join" | "channel_leave") => {
            return None;
        }
        _ => {}
    }

    let token = session
        .user_access_token
        .as_deref()
        .or(session.bot_access_token.as_deref());
    let attachments = cached_message_attachments_blocking(token, image_cache_dir, &event.files);
    let rich_text_blocks = event.blocks;
    let mut body = event.text.unwrap_or_default().trim().to_string();
    if body.is_empty() && !rich_text_blocks.is_empty() {
        body =
            slack_plain_text_from_blocks(&rich_text_blocks, &SlackRenderLookup::from_state(state))
                .trim()
                .to_string();
    }
    if body.is_empty() && attachments.is_empty() {
        return None;
    }

    let thread_ts = event.thread_ts.clone().unwrap_or_else(|| event.ts.clone());
    let author_id = event.user.unwrap_or_else(|| "unknown".to_string());
    let participant = session.user_id.as_deref() == Some(author_id.as_str());
    let author_name = state.author_name_for(&author_id, participant);
    let author_avatar_path = state.author_avatar_path_for(&author_id);

    Some(TimelineItem {
        workspace_id: session
            .team_id
            .clone()
            .unwrap_or_else(|| "workspace".to_string()),
        channel_id: event.channel.clone(),
        channel_name: state
            .channel_name_for(&event.channel)
            .unwrap_or_else(|| event.channel.clone()),
        message_ts: event.ts.clone(),
        thread_ts,
        author_id,
        author_name,
        author_avatar_path,
        body: body.clone(),
        rich_text_blocks,
        reactions: reaction_summaries(&event.reactions, session.user_id.as_deref()),
        attachments,
        unread: true,
        participant,
        direct_mention: session
            .user_id
            .as_deref()
            .is_some_and(|user_id| body.contains(&format!("<@{user_id}>"))),
        focus_keyword_hits: state.settings.timeline.matching_keywords(&body),
        watch_weight: state
            .settings
            .timeline
            .channel_weights
            .get(&event.channel)
            .copied()
            .unwrap_or(1),
        last_activity_at: slack_ts_to_datetime(&event.ts).unwrap_or_else(Utc::now),
        reply_state: ReplyState::Idle,
    })
}

fn live_message_changed_snapshot(
    session: &StoredSlackSession,
    image_cache_dir: Option<PathBuf>,
    event: &SlackMessageChangedEvent,
) -> Option<(
    String,
    String,
    Vec<serde_json::Value>,
    Vec<ReactionSummary>,
    Vec<AttachmentSummary>,
)> {
    let token = session
        .user_access_token
        .as_deref()
        .or(session.bot_access_token.as_deref());
    let attachments =
        cached_message_attachments_blocking(token, image_cache_dir, &event.message.files);
    let body = event
        .message
        .text
        .clone()
        .unwrap_or_default()
        .trim()
        .to_string();
    let body = if body.is_empty() && !event.message.blocks.is_empty() {
        slack_plain_text_from_blocks(&event.message.blocks, &SlackRenderLookup::default())
            .trim()
            .to_string()
    } else {
        body
    };

    Some((
        event.message.ts.clone(),
        body,
        event.message.blocks.clone(),
        reaction_summaries(&event.message.reactions, session.user_id.as_deref()),
        attachments,
    ))
}

fn live_message_deleted_ts(event: &SlackMessageDeletedEvent) -> Option<&str> {
    event.deleted_ts.as_deref().or_else(|| {
        event
            .previous_message
            .as_ref()
            .map(|message| message.ts.as_str())
    })
}

fn notification_action_for_item(
    settings: &AppSettings,
    item: &TimelineItem,
) -> Option<NotificationAction> {
    let mut best_action = None;

    for rule in &settings.notification_rules {
        if !rule.enabled || quiet_hours_active(rule.quiet_hours.as_ref()) {
            continue;
        }

        match settings.notification_rule_matches(rule, item) {
            Ok(true) => {
                if best_action.as_ref().is_none_or(|current| {
                    notification_priority(&rule.action) > notification_priority(current)
                }) {
                    best_action = Some(rule.action.clone());
                }
            }
            Ok(false) => {}
            Err(error) => {
                eprintln!(
                    "[slaxide] notification rule `{}` failed: {error}",
                    rule.label
                );
            }
        }
    }

    best_action.or_else(|| {
        if item.direct_mention {
            Some(NotificationAction::Critical)
        } else if settings.notification_rules.is_empty() {
            Some(NotificationAction::Notify)
        } else if settings.timeline.is_watched(&item.channel_id) {
            Some(NotificationAction::Notify)
        } else {
            None
        }
    })
}

fn quiet_hours_active(quiet_hours: Option<&QuietHours>) -> bool {
    let Some(quiet_hours) = quiet_hours else {
        return false;
    };

    let hour = Local::now().hour() as u8;
    match quiet_hours.start_hour.cmp(&quiet_hours.end_hour) {
        std::cmp::Ordering::Less => hour >= quiet_hours.start_hour && hour < quiet_hours.end_hour,
        std::cmp::Ordering::Greater => {
            hour >= quiet_hours.start_hour || hour < quiet_hours.end_hour
        }
        std::cmp::Ordering::Equal => true,
    }
}

fn notification_priority(action: &NotificationAction) -> u8 {
    match action {
        NotificationAction::Silent => 0,
        NotificationAction::Notify => 1,
        NotificationAction::Critical => 2,
    }
}

fn should_notify_for_item(session: &StoredSlackSession, item: &TimelineItem) -> bool {
    session.user_id.as_deref() != Some(item.author_id.as_str())
        && session.bot_user_id.as_deref() != Some(item.author_id.as_str())
}

fn notify_about_item(
    notifier: Arc<NotifyRustBackend>,
    item: &TimelineItem,
    action: NotificationAction,
) {
    let sound_name = match action {
        NotificationAction::Silent => None,
        NotificationAction::Notify | NotificationAction::Critical => {
            Some("message-new-instant".to_string())
        }
    };
    let request = NotificationRequest {
        summary: format!("{} • {}", item.channel_name, item.author_name),
        body: if item.body.trim().is_empty() {
            item.attachments
                .first()
                .map(|attachment| attachment.title.clone())
                .unwrap_or_else(|| "New Slack attachment".to_string())
        } else {
            render_slack_text(&item.body)
        },
        action,
        icon: item
            .author_avatar_path
            .as_ref()
            .filter(|path| Path::new(path).exists())
            .cloned(),
        category: Some("Slaxide".into()),
        sound_name,
        default_action_target: Some(item.message_ts.clone()),
    };

    std::thread::spawn(move || {
        let runtime = match Runtime::new() {
            Ok(runtime) => runtime,
            Err(error) => {
                eprintln!("[slaxide] notification runtime failed: {error}");
                return;
            }
        };
        if let Err(error) = runtime.block_on(notifier.send(&request)) {
            eprintln!("[slaxide] desktop notification failed: {error:#}");
        }
    });
}

fn focus_message_from_notification(
    window: &ApplicationWindow,
    handles: &UiHandles,
    runtime: &UiRuntime,
    message_ts: &str,
) {
    let Some(target) = ({
        let mut state = runtime.state.borrow_mut();
        state.focus_message(message_ts)
    }) else {
        handles
            .startup_status
            .set_text("The notified message is no longer in the local cache.");
        return;
    };

    if target.message_ts != target.thread_ts {
        runtime
            .deck_state
            .borrow_mut()
            .ensure_thread_open(target.thread_ts.clone());
        handles
            .startup_status
            .set_text("Opened the notified thread.");
    } else {
        handles
            .startup_status
            .set_text("Focused the notified Slack post.");
    }

    refresh_ui(handles, runtime);
    window.present();
}

fn ensure_active_workspace_profile(
    runtime: &UiRuntime,
    workspace_key: &str,
    session: &StoredSlackSession,
) -> bool {
    let profile = workspace_profile_from_session(workspace_key, session);
    let changed = {
        let mut state = runtime.state.borrow_mut();
        state.upsert_workspace_profile(profile, true)
    };
    if changed {
        let settings = runtime.state.borrow().settings.clone();
        runtime.bootstrap.save_settings(&settings);
    }
    changed
}

fn switch_active_workspace(handles: &UiHandles, runtime: &UiRuntime, workspace_key: String) {
    if !runtime
        .state
        .borrow_mut()
        .set_active_workspace(workspace_key.clone())
    {
        return;
    }

    stop_live_sync(runtime);
    *runtime.pending_login.borrow_mut() = None;

    match runtime.bootstrap.load_timeline_items(&workspace_key) {
        Ok(items) => {
            let mut state = runtime.state.borrow_mut();
            state.replace_items(
                items,
                BTreeMap::new(),
                BTreeMap::new(),
                BTreeMap::new(),
                BTreeMap::new(),
            );
            let settings = state.settings.clone();
            drop(state);
            runtime.bootstrap.save_settings(&settings);
        }
        Err(error) => {
            handles
                .startup_status
                .set_text(&format!("Failed to load cached room timeline: {error}"));
        }
    }

    *runtime.auth_state.borrow_mut() =
        load_initial_auth_status(runtime.auth_controller.clone(), &workspace_key);
    maybe_start_initial_history_load(
        &runtime.bootstrap,
        &runtime.state,
        &runtime.auth_state,
        &handles.startup_status,
        &runtime.auth_tx,
    );
    maybe_start_live_sync(runtime);
    refresh_ui(handles, runtime);
}

fn load_initial_history(
    session: &StoredSlackSession,
    settings: &AppSettings,
    avatar_cache_dir: Option<PathBuf>,
    image_cache_dir: Option<PathBuf>,
) -> Result<InitialHistoryLoad> {
    let client = SlackClient::api_only()?;
    let token = session
        .user_access_token
        .as_deref()
        .or(session.bot_access_token.as_deref())
        .context("no Slack access token is stored")?;
    let workspace_id = session
        .team_id
        .clone()
        .unwrap_or_else(|| "workspace".to_string());
    let current_user_id = session.user_id.as_deref();
    let history_window = chrono::Duration::days(settings.timeline.recent_window_days.into());
    let oldest = slack_ts_from_datetime(Utc::now() - history_window);
    let runtime = Runtime::new().context("failed to create runtime for Slack history ingest")?;

    runtime.block_on(async move {
        let (directory, missing_user_names_scope) =
            match load_user_directory(token, avatar_cache_dir.clone()).await {
                Ok(directory) => (directory, false),
                Err(error) => {
                    eprintln!("[slaxide] Slack user directory lookup failed: {error}");
                    let missing_user_names_scope =
                        matches!(&error, slaxide_slack::SlackError::Api(code) if code == "missing_scope");
                    (UserDirectoryLoad::default(), missing_user_names_scope)
                }
            };
        let user_names = directory.user_names;
        let user_avatar_paths = directory.user_avatar_paths;

        let conversations = client
            .list_conversations(token, None, INITIAL_CHANNEL_PAGE_LIMIT)
            .await
            .context("failed to list Slack conversations")?;

        let channels = conversations
            .channels
            .into_iter()
            .filter(is_syncable_conversation)
            .collect::<Vec<_>>();
        let known_channel_creators = channels
            .iter()
            .filter_map(|channel| {
                channel
                    .creator
                    .as_ref()
                    .filter(|creator| !creator.is_empty())
                    .map(|creator| (channel.id.clone(), creator.clone()))
            })
            .collect::<BTreeMap<_, _>>();
        let channels = prioritize_initial_sync_conversations(channels, settings);
        let known_channels = channels
            .iter()
            .map(|channel| {
                (
                    channel.id.clone(),
                    channel
                        .display_name()
                        .map(str::to_string)
                        .unwrap_or_else(|| channel.id.clone()),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let prioritized_match_count = channels
            .iter()
            .take_while(|channel| conversation_matches_initial_priority(settings, channel))
            .count();
        let sync_limit = INITIAL_CHANNEL_SYNC_LIMIT.max(prioritized_match_count);
        let channels = channels.into_iter().take(sync_limit).collect::<Vec<_>>();

        if channels.is_empty() {
            return Ok(InitialHistoryLoad {
                items: Vec::new(),
                channel_count: 0,
                user_names,
                user_avatar_paths,
                known_channels,
                known_channel_creators,
                missing_user_names_scope,
            });
        }

        let mut items = Vec::new();
        let mut channel_count = 0;
        let mut failures = Vec::new();

        for channel in channels {
            match client
                .conversations_history(
                    token,
                    &channel.id,
                    Some(&oldest),
                    INITIAL_MESSAGES_PER_CHANNEL,
                )
                .await
            {
                Ok(history) => {
                    channel_count += 1;
                    for message in history.messages {
                        if let Some(item) = history_message_to_timeline_item(
                            &workspace_id,
                            &channel,
                            current_user_id,
                            settings,
                            &user_names,
                            &user_avatar_paths,
                            token,
                            image_cache_dir.as_deref(),
                            message,
                        )
                        .await
                        {
                            items.push(item);
                        }
                    }
                }
                Err(error) => failures.push(format!("{}: {error}", channel.id)),
            }
        }

        items.sort_by(|left, right| {
            right
                .last_activity_at
                .cmp(&left.last_activity_at)
                .then_with(|| right.message_ts.cmp(&left.message_ts))
        });
        items.truncate(TIMELINE_LIMIT);

        if items.is_empty() && !failures.is_empty() {
            anyhow::bail!(
                "no channel history could be loaded ({})",
                failures.join(" | ")
            );
        }

        Ok(InitialHistoryLoad {
            items,
            channel_count,
            user_names,
            user_avatar_paths,
            known_channels,
            known_channel_creators,
            missing_user_names_scope,
        })
    })
}

fn load_background_refresh(
    session: &StoredSlackSession,
    settings: &AppSettings,
    user_names: BTreeMap<String, String>,
    user_avatar_paths: BTreeMap<String, String>,
    image_cache_dir: Option<PathBuf>,
    channels: Vec<(String, String)>,
) -> Result<Vec<TimelineItem>> {
    let client = SlackClient::api_only()?;
    let token = session
        .user_access_token
        .as_deref()
        .or(session.bot_access_token.as_deref())
        .context("no Slack access token is stored")?;
    let workspace_id = session
        .team_id
        .clone()
        .unwrap_or_else(|| "workspace".to_string());
    let current_user_id = session.user_id.as_deref();
    let oldest = slack_ts_from_datetime(Utc::now() - chrono::Duration::days(2));
    let runtime = Runtime::new().context("failed to create runtime for background refresh")?;

    runtime.block_on(async move {
        let mut items = Vec::new();
        for (channel_id, channel_name) in channels {
            let history = client
                .conversations_history(
                    token,
                    &channel_id,
                    Some(&oldest),
                    LIVE_RECONCILE_MESSAGES_PER_CHANNEL,
                )
                .await
                .with_context(|| format!("failed to refresh channel {channel_id}"))?;
            let channel = SlackConversation {
                id: channel_id.clone(),
                name: Some(channel_name.trim_start_matches('#').to_string()),
                name_normalized: Some(channel_name.trim_start_matches('#').to_string()),
                creator: None,
                is_member: Some(true),
                is_private: None,
                is_archived: Some(false),
            };
            for message in history.messages {
                if let Some(item) = history_message_to_timeline_item(
                    &workspace_id,
                    &channel,
                    current_user_id,
                    settings,
                    &user_names,
                    &user_avatar_paths,
                    token,
                    image_cache_dir.as_deref(),
                    message,
                )
                .await
                {
                    items.push(item);
                }
            }
        }
        Ok(items)
    })
}

async fn load_user_directory(
    token: &str,
    avatar_cache_dir: Option<PathBuf>,
) -> Result<UserDirectoryLoad, slaxide_slack::SlackError> {
    let client = SlackClient::api_only()?;
    let http = HttpClient::new();
    let mut user_names = BTreeMap::new();
    let mut user_avatar_paths = BTreeMap::new();
    let mut cursor = None::<String>;
    let avatar_cache_dir = avatar_cache_dir.map(|path| {
        let _ = fs::create_dir_all(&path);
        path
    });

    loop {
        let response = client.users_list(token, cursor.as_deref(), 500).await?;
        for user in response.members {
            if user.deleted.unwrap_or(false) {
                continue;
            }
            if let Some(display_name) = slack_user_display_name(&user) {
                user_names.insert(user.id.clone(), display_name);
            }
            if let Some(avatar_path) =
                cache_user_avatar(&http, avatar_cache_dir.as_deref(), &user).await
            {
                user_avatar_paths.insert(user.id.clone(), avatar_path);
            }
        }

        cursor = response
            .response_metadata
            .and_then(|metadata| metadata.next_cursor)
            .filter(|next| !next.is_empty());
        if cursor.is_none() {
            break;
        }
    }

    Ok(UserDirectoryLoad {
        user_names,
        user_avatar_paths,
    })
}

fn slack_user_display_name(user: &SlackUser) -> Option<String> {
    user.profile
        .as_ref()
        .and_then(|profile| {
            profile
                .display_name_normalized
                .as_deref()
                .filter(|name| !name.trim().is_empty())
                .or(profile
                    .display_name
                    .as_deref()
                    .filter(|name| !name.trim().is_empty()))
                .or(profile
                    .real_name_normalized
                    .as_deref()
                    .filter(|name| !name.trim().is_empty()))
                .or(profile
                    .real_name
                    .as_deref()
                    .filter(|name| !name.trim().is_empty()))
        })
        .or(user.name.as_deref().filter(|name| !name.trim().is_empty()))
        .map(str::to_string)
}

fn slack_user_avatar_url(user: &SlackUser) -> Option<&str> {
    user.profile.as_ref().and_then(|profile| {
        profile
            .image_72
            .as_deref()
            .or(profile.image_192.as_deref())
            .or(profile.image_48.as_deref())
    })
}

async fn cache_user_avatar(
    http: &HttpClient,
    avatar_cache_dir: Option<&Path>,
    user: &SlackUser,
) -> Option<String> {
    let avatar_cache_dir = avatar_cache_dir?;
    let avatar_url = slack_user_avatar_url(user)?;
    let file_extension = avatar_extension_from_url(avatar_url);
    let avatar_path = avatar_cache_dir.join(format!("{}.{file_extension}", user.id));
    if avatar_path.exists() {
        return Some(avatar_path.to_string_lossy().into_owned());
    }

    let response = http.get(avatar_url).send().await.ok()?;
    let bytes = response.bytes().await.ok()?;
    if fs::write(&avatar_path, bytes).is_ok() {
        Some(avatar_path.to_string_lossy().into_owned())
    } else {
        None
    }
}

fn avatar_extension_from_url(raw_url: &str) -> String {
    url::Url::parse(raw_url)
        .ok()
        .and_then(|url| {
            Path::new(url.path())
                .extension()
                .and_then(|ext| ext.to_str())
                .map(str::to_string)
        })
        .filter(|ext| !ext.is_empty())
        .unwrap_or_else(|| "img".to_string())
}

fn image_extension_from_file(file: &SlackFile, raw_url: &str) -> String {
    let url_extension = avatar_extension_from_url(raw_url)
        .chars()
        .take_while(|character| character.is_ascii_alphanumeric())
        .collect::<String>()
        .to_lowercase();
    (!url_extension.is_empty() && url_extension != "img")
        .then_some(url_extension)
        .or_else(|| {
            file.mimetype.as_deref().and_then(|mime| {
                mime.strip_prefix("image/")
                    .map(|extension| extension.split('+').next().unwrap_or("img").to_string())
            })
        })
        .unwrap_or_else(|| "img".to_string())
}

fn slack_file_preview_url(file: &SlackFile) -> Option<&str> {
    file.thumb_720
        .as_deref()
        .or(file.thumb_360.as_deref())
        .or(file.url_private_download.as_deref())
        .or(file.url_private.as_deref())
}

fn slack_file_is_image(file: &SlackFile) -> bool {
    file.mimetype
        .as_deref()
        .is_some_and(|mime| mime.starts_with("image/"))
        || file.thumb_360.is_some()
        || file.thumb_720.is_some()
}

fn cached_file_is_renderable_image(path: &Path) -> bool {
    let Ok(bytes) = fs::read(path) else {
        return false;
    };
    if bytes.is_empty() {
        return false;
    }

    bytes.starts_with(&[0xFF, 0xD8, 0xFF])
        || bytes.starts_with(b"\x89PNG\r\n\x1A\n")
        || bytes.starts_with(b"GIF87a")
        || bytes.starts_with(b"GIF89a")
        || bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(&b"WEBP"[..])
}

async fn cached_message_attachments(
    token: Option<&str>,
    image_cache_dir: Option<&Path>,
    files: &[SlackFile],
) -> Vec<AttachmentSummary> {
    let Some(token) = token else {
        return files.iter().map(fallback_attachment_summary).collect();
    };
    let http = HttpClient::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .ok();
    let image_cache_dir = image_cache_dir.map(|path| {
        let _ = fs::create_dir_all(path);
        path.to_path_buf()
    });
    let mut attachments = Vec::new();

    for file in files {
        if slack_file_is_image(file)
            && let Some(path) =
                cache_slack_image_attachment(http.as_ref(), token, image_cache_dir.as_deref(), file)
                    .await
        {
            attachments.push(AttachmentSummary {
                kind: AttachmentKind::Image,
                title: file
                    .name
                    .clone()
                    .unwrap_or_else(|| "Image attachment".to_string()),
                url: Some(path),
                mime: file.mimetype.clone(),
            });
            continue;
        }

        attachments.push(fallback_attachment_summary(file));
    }

    attachments
}

fn cached_message_attachments_blocking(
    token: Option<&str>,
    image_cache_dir: Option<PathBuf>,
    files: &[SlackFile],
) -> Vec<AttachmentSummary> {
    if files.is_empty() {
        return Vec::new();
    }
    let Ok(runtime) = Runtime::new() else {
        return files.iter().map(fallback_attachment_summary).collect();
    };
    runtime.block_on(cached_message_attachments(
        token,
        image_cache_dir.as_deref(),
        files,
    ))
}

async fn cache_slack_image_attachment(
    http: Option<&HttpClient>,
    token: &str,
    image_cache_dir: Option<&Path>,
    file: &SlackFile,
) -> Option<String> {
    let image_cache_dir = image_cache_dir?;
    let preview_url = slack_file_preview_url(file)?;
    let image_path = image_cache_dir.join(format!(
        "{}.{}",
        file.id,
        image_extension_from_file(file, preview_url)
    ));
    if image_path.exists() && cached_file_is_renderable_image(&image_path) {
        return Some(image_path.to_string_lossy().into_owned());
    }
    if image_path.exists() {
        let _ = fs::remove_file(&image_path);
    }

    let bytes = download_authenticated_bytes(http?, preview_url, token).await?;
    if !looks_like_image_bytes(&bytes) {
        return None;
    }
    if fs::write(&image_path, bytes).is_ok() {
        Some(image_path.to_string_lossy().into_owned())
    } else {
        None
    }
}

async fn download_authenticated_bytes(
    http: &HttpClient,
    initial_url: &str,
    token: &str,
) -> Option<Vec<u8>> {
    let mut url = url::Url::parse(initial_url).ok()?;

    for _ in 0..5 {
        let response = http.get(url.clone()).bearer_auth(token).send().await.ok()?;
        if response.status().is_redirection() {
            let location = response.headers().get(header::LOCATION)?.to_str().ok()?;
            url = url.join(location).ok()?;
            continue;
        }
        if !response.status().is_success() {
            return None;
        }
        return response.bytes().await.ok().map(|bytes| bytes.to_vec());
    }

    None
}

fn looks_like_image_bytes(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0xFF, 0xD8, 0xFF])
        || bytes.starts_with(b"\x89PNG\r\n\x1A\n")
        || bytes.starts_with(b"GIF87a")
        || bytes.starts_with(b"GIF89a")
        || bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(&b"WEBP"[..])
}

fn fallback_attachment_summary(file: &SlackFile) -> AttachmentSummary {
    AttachmentSummary {
        kind: if slack_file_is_image(file) {
            AttachmentKind::Image
        } else {
            AttachmentKind::File
        },
        title: file
            .name
            .clone()
            .unwrap_or_else(|| "Slack attachment".to_string()),
        url: file
            .permalink
            .clone()
            .or_else(|| file.url_private.clone())
            .or_else(|| file.url_private_download.clone()),
        mime: file.mimetype.clone(),
    }
}

fn emoji_from_shortcode(name: &str) -> Option<String> {
    let normalized = name.trim_matches(':').replace('-', "_");
    match normalized.as_str() {
        "+1" | "thumbsup" => Some("👍".to_string()),
        "-1" | "thumbsdown" => Some("👎".to_string()),
        "simple_smile" => Some("😄".to_string()),
        "shipit" => Some("🚢".to_string()),
        _ => get_by_shortcode(&normalized).map(|emoji| emoji.as_str().to_string()),
    }
}

fn render_slack_text(text: &str) -> String {
    let mut rendered = String::with_capacity(text.len());
    let mut remainder = text;

    while let Some(start) = remainder.find(':') {
        rendered.push_str(&remainder[..start]);
        let candidate_remainder = &remainder[start + 1..];
        let Some(end) = candidate_remainder.find(':') else {
            rendered.push_str(&remainder[start..]);
            return rendered;
        };
        let shortcode = &candidate_remainder[..end];
        if !shortcode.is_empty()
            && shortcode.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '+' | '/')
            })
            && let Some(emoji) = emoji_from_shortcode(shortcode)
        {
            rendered.push_str(&emoji);
            remainder = &candidate_remainder[end + 1..];
            continue;
        }

        rendered.push(':');
        remainder = &remainder[start + 1..];
    }

    rendered.push_str(remainder);
    rendered
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct SlackTextStyle {
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
    code: bool,
    link: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SlackTextSegment {
    text: String,
    style: SlackTextStyle,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SlackRenderBlock {
    Paragraph(Vec<SlackTextSegment>),
    Quote(Vec<SlackTextSegment>),
    CodeBlock {
        code: String,
        language_hint: Option<String>,
    },
    Table {
        headers: Vec<Vec<SlackTextSegment>>,
        rows: Vec<Vec<Vec<SlackTextSegment>>>,
        alignments: Vec<SlackTableAlignment>,
    },
    UnorderedList(Vec<Vec<SlackTextSegment>>),
    OrderedList(Vec<(usize, Vec<SlackTextSegment>)>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SlackTableAlignment {
    Left,
    Center,
    Right,
}

#[derive(Clone, Debug, Default)]
struct SlackRenderLookup {
    user_names: BTreeMap<String, String>,
    channel_names: BTreeMap<String, String>,
}

impl SlackRenderLookup {
    fn from_state(state: &UiState) -> Self {
        Self {
            user_names: state.user_names.clone(),
            channel_names: state.known_channels.clone(),
        }
    }

    fn user_label(&self, user_id: &str) -> String {
        self.user_names
            .get(user_id)
            .cloned()
            .filter(|value| !value.is_empty())
            .map(|value| {
                if value.starts_with('@') {
                    value
                } else {
                    format!("@{value}")
                }
            })
            .unwrap_or_else(|| format!("@{user_id}"))
    }

    fn channel_label(&self, channel_id: &str) -> String {
        self.channel_names
            .get(channel_id)
            .cloned()
            .filter(|value| !value.is_empty())
            .map(|value| {
                if value.starts_with('#') {
                    value
                } else {
                    format!("#{value}")
                }
            })
            .unwrap_or_else(|| format!("#{channel_id}"))
    }
}

fn push_text_segment(
    segments: &mut Vec<SlackTextSegment>,
    text: impl Into<String>,
    style: &SlackTextStyle,
) {
    let text = text.into();
    if text.is_empty() {
        return;
    }
    if let Some(last) = segments.last_mut()
        && last.style == *style
    {
        last.text.push_str(&text);
        return;
    }
    segments.push(SlackTextSegment {
        text,
        style: style.clone(),
    });
}

fn append_segments(target: &mut Vec<SlackTextSegment>, segments: Vec<SlackTextSegment>) {
    for segment in segments {
        push_text_segment(target, segment.text, &segment.style);
    }
}

fn parse_wrapped_span<'a>(text: &'a str, marker: &str) -> Option<(usize, &'a str)> {
    let remainder = text.strip_prefix(marker)?;
    let end = remainder.find(marker)?;
    let inner = &remainder[..end];
    if inner.is_empty() {
        return None;
    }
    Some((marker.len() + end + marker.len(), inner))
}

fn parse_angle_link(text: &str) -> Option<(usize, String, String)> {
    if !text.starts_with('<') {
        return None;
    }
    let end = text.find('>')?;
    let inner = &text[1..end];
    let (url, label) = inner
        .split_once('|')
        .map(|(left, right)| (left.trim(), right.trim()))
        .unwrap_or_else(|| (inner.trim(), inner.trim()));
    if !(url.starts_with("http://") || url.starts_with("https://") || url.starts_with("mailto:")) {
        return None;
    }
    Some((end + 1, url.to_string(), label.to_string()))
}

fn style_with<F>(base: &SlackTextStyle, apply: F) -> SlackTextStyle
where
    F: FnOnce(&mut SlackTextStyle),
{
    let mut style = base.clone();
    apply(&mut style);
    style
}

fn parse_fallback_inline_segments(
    text: &str,
    base_style: &SlackTextStyle,
) -> Vec<SlackTextSegment> {
    let mut segments = Vec::new();
    let mut index = 0;

    while index < text.len() {
        let remainder = &text[index..];

        if let Some((consumed, url, label)) = parse_angle_link(remainder) {
            let link_label = if label.is_empty() { url.clone() } else { label };
            let mut linked = parse_fallback_inline_segments(
                &link_label,
                &style_with(base_style, |style| {
                    style.link = Some(url.clone());
                    style.underline = true;
                }),
            );
            if linked.is_empty() {
                push_text_segment(
                    &mut segments,
                    link_label,
                    &style_with(base_style, |style| {
                        style.link = Some(url);
                        style.underline = true;
                    }),
                );
            } else {
                append_segments(&mut segments, std::mem::take(&mut linked));
            }
            index += consumed;
            continue;
        }

        if let Some((consumed, inner)) = parse_wrapped_span(remainder, "`") {
            push_text_segment(
                &mut segments,
                inner,
                &style_with(base_style, |style| style.code = true),
            );
            index += consumed;
            continue;
        }
        if let Some((consumed, inner)) = parse_wrapped_span(remainder, "__") {
            append_segments(
                &mut segments,
                parse_fallback_inline_segments(
                    inner,
                    &style_with(base_style, |style| style.underline = true),
                ),
            );
            index += consumed;
            continue;
        }
        if let Some((consumed, inner)) = parse_wrapped_span(remainder, "*") {
            append_segments(
                &mut segments,
                parse_fallback_inline_segments(
                    inner,
                    &style_with(base_style, |style| style.bold = true),
                ),
            );
            index += consumed;
            continue;
        }
        if let Some((consumed, inner)) = parse_wrapped_span(remainder, "_") {
            append_segments(
                &mut segments,
                parse_fallback_inline_segments(
                    inner,
                    &style_with(base_style, |style| style.italic = true),
                ),
            );
            index += consumed;
            continue;
        }
        if let Some((consumed, inner)) = parse_wrapped_span(remainder, "~") {
            append_segments(
                &mut segments,
                parse_fallback_inline_segments(
                    inner,
                    &style_with(base_style, |style| style.strike = true),
                ),
            );
            index += consumed;
            continue;
        }

        let next_special = remainder
            .char_indices()
            .find(|(_, character)| matches!(character, '<' | '`' | '*' | '_' | '~'))
            .map(|(offset, _)| offset)
            .unwrap_or(remainder.len());
        if next_special > 0 {
            push_text_segment(&mut segments, &remainder[..next_special], base_style);
            index += next_special;
            continue;
        }

        let Some((_, character)) = remainder.char_indices().next() else {
            break;
        };
        push_text_segment(&mut segments, character.to_string(), base_style);
        index += character.len_utf8();
    }

    segments
}

fn unordered_list_content(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
        .or_else(|| trimmed.strip_prefix("• "))
}

fn ordered_list_content(line: &str) -> Option<(usize, &str)> {
    let trimmed = line.trim_start();
    let digit_end = trimmed.find(|character: char| !character.is_ascii_digit())?;
    if digit_end == 0 {
        return None;
    }
    let marker = trimmed.chars().nth(digit_end)?;
    if marker != '.' && marker != ')' {
        return None;
    }
    let body = trimmed[digit_end + 1..].trim_start();
    if body.is_empty() {
        return None;
    }
    Some((trimmed[..digit_end].parse().ok()?, body))
}

fn quote_content(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    trimmed
        .strip_prefix("> ")
        .or_else(|| trimmed.strip_prefix('>'))
}

fn fenced_code_language(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    if !trimmed.starts_with("```") {
        return None;
    }
    let remainder = trimmed.trim_start_matches('`').trim();
    if remainder.is_empty() {
        None
    } else {
        remainder
            .split_whitespace()
            .next()
            .map(|value| value.to_ascii_lowercase())
    }
}

fn split_gfm_table_row(line: &str) -> Option<Vec<String>> {
    let trimmed = line.trim();
    if !trimmed.contains('|') {
        return None;
    }
    let trimmed = trimmed.trim_matches('|');
    let cells = trimmed
        .split('|')
        .map(|cell| cell.trim().to_string())
        .collect::<Vec<_>>();
    if cells.is_empty() || cells.iter().all(|cell| cell.is_empty()) {
        None
    } else {
        Some(cells)
    }
}

fn parse_gfm_alignment_cell(cell: &str) -> Option<SlackTableAlignment> {
    let trimmed = cell.trim();
    if trimmed.len() < 3
        || !trimmed
            .chars()
            .all(|character| matches!(character, '-' | ':' | ' '))
    {
        return None;
    }
    let compact = trimmed.replace(' ', "");
    if compact
        .chars()
        .filter(|character| *character == '-')
        .count()
        < 1
    {
        return None;
    }
    let starts = compact.starts_with(':');
    let ends = compact.ends_with(':');
    Some(match (starts, ends) {
        (true, true) => SlackTableAlignment::Center,
        (false, true) => SlackTableAlignment::Right,
        _ => SlackTableAlignment::Left,
    })
}

fn parse_gfm_table_at(lines: &[&str], start: usize) -> Option<(usize, SlackRenderBlock)> {
    let header_cells = split_gfm_table_row(*lines.get(start)?)?;
    let separator_cells = split_gfm_table_row(*lines.get(start + 1)?)?;
    if header_cells.len() != separator_cells.len() || header_cells.is_empty() {
        return None;
    }
    let alignments = separator_cells
        .iter()
        .map(|cell| parse_gfm_alignment_cell(cell))
        .collect::<Option<Vec<_>>>()?;

    let mut rows = Vec::new();
    let mut index = start + 2;
    while let Some(line) = lines.get(index) {
        if line.trim().is_empty() {
            break;
        }
        let Some(cells) = split_gfm_table_row(line) else {
            break;
        };
        if cells.len() != header_cells.len() {
            break;
        }
        rows.push(
            cells
                .into_iter()
                .map(|cell| parse_fallback_inline_segments(&cell, &SlackTextStyle::default()))
                .collect::<Vec<_>>(),
        );
        index += 1;
    }

    Some((
        index,
        SlackRenderBlock::Table {
            headers: header_cells
                .into_iter()
                .map(|cell| parse_fallback_inline_segments(&cell, &SlackTextStyle::default()))
                .collect(),
            rows,
            alignments,
        },
    ))
}

fn parse_fallback_slack_blocks(text: &str) -> Vec<SlackRenderBlock> {
    let normalized = text.replace("\r\n", "\n");
    let lines = normalized.lines().collect::<Vec<_>>();
    let mut blocks = Vec::new();
    let mut index = 0;

    while index < lines.len() {
        let line = lines[index];
        if line.trim().is_empty() {
            index += 1;
            continue;
        }

        if line.trim_start().starts_with("```") {
            let mut code_lines = Vec::new();
            let language_hint = fenced_code_language(line);
            index += 1;
            while index < lines.len() && !lines[index].trim_start().starts_with("```") {
                code_lines.push(lines[index]);
                index += 1;
            }
            if index < lines.len() {
                index += 1;
            }
            blocks.push(SlackRenderBlock::CodeBlock {
                code: code_lines.join("\n"),
                language_hint,
            });
            continue;
        }

        if let Some((next_index, table)) = parse_gfm_table_at(&lines, index) {
            blocks.push(table);
            index = next_index;
            continue;
        }

        if let Some(content) = quote_content(line) {
            let mut quote_lines = vec![content];
            index += 1;
            while index < lines.len() {
                let Some(content) = quote_content(lines[index]) else {
                    break;
                };
                quote_lines.push(content);
                index += 1;
            }
            blocks.push(SlackRenderBlock::Quote(parse_fallback_inline_segments(
                &quote_lines.join("\n"),
                &SlackTextStyle::default(),
            )));
            continue;
        }

        if let Some(content) = unordered_list_content(line) {
            let mut items = vec![parse_fallback_inline_segments(
                content,
                &SlackTextStyle::default(),
            )];
            index += 1;
            while index < lines.len() {
                let Some(content) = unordered_list_content(lines[index]) else {
                    break;
                };
                items.push(parse_fallback_inline_segments(
                    content,
                    &SlackTextStyle::default(),
                ));
                index += 1;
            }
            blocks.push(SlackRenderBlock::UnorderedList(items));
            continue;
        }

        if let Some((number, content)) = ordered_list_content(line) {
            let mut items = vec![(
                number,
                parse_fallback_inline_segments(content, &SlackTextStyle::default()),
            )];
            index += 1;
            while index < lines.len() {
                let Some((number, content)) = ordered_list_content(lines[index]) else {
                    break;
                };
                items.push((
                    number,
                    parse_fallback_inline_segments(content, &SlackTextStyle::default()),
                ));
                index += 1;
            }
            blocks.push(SlackRenderBlock::OrderedList(items));
            continue;
        }

        let mut paragraph_lines = vec![line];
        index += 1;
        while index < lines.len()
            && !lines[index].trim().is_empty()
            && !lines[index].trim_start().starts_with("```")
            && quote_content(lines[index]).is_none()
            && unordered_list_content(lines[index]).is_none()
            && ordered_list_content(lines[index]).is_none()
        {
            paragraph_lines.push(lines[index]);
            index += 1;
        }
        blocks.push(SlackRenderBlock::Paragraph(parse_fallback_inline_segments(
            &paragraph_lines.join("\n"),
            &SlackTextStyle::default(),
        )));
    }

    blocks
}

fn rich_style_from_value(value: Option<&serde_json::Value>) -> SlackTextStyle {
    let Some(value) = value else {
        return SlackTextStyle::default();
    };
    SlackTextStyle {
        bold: value
            .get("bold")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        italic: value
            .get("italic")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        underline: value
            .get("underline")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        strike: value
            .get("strike")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        code: value
            .get("code")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        link: None,
    }
}

fn plain_text_from_rich_element(element: &serde_json::Value, lookup: &SlackRenderLookup) -> String {
    let kind = element
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    match kind {
        "text" => element
            .get("text")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
        "emoji" => element
            .get("name")
            .and_then(serde_json::Value::as_str)
            .and_then(emoji_from_shortcode)
            .unwrap_or_else(|| {
                element
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .map(|value| format!(":{value}:"))
                    .unwrap_or_default()
            }),
        "link" => element
            .get("text")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
            .or_else(|| element.get("url").and_then(serde_json::Value::as_str))
            .unwrap_or_default()
            .to_string(),
        "user" => element
            .get("user_id")
            .and_then(serde_json::Value::as_str)
            .map(|user_id| lookup.user_label(user_id))
            .unwrap_or_default(),
        "channel" => element
            .get("channel_id")
            .and_then(serde_json::Value::as_str)
            .map(|channel_id| lookup.channel_label(channel_id))
            .unwrap_or_default(),
        "broadcast" => element
            .get("range")
            .and_then(serde_json::Value::as_str)
            .map(|range| format!("@{range}"))
            .unwrap_or_default(),
        "usergroup" => element
            .get("name")
            .and_then(serde_json::Value::as_str)
            .map(|value| format!("@{value}"))
            .unwrap_or_default(),
        "date" => element
            .get("fallback")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
        _ => element
            .get("elements")
            .and_then(serde_json::Value::as_array)
            .map(|elements| {
                elements
                    .iter()
                    .map(|element| plain_text_from_rich_element(element, lookup))
                    .collect::<String>()
            })
            .unwrap_or_default(),
    }
}

fn rich_elements_to_segments(
    elements: &[serde_json::Value],
    lookup: &SlackRenderLookup,
) -> Vec<SlackTextSegment> {
    let mut segments = Vec::new();
    for element in elements {
        let kind = element
            .get("type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        match kind {
            "text" => {
                let style = rich_style_from_value(element.get("style"));
                let text = element
                    .get("text")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                push_text_segment(&mut segments, text, &style);
            }
            "emoji" => {
                let text = element
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .and_then(emoji_from_shortcode)
                    .unwrap_or_else(|| {
                        element
                            .get("name")
                            .and_then(serde_json::Value::as_str)
                            .map(|value| format!(":{value}:"))
                            .unwrap_or_default()
                    });
                push_text_segment(&mut segments, text, &SlackTextStyle::default());
            }
            "link" => {
                let url = element
                    .get("url")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let label = element
                    .get("text")
                    .and_then(serde_json::Value::as_str)
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned)
                    .unwrap_or_else(|| url.clone());
                let style = style_with(&rich_style_from_value(element.get("style")), |style| {
                    style.link = Some(url);
                    style.underline = true;
                });
                push_text_segment(&mut segments, label, &style);
            }
            "user" => {
                let text = element
                    .get("user_id")
                    .and_then(serde_json::Value::as_str)
                    .map(|user_id| lookup.user_label(user_id))
                    .unwrap_or_default();
                push_text_segment(&mut segments, text, &SlackTextStyle::default());
            }
            "channel" => {
                let text = element
                    .get("channel_id")
                    .and_then(serde_json::Value::as_str)
                    .map(|channel_id| lookup.channel_label(channel_id))
                    .unwrap_or_default();
                push_text_segment(&mut segments, text, &SlackTextStyle::default());
            }
            "broadcast" => {
                let text = element
                    .get("range")
                    .and_then(serde_json::Value::as_str)
                    .map(|range| format!("@{range}"))
                    .unwrap_or_default();
                push_text_segment(&mut segments, text, &SlackTextStyle::default());
            }
            "usergroup" => {
                let text = element
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .map(|value| format!("@{value}"))
                    .unwrap_or_default();
                push_text_segment(&mut segments, text, &SlackTextStyle::default());
            }
            "date" => {
                let text = element
                    .get("fallback")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                push_text_segment(&mut segments, text, &SlackTextStyle::default());
            }
            _ => {
                if let Some(nested) = element
                    .get("elements")
                    .and_then(serde_json::Value::as_array)
                {
                    append_segments(&mut segments, rich_elements_to_segments(nested, lookup));
                } else {
                    let text = plain_text_from_rich_element(element, lookup);
                    push_text_segment(&mut segments, text, &SlackTextStyle::default());
                }
            }
        }
    }
    segments
}

fn rich_list_item_segments(
    value: &serde_json::Value,
    lookup: &SlackRenderLookup,
) -> Vec<SlackTextSegment> {
    if let Some(elements) = value.get("elements").and_then(serde_json::Value::as_array) {
        return rich_elements_to_segments(elements, lookup);
    }
    vec![SlackTextSegment {
        text: plain_text_from_rich_element(value, lookup),
        style: SlackTextStyle::default(),
    }]
}

fn rich_block_to_render_blocks(
    block: &serde_json::Value,
    lookup: &SlackRenderLookup,
) -> Vec<SlackRenderBlock> {
    let kind = block
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    match kind {
        "rich_text" => block
            .get("elements")
            .and_then(serde_json::Value::as_array)
            .map(|elements| {
                elements
                    .iter()
                    .flat_map(|element| rich_block_to_render_blocks(element, lookup))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
        "rich_text_section" => vec![SlackRenderBlock::Paragraph(
            block
                .get("elements")
                .and_then(serde_json::Value::as_array)
                .map(|elements| rich_elements_to_segments(elements, lookup))
                .unwrap_or_default(),
        )],
        "rich_text_quote" => vec![SlackRenderBlock::Quote(
            block
                .get("elements")
                .and_then(serde_json::Value::as_array)
                .map(|elements| rich_elements_to_segments(elements, lookup))
                .unwrap_or_default(),
        )],
        "rich_text_preformatted" => vec![SlackRenderBlock::CodeBlock {
            code: block
                .get("elements")
                .and_then(serde_json::Value::as_array)
                .map(|elements| {
                    elements
                        .iter()
                        .map(|element| plain_text_from_rich_element(element, lookup))
                        .collect::<String>()
                })
                .unwrap_or_default(),
            language_hint: None,
        }],
        "rich_text_list" => {
            let style = block
                .get("style")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("bullet");
            let offset = block
                .get("offset")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0) as usize;
            let items = block
                .get("elements")
                .and_then(serde_json::Value::as_array)
                .map(|elements| {
                    elements
                        .iter()
                        .map(|element| rich_list_item_segments(element, lookup))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if style == "ordered" {
                vec![SlackRenderBlock::OrderedList(
                    items
                        .into_iter()
                        .enumerate()
                        .map(|(index, item)| (offset + index + 1, item))
                        .collect(),
                )]
            } else {
                vec![SlackRenderBlock::UnorderedList(items)]
            }
        }
        "section" => {
            let mut rendered = Vec::new();
            if let Some(text) = block
                .get("text")
                .and_then(|value| value.get("text"))
                .and_then(serde_json::Value::as_str)
            {
                rendered.extend(parse_fallback_slack_blocks(text));
            }
            if let Some(fields) = block.get("fields").and_then(serde_json::Value::as_array) {
                for field in fields {
                    if let Some(text) = field.get("text").and_then(serde_json::Value::as_str) {
                        rendered.extend(parse_fallback_slack_blocks(text));
                    }
                }
            }
            rendered
        }
        _ => {
            let text = plain_text_from_rich_element(block, lookup);
            if text.trim().is_empty() {
                Vec::new()
            } else {
                vec![SlackRenderBlock::Paragraph(parse_fallback_inline_segments(
                    &text,
                    &SlackTextStyle::default(),
                ))]
            }
        }
    }
}

fn parse_rich_text_blocks(
    blocks: &[serde_json::Value],
    lookup: &SlackRenderLookup,
) -> Vec<SlackRenderBlock> {
    blocks
        .iter()
        .flat_map(|block| rich_block_to_render_blocks(block, lookup))
        .filter(|block| match block {
            SlackRenderBlock::Paragraph(segments) | SlackRenderBlock::Quote(segments) => {
                !segments.is_empty()
            }
            SlackRenderBlock::CodeBlock { code, .. } => !code.trim().is_empty(),
            SlackRenderBlock::Table { headers, .. } => !headers.is_empty(),
            SlackRenderBlock::UnorderedList(items) => !items.is_empty(),
            SlackRenderBlock::OrderedList(items) => !items.is_empty(),
        })
        .collect()
}

fn slack_plain_text_from_blocks(
    blocks: &[serde_json::Value],
    lookup: &SlackRenderLookup,
) -> String {
    let render_blocks = parse_rich_text_blocks(blocks, lookup);
    let mut chunks = Vec::new();
    for block in render_blocks {
        match block {
            SlackRenderBlock::Paragraph(segments) | SlackRenderBlock::Quote(segments) => chunks
                .push(
                    segments
                        .into_iter()
                        .map(|segment| segment.text)
                        .collect::<String>(),
                ),
            SlackRenderBlock::CodeBlock { code, .. } => chunks.push(code),
            SlackRenderBlock::Table { headers, rows, .. } => {
                chunks.push(
                    headers
                        .into_iter()
                        .map(|cell| {
                            cell.into_iter()
                                .map(|segment| segment.text)
                                .collect::<String>()
                        })
                        .collect::<Vec<_>>()
                        .join(" | "),
                );
                for row in rows {
                    chunks.push(
                        row.into_iter()
                            .map(|cell| {
                                cell.into_iter()
                                    .map(|segment| segment.text)
                                    .collect::<String>()
                            })
                            .collect::<Vec<_>>()
                            .join(" | "),
                    );
                }
            }
            SlackRenderBlock::UnorderedList(items) => {
                for item in items {
                    chunks.push(
                        item.into_iter()
                            .map(|segment| segment.text)
                            .collect::<String>(),
                    );
                }
            }
            SlackRenderBlock::OrderedList(items) => {
                for (_, item) in items {
                    chunks.push(
                        item.into_iter()
                            .map(|segment| segment.text)
                            .collect::<String>(),
                    );
                }
            }
        }
    }
    chunks.join("\n")
}

fn render_segments_markup(segments: &[SlackTextSegment]) -> String {
    segments
        .iter()
        .map(|segment| {
            let mut content =
                glib::markup_escape_text(&render_slack_text(&segment.text)).to_string();
            if content.is_empty() {
                return String::new();
            }
            if segment.style.code {
                content = format!("<tt>{content}</tt>");
            }
            if segment.style.bold {
                content = format!("<b>{content}</b>");
            }
            if segment.style.italic {
                content = format!("<i>{content}</i>");
            }
            if segment.style.underline {
                content = format!("<u>{content}</u>");
            }
            if segment.style.strike {
                content = format!("<s>{content}</s>");
            }
            if let Some(url) = segment.style.link.as_ref() {
                let href = glib::markup_escape_text(url).to_string();
                content = format!("<a href=\"{href}\">{content}</a>");
            }
            content
        })
        .collect::<String>()
}

fn build_code_block_widget(code: &str) -> Label {
    let label = Label::new(Some(code));
    label.set_selectable(true);
    label.set_wrap(false);
    label.set_xalign(0.0);
    label.add_css_class("slack-code-text");
    label
}

fn build_markup_label(markup: &str, css_class: &str) -> Label {
    let label = Label::new(None);
    label.set_markup(markup);
    label.set_selectable(true);
    label.set_wrap(true);
    label.set_xalign(0.0);
    label.add_css_class(css_class);
    label
}

fn table_cell_xalign(alignment: SlackTableAlignment) -> f32 {
    match alignment {
        SlackTableAlignment::Left => 0.0,
        SlackTableAlignment::Center => 0.5,
        SlackTableAlignment::Right => 1.0,
    }
}

fn build_table_cell(
    segments: &[SlackTextSegment],
    alignment: SlackTableAlignment,
    css_class: &str,
) -> GtkBox {
    let shell = GtkBox::new(Orientation::Vertical, 0);
    shell.add_css_class(css_class);
    let label = build_markup_label(&render_segments_markup(segments), "slack-rich-table-text");
    label.set_xalign(table_cell_xalign(alignment));
    shell.append(&label);
    shell
}

fn build_table_widget(
    headers: &[Vec<SlackTextSegment>],
    rows: &[Vec<Vec<SlackTextSegment>>],
    alignments: &[SlackTableAlignment],
) -> GtkBox {
    let wrapper = GtkBox::new(Orientation::Vertical, 0);
    wrapper.add_css_class("slack-rich-table-wrap");
    wrapper.set_overflow(Overflow::Hidden);
    let grid = Grid::new();
    grid.add_css_class("slack-rich-table");
    grid.set_column_spacing(0);
    grid.set_row_spacing(0);
    grid.set_hexpand(true);
    grid.set_column_homogeneous(true);
    let last_column_index = headers.len().saturating_sub(1);
    let last_row_index = rows.len().saturating_sub(1);

    for (column, header) in headers.iter().enumerate() {
        let alignment = alignments
            .get(column)
            .copied()
            .unwrap_or(SlackTableAlignment::Left);
        let cell = build_table_cell(header, alignment, "slack-rich-table-header");
        if column == 0 {
            cell.add_css_class("slack-rich-table-corner-top-left");
        }
        if column == last_column_index {
            cell.add_css_class("slack-rich-table-edge-right");
            cell.add_css_class("slack-rich-table-corner-top-right");
            if rows.is_empty() {
                cell.add_css_class("slack-rich-table-corner-bottom-right");
            }
        }
        if column == 0 && rows.is_empty() {
            cell.add_css_class("slack-rich-table-corner-bottom-left");
        }
        if rows.is_empty() {
            cell.add_css_class("slack-rich-table-edge-bottom");
        }
        grid.attach(&cell, column as i32, 0, 1, 1);
    }

    for (row_index, row) in rows.iter().enumerate() {
        for (column, cell_segments) in row.iter().enumerate() {
            let alignment = alignments
                .get(column)
                .copied()
                .unwrap_or(SlackTableAlignment::Left);
            let cell = build_table_cell(cell_segments, alignment, "slack-rich-table-cell");
            if column == last_column_index {
                cell.add_css_class("slack-rich-table-edge-right");
            }
            if row_index == last_row_index {
                cell.add_css_class("slack-rich-table-edge-bottom");
            }
            if row_index == last_row_index && column == 0 {
                cell.add_css_class("slack-rich-table-corner-bottom-left");
            }
            if row_index == last_row_index && column == last_column_index {
                cell.add_css_class("slack-rich-table-corner-bottom-right");
            }
            grid.attach(&cell, column as i32, row_index as i32 + 1, 1, 1);
        }
    }

    wrapper.append(&grid);
    wrapper
}

fn should_prefer_markdown_blocks(blocks: &[SlackRenderBlock]) -> bool {
    blocks.iter().any(|block| {
        matches!(
            block,
            SlackRenderBlock::Table { .. } | SlackRenderBlock::CodeBlock { .. }
        )
    })
}

fn build_slack_body_widget(item: &TimelineItem, runtime: &UiRuntime) -> Option<GtkBox> {
    let lookup = {
        let state = runtime.state.borrow();
        SlackRenderLookup::from_state(&state)
    };
    let fallback_blocks = if item.body.trim().is_empty() {
        Vec::new()
    } else {
        parse_fallback_slack_blocks(&item.body)
    };
    let mut blocks = if !item.rich_text_blocks.is_empty() {
        let rich_blocks = parse_rich_text_blocks(&item.rich_text_blocks, &lookup);
        if should_prefer_markdown_blocks(&fallback_blocks) {
            fallback_blocks.clone()
        } else {
            rich_blocks
        }
    } else {
        fallback_blocks.clone()
    };
    if blocks.is_empty() {
        blocks = fallback_blocks;
    }
    if blocks.is_empty() {
        return None;
    }

    let container = GtkBox::new(Orientation::Vertical, 8);
    container.add_css_class("slack-rich-body");

    for block in blocks {
        match block {
            SlackRenderBlock::Paragraph(segments) => {
                let label =
                    build_markup_label(&render_segments_markup(&segments), "slack-rich-paragraph");
                container.append(&label);
            }
            SlackRenderBlock::Quote(segments) => {
                let wrapper = GtkBox::new(Orientation::Vertical, 0);
                wrapper.add_css_class("slack-rich-quote");
                let label =
                    build_markup_label(&render_segments_markup(&segments), "slack-rich-quote-text");
                wrapper.append(&label);
                container.append(&wrapper);
            }
            SlackRenderBlock::CodeBlock {
                code,
                language_hint: _,
            } => {
                let wrapper = GtkBox::new(Orientation::Vertical, 0);
                wrapper.add_css_class("slack-code-block");
                let label = build_code_block_widget(&code);
                wrapper.append(&label);
                container.append(&wrapper);
            }
            SlackRenderBlock::Table {
                headers,
                rows,
                alignments,
            } => {
                let table = build_table_widget(&headers, &rows, &alignments);
                container.append(&table);
            }
            SlackRenderBlock::UnorderedList(items) => {
                let list = GtkBox::new(Orientation::Vertical, 6);
                list.add_css_class("slack-rich-list");
                for item_segments in items {
                    let row = GtkBox::new(Orientation::Horizontal, 8);
                    row.add_css_class("slack-rich-list-row");
                    let bullet = Label::new(Some("•"));
                    bullet.add_css_class("slack-rich-list-bullet");
                    let label = build_markup_label(
                        &render_segments_markup(&item_segments),
                        "slack-rich-list-text",
                    );
                    row.append(&bullet);
                    row.append(&label);
                    list.append(&row);
                }
                container.append(&list);
            }
            SlackRenderBlock::OrderedList(items) => {
                let list = GtkBox::new(Orientation::Vertical, 6);
                list.add_css_class("slack-rich-list");
                for (number, item_segments) in items {
                    let row = GtkBox::new(Orientation::Horizontal, 8);
                    row.add_css_class("slack-rich-list-row");
                    let bullet = Label::new(Some(&format!("{number}.")));
                    bullet.add_css_class("slack-rich-list-bullet");
                    let label = build_markup_label(
                        &render_segments_markup(&item_segments),
                        "slack-rich-list-text",
                    );
                    row.append(&bullet);
                    row.append(&label);
                    list.append(&row);
                }
                container.append(&list);
            }
        }
    }

    Some(container)
}

fn normalize_reaction_name(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Some(emoji) = emojis::get(trimmed)
        && let Some(shortcode) = emoji.shortcode()
    {
        return Some(shortcode.to_string());
    }

    let normalized = trimmed.trim_matches(':').trim().to_ascii_lowercase();
    if normalized.is_empty()
        || !normalized.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '+')
        })
    {
        return None;
    }

    match normalized.as_str() {
        "thumbsup" => return Some("+1".to_string()),
        "thumbsdown" => return Some("-1".to_string()),
        _ => {}
    }

    if get_by_shortcode(&normalized).is_some() {
        return Some(normalized);
    }

    let underscored = normalized.replace('-', "_");
    if get_by_shortcode(&underscored).is_some() {
        return Some(underscored);
    }

    Some(normalized)
}

fn reaction_emoji(name: &str) -> String {
    emoji_from_shortcode(name).unwrap_or_else(|| format!(":{name}:"))
}

fn reaction_summaries(
    reactions: &[SlackReaction],
    current_user_id: Option<&str>,
) -> Vec<ReactionSummary> {
    let mut summaries = reactions
        .iter()
        .map(|reaction| ReactionSummary {
            name: reaction.name.clone(),
            emoji: reaction_emoji(&reaction.name),
            count: reaction.count,
            me: current_user_id
                .is_some_and(|user_id| reaction.users.iter().any(|entry| entry == user_id)),
        })
        .collect::<Vec<_>>();
    summaries.sort_by(|left, right| right.count.cmp(&left.count));
    summaries
}

fn send_reaction(
    session: &StoredSlackSession,
    item: &TimelineItem,
    reaction_name: &str,
) -> Result<ReactionUpdate> {
    let client = SlackClient::api_only()?;
    let token = session
        .user_access_token
        .as_deref()
        .or(session.bot_access_token.as_deref())
        .context("no Slack access token is stored")?;
    let runtime = Runtime::new().context("failed to create runtime for Slack reaction send")?;
    runtime
        .block_on(client.add_reaction(token, &item.channel_id, &item.message_ts, reaction_name))
        .context("failed to add Slack reaction")?;

    Ok(ReactionUpdate {
        message_ts: item.message_ts.clone(),
        reaction_name: reaction_name.to_string(),
        reactor_user_id: session.user_id.clone().or(session.bot_user_id.clone()),
    })
}

fn start_add_reaction(
    item: TimelineItem,
    raw_reaction_name: &str,
    handles: &UiHandles,
    runtime: &UiRuntime,
) -> bool {
    let Some(reaction_name) = normalize_reaction_name(raw_reaction_name) else {
        handles
            .startup_status
            .set_text("Reaction must be like :thumbsup:, party-parrot, or 👍.");
        return false;
    };

    if item.reactions.iter().any(|reaction| {
        reaction.me
            && normalize_reaction_name(&reaction.name).as_deref() == Some(reaction_name.as_str())
    }) {
        handles
            .startup_status
            .set_text("You already reacted with that emoji.");
        return false;
    }

    let session = match &*runtime.auth_state.borrow() {
        SlackAuthStatus::Connected(session) => session.clone(),
        _ => {
            handles
                .startup_status
                .set_text("Connect Slack before adding a reaction.");
            return false;
        }
    };

    handles
        .startup_status
        .set_text(&format!("Adding :{reaction_name}: to Slack..."));
    let auth_tx = runtime.auth_tx.clone();
    let workspace_key = runtime.state.borrow().active_workspace_key().to_string();
    std::thread::spawn(move || {
        let result =
            send_reaction(&session, &item, &reaction_name).map_err(|error| error.to_string());
        let _ = auth_tx.send(AuthEvent::ReactionAdded {
            workspace_key,
            result,
        });
    });
    true
}

fn load_share_link(session: &StoredSlackSession, item: &TimelineItem) -> Result<ShareLinkUpdate> {
    let client = SlackClient::api_only()?;
    let token = session
        .user_access_token
        .as_deref()
        .or(session.bot_access_token.as_deref())
        .context("no Slack access token is stored")?;
    let runtime = Runtime::new().context("failed to create runtime for Slack permalink lookup")?;
    let response = runtime
        .block_on(client.get_permalink(token, &item.channel_id, &item.message_ts))
        .context("failed to fetch Slack permalink")?;
    Ok(ShareLinkUpdate {
        permalink: response.permalink,
    })
}

fn start_share_link_copy(item: TimelineItem, handles: &UiHandles, runtime: &UiRuntime) {
    let session = match &*runtime.auth_state.borrow() {
        SlackAuthStatus::Connected(session) => session.clone(),
        _ => {
            handles
                .startup_status
                .set_text("Connect Slack before copying a share link.");
            return;
        }
    };

    handles
        .startup_status
        .set_text("Fetching Slack message URL...");
    let auth_tx = runtime.auth_tx.clone();
    let workspace_key = runtime.state.borrow().active_workspace_key().to_string();
    std::thread::spawn(move || {
        let result = load_share_link(&session, &item).map_err(|error| error.to_string());
        let _ = auth_tx.send(AuthEvent::ShareLinkResolved {
            workspace_key,
            result,
        });
    });
}

fn start_edit_message(
    item: TimelineItem,
    body: String,
    handles: &UiHandles,
    runtime: &UiRuntime,
) -> bool {
    let body = body.trim().to_string();
    if body.is_empty() {
        handles
            .startup_status
            .set_text("Edited message body is empty.");
        return false;
    }
    if !item_owned_by_connected_user(&item, runtime) {
        handles
            .startup_status
            .set_text("Only your own Slack messages can be edited.");
        return false;
    }

    let session = match &*runtime.auth_state.borrow() {
        SlackAuthStatus::Connected(session) => session.clone(),
        _ => {
            handles
                .startup_status
                .set_text("Connect Slack before editing a message.");
            return false;
        }
    };

    handles.startup_status.set_text("Saving Slack edit...");
    let auth_tx = runtime.auth_tx.clone();
    let workspace_key = runtime.state.borrow().active_workspace_key().to_string();
    std::thread::spawn(move || {
        let result =
            edit_message_on_slack(&session, &item, &body).map_err(|error| error.to_string());
        let _ = auth_tx.send(AuthEvent::MessageEdited {
            workspace_key,
            result,
        });
    });
    true
}

fn edit_message_on_slack(
    session: &StoredSlackSession,
    item: &TimelineItem,
    body: &str,
) -> Result<MessageEditUpdate> {
    let client = SlackClient::api_only()?;
    let token = session
        .user_access_token
        .as_deref()
        .or(session.bot_access_token.as_deref())
        .context("no Slack access token is stored")?;
    let runtime = Runtime::new().context("failed to create runtime for Slack edit")?;
    let response = runtime
        .block_on(client.update_message(token, &item.channel_id, &item.message_ts, body))
        .context("failed to edit Slack message")?;

    Ok(MessageEditUpdate {
        message_ts: response.ts,
        body: body.to_string(),
    })
}

fn start_delete_message(item: TimelineItem, handles: &UiHandles, runtime: &UiRuntime) -> bool {
    if !item_owned_by_connected_user(&item, runtime) {
        handles
            .startup_status
            .set_text("Only your own Slack messages can be deleted.");
        return false;
    }

    let session = match &*runtime.auth_state.borrow() {
        SlackAuthStatus::Connected(session) => session.clone(),
        _ => {
            handles
                .startup_status
                .set_text("Connect Slack before deleting a message.");
            return false;
        }
    };

    handles.startup_status.set_text("Deleting Slack message...");
    let auth_tx = runtime.auth_tx.clone();
    let workspace_key = runtime.state.borrow().active_workspace_key().to_string();
    std::thread::spawn(move || {
        let result = delete_message_on_slack(&session, &item).map_err(|error| error.to_string());
        let _ = auth_tx.send(AuthEvent::MessageDeleted {
            workspace_key,
            result,
        });
    });
    true
}

fn delete_message_on_slack(
    session: &StoredSlackSession,
    item: &TimelineItem,
) -> Result<MessageDeleteUpdate> {
    let client = SlackClient::api_only()?;
    let token = session
        .user_access_token
        .as_deref()
        .or(session.bot_access_token.as_deref())
        .context("no Slack access token is stored")?;
    let runtime = Runtime::new().context("failed to create runtime for Slack delete")?;
    runtime
        .block_on(client.delete_message(token, &item.channel_id, &item.message_ts))
        .context("failed to delete Slack message")?;

    Ok(MessageDeleteUpdate {
        message_ts: item.message_ts.clone(),
    })
}

fn start_create_channel(
    raw_name: String,
    is_private: bool,
    handles: &UiHandles,
    runtime: &UiRuntime,
) {
    let name = raw_name.trim().to_string();
    if name.is_empty() {
        handles
            .startup_status
            .set_text("Enter a channel name first.");
        return;
    }
    let session = match &*runtime.auth_state.borrow() {
        SlackAuthStatus::Connected(session) => session.clone(),
        _ => {
            handles
                .startup_status
                .set_text("Connect Slack before creating a channel.");
            return;
        }
    };
    handles.startup_status.set_text("Creating Slack channel...");
    let auth_tx = runtime.auth_tx.clone();
    let workspace_key = runtime.state.borrow().active_workspace_key().to_string();
    std::thread::spawn(move || {
        let result =
            create_channel_on_slack(&session, &name, is_private).map_err(|error| error.to_string());
        let _ = auth_tx.send(AuthEvent::AdminActionFinished {
            workspace_key,
            result,
        });
    });
}

fn start_rename_channel(
    channel_id: Option<String>,
    raw_name: String,
    handles: &UiHandles,
    runtime: &UiRuntime,
) {
    let Some(channel_id) = channel_id.filter(|value| !value.is_empty()) else {
        handles.startup_status.set_text("Select a channel first.");
        return;
    };
    let name = raw_name.trim().to_string();
    if name.is_empty() {
        handles
            .startup_status
            .set_text("Enter a new channel name first.");
        return;
    }
    let session = match &*runtime.auth_state.borrow() {
        SlackAuthStatus::Connected(session) => session.clone(),
        _ => {
            handles
                .startup_status
                .set_text("Connect Slack before renaming a channel.");
            return;
        }
    };
    handles.startup_status.set_text("Renaming Slack channel...");
    let auth_tx = runtime.auth_tx.clone();
    let workspace_key = runtime.state.borrow().active_workspace_key().to_string();
    std::thread::spawn(move || {
        let result = rename_channel_on_slack(&session, &channel_id, &name)
            .map_err(|error| error.to_string());
        let _ = auth_tx.send(AuthEvent::AdminActionFinished {
            workspace_key,
            result,
        });
    });
}

fn start_archive_channel(channel_id: Option<String>, handles: &UiHandles, runtime: &UiRuntime) {
    let Some(channel_id) = channel_id.filter(|value| !value.is_empty()) else {
        handles.startup_status.set_text("Select a channel first.");
        return;
    };
    let session = match &*runtime.auth_state.borrow() {
        SlackAuthStatus::Connected(session) => session.clone(),
        _ => {
            handles
                .startup_status
                .set_text("Connect Slack before archiving a channel.");
            return;
        }
    };
    handles
        .startup_status
        .set_text("Archiving Slack channel...");
    let auth_tx = runtime.auth_tx.clone();
    let workspace_key = runtime.state.borrow().active_workspace_key().to_string();
    std::thread::spawn(move || {
        let result =
            archive_channel_on_slack(&session, &channel_id).map_err(|error| error.to_string());
        let _ = auth_tx.send(AuthEvent::AdminActionFinished {
            workspace_key,
            result,
        });
    });
}

fn start_invite_member(
    channel_id: Option<String>,
    user_id: Option<String>,
    handles: &UiHandles,
    runtime: &UiRuntime,
) {
    let Some(channel_id) = channel_id.filter(|value| !value.is_empty()) else {
        handles.startup_status.set_text("Select a channel first.");
        return;
    };
    let Some(user_id) = user_id.filter(|value| !value.is_empty()) else {
        handles.startup_status.set_text("Select a member first.");
        return;
    };
    let session = match &*runtime.auth_state.borrow() {
        SlackAuthStatus::Connected(session) => session.clone(),
        _ => {
            handles
                .startup_status
                .set_text("Connect Slack before inviting a member.");
            return;
        }
    };
    handles
        .startup_status
        .set_text("Inviting member to Slack channel...");
    let auth_tx = runtime.auth_tx.clone();
    let workspace_key = runtime.state.borrow().active_workspace_key().to_string();
    std::thread::spawn(move || {
        let result = invite_member_on_slack(&session, &channel_id, &user_id)
            .map_err(|error| error.to_string());
        let _ = auth_tx.send(AuthEvent::AdminActionFinished {
            workspace_key,
            result,
        });
    });
}

fn start_kick_member(
    channel_id: Option<String>,
    user_id: Option<String>,
    handles: &UiHandles,
    runtime: &UiRuntime,
) {
    let Some(channel_id) = channel_id.filter(|value| !value.is_empty()) else {
        handles.startup_status.set_text("Select a channel first.");
        return;
    };
    let Some(user_id) = user_id.filter(|value| !value.is_empty()) else {
        handles.startup_status.set_text("Select a member first.");
        return;
    };
    let session = match &*runtime.auth_state.borrow() {
        SlackAuthStatus::Connected(session) => session.clone(),
        _ => {
            handles
                .startup_status
                .set_text("Connect Slack before removing a member.");
            return;
        }
    };
    handles
        .startup_status
        .set_text("Removing member from Slack channel...");
    let auth_tx = runtime.auth_tx.clone();
    let workspace_key = runtime.state.borrow().active_workspace_key().to_string();
    std::thread::spawn(move || {
        let result = kick_member_on_slack(&session, &channel_id, &user_id)
            .map_err(|error| error.to_string());
        let _ = auth_tx.send(AuthEvent::AdminActionFinished {
            workspace_key,
            result,
        });
    });
}

fn create_channel_on_slack(
    session: &StoredSlackSession,
    name: &str,
    is_private: bool,
) -> Result<AdminActionUpdate> {
    let client = SlackClient::api_only()?;
    let token = session
        .user_access_token
        .as_deref()
        .or(session.bot_access_token.as_deref())
        .context("no Slack access token is stored")?;
    let runtime = Runtime::new().context("failed to create runtime for Slack admin action")?;
    let response = runtime
        .block_on(client.create_conversation(token, name, is_private))
        .context("failed to create Slack channel")?;
    let channel_name = response
        .channel
        .display_name()
        .map(str::to_string)
        .unwrap_or_else(|| response.channel.id.clone());
    Ok(AdminActionUpdate {
        message: format!("Created Slack channel #{channel_name}."),
        created_channel: Some((response.channel.id, channel_name)),
        renamed_channel: None,
        archived_channel_id: None,
    })
}

fn rename_channel_on_slack(
    session: &StoredSlackSession,
    channel_id: &str,
    name: &str,
) -> Result<AdminActionUpdate> {
    let client = SlackClient::api_only()?;
    let token = session
        .user_access_token
        .as_deref()
        .or(session.bot_access_token.as_deref())
        .context("no Slack access token is stored")?;
    let runtime = Runtime::new().context("failed to create runtime for Slack admin action")?;
    let response = runtime
        .block_on(client.rename_conversation(token, channel_id, name))
        .context("failed to rename Slack channel")?;
    let channel_name = response
        .channel
        .display_name()
        .map(str::to_string)
        .unwrap_or_else(|| response.channel.id.clone());
    Ok(AdminActionUpdate {
        message: format!("Renamed Slack channel to #{channel_name}."),
        created_channel: None,
        renamed_channel: Some((response.channel.id, channel_name)),
        archived_channel_id: None,
    })
}

fn archive_channel_on_slack(
    session: &StoredSlackSession,
    channel_id: &str,
) -> Result<AdminActionUpdate> {
    let client = SlackClient::api_only()?;
    let token = session
        .user_access_token
        .as_deref()
        .or(session.bot_access_token.as_deref())
        .context("no Slack access token is stored")?;
    let runtime = Runtime::new().context("failed to create runtime for Slack admin action")?;
    runtime
        .block_on(client.archive_conversation(token, channel_id))
        .context("failed to archive Slack channel")?;
    Ok(AdminActionUpdate {
        message: "Archived Slack channel.".to_string(),
        created_channel: None,
        renamed_channel: None,
        archived_channel_id: Some(channel_id.to_string()),
    })
}

fn invite_member_on_slack(
    session: &StoredSlackSession,
    channel_id: &str,
    user_id: &str,
) -> Result<AdminActionUpdate> {
    let client = SlackClient::api_only()?;
    let token = session
        .user_access_token
        .as_deref()
        .or(session.bot_access_token.as_deref())
        .context("no Slack access token is stored")?;
    let runtime = Runtime::new().context("failed to create runtime for Slack admin action")?;
    runtime
        .block_on(client.invite_to_conversation(token, channel_id, user_id))
        .context("failed to invite member to Slack channel")?;
    Ok(AdminActionUpdate {
        message: format!("Invited {user_id} to the Slack channel."),
        created_channel: None,
        renamed_channel: None,
        archived_channel_id: None,
    })
}

fn kick_member_on_slack(
    session: &StoredSlackSession,
    channel_id: &str,
    user_id: &str,
) -> Result<AdminActionUpdate> {
    let client = SlackClient::api_only()?;
    let token = session
        .user_access_token
        .as_deref()
        .or(session.bot_access_token.as_deref())
        .context("no Slack access token is stored")?;
    let runtime = Runtime::new().context("failed to create runtime for Slack admin action")?;
    runtime
        .block_on(client.kick_from_conversation(token, channel_id, user_id))
        .context("failed to remove member from Slack channel")?;
    Ok(AdminActionUpdate {
        message: format!("Removed {user_id} from the Slack channel."),
        created_channel: None,
        renamed_channel: None,
        archived_channel_id: None,
    })
}

fn send_channel_message(
    session: &StoredSlackSession,
    channel_id: &str,
    channel_name: &str,
    body: &str,
    settings: &AppSettings,
    self_avatar_path: Option<String>,
) -> Result<TimelineItem> {
    let client = SlackClient::api_only()?;
    let token = session
        .user_access_token
        .as_deref()
        .or(session.bot_access_token.as_deref())
        .context("no Slack access token is stored")?;
    let runtime = Runtime::new().context("failed to create runtime for Slack post send")?;
    let response = runtime
        .block_on(client.post_message(token, channel_id, body))
        .context("failed to post Slack message")?;

    let now = Utc::now();
    Ok(TimelineItem {
        workspace_id: session
            .team_id
            .clone()
            .unwrap_or_else(|| "workspace".to_string()),
        channel_id: response.channel,
        channel_name: channel_name.to_string(),
        message_ts: response.ts.clone(),
        thread_ts: response.ts,
        author_id: session
            .user_id
            .clone()
            .or_else(|| session.bot_user_id.clone())
            .unwrap_or_else(|| "U-me".to_string()),
        author_name: "you".to_string(),
        author_avatar_path: self_avatar_path,
        body: body.to_string(),
        rich_text_blocks: vec![],
        reactions: vec![],
        attachments: vec![],
        unread: false,
        participant: true,
        direct_mention: false,
        focus_keyword_hits: settings.timeline.matching_keywords(body),
        watch_weight: settings
            .timeline
            .channel_weights
            .get(channel_id)
            .copied()
            .unwrap_or(1),
        last_activity_at: now,
        reply_state: ReplyState::Idle,
    })
}

fn send_thread_reply(
    session: &StoredSlackSession,
    selected: &TimelineItem,
    body: &str,
    settings: &AppSettings,
    self_avatar_path: Option<String>,
) -> Result<TimelineItem> {
    let client = SlackClient::api_only()?;
    let token = session
        .user_access_token
        .as_deref()
        .or(session.bot_access_token.as_deref())
        .context("no Slack access token is stored")?;
    let runtime = Runtime::new().context("failed to create runtime for Slack reply send")?;
    let response = runtime
        .block_on(client.post_thread_reply(token, &selected.channel_id, &selected.thread_ts, body))
        .context("failed to post Slack thread reply")?;

    let now = Utc::now();
    Ok(TimelineItem {
        workspace_id: selected.workspace_id.clone(),
        channel_id: response.channel,
        channel_name: selected.channel_name.clone(),
        message_ts: response.ts,
        thread_ts: selected.thread_ts.clone(),
        author_id: session
            .user_id
            .clone()
            .or_else(|| session.bot_user_id.clone())
            .unwrap_or_else(|| "U-me".to_string()),
        author_name: "you".to_string(),
        author_avatar_path: self_avatar_path,
        body: body.to_string(),
        rich_text_blocks: vec![],
        reactions: vec![],
        attachments: vec![],
        unread: false,
        participant: true,
        direct_mention: false,
        focus_keyword_hits: settings.timeline.matching_keywords(body),
        watch_weight: selected.watch_weight,
        last_activity_at: now,
        reply_state: ReplyState::Idle,
    })
}

fn is_syncable_conversation(channel: &SlackConversation) -> bool {
    if channel.is_archived.unwrap_or(false) {
        return false;
    }
    if channel.is_private.unwrap_or(false) {
        channel.is_member.unwrap_or(true)
    } else {
        true
    }
}

fn prioritize_initial_sync_conversations(
    mut channels: Vec<SlackConversation>,
    settings: &AppSettings,
) -> Vec<SlackConversation> {
    channels.sort_by(|left, right| {
        let left_priority = conversation_matches_initial_priority(settings, left);
        let right_priority = conversation_matches_initial_priority(settings, right);
        right_priority
            .cmp(&left_priority)
            .then_with(|| left.id.cmp(&right.id))
    });
    channels
}

fn conversation_matches_initial_priority(
    settings: &AppSettings,
    channel: &SlackConversation,
) -> bool {
    conversation_matches_active_search_profile(settings, channel)
        || conversation_matches_office_profile(settings, channel)
}

fn conversation_matches_active_search_profile(
    settings: &AppSettings,
    channel: &SlackConversation,
) -> bool {
    let channel_name = channel
        .display_name()
        .map(|name| format!("#{name}"))
        .unwrap_or_else(|| channel.id.clone());
    settings
        .channel_matches_active_search_profile(&channel.id, &channel_name)
        .unwrap_or(false)
}

fn conversation_matches_office_profile(
    settings: &AppSettings,
    channel: &SlackConversation,
) -> bool {
    let Some(profile_id) = settings.office.channel_profile_id.as_deref().or_else(|| {
        settings
            .channel_profiles
            .iter()
            .find(|profile| profile.id == "times_channels")
            .map(|profile| profile.id.as_str())
    }) else {
        return false;
    };
    let Some(profile) = settings
        .channel_profiles
        .iter()
        .find(|profile| profile.id == profile_id)
    else {
        return false;
    };
    let channel_name = channel
        .display_name()
        .map(|name| format!("#{name}"))
        .unwrap_or_else(|| channel.id.clone());
    channel_profile_matches_channel(profile, &channel.id, &channel_name).unwrap_or(false)
}

async fn history_message_to_timeline_item(
    workspace_id: &str,
    channel: &SlackConversation,
    current_user_id: Option<&str>,
    settings: &AppSettings,
    user_names: &BTreeMap<String, String>,
    user_avatar_paths: &BTreeMap<String, String>,
    token: &str,
    image_cache_dir: Option<&Path>,
    message: SlackHistoryMessage,
) -> Option<TimelineItem> {
    let thread_ts = message
        .thread_ts
        .clone()
        .unwrap_or_else(|| message.ts.clone());
    if thread_ts != message.ts {
        return None;
    }

    match message.subtype.as_deref() {
        Some("message_changed" | "message_deleted" | "channel_join" | "channel_leave") => {
            return None;
        }
        _ => {}
    }

    let attachments =
        cached_message_attachments(Some(token), image_cache_dir, &message.files).await;
    let rich_text_blocks = message.blocks.clone();
    let mut body = message.text.unwrap_or_default().trim().to_string();
    if body.is_empty() && !rich_text_blocks.is_empty() {
        let lookup = SlackRenderLookup {
            user_names: user_names.clone(),
            channel_names: BTreeMap::new(),
        };
        body = slack_plain_text_from_blocks(&rich_text_blocks, &lookup)
            .trim()
            .to_string();
    }
    if body.is_empty() && attachments.is_empty() {
        return None;
    }

    let author_id = message
        .user
        .clone()
        .or_else(|| message.bot_id.clone())
        .unwrap_or_else(|| "unknown".to_string());
    let participant = current_user_id.is_some_and(|user_id| user_id == author_id);
    let author_name = if participant {
        "you".to_string()
    } else {
        user_names
            .get(&author_id)
            .cloned()
            .unwrap_or_else(|| author_id.clone())
    };
    let author_avatar_path = user_avatar_paths.get(&author_id).cloned();
    let direct_mention =
        current_user_id.is_some_and(|user_id| body.contains(&format!("<@{user_id}>")));
    let last_activity_at = message
        .latest_reply
        .as_deref()
        .and_then(slack_ts_to_datetime)
        .or_else(|| slack_ts_to_datetime(&message.ts))
        .unwrap_or_else(Utc::now);

    Some(TimelineItem {
        workspace_id: workspace_id.to_string(),
        channel_id: channel.id.clone(),
        channel_name: channel
            .display_name()
            .map(|name| format!("#{name}"))
            .unwrap_or_else(|| channel.id.clone()),
        message_ts: message.ts.clone(),
        thread_ts,
        author_id,
        author_name,
        author_avatar_path,
        body: body.clone(),
        rich_text_blocks,
        reactions: reaction_summaries(&message.reactions, current_user_id),
        attachments,
        unread: true,
        participant,
        direct_mention,
        focus_keyword_hits: settings.timeline.matching_keywords(&body),
        watch_weight: settings
            .timeline
            .channel_weights
            .get(&channel.id)
            .copied()
            .unwrap_or(1),
        last_activity_at,
        reply_state: ReplyState::Idle,
    })
}

fn seed_watched_channels(settings: &mut AppSettings, items: &[TimelineItem]) -> usize {
    if !settings.timeline.watched_channels.is_empty() {
        return 0;
    }

    let channel_ids = items
        .iter()
        .map(|item| item.channel_id.clone())
        .collect::<BTreeSet<_>>();
    for channel_id in &channel_ids {
        settings
            .timeline
            .watched_channels
            .insert(channel_id.clone());
        settings
            .timeline
            .channel_weights
            .entry(channel_id.clone())
            .or_insert(1);
    }
    channel_ids.len()
}

fn seed_default_notification_rules(settings: &mut AppSettings) -> bool {
    if settings.notification_rules.is_empty() {
        settings.notification_rules.push(NotificationRule {
            id: Uuid::new_v4(),
            label: "All incoming activity".into(),
            enabled: true,
            channels: BTreeSet::new(),
            authors: BTreeSet::new(),
            include: vec![],
            exclude: vec![],
            keyword_profile_ids: vec![],
            section_profile_ids: vec![],
            channel_profile_ids: vec![],
            author_profile_ids: vec![],
            search_profile_ids: vec![],
            thread_participation_only: false,
            quiet_hours: None,
            action: NotificationAction::Notify,
        });
        return true;
    }

    if settings.notification_rules.len() != 1 {
        return false;
    }

    let rule = &mut settings.notification_rules[0];
    if rule.label != "Watched channel activity"
        || !rule.enabled
        || !rule.authors.is_empty()
        || !rule.include.is_empty()
        || !rule.exclude.is_empty()
        || !rule.keyword_profile_ids.is_empty()
        || !rule.section_profile_ids.is_empty()
        || !rule.channel_profile_ids.is_empty()
        || !rule.author_profile_ids.is_empty()
        || rule.thread_participation_only
        || rule.quiet_hours.is_some()
        || rule.action != NotificationAction::Notify
    {
        return false;
    }

    rule.label = "All incoming activity".into();
    if rule.channels.is_empty() {
        return true;
    }
    rule.channels.clear();
    true
}

fn slack_ts_from_datetime(timestamp: chrono::DateTime<Utc>) -> String {
    format!(
        "{}.{:06}",
        timestamp.timestamp(),
        timestamp.timestamp_subsec_micros()
    )
}

fn slack_ts_to_datetime(raw: &str) -> Option<chrono::DateTime<Utc>> {
    let (seconds, fraction) = raw.split_once('.').unwrap_or((raw, "0"));
    let seconds = seconds.parse::<i64>().ok()?;
    let micros = fraction
        .chars()
        .take(6)
        .collect::<String>()
        .parse::<u32>()
        .ok()?;
    chrono::DateTime::<Utc>::from_timestamp(seconds, micros * 1_000)
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct SlackPermalinkTarget {
    channel_id: String,
    message_ts: String,
}

fn permalink_token_to_slack_ts(token: &str) -> Option<String> {
    let digits = token.strip_prefix('p')?;
    if digits.len() < 7 || !digits.chars().all(|character| character.is_ascii_digit()) {
        return None;
    }
    let (seconds, micros) = digits.split_at(digits.len() - 6);
    Some(format!("{seconds}.{micros}"))
}

fn extract_slack_permalink_targets(text: &str) -> Vec<SlackPermalinkTarget> {
    let mut targets = BTreeSet::new();

    for token in text.split_whitespace() {
        let candidate = token
            .trim_matches(|character: char| {
                matches!(
                    character,
                    '<' | '>' | '(' | ')' | '[' | ']' | ',' | '.' | '"' | '\''
                )
            })
            .split('|')
            .next()
            .unwrap_or("");
        let Ok(url) = url::Url::parse(candidate) else {
            continue;
        };
        if !url
            .domain()
            .is_some_and(|domain| domain.ends_with("slack.com"))
        {
            continue;
        }

        let Some(mut segments) = url.path_segments() else {
            continue;
        };
        if segments.next() != Some("archives") {
            continue;
        }
        let Some(channel_id) = segments.next() else {
            continue;
        };
        let Some(message_token) = segments.next() else {
            continue;
        };
        let Some(message_ts) = permalink_token_to_slack_ts(message_token) else {
            continue;
        };

        targets.insert(SlackPermalinkTarget {
            channel_id: channel_id.to_string(),
            message_ts,
        });
    }

    targets.into_iter().collect()
}

fn build_icon_button(icon_name: &str, tooltip: &str) -> Button {
    let button = Button::new();
    button.add_css_class("nav-button");
    button.set_tooltip_text(Some(tooltip));
    let icon = Image::builder().icon_name(icon_name).pixel_size(20).build();
    button.set_child(Some(&icon));
    button
}

fn build_reply_button(reply_count: usize, tooltip: &str) -> Button {
    let button = Button::new();
    button.add_css_class("reply-button");
    button.set_tooltip_text(Some(tooltip));

    let shell = GtkBox::new(Orientation::Horizontal, 6);
    let icon = Image::builder()
        .icon_name("mail-reply-sender-symbolic")
        .pixel_size(16)
        .build();
    let count = Label::new(Some(&reply_count.to_string()));
    count.add_css_class("reply-count");
    shell.append(&icon);
    shell.append(&count);
    button.set_child(Some(&shell));
    button
}

fn build_inline_icon_button(icon_name: &str, tooltip: &str) -> Button {
    let button = Button::new();
    button.add_css_class("reply-button");
    button.set_tooltip_text(Some(tooltip));
    let icon = Image::builder().icon_name(icon_name).pixel_size(16).build();
    button.set_child(Some(&icon));
    button
}

fn connected_principal_ids(runtime: &UiRuntime) -> BTreeSet<String> {
    match &*runtime.auth_state.borrow() {
        SlackAuthStatus::Connected(session) => session
            .user_id
            .iter()
            .chain(session.bot_user_id.iter())
            .cloned()
            .collect(),
        _ => BTreeSet::new(),
    }
}

fn item_owned_by_connected_user(item: &TimelineItem, runtime: &UiRuntime) -> bool {
    connected_principal_ids(runtime).contains(&item.author_id)
}

fn build_edit_message_menu_button(
    item: &TimelineItem,
    handles: &UiHandles,
    runtime: &UiRuntime,
) -> MenuButton {
    let button = MenuButton::new();
    button.add_css_class("reply-button");
    button.set_tooltip_text(Some("Edit this Slack message"));
    button.set_always_show_arrow(false);
    let icon = Image::builder()
        .icon_name("document-edit-symbolic")
        .pixel_size(16)
        .build();
    button.set_child(Some(&icon));

    let popover = gtk::Popover::new();
    popover.set_has_arrow(false);
    let content = GtkBox::new(Orientation::Vertical, 8);
    let editor = TextView::new();
    editor.set_wrap_mode(WrapMode::WordChar);
    editor.set_size_request(280, 120);
    editor.buffer().set_text(&item.body);
    let save_button = Button::with_label("Save edit");
    save_button.add_css_class("suggested-action");
    content.append(&editor);
    content.append(&save_button);
    popover.set_child(Some(&content));
    button.set_popover(Some(&popover));

    {
        let handles = handles.clone();
        let runtime = runtime.clone();
        let item = item.clone();
        let editor = editor.clone();
        let popover = popover.clone();
        save_button.connect_clicked(move |_| {
            let body = buffer_text(&editor.buffer());
            if start_edit_message(item.clone(), body, &handles, &runtime) {
                popover.popdown();
            }
        });
    }

    button
}

fn build_delete_message_menu_button(
    item: &TimelineItem,
    handles: &UiHandles,
    runtime: &UiRuntime,
) -> MenuButton {
    let button = MenuButton::new();
    button.add_css_class("reply-button");
    button.set_tooltip_text(Some("Delete this Slack message"));
    button.set_always_show_arrow(false);
    let icon = Image::builder()
        .icon_name("user-trash-symbolic")
        .pixel_size(16)
        .build();
    button.set_child(Some(&icon));

    let popover = gtk::Popover::new();
    popover.set_has_arrow(false);
    let content = GtkBox::new(Orientation::Vertical, 8);
    let prompt = Label::new(Some("Delete this message from Slack?"));
    prompt.add_css_class("meta");
    prompt.set_wrap(true);
    prompt.set_xalign(0.0);
    let confirm = Button::with_label("Delete");
    content.append(&prompt);
    content.append(&confirm);
    popover.set_child(Some(&content));
    button.set_popover(Some(&popover));

    {
        let handles = handles.clone();
        let runtime = runtime.clone();
        let item = item.clone();
        let popover = popover.clone();
        confirm.connect_clicked(move |_| {
            if start_delete_message(item.clone(), &handles, &runtime) {
                popover.popdown();
            }
        });
    }

    button
}

fn copy_text_to_clipboard(handles: &UiHandles, text: &str, success_message: &str) -> bool {
    let Some(display) = gdk::Display::default() else {
        handles
            .startup_status
            .set_text("Clipboard is unavailable in this GTK session.");
        return false;
    };
    display.clipboard().set_text(text);
    handles.startup_status.set_text(success_message);
    true
}

fn start_channel_post(handles: &UiHandles, runtime: &UiRuntime) {
    let body = handles.composer_entry.text().to_string();
    if body.trim().is_empty() {
        handles.startup_status.set_text("Message body is empty.");
        return;
    }

    let Some(channel_id) = handles
        .composer_channel
        .active_id()
        .map(|id| id.to_string())
    else {
        handles
            .startup_status
            .set_text("Choose a channel before sending.");
        return;
    };
    if channel_id.is_empty() {
        handles
            .startup_status
            .set_text("Choose a channel before sending.");
        return;
    }

    let session = match &*runtime.auth_state.borrow() {
        SlackAuthStatus::Connected(session) => session.clone(),
        _ => {
            handles
                .startup_status
                .set_text("Connect Slack before sending a message.");
            return;
        }
    };
    let (channel_name, settings, self_avatar_path) = {
        let state = runtime.state.borrow();
        if !state.settings.can_write_channel(&channel_id) {
            handles.startup_status.set_text(
                "This channel is configured as read-only. Change channel permissions before posting.",
            );
            return;
        }
        let channel_name = state
            .channel_name_for(&channel_id)
            .unwrap_or_else(|| channel_id.clone());
        let self_avatar_path = session
            .user_id
            .as_deref()
            .or(session.bot_user_id.as_deref())
            .and_then(|author_id| state.author_avatar_path_for(author_id));
        (channel_name, state.settings.clone(), self_avatar_path)
    };

    handles
        .startup_status
        .set_text("Sending message to Slack...");
    handles.composer_entry.set_sensitive(false);
    handles.composer_channel.set_sensitive(false);
    handles.composer_send_button.set_sensitive(false);

    let auth_tx = runtime.auth_tx.clone();
    let workspace_key = runtime.state.borrow().active_workspace_key().to_string();
    std::thread::spawn(move || {
        let result = send_channel_message(
            &session,
            &channel_id,
            &channel_name,
            &body,
            &settings,
            self_avatar_path,
        )
        .map_err(|error| error.to_string());
        let _ = auth_tx.send(AuthEvent::ChannelPostSent {
            workspace_key,
            result,
        });
    });
}

fn refresh_ui(handles: &UiHandles, runtime: &UiRuntime) {
    runtime.state.borrow_mut().cleanup_expired_typing();
    refresh_rail_buttons(handles, runtime);
    refresh_main_surface(handles, runtime);
    refresh_filter_controls(handles, runtime);
    sync_timeline_store(&handles.timeline_store, runtime);
    rebuild_stack_columns(&handles.deck_columns, runtime, handles);
}

fn refresh_ui_resetting_config_editor(handles: &UiHandles, runtime: &UiRuntime) {
    *runtime.config_editor_widget.borrow_mut() = None;
    refresh_ui(handles, runtime);
}

fn refresh_rail_buttons(handles: &UiHandles, runtime: &UiRuntime) {
    let deck_state = runtime.deck_state.borrow();
    let main_view = runtime.state.borrow().main_view();
    set_button_active(&handles.timeline_button, main_view == MainView::Timeline);
    set_button_active(&handles.office_button, main_view == MainView::Office);
    set_button_active(
        &handles.admin_button,
        deck_state.has_admin() || deck_state.has_config_editor(),
    );
    set_button_active(&handles.settings_button, deck_state.has_settings());
}

fn refresh_main_surface(handles: &UiHandles, runtime: &UiRuntime) {
    let state = runtime.state.borrow();
    let is_office = state.main_view() == MainView::Office;
    handles
        .timeline_title
        .set_text(if is_office { "Office" } else { "Recent" });
    handles.filter_bar.set_visible(!is_office);
    handles.composer_bar.set_visible(!is_office);
    handles.composer_typing_label.set_visible(!is_office);
    handles.timeline_scroll.set_visible(!is_office);
    handles.office_scroll.set_visible(is_office);
    handles.office_summary.set_visible(is_office);

    if !is_office {
        handles.office_summary.set_text("");
        while let Some(child) = handles.office_scene.first_child() {
            handles.office_scene.remove(&child);
        }
        return;
    }

    let office_profile_label = state
        .office_channel_profile()
        .map(|profile| profile.label.clone())
        .unwrap_or_else(|| "No office channel profile selected.".to_string());
    let presence = state.office_presence_items();
    if presence.is_empty() {
        handles.office_summary.set_text(&format!(
            "Office source: {office_profile_label}. Pick a channel profile in Settings and make sure it matches readable channels."
        ));
    } else {
        handles.office_summary.set_text(&format!(
            "Office source: {office_profile_label}. {} desks are currently active.",
            presence.len()
        ));
    }
    drop(state);

    rebuild_office_view(&handles.office_scene, &presence, handles, runtime);
}

fn refresh_filter_controls(handles: &UiHandles, runtime: &UiRuntime) {
    *runtime.ui_sync_suppressed.borrow_mut() = true;
    let state = runtime.state.borrow();
    let search_profiles = state.search_profiles();
    let available_sections = state.available_sections();
    let available_channels = state.available_channels();
    let available_post_channels = state.available_post_channels();
    let available_authors = state.available_authors();
    let selected_post_channel = handles
        .composer_channel
        .active_id()
        .map(|value| value.to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| available_post_channels.first().map(|(id, _)| id.clone()));
    let typing_summary = selected_post_channel
        .as_deref()
        .and_then(|channel_id| state.typing_summary_for_channel(channel_id));

    if handles.search_entry.text().as_str() != state.search_query() {
        handles.search_entry.set_text(state.search_query());
    }

    sync_filter_picker(
        &handles.search_profile_filter,
        "All profiles",
        &search_profiles,
        state.active_search_profile_id(),
    );
    sync_filter_picker(
        &handles.section_filter,
        "All sections",
        &available_sections,
        state.section_filter(),
    );
    sync_filter_picker(
        &handles.channel_filter,
        "All channels",
        &available_channels,
        state.channel_filter(),
    );
    sync_filter_picker(
        &handles.author_filter,
        "All people",
        &available_authors,
        state.author_filter(),
    );
    sync_filter_picker(
        &handles.composer_channel,
        "Post to...",
        &available_post_channels,
        selected_post_channel.as_deref(),
    );
    let can_post = matches!(&*runtime.auth_state.borrow(), SlackAuthStatus::Connected(_))
        && !available_post_channels.is_empty();
    handles.composer_entry.set_sensitive(can_post);
    handles.composer_channel.set_sensitive(can_post);
    handles.composer_send_button.set_sensitive(
        can_post
            && handles
                .composer_channel
                .active_id()
                .is_some_and(|value| !value.is_empty()),
    );
    handles
        .composer_typing_label
        .set_text(typing_summary.as_deref().unwrap_or(""));
    drop(state);
    *runtime.ui_sync_suppressed.borrow_mut() = false;
}

fn set_button_active(button: &Button, active: bool) {
    if active {
        button.add_css_class("active");
    } else {
        button.remove_css_class("active");
    }
}

fn normalize_shortcut_modifiers(modifiers: gdk::ModifierType) -> gdk::ModifierType {
    modifiers
        & (gdk::ModifierType::CONTROL_MASK
            | gdk::ModifierType::SHIFT_MASK
            | gdk::ModifierType::ALT_MASK
            | gdk::ModifierType::META_MASK
            | gdk::ModifierType::SUPER_MASK)
}

fn accelerator_matches(shortcut: &str, key: gdk::Key, modifiers: gdk::ModifierType) -> bool {
    let Some((expected_key, expected_modifiers)) = gtk::accelerator_parse(shortcut) else {
        eprintln!("[slaxide] invalid shortcut binding ignored: {shortcut}");
        return false;
    };
    key == expected_key
        && normalize_shortcut_modifiers(modifiers)
            == normalize_shortcut_modifiers(expected_modifiers)
}

fn handle_window_shortcuts(
    handles: &UiHandles,
    runtime: &UiRuntime,
    key: gdk::Key,
    modifiers: gdk::ModifierType,
) -> bool {
    let shortcuts = runtime.state.borrow().settings.shortcuts.clone();

    let matches_any = |bindings: &[String]| {
        bindings
            .iter()
            .any(|shortcut| accelerator_matches(shortcut, key, modifiers))
    };

    if matches_any(&shortcuts.open_settings) {
        runtime.deck_state.borrow_mut().open_settings();
        refresh_ui(handles, runtime);
        return true;
    }
    if matches_any(&shortcuts.open_admin) {
        runtime.deck_state.borrow_mut().open_admin();
        refresh_ui(handles, runtime);
        return true;
    }
    if matches_any(&shortcuts.focus_search) {
        handles.search_entry.grab_focus();
        return true;
    }
    if matches_any(&shortcuts.focus_composer) {
        handles.composer_entry.grab_focus();
        return true;
    }
    if matches_any(&shortcuts.close_column) {
        let mut deck_state = runtime.deck_state.borrow_mut();
        if deck_state.columns.is_empty() {
            return false;
        }
        let last_index = deck_state.columns.len().saturating_sub(1);
        deck_state.close(last_index);
        drop(deck_state);
        refresh_ui(handles, runtime);
        return true;
    }

    false
}

fn shortcut_summary_lines(shortcuts: &ShortcutBindings) -> Vec<String> {
    [
        ("Open settings", &shortcuts.open_settings),
        ("Open admin", &shortcuts.open_admin),
        ("Focus search", &shortcuts.focus_search),
        ("Focus composer", &shortcuts.focus_composer),
        ("Close right column", &shortcuts.close_column),
    ]
    .into_iter()
    .map(|(label, bindings)| format!("{label}: {}", bindings.join(", ")))
    .collect()
}

fn sync_filter_picker(
    picker: &ComboBoxText,
    all_label: &str,
    options: &[(String, String)],
    selected_id: Option<&str>,
) {
    picker.remove_all();
    picker.append(Some(""), all_label);
    for (id, label) in options {
        picker.append(Some(id), label);
    }
    let applied = selected_id.is_some_and(|id| picker.set_active_id(Some(id)));
    if !applied {
        picker.set_active(Some(0));
    }
}

fn relative_timestamp_text(timestamp: chrono::DateTime<Utc>) -> String {
    let now = Utc::now();
    let age = now.signed_duration_since(timestamp);
    let seconds = age.num_seconds().max(0);

    if seconds < 45 {
        "now".to_string()
    } else if seconds < 3_600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h", seconds / 3_600)
    } else if seconds < 604_800 {
        format!("{}d", seconds / 86_400)
    } else {
        let local = timestamp.with_timezone(&Local);
        if local.year() == Local::now().year() {
            local.format("%b %-d").to_string()
        } else {
            local.format("%Y-%m-%d").to_string()
        }
    }
}

fn absolute_local_timestamp_text(timestamp: chrono::DateTime<Utc>) -> String {
    timestamp
        .with_timezone(&Local)
        .format("%Y-%m-%d %H:%M")
        .to_string()
}

fn attachment_image_needs_refresh(attachment: &AttachmentSummary) -> bool {
    attachment.kind == AttachmentKind::Image
        && attachment
            .url
            .as_deref()
            .is_some_and(|path| !cached_file_is_renderable_image(Path::new(path)))
}

fn build_attachment_strip(attachments: &[AttachmentSummary], image_width: i32) -> GtkBox {
    let strip = GtkBox::new(Orientation::Vertical, 8);
    strip.add_css_class("attachment-strip");

    for attachment in attachments {
        strip.append(&build_attachment_widget(attachment, image_width));
    }

    strip
}

fn build_attachment_widget(attachment: &AttachmentSummary, image_width: i32) -> GtkBox {
    if attachment.kind == AttachmentKind::Image
        && let Some(path) = attachment
            .url
            .as_deref()
            .filter(|path| Path::new(path).exists())
        && let Some(picture) = build_local_picture(path, image_width, 420)
    {
        let shell = GtkBox::new(Orientation::Vertical, 0);
        picture.add_css_class("attachment-image");
        shell.append(&picture);
        return shell;
    }

    let card = GtkBox::new(Orientation::Vertical, 6);
    card.add_css_class("attachment-card");

    let title = Label::new(Some(&attachment.title));
    title.add_css_class("attachment-label");
    title.set_wrap(true);
    title.set_xalign(0.0);
    card.append(&title);

    if let Some(mime) = attachment.mime.as_deref() {
        let mime_label = Label::new(Some(mime));
        mime_label.add_css_class("meta");
        mime_label.set_xalign(0.0);
        card.append(&mime_label);
    }

    card
}

fn office_message_preview(item: &TimelineItem) -> String {
    let rendered = render_slack_text(&item.body)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if !rendered.is_empty() {
        let mut preview = rendered
            .chars()
            .take(OFFICE_BUBBLE_BODY_LIMIT)
            .collect::<String>();
        if rendered.chars().count() > OFFICE_BUBBLE_BODY_LIMIT {
            preview.push('…');
        }
        return preview;
    }

    match item.attachments.as_slice() {
        [attachment] if attachment.kind == AttachmentKind::Image => "shared a new image".into(),
        [attachment] => format!("shared {}", attachment.title),
        attachments if !attachments.is_empty() => {
            format!("shared {} attachments", attachments.len())
        }
        _ => "is quiet right now".to_string(),
    }
}

fn office_activity_tier_class(activity_count: usize) -> &'static str {
    if activity_count >= 8 {
        "office-desk-hot"
    } else if activity_count >= 4 {
        "office-desk-warm"
    } else {
        "office-desk-calm"
    }
}

fn build_office_prop(class_name: &str, width: i32, height: i32) -> GtkBox {
    let widget = GtkBox::new(Orientation::Vertical, 0);
    widget.add_css_class(class_name);
    widget.set_size_request(width, height);
    widget
}

fn build_office_desk_widget() -> gtk::Fixed {
    let desk = gtk::Fixed::new();
    desk.set_size_request(136, 58);
    let top = build_office_prop("office-desk-top", 136, 18);
    let trim = build_office_prop("office-desk-trim", 136, 6);
    let modesty = build_office_prop("office-desk-modesty", 96, 14);
    let left_leg = build_office_prop("office-desk-leg", 10, 34);
    let right_leg = build_office_prop("office-desk-leg", 10, 34);
    let drawer = build_office_prop("office-desk-drawer", 28, 14);
    desk.put(&top, 0.0, 0.0);
    desk.put(&trim, 0.0, 18.0);
    desk.put(&modesty, 20.0, 22.0);
    desk.put(&left_leg, 14.0, 24.0);
    desk.put(&right_leg, 112.0, 24.0);
    desk.put(&drawer, 98.0, 24.0);
    desk
}

fn build_office_monitor_widget() -> gtk::Fixed {
    let monitor = gtk::Fixed::new();
    monitor.set_size_request(46, 40);
    let shell = build_office_prop("office-monitor-shell", 42, 26);
    let screen = build_office_prop("office-monitor-screen", 32, 18);
    let stand = build_office_prop("office-monitor-stand", 8, 9);
    let base = build_office_prop("office-monitor-base", 22, 4);
    monitor.put(&shell, 2.0, 0.0);
    monitor.put(&screen, 7.0, 4.0);
    monitor.put(&stand, 19.0, 26.0);
    monitor.put(&base, 12.0, 35.0);
    monitor
}

fn build_office_keyboard_widget() -> gtk::Fixed {
    let keyboard = gtk::Fixed::new();
    keyboard.set_size_request(56, 18);
    let shell = build_office_prop("office-keyboard-shell", 56, 18);
    keyboard.put(&shell, 0.0, 0.0);
    for (row, count) in [(2.0, 7_i32), (7.0, 8_i32), (12.0, 6_i32)] {
        for index in 0..count {
            let key = build_office_prop("office-key", 4, 3);
            keyboard.put(&key, 4.0 + (index as f64 * 6.0), row);
        }
    }
    keyboard
}

fn build_office_chair_widget() -> gtk::Fixed {
    let chair = gtk::Fixed::new();
    chair.set_size_request(66, 54);
    let back = build_office_prop("office-chair-back", 28, 16);
    let seat = build_office_prop("office-chair-seat", 40, 12);
    let stem = build_office_prop("office-chair-stem", 6, 11);
    let base = build_office_prop("office-chair-base", 24, 4);
    chair.put(&back, 18.0, 0.0);
    chair.put(&seat, 13.0, 16.0);
    chair.put(&stem, 30.0, 28.0);
    chair.put(&base, 21.0, 39.0);
    for x in [8.0, 24.0, 38.0, 52.0] {
        let wheel = build_office_prop("office-chair-wheel", 6, 6);
        chair.put(&wheel, x, 44.0);
    }
    chair
}

fn build_office_mug_widget() -> gtk::Fixed {
    let mug = gtk::Fixed::new();
    mug.set_size_request(16, 16);
    let body = build_office_prop("office-mug-body", 11, 11);
    let handle = build_office_prop("office-mug-handle", 4, 7);
    mug.put(&body, 0.0, 3.0);
    mug.put(&handle, 11.0, 5.0);
    mug
}

fn build_office_sofa_widget() -> gtk::Fixed {
    let sofa = gtk::Fixed::new();
    sofa.set_size_request(132, 72);
    let back = build_office_prop("office-sofa-back", 132, 24);
    let left_arm = build_office_prop("office-sofa-arm", 16, 36);
    let right_arm = build_office_prop("office-sofa-arm", 16, 36);
    let seat = build_office_prop("office-sofa-seat", 100, 18);
    let left_cushion = build_office_prop("office-sofa-cushion", 44, 20);
    let right_cushion = build_office_prop("office-sofa-cushion", 44, 20);
    let left_leg = build_office_prop("office-sofa-leg", 8, 10);
    let right_leg = build_office_prop("office-sofa-leg", 8, 10);
    sofa.put(&back, 0.0, 0.0);
    sofa.put(&left_arm, 0.0, 20.0);
    sofa.put(&right_arm, 116.0, 20.0);
    sofa.put(&seat, 16.0, 28.0);
    sofa.put(&left_cushion, 20.0, 24.0);
    sofa.put(&right_cushion, 68.0, 24.0);
    sofa.put(&left_leg, 24.0, 60.0);
    sofa.put(&right_leg, 100.0, 60.0);
    sofa
}

fn build_office_coffee_table_widget() -> gtk::Fixed {
    let table = gtk::Fixed::new();
    table.set_size_request(128, 58);
    let top = build_office_prop("office-table-top", 128, 16);
    let shelf = build_office_prop("office-table-shelf", 84, 8);
    let left_leg = build_office_prop("office-table-leg", 10, 28);
    let right_leg = build_office_prop("office-table-leg", 10, 28);
    let book = build_office_prop("office-table-book", 26, 8);
    let mug = build_office_prop("office-table-mug", 12, 12);
    table.put(&top, 0.0, 0.0);
    table.put(&left_leg, 18.0, 16.0);
    table.put(&right_leg, 100.0, 16.0);
    table.put(&shelf, 22.0, 34.0);
    table.put(&book, 34.0, 4.0);
    table.put(&mug, 88.0, 2.0);
    table
}

fn build_office_bubble_tail() -> gtk::DrawingArea {
    let tail = gtk::DrawingArea::new();
    tail.set_content_width(28);
    tail.set_content_height(18);
    tail.add_css_class("office-bubble-tail");
    tail.set_draw_func(|_, cr, width, height| {
        cr.set_antialias(gtk::cairo::Antialias::Best);
        cr.set_source_rgb(0.98, 0.98, 0.98);
        cr.move_to(5.0, 2.0);
        cr.line_to((width as f64) - 6.0, 2.0);
        cr.line_to((width as f64 / 2.0) + 2.0, (height as f64) - 3.0);
        cr.close_path();
        let _ = cr.fill_preserve();
        cr.set_source_rgb(0.66, 0.66, 0.62);
        cr.set_line_width(2.0);
        let _ = cr.stroke();
    });
    tail
}

fn build_office_planter_widget() -> gtk::Fixed {
    let planter = gtk::Fixed::new();
    planter.set_size_request(58, 58);
    let pot = build_office_prop("office-planter-pot", 28, 16);
    let soil = build_office_prop("office-planter-soil", 24, 4);
    planter.put(&pot, 15.0, 38.0);
    planter.put(&soil, 17.0, 38.0);
    for (class_name, x, y, w, h) in [
        ("office-planter-leaf-a", 6.0, 18.0, 14, 18),
        ("office-planter-leaf-b", 18.0, 8.0, 16, 24),
        ("office-planter-leaf-c", 30.0, 14.0, 16, 20),
        ("office-planter-leaf-b", 22.0, 20.0, 10, 16),
        ("office-planter-leaf-a", 40.0, 22.0, 10, 15),
    ] {
        let leaf = build_office_prop(class_name, w, h);
        planter.put(&leaf, x, y);
    }
    planter
}

fn build_office_break_area_widget() -> gtk::Fixed {
    let area = gtk::Fixed::new();
    area.set_size_request(360, 220);
    let mat = build_office_prop("office-break-mat", 360, 220);
    let sofa_left = build_office_sofa_widget();
    let sofa_right = build_office_sofa_widget();
    let table = build_office_coffee_table_widget();
    let label = Label::new(Some("break space"));
    label.add_css_class("office-zone-label");
    area.put(&mat, 0.0, 0.0);
    area.put(&sofa_left, 34.0, 18.0);
    area.put(&sofa_right, 194.0, 18.0);
    area.put(&table, 116.0, 124.0);
    area.put(&label, 126.0, 184.0);
    area
}

fn build_office_whiteboard_widget() -> gtk::Fixed {
    let board = gtk::Fixed::new();
    board.set_size_request(180, 92);
    let frame = build_office_prop("office-whiteboard-frame", 180, 92);
    let panel = build_office_prop("office-whiteboard-panel", 164, 76);
    board.put(&frame, 0.0, 0.0);
    board.put(&panel, 8.0, 8.0);
    for (class_name, x, y) in [
        ("office-note-yellow", 20.0, 18.0),
        ("office-note-blue", 54.0, 22.0),
        ("office-note-red", 92.0, 16.0),
        ("office-note-yellow", 126.0, 26.0),
    ] {
        let note = build_office_prop(class_name, 18, 18);
        board.put(&note, x, y);
    }
    board
}

fn build_office_vending_widget() -> gtk::Fixed {
    let vending = gtk::Fixed::new();
    vending.set_size_request(74, 138);
    let shell = build_office_prop("office-vending-shell", 74, 138);
    let window = build_office_prop("office-vending-window", 42, 62);
    let panel = build_office_prop("office-vending-panel", 18, 48);
    let slot = build_office_prop("office-vending-slot", 34, 8);
    vending.put(&shell, 0.0, 0.0);
    vending.put(&window, 8.0, 10.0);
    vending.put(&panel, 50.0, 18.0);
    vending.put(&slot, 20.0, 114.0);
    for row in 0..4 {
        let light = build_office_prop("office-vending-button", 6, 6);
        vending.put(&light, 56.0, 24.0 + row as f64 * 10.0);
    }
    vending
}

fn build_office_bookshelf_widget() -> gtk::Fixed {
    let shelf = gtk::Fixed::new();
    shelf.set_size_request(96, 156);
    let shell = build_office_prop("office-bookshelf-shell", 96, 156);
    shelf.put(&shell, 0.0, 0.0);
    for y in [18.0, 52.0, 86.0, 120.0] {
        let plank = build_office_prop("office-bookshelf-plank", 84, 6);
        shelf.put(&plank, 6.0, y);
    }
    for (class_name, x, y, h) in [
        ("office-book-green", 12.0, 24.0, 20),
        ("office-book-red", 24.0, 22.0, 22),
        ("office-book-blue", 38.0, 23.0, 21),
        ("office-book-yellow", 54.0, 24.0, 20),
        ("office-book-blue", 16.0, 58.0, 22),
        ("office-book-green", 30.0, 60.0, 20),
        ("office-book-red", 46.0, 58.0, 22),
        ("office-book-yellow", 62.0, 60.0, 20),
        ("office-book-red", 18.0, 94.0, 20),
        ("office-book-blue", 34.0, 92.0, 22),
        ("office-book-green", 50.0, 94.0, 20),
        ("office-book-yellow", 66.0, 92.0, 22),
    ] {
        let book = build_office_prop(class_name, 10, h);
        shelf.put(&book, x, y);
    }
    shelf
}

fn office_tile_seed(column: i32, row: i32) -> f64 {
    let raw = ((column * 37) + (row * 53) + (column * row * 11)).rem_euclid(17) as f64;
    raw / 17.0
}

fn draw_wood_tile(cr: &gtk::cairo::Context, x: f64, y: f64, size: f64, column: i32, row: i32) {
    let seed = office_tile_seed(column, row);
    let base_red = 0.77 + (seed * 0.08);
    let base_green = 0.60 + (seed * 0.05);
    let base_blue = 0.42 + (seed * 0.03);
    cr.set_source_rgb(base_red, base_green, base_blue);
    cr.rectangle(x, y, size, size);
    let _ = cr.fill();

    cr.set_source_rgb(0.57, 0.39, 0.23);
    for band in 0..5 {
        let band_y = y + (band as f64 * (size / 5.0)) + ((seed * 7.0) % 6.0);
        cr.move_to(x + 6.0, band_y);
        cr.curve_to(
            x + (size * 0.26),
            band_y - 3.0,
            x + (size * 0.62),
            band_y + 4.0,
            x + size - 8.0,
            band_y + 1.0,
        );
        let _ = cr.stroke();
    }

    cr.set_source_rgb(0.46, 0.29, 0.16);
    cr.rectangle(x, y, size, size);
    let _ = cr.stroke();
}

fn build_office_background() -> gtk::DrawingArea {
    let background = gtk::DrawingArea::new();
    background.add_css_class("office-background");
    background.set_content_width(OFFICE_SCENE_WIDTH_PX);
    background.set_content_height(OFFICE_SCENE_HEIGHT_PX);
    background.set_draw_func(|_, cr, width, height| {
        cr.set_antialias(gtk::cairo::Antialias::None);
        let width = width as f64;
        let height = height as f64;
        let wall_height = 252.0;
        let baseboard_height = 16.0;
        let tile_size = 96.0;

        cr.set_source_rgb(0.97, 0.97, 0.95);
        cr.rectangle(0.0, 0.0, width, wall_height);
        let _ = cr.fill();

        cr.set_source_rgb(0.92, 0.92, 0.90);
        for stripe in 0..=((width / 28.0).ceil() as i32) {
            cr.rectangle(stripe as f64 * 28.0, 0.0, 2.0, wall_height);
            let _ = cr.fill();
        }

        cr.set_source_rgb(0.90, 0.90, 0.88);
        for dot_row in 0..=((wall_height / 34.0).ceil() as i32) {
            for dot_col in 0..=((width / 36.0).ceil() as i32) {
                let center_x = 14.0 + dot_col as f64 * 36.0;
                let center_y = 12.0 + dot_row as f64 * 34.0;
                cr.arc(center_x, center_y, 1.2, 0.0, std::f64::consts::TAU);
                let _ = cr.fill();
            }
        }

        cr.set_source_rgb(0.86, 0.86, 0.83);
        cr.rectangle(0.0, wall_height - baseboard_height, width, baseboard_height);
        let _ = cr.fill();

        let floor_y = wall_height;
        let rows = ((height - floor_y) / tile_size).ceil() as i32;
        let columns = (width / tile_size).ceil() as i32;
        for row in 0..=rows {
            for column in 0..=columns {
                draw_wood_tile(
                    cr,
                    column as f64 * tile_size,
                    floor_y + row as f64 * tile_size,
                    tile_size,
                    column,
                    row,
                );
            }
        }
    });
    background
}

fn office_workstation_scene(presence: Option<&OfficePresence>) -> gtk::Fixed {
    let scene = gtk::Fixed::new();
    scene.set_size_request(OFFICE_WORKSTATION_WIDTH_PX, OFFICE_WORKSTATION_HEIGHT_PX);

    let desk = build_office_desk_widget();
    scene.put(&desk, 56.0, 138.0);
    let monitor = build_office_monitor_widget();
    scene.put(&monitor, 96.0, 102.0);
    let keyboard = build_office_keyboard_widget();
    scene.put(&keyboard, 85.0, 148.0);
    let chair = build_office_chair_widget();
    scene.put(&chair, 68.0, 176.0);
    let mug = build_office_mug_widget();
    scene.put(&mug, 146.0, 122.0);

    if let Some(presence) = presence {
        let bubble = GtkBox::new(Orientation::Vertical, 4);
        bubble.add_css_class("office-bubble");
        bubble.set_size_request(184, 92);
        let channel = Label::new(Some(&presence.channel_name));
        channel.add_css_class("office-channel");
        channel.set_xalign(0.0);
        let body = Label::new(Some(&presence.latest_body));
        body.add_css_class("office-bubble-body");
        body.set_wrap(true);
        body.set_xalign(0.0);
        bubble.append(&channel);
        bubble.append(&body);
        scene.put(&bubble, 8.0, 0.0);

        let tail = build_office_bubble_tail();
        scene.put(&tail, 30.0, 86.0);

        let avatar = build_avatar_widget(
            presence.speaker_avatar_path.as_deref(),
            &presence.speaker_name,
            OFFICE_PIXEL_AVATAR_SIZE_PX,
        );
        avatar.add_css_class("office-avatar");
        scene.put(&avatar, 152.0, 112.0);

        let nameplate = GtkBox::new(Orientation::Horizontal, 8);
        nameplate.add_css_class("office-nameplate");
        let signal = Label::new(Some(if presence.activity_count >= 8 {
            "!!"
        } else if presence.activity_count >= 4 {
            "!"
        } else {
            "·"
        }));
        signal.add_css_class("office-signal");
        let speaker = Label::new(Some(&presence.speaker_name));
        speaker.add_css_class("office-speaker");
        speaker.set_xalign(0.0);
        nameplate.append(&signal);
        nameplate.append(&speaker);
        scene.put(&nameplate, 8.0, 206.0);
    }

    scene
}

fn build_office_presence_card(
    presence: &OfficePresence,
    handles: &UiHandles,
    runtime: &UiRuntime,
) -> Button {
    let button = Button::new();
    button.add_css_class("office-desk");
    button.add_css_class(office_activity_tier_class(presence.activity_count));
    button.set_tooltip_text(Some(&format!(
        "{} in {}",
        presence.speaker_name, presence.channel_name
    )));
    let scene = office_workstation_scene(Some(presence));
    button.set_child(Some(&scene));

    {
        let handles = handles.clone();
        let runtime = runtime.clone();
        let thread_ts = presence.thread_ts.clone();
        button.connect_clicked(move |_| {
            runtime
                .deck_state
                .borrow_mut()
                .open_thread(thread_ts.clone());
            refresh_ui(&handles, &runtime);
        });
    }

    button
}

fn rebuild_office_view(
    canvas: &gtk::Fixed,
    presence: &[OfficePresence],
    handles: &UiHandles,
    runtime: &UiRuntime,
) {
    while let Some(child) = canvas.first_child() {
        canvas.remove(&child);
    }

    canvas.set_size_request(OFFICE_SCENE_WIDTH_PX, OFFICE_SCENE_HEIGHT_PX);
    let background = build_office_background();
    canvas.put(&background, 0.0, 0.0);

    let break_area = build_office_break_area_widget();
    canvas.put(&break_area, 480.0, 442.0);

    let vending = build_office_vending_widget();
    canvas.put(&vending, 632.0, 250.0);
    let board = build_office_whiteboard_widget();
    canvas.put(&board, 540.0, 146.0);
    let board_label = Label::new(Some("standup board"));
    board_label.add_css_class("office-zone-label");
    canvas.put(&board_label, 562.0, 244.0);

    let bookshelf = build_office_bookshelf_widget();
    canvas.put(&bookshelf, 1090.0, 512.0);

    for (x, y) in [
        (24.0, 156.0),
        (1300.0, 156.0),
        (24.0, 968.0),
        (1300.0, 968.0),
        (438.0, 386.0),
        (842.0, 386.0),
        (438.0, 832.0),
        (842.0, 832.0),
    ] {
        let planter = build_office_planter_widget();
        canvas.put(&planter, x, y);
    }

    if presence.is_empty() {
        let empty = GtkBox::new(Orientation::Vertical, 8);
        empty.add_css_class("office-empty-card");
        let title = Label::new(Some("No trainers are on the floor yet"));
        title.add_css_class("title-4");
        title.set_xalign(0.0);
        let body = Label::new(Some(
            "Point the office source at a times_* channel profile and this area will fill with avatar desks and live speech bubbles.",
        ));
        body.add_css_class("meta");
        body.set_wrap(true);
        body.set_xalign(0.0);
        empty.append(&title);
        empty.append(&body);
        canvas.put(&empty, 520.0, 790.0);
    }

    for (index, (x, y)) in OFFICE_SLOT_COORDS.into_iter().enumerate() {
        if let Some(entry) = presence.get(index) {
            let card = build_office_presence_card(entry, handles, runtime);
            canvas.put(&card, x, y);
        } else {
            let empty_desk = office_workstation_scene(None);
            empty_desk.add_css_class("office-empty-workstation");
            canvas.put(&empty_desk, x, y);
        }
    }
}

fn shared_message_preview_text(item: &TimelineItem) -> String {
    let body = render_slack_text(&item.body);
    let body = body.trim();
    if !body.is_empty() {
        let mut preview = body.chars().take(180).collect::<String>();
        if body.chars().count() > 180 {
            preview.push('…');
        }
        return preview;
    }

    if item.attachments.is_empty() {
        "Shared Slack message".to_string()
    } else if item.attachments.len() == 1 {
        "1 attachment".to_string()
    } else {
        format!("{} attachments", item.attachments.len())
    }
}

fn build_shared_message_preview_card(item: &TimelineItem) -> GtkBox {
    let card = GtkBox::new(Orientation::Horizontal, 10);
    card.add_css_class("shared-preview-card");

    let avatar = build_avatar_widget(
        item.author_avatar_path.as_deref(),
        &item.author_name,
        SHARED_PREVIEW_AVATAR_SIZE_PX,
    );
    let content = GtkBox::new(Orientation::Vertical, 4);
    content.set_hexpand(true);

    let title = Label::new(Some(&format!(
        "{} • {}",
        item.author_name, item.channel_name
    )));
    title.add_css_class("meta");
    title.set_xalign(0.0);
    let body = Label::new(Some(&shared_message_preview_text(item)));
    body.add_css_class("shared-preview-body");
    body.set_wrap(true);
    body.set_xalign(0.0);

    content.append(&title);
    content.append(&body);
    card.append(&avatar);
    card.append(&content);
    card
}

fn build_shared_message_previews(text: &str, runtime: &UiRuntime) -> Option<GtkBox> {
    let targets = extract_slack_permalink_targets(text);
    if targets.is_empty() {
        return None;
    }

    let linked_items = {
        let state = runtime.state.borrow();
        targets
            .into_iter()
            .filter_map(|target| {
                state.item_by_channel_and_message_ts(&target.channel_id, &target.message_ts)
            })
            .take(2)
            .collect::<Vec<_>>()
    };
    if linked_items.is_empty() {
        return None;
    }

    let column = GtkBox::new(Orientation::Vertical, 6);
    column.add_css_class("shared-preview-strip");
    for item in linked_items {
        column.append(&build_shared_message_preview_card(&item));
    }
    Some(column)
}

fn build_reaction_bar(
    item: &TimelineItem,
    handles: &UiHandles,
    runtime: &UiRuntime,
) -> Option<GtkBox> {
    let bar = GtkBox::new(Orientation::Horizontal, 6);
    bar.add_css_class("reaction-bar");

    for reaction in &item.reactions {
        let chip = Button::with_label(&format!("{} {}", reaction.emoji, reaction.count));
        chip.add_css_class("reaction-chip");
        if reaction.me {
            chip.add_css_class("active");
            chip.set_tooltip_text(Some("You already reacted"));
        } else {
            chip.set_tooltip_text(Some("Add this reaction"));
            let handles = handles.clone();
            let runtime = runtime.clone();
            let item = item.clone();
            let reaction_name = reaction.name.clone();
            chip.connect_clicked(move |_| {
                let _ = start_add_reaction(item.clone(), &reaction_name, &handles, &runtime);
            });
        }
        bar.append(&chip);
    }

    bar.append(&build_add_reaction_menu_button(item, handles, runtime));

    Some(bar)
}

fn build_add_reaction_menu_button(
    item: &TimelineItem,
    handles: &UiHandles,
    runtime: &UiRuntime,
) -> MenuButton {
    let button = MenuButton::new();
    button.add_css_class("reaction-add-button");
    button.set_always_show_arrow(false);
    button.set_tooltip_text(Some("Add another reaction"));
    let icon = Image::builder()
        .icon_name("list-add-symbolic")
        .pixel_size(16)
        .build();
    button.set_child(Some(&icon));

    let popover = gtk::Popover::new();
    popover.add_css_class("reaction-popover");
    popover.set_has_arrow(false);

    let content = GtkBox::new(Orientation::Vertical, 8);
    let title = Label::new(Some("Add reaction"));
    title.add_css_class("meta");
    title.set_xalign(0.0);
    content.append(&title);

    for names in QUICK_REACTION_NAMES.chunks(4) {
        let row = GtkBox::new(Orientation::Horizontal, 6);
        for name in names {
            let quick_button = Button::with_label(&reaction_emoji(name));
            quick_button.add_css_class("reaction-chip");
            quick_button.add_css_class("quick-reaction-chip");
            quick_button.set_tooltip_text(Some(&format!("Add :{name}:")));
            let handles = handles.clone();
            let runtime = runtime.clone();
            let item = item.clone();
            let popover = popover.clone();
            let reaction_name = (*name).to_string();
            quick_button.connect_clicked(move |_| {
                if start_add_reaction(item.clone(), &reaction_name, &handles, &runtime) {
                    popover.popdown();
                }
            });
            row.append(&quick_button);
        }
        content.append(&row);
    }

    let input_row = GtkBox::new(Orientation::Horizontal, 6);
    let entry = Entry::new();
    entry.add_css_class("reaction-entry");
    entry.set_hexpand(true);
    entry.set_placeholder_text(Some(":thumbsup: or 👍"));
    let add_button = Button::with_label("Add");
    add_button.add_css_class("quick-reaction-chip");
    input_row.append(&entry);
    input_row.append(&add_button);
    content.append(&input_row);

    let submit_reaction: Rc<dyn Fn(String) -> bool> = Rc::new({
        let handles = handles.clone();
        let runtime = runtime.clone();
        let item = item.clone();
        move |raw_reaction| start_add_reaction(item.clone(), &raw_reaction, &handles, &runtime)
    });

    {
        let entry = entry.clone();
        let popover = popover.clone();
        let submit_reaction = Rc::clone(&submit_reaction);
        add_button.connect_clicked(move |_| {
            let raw_reaction = entry.text().to_string();
            if submit_reaction(raw_reaction) {
                entry.set_text("");
                popover.popdown();
            }
        });
    }

    {
        let popover = popover.clone();
        let submit_reaction = Rc::clone(&submit_reaction);
        entry.connect_activate(move |entry| {
            let raw_reaction = entry.text().to_string();
            if submit_reaction(raw_reaction) {
                entry.set_text("");
                popover.popdown();
            }
        });
    }

    popover.set_child(Some(&content));
    button.set_popover(Some(&popover));
    button
}

fn build_local_picture(path: &str, width: i32, max_height: i32) -> Option<gtk::Picture> {
    let texture = gdk::Texture::from_filename(path).ok()?;
    let raw_width = texture.width().max(1);
    let raw_height = texture.height().max(1);
    let min_height = max_height.min(96).max(1) as i64;
    let max_height = max_height.max(1) as i64;
    let height = ((width as i64) * (raw_height as i64) / (raw_width as i64))
        .clamp(min_height, max_height) as i32;
    let picture = gtk::Picture::for_paintable(&texture);
    picture.set_can_shrink(true);
    picture.set_keep_aspect_ratio(true);
    picture.set_size_request(width, height);
    picture.set_halign(gtk::Align::Start);
    picture.set_valign(gtk::Align::Start);
    Some(picture)
}

#[derive(Clone, Copy)]
struct EmptyStateView {
    has_cached_items: bool,
}

#[derive(Clone)]
enum TimelineListEntry {
    Empty {
        title: String,
        body: String,
    },
    Item {
        ranked: RankedTimelineItem,
        is_active: bool,
        animate_insert: bool,
    },
}

fn empty_state_content(state: &EmptyStateView, auth_state: &SlackAuthStatus) -> (String, String) {
    if state.has_cached_items {
        (
            "No messages match the current view".to_string(),
            "Try clearing the search or filter, or wait for new Slack activity.".to_string(),
        )
    } else {
        let (title, body) = auth_state.empty_state();
        (title.to_string(), body)
    }
}

fn build_timeline_factory(handles: &UiHandles, runtime: &UiRuntime) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    let handles = handles.clone();
    let runtime = runtime.clone();
    factory.connect_bind(move |_, list_item| {
        let Some(item) = list_item.item() else {
            return;
        };
        let Ok(item) = item.downcast::<glib::BoxedAnyObject>() else {
            return;
        };
        let entry = item.borrow::<TimelineListEntry>().clone();
        let widget = build_timeline_row_widget(entry, &handles, &runtime);
        list_item.set_child(Some(&widget));
    });
    factory.connect_unbind(|_, list_item| {
        list_item.set_child(Option::<&gtk::Widget>::None);
    });
    factory
}

fn sync_timeline_store(store: &gio::ListStore, runtime: &UiRuntime) {
    store.remove_all();

    let entries = {
        let borrowed = runtime.state.borrow();
        if borrowed.visible_items().is_empty() {
            let empty_state = EmptyStateView {
                has_cached_items: borrowed.has_cached_items(),
            };
            let (title, body) = empty_state_content(&empty_state, &runtime.auth_state.borrow());
            vec![TimelineListEntry::Empty { title, body }]
        } else {
            borrowed
                .loaded_visible_items()
                .iter()
                .cloned()
                .map(|ranked| TimelineListEntry::Item {
                    animate_insert: runtime
                        .animated_threads
                        .borrow()
                        .contains(&ranked.item.message_ts),
                    is_active: borrowed
                        .highlighted_timeline_message_ts
                        .as_ref()
                        .is_some_and(|selected| selected == &ranked.item.message_ts),
                    ranked,
                })
                .collect::<Vec<_>>()
        }
    };
    runtime.animated_threads.borrow_mut().clear();

    for entry in entries {
        store.append(&glib::BoxedAnyObject::new(entry));
    }
}

fn build_timeline_row_widget(
    entry: TimelineListEntry,
    handles: &UiHandles,
    runtime: &UiRuntime,
) -> GtkBox {
    match entry {
        TimelineListEntry::Empty { title, body } => build_empty_timeline_tile(&title, &body),
        TimelineListEntry::Item {
            ranked,
            is_active,
            animate_insert,
        } => build_timeline_tile(&ranked, is_active, animate_insert, handles, runtime),
    }
}

fn build_empty_timeline_tile(title_text: &str, body_text: &str) -> GtkBox {
    let container = GtkBox::new(Orientation::Vertical, 8);
    container.add_css_class("timeline-tile");
    container.add_css_class("timeline-empty");
    let title = Label::new(Some(title_text));
    title.add_css_class("title-3");
    title.set_xalign(0.0);

    let body = Label::new(Some(body_text));
    body.add_css_class("meta");
    body.set_wrap(true);
    body.set_xalign(0.0);

    container.append(&title);
    container.append(&body);
    container
}

fn build_timeline_tile(
    ranked: &RankedTimelineItem,
    is_active: bool,
    animate_insert: bool,
    handles: &UiHandles,
    runtime: &UiRuntime,
) -> GtkBox {
    let row = GtkBox::new(Orientation::Horizontal, 12);
    row.add_css_class("timeline-tile");
    if is_active {
        row.add_css_class("timeline-tile-active");
    }
    if animate_insert {
        row.add_css_class("timeline-tile-fresh");
    }

    let avatar = build_avatar_widget(
        ranked.item.author_avatar_path.as_deref(),
        &ranked.item.author_name,
        AVATAR_SIZE_PX,
    );

    let content = GtkBox::new(Orientation::Vertical, 8);
    content.set_hexpand(true);

    let header_row = GtkBox::new(Orientation::Horizontal, 8);
    header_row.set_valign(gtk::Align::Start);
    let author = Label::new(Some(&ranked.item.author_name));
    author.add_css_class("author");
    author.set_xalign(0.0);
    let channel = Label::new(Some(&ranked.item.channel_name));
    channel.add_css_class("meta");
    channel.set_xalign(0.0);
    let header = GtkBox::new(Orientation::Horizontal, 6);
    header.append(&author);
    header.append(&channel);
    let header_spacer = GtkBox::new(Orientation::Horizontal, 0);
    header_spacer.set_hexpand(true);
    let timestamp = Label::new(Some(&relative_timestamp_text(ranked.item.last_activity_at)));
    timestamp.add_css_class("timestamp");
    timestamp.set_tooltip_text(Some(&absolute_local_timestamp_text(
        ranked.item.last_activity_at,
    )));
    header_row.append(&header);
    header_row.append(&header_spacer);
    header_row.append(&timestamp);

    let actions = GtkBox::new(Orientation::Horizontal, 8);
    let reply_count = runtime
        .state
        .borrow()
        .thread_reply_count(&ranked.item.thread_ts);
    let reply_button = build_reply_button(reply_count, "Open this thread in a new column");
    let copy_button = build_inline_icon_button("edit-copy-symbolic", "Copy only the message text");
    let share_button =
        build_inline_icon_button("insert-link-symbolic", "Copy the Slack message URL");
    let can_manage_message = item_owned_by_connected_user(&ranked.item, runtime);
    copy_button.set_sensitive(!ranked.item.body.trim().is_empty());
    actions.append(&reply_button);
    actions.append(&copy_button);
    actions.append(&share_button);
    if can_manage_message {
        let edit_button = build_edit_message_menu_button(&ranked.item, handles, runtime);
        let delete_button = build_delete_message_menu_button(&ranked.item, handles, runtime);
        actions.append(&edit_button);
        actions.append(&delete_button);
    }

    {
        let handles = handles.clone();
        let runtime = runtime.clone();
        let thread_ts = ranked.item.thread_ts.clone();
        reply_button.connect_clicked(move |_| {
            runtime.state.borrow_mut().select(Some(thread_ts.clone()));
            runtime
                .deck_state
                .borrow_mut()
                .open_thread(thread_ts.clone());
            refresh_ui(&handles, &runtime);
        });
    }

    {
        let handles = handles.clone();
        let message_text = render_slack_text(&ranked.item.body);
        copy_button.connect_clicked(move |_| {
            let _ = copy_text_to_clipboard(
                &handles,
                &message_text,
                "Copied message text to clipboard.",
            );
        });
    }

    {
        let handles = handles.clone();
        let runtime = runtime.clone();
        let item = ranked.item.clone();
        share_button.connect_clicked(move |_| {
            start_share_link_copy(item.clone(), &handles, &runtime);
        });
    }

    content.append(&header_row);
    if let Some(body) = build_slack_body_widget(&ranked.item, runtime) {
        content.append(&body);
    }
    if let Some(previews) = build_shared_message_previews(&ranked.item.body, runtime) {
        content.append(&previews);
    }
    if !ranked.item.attachments.is_empty() {
        content.append(&build_attachment_strip(
            &ranked.item.attachments,
            TIMELINE_ATTACHMENT_WIDTH_PX,
        ));
    }
    if let Some(reaction_bar) = build_reaction_bar(&ranked.item, handles, runtime) {
        content.append(&reaction_bar);
    }
    content.append(&actions);
    row.append(&avatar);
    row.append(&content);

    if animate_insert {
        animate_timeline_insert(&row);
    }
    row
}

fn build_avatar_widget(avatar_path: Option<&str>, display_name: &str, size_px: i32) -> GtkBox {
    let shell = GtkBox::new(Orientation::Vertical, 0);
    shell.add_css_class("avatar-shell");
    shell.set_width_request(size_px);
    shell.set_height_request(size_px);
    shell.set_halign(gtk::Align::Start);
    shell.set_valign(gtk::Align::Start);
    shell.set_hexpand(false);
    shell.set_vexpand(false);

    if let Some(path) = avatar_path.filter(|path| Path::new(path).exists()) {
        shell.add_css_class("avatar-shell-media");
        if let Some(picture) = build_local_picture(path, size_px, size_px) {
            picture.add_css_class("avatar-media");
            shell.append(&picture);
            return shell;
        }
    }

    shell.add_css_class("avatar-fallback");
    let initials = Label::new(Some(&avatar_initials(display_name)));
    initials.add_css_class("avatar-initials");
    shell.append(&initials);
    shell
}

fn avatar_initials(display_name: &str) -> String {
    let mut initials = display_name
        .split(|character: char| character.is_whitespace() || character == '-' || character == '_')
        .filter(|segment| !segment.is_empty())
        .filter_map(|segment| segment.chars().next())
        .take(2)
        .collect::<String>()
        .to_uppercase();
    if initials.is_empty() {
        initials = display_name
            .chars()
            .take(2)
            .collect::<String>()
            .to_uppercase();
    }
    if initials.is_empty() {
        "?".to_string()
    } else {
        initials
    }
}

fn animate_timeline_insert(row: &GtkBox) {
    row.set_opacity(0.4);
    row.set_margin_top(18);
    let started_at = Instant::now();
    row.add_tick_callback(move |widget, _clock| {
        let progress = (started_at.elapsed().as_secs_f64() * 1_000.0
            / TIMELINE_INSERT_ANIMATION_MS)
            .clamp(0.0, 1.0);
        let eased = 1.0 - (1.0 - progress).powi(3);
        widget.set_opacity(0.4 + (0.6 * eased));
        widget.set_margin_top(((1.0 - eased) * 18.0).round() as i32);

        if progress >= 1.0 {
            widget.set_opacity(1.0);
            widget.set_margin_top(0);
            widget.remove_css_class("timeline-tile-fresh");
            glib::ControlFlow::Break
        } else {
            glib::ControlFlow::Continue
        }
    });
}

fn rebuild_stack_columns(deck_columns: &GtkBox, runtime: &UiRuntime, handles: &UiHandles) {
    while let Some(child) = deck_columns.first_child() {
        deck_columns.remove(&child);
    }

    let columns = runtime.deck_state.borrow().columns.clone();
    if !columns.contains(&DeckColumn::ConfigEditor) {
        *runtime.config_editor_widget.borrow_mut() = None;
    }

    for (index, column) in columns.into_iter().enumerate() {
        let widget = match column {
            DeckColumn::Settings => build_settings_column(index, handles, runtime),
            DeckColumn::Admin => build_admin_column(index, handles, runtime),
            DeckColumn::ConfigEditor => {
                if let Some(widget) = runtime.config_editor_widget.borrow().clone() {
                    widget
                } else {
                    let widget = build_config_editor_column(handles, runtime);
                    *runtime.config_editor_widget.borrow_mut() = Some(widget.clone());
                    widget
                }
            }
            DeckColumn::Thread { thread_ts } => {
                build_thread_column(index, &thread_ts, handles, runtime)
            }
        };
        deck_columns.append(&widget);
    }
}

fn build_settings_column(index: usize, handles: &UiHandles, runtime: &UiRuntime) -> GtkBox {
    let column = GtkBox::new(Orientation::Vertical, 12);
    column.add_css_class("deck-column");
    column.add_css_class("settings-column");
    column.set_width_request(360);

    let header = GtkBox::new(Orientation::Horizontal, 8);
    let title_box = GtkBox::new(Orientation::Vertical, 4);
    let title = Label::new(Some("Settings"));
    title.add_css_class("title-3");
    title.set_xalign(0.0);
    let subtitle = Label::new(Some("Theme, Slack auth, and local cache"));
    subtitle.add_css_class("meta");
    subtitle.set_xalign(0.0);
    title_box.append(&title);
    title_box.append(&subtitle);
    let spacer = GtkBox::new(Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    let close_button = build_icon_button("window-close-symbolic", "Close this column");
    close_button.remove_css_class("nav-button");
    close_button.add_css_class("close-button");
    {
        let handles = handles.clone();
        let runtime = runtime.clone();
        close_button.connect_clicked(move |_| {
            runtime.deck_state.borrow_mut().close(index);
            refresh_ui(&handles, &runtime);
        });
    }
    header.append(&title_box);
    header.append(&spacer);
    header.append(&close_button);
    column.append(&header);

    let appearance_title = Label::new(Some("Appearance"));
    appearance_title.add_css_class("meta");
    appearance_title.set_xalign(0.0);
    column.append(&appearance_title);

    let theme_picker = ComboBoxText::new();
    for theme in ThemeId::ALL {
        theme_picker.append(Some(theme.slug()), theme.label());
    }
    theme_picker.set_active_id(Some(runtime.state.borrow().theme_id().slug()));
    {
        let handles = handles.clone();
        let runtime = runtime.clone();
        theme_picker.connect_changed(move |picker| {
            let Some(theme_slug) = picker.active_id() else {
                return;
            };
            let Some(theme_id) = ThemeId::from_slug(theme_slug.as_str()) else {
                return;
            };

            let settings = {
                let mut state = runtime.state.borrow_mut();
                if !state.set_theme(theme_id) {
                    return;
                }
                state.settings.clone()
            };

            runtime.bootstrap.save_settings(&settings);
            apply_theme(&runtime.provider, theme_id);
            refresh_ui(&handles, &runtime);
        });
    }
    column.append(&theme_picker);

    column.append(&gtk::Separator::new(Orientation::Horizontal));

    let rooms_title = Label::new(Some("Rooms"));
    rooms_title.add_css_class("meta");
    rooms_title.set_xalign(0.0);
    column.append(&rooms_title);

    let workspace_picker = ComboBoxText::new();
    let (active_workspace_key, workspace_options) = {
        let state = runtime.state.borrow();
        let active_workspace_key = state.active_workspace_key().to_string();
        let mut options = state
            .workspace_profiles()
            .iter()
            .map(|profile| (profile.key.clone(), profile.label.clone()))
            .collect::<Vec<_>>();
        if !options.iter().any(|(key, _)| key == &active_workspace_key) {
            options.insert(
                0,
                (active_workspace_key.clone(), "Default room".to_string()),
            );
        }
        (active_workspace_key, options)
    };
    for (key, label) in workspace_options {
        workspace_picker.append(Some(&key), &label);
    }
    workspace_picker.set_active_id(Some(&active_workspace_key));
    column.append(&workspace_picker);

    {
        let handles = handles.clone();
        let runtime = runtime.clone();
        workspace_picker.connect_changed(move |picker| {
            let Some(workspace_key) = picker.active_id() else {
                return;
            };
            switch_active_workspace(&handles, &runtime, workspace_key.to_string());
        });
    }

    column.append(&gtk::Separator::new(Orientation::Horizontal));

    let office_title = Label::new(Some("Virtual office"));
    office_title.add_css_class("meta");
    office_title.set_xalign(0.0);
    column.append(&office_title);

    let office_source_picker = ComboBoxText::new();
    office_source_picker.append(Some(""), "No office source");
    let office_profiles = runtime
        .state
        .borrow()
        .settings
        .channel_profiles
        .iter()
        .map(|profile| (profile.id.clone(), profile.label.clone()))
        .collect::<Vec<_>>();
    for (profile_id, label) in office_profiles {
        office_source_picker.append(Some(&profile_id), &label);
    }
    {
        let selected = runtime
            .state
            .borrow()
            .office_channel_profile_id()
            .map(str::to_string);
        if !selected
            .as_deref()
            .is_some_and(|profile_id| office_source_picker.set_active_id(Some(profile_id)))
        {
            office_source_picker.set_active(Some(0));
        }
    }
    column.append(&office_source_picker);

    let office_help = Label::new(Some(
        "Pick a channel profile such as times_* to render a live office. Each channel becomes one desk, and hot desks rise visually as activity grows.",
    ));
    office_help.add_css_class("meta");
    office_help.set_wrap(true);
    office_help.set_xalign(0.0);
    column.append(&office_help);

    {
        let handles = handles.clone();
        let runtime = runtime.clone();
        office_source_picker.connect_changed(move |picker| {
            let selected = picker
                .active_id()
                .map(|value| value.to_string())
                .filter(|value| !value.is_empty());
            let settings = {
                let mut state = runtime.state.borrow_mut();
                if state.settings.office.channel_profile_id == selected {
                    return;
                }
                state.settings.office = OfficeSettings {
                    channel_profile_id: selected,
                };
                state.settings.clone()
            };
            runtime.bootstrap.save_settings(&settings);
            refresh_ui(&handles, &runtime);
        });
    }

    let config_title = Label::new(Some("Config overlay"));
    config_title.add_css_class("meta");
    config_title.set_xalign(0.0);
    column.append(&config_title);

    let config_path = runtime
        .bootstrap
        .paths
        .as_ref()
        .map(|paths| paths.config_file_path().display().to_string())
        .unwrap_or_else(|| "~/.config/slaxide/config.toml".to_string());
    let config_help = Label::new(Some(&format!(
        "Optional TOML overlay: {}. Use it for search profiles, channel permissions, and rule presets. UI changes still persist in SQLite.",
        config_path
    )));
    config_help.add_css_class("meta");
    config_help.set_wrap(true);
    config_help.set_xalign(0.0);
    column.append(&config_help);

    let slack_title = Label::new(Some("Slack"));
    slack_title.add_css_class("meta");
    slack_title.set_xalign(0.0);
    column.append(&slack_title);

    let auth_summary = Label::new(Some(&runtime.auth_state.borrow().summary()));
    auth_summary.add_css_class("meta");
    auth_summary.set_wrap(true);
    auth_summary.set_xalign(0.0);
    column.append(&auth_summary);

    let slack_config_title = Label::new(Some("Slack credentials"));
    slack_config_title.add_css_class("meta");
    slack_config_title.set_xalign(0.0);
    column.append(&slack_config_title);

    let stored_slack = runtime.state.borrow().settings.slack.clone();
    let client_id_entry = Entry::builder()
        .placeholder_text(CLIENT_ID_ENV)
        .text(&resolved_slack_config_value(
            CLIENT_ID_ENV,
            stored_slack.client_id.as_deref(),
        ))
        .build();
    let client_secret_entry = Entry::builder()
        .placeholder_text(CLIENT_SECRET_ENV)
        .text(&resolved_slack_config_value(
            CLIENT_SECRET_ENV,
            stored_slack.client_secret.as_deref(),
        ))
        .build();
    let redirect_uri_entry = Entry::builder()
        .placeholder_text(REDIRECT_URI_ENV)
        .text(&resolved_slack_config_value(
            REDIRECT_URI_ENV,
            stored_slack.redirect_uri.as_deref(),
        ))
        .build();
    let user_scopes_entry = Entry::builder()
        .placeholder_text(USER_SCOPES_ENV)
        .text(&resolved_slack_config_value(
            USER_SCOPES_ENV,
            stored_slack.user_scopes.as_deref(),
        ))
        .build();
    let app_token_entry = Entry::builder()
        .placeholder_text(SLACK_APP_TOKEN_ENV)
        .text(&resolved_slack_config_value(
            SLACK_APP_TOKEN_ENV,
            stored_slack.app_token.as_deref(),
        ))
        .build();
    for (label_text, entry) in [
        ("Client ID", &client_id_entry),
        ("Client Secret", &client_secret_entry),
        ("Redirect URI", &redirect_uri_entry),
        ("User scopes", &user_scopes_entry),
        ("App token", &app_token_entry),
    ] {
        let label = Label::new(Some(label_text));
        label.add_css_class("meta");
        label.set_xalign(0.0);
        column.append(&label);
        entry.add_css_class("slack-config-entry");
        column.append(entry);
    }

    let slack_config_hint = Label::new(Some(
        ".env / exported env values win over these saved settings. Clear the env value if you want the saved setting to take effect immediately.",
    ));
    slack_config_hint.add_css_class("meta");
    slack_config_hint.set_wrap(true);
    slack_config_hint.set_xalign(0.0);
    column.append(&slack_config_hint);

    let save_slack_config_button = Button::with_label("Save Slack config");
    column.append(&save_slack_config_button);
    {
        let handles = handles.clone();
        let runtime = runtime.clone();
        let client_id_entry = client_id_entry.clone();
        let client_secret_entry = client_secret_entry.clone();
        let redirect_uri_entry = redirect_uri_entry.clone();
        let user_scopes_entry = user_scopes_entry.clone();
        let app_token_entry = app_token_entry.clone();
        save_slack_config_button.connect_clicked(move |_| {
            let slack = SlackConfigSettings {
                client_id: normalized_entry_value(client_id_entry.text().to_string()),
                client_secret: normalized_entry_value(client_secret_entry.text().to_string()),
                redirect_uri: normalized_entry_value(redirect_uri_entry.text().to_string()),
                user_scopes: normalized_entry_value(user_scopes_entry.text().to_string()),
                app_token: normalized_entry_value(app_token_entry.text().to_string()),
            };
            let (settings, workspace_key) = {
                let mut state = runtime.state.borrow_mut();
                state.settings.slack = slack;
                (
                    state.settings.clone(),
                    state.active_workspace_key().to_string(),
                )
            };
            runtime.bootstrap.save_settings(&settings);
            sync_settings_env(&settings, runtime.env_locked_keys.as_ref());
            stop_live_sync(&runtime);
            *runtime.auth_state.borrow_mut() =
                load_initial_auth_status(runtime.auth_controller.clone(), &workspace_key);
            maybe_start_live_sync(&runtime);
            handles.startup_status.set_text(
                "Saved Slack config. Environment variables and .env still take precedence.",
            );
            refresh_ui(&handles, &runtime);
        });
    }

    let live_sync_summary = if slack_app_token().is_some() {
        if runtime.live_sync_started_generation.borrow().is_some() {
            "Live notifications are enabled via Socket Mode.".to_string()
        } else {
            "Live notifications are configured and will start after Slack auth is connected."
                .to_string()
        }
    } else {
        format!(
            "Live notifications are disabled. Set {} to receive other people's posts while the app is open.",
            SLACK_APP_TOKEN_ENV
        )
    };
    let live_sync_label = Label::new(Some(&live_sync_summary));
    live_sync_label.add_css_class("meta");
    live_sync_label.set_wrap(true);
    live_sync_label.set_xalign(0.0);
    column.append(&live_sync_label);

    let auth_actions = GtkBox::new(Orientation::Horizontal, 8);
    let connect_button = Button::with_label(runtime.auth_state.borrow().button_label());
    connect_button.add_css_class("suggested-action");
    let connect_new_button = Button::with_label("Connect new room");
    let clear_button = Button::with_label("Remove room");
    let pending = runtime.pending_login.borrow().is_some();
    connect_button.set_sensitive(runtime.auth_state.borrow().can_start_login() && !pending);
    connect_new_button.set_sensitive(!pending);
    clear_button.set_sensitive(!pending);
    auth_actions.append(&connect_button);
    auth_actions.append(&connect_new_button);
    auth_actions.append(&clear_button);
    column.append(&auth_actions);

    let slack_guide = Label::new(Some(&format!(
        "Slack setup: 1) OAuth & Permissions で Redirect URL に {} を追加。 2) current room は Connect Slack、別 workspace は Connect new room。 3) 認可後に 127.0.0.1 の失敗画面へ飛んだら、その address bar の URL 全体を下の callback 欄へ貼り戻す。 4) 通知も使うなら Socket Mode を ON にして {} を設定。 5) Event Subscriptions の user/team 側に message.channels / message.groups / reaction_added / reaction_removed / user_typing を追加。",
        DEFAULT_REDIRECT_URI, SLACK_APP_TOKEN_ENV
    )));
    slack_guide.add_css_class("meta");
    slack_guide.set_wrap(true);
    slack_guide.set_xalign(0.0);
    column.append(&slack_guide);

    {
        let handles = handles.clone();
        let runtime = runtime.clone();
        connect_button.connect_clicked(move |_| {
            if !runtime.auth_state.borrow().can_start_login() {
                return;
            }
            let workspace_key = runtime.state.borrow().active_workspace_key().to_string();

            let next_status = match runtime.auth_controller.begin_login() {
                Ok(login) => {
                    let message = format!(
                        "Browser opened. After Slack redirects to {}, copy the full URL from the address bar and paste it below.",
                        login.redirect_uri()
                    );
                    *runtime.pending_login.borrow_mut() = Some(PendingWorkspaceLogin {
                        workspace_key,
                        login,
                    });
                    SlackAuthStatus::Connecting(message)
                }
                Err(error) => SlackAuthStatus::Error(error.to_string()),
            };
            *runtime.auth_state.borrow_mut() = next_status;
            refresh_ui(&handles, &runtime);
        });
    }

    {
        let handles = handles.clone();
        let runtime = runtime.clone();
        connect_new_button.connect_clicked(move |_| {
            let workspace_key = Uuid::new_v4().simple().to_string();
            let next_status = match runtime.auth_controller.begin_login() {
                Ok(login) => {
                    let message = format!(
                        "Browser opened. After Slack redirects to {}, copy the full URL from the address bar and paste it below to add a new room.",
                        login.redirect_uri()
                    );
                    *runtime.pending_login.borrow_mut() = Some(PendingWorkspaceLogin {
                        workspace_key,
                        login,
                    });
                    SlackAuthStatus::Connecting(message)
                }
                Err(error) => SlackAuthStatus::Error(error.to_string()),
            };
            *runtime.auth_state.borrow_mut() = next_status;
            refresh_ui(&handles, &runtime);
        });
    }

    {
        let handles = handles.clone();
        let runtime = runtime.clone();
        clear_button.connect_clicked(move |_| {
            stop_live_sync(&runtime);
            *runtime.pending_login.borrow_mut() = None;
            let active_workspace_key = runtime.state.borrow().active_workspace_key().to_string();
            if let Err(error) = runtime
                .auth_controller
                .clear_session_for(&active_workspace_key)
            {
                *runtime.auth_state.borrow_mut() = SlackAuthStatus::Error(error.to_string());
                refresh_ui(&handles, &runtime);
                return;
            }
            runtime
                .bootstrap
                .replace_timeline_items(&active_workspace_key, &[]);
            let next_workspace_key = {
                let mut state = runtime.state.borrow_mut();
                state.remove_workspace_profile(&active_workspace_key);
                let next = state.active_workspace_key().to_string();
                let settings = state.settings.clone();
                drop(state);
                runtime.bootstrap.save_settings(&settings);
                next
            };
            match runtime.bootstrap.load_timeline_items(&next_workspace_key) {
                Ok(items) => runtime.state.borrow_mut().replace_items(
                    items,
                    BTreeMap::new(),
                    BTreeMap::new(),
                    BTreeMap::new(),
                    BTreeMap::new(),
                ),
                Err(error) => handles
                    .startup_status
                    .set_text(&format!("Failed to load next room cache: {error}")),
            }
            *runtime.auth_state.borrow_mut() =
                load_initial_auth_status(runtime.auth_controller.clone(), &next_workspace_key);
            maybe_start_initial_history_load(
                &runtime.bootstrap,
                &runtime.state,
                &runtime.auth_state,
                &handles.startup_status,
                &runtime.auth_tx,
            );
            maybe_start_live_sync(&runtime);
            refresh_ui(&handles, &runtime);
        });
    }

    if runtime.pending_login.borrow().is_some() {
        let callback_entry = Entry::builder()
            .placeholder_text("Paste the redirected Slack URL here")
            .build();
        let complete_auth_button = Button::with_label("Complete auth");
        let callback_entry_for_action = callback_entry.clone();
        {
            let handles = handles.clone();
            let runtime = runtime.clone();
            complete_auth_button.connect_clicked(move |_| {
                let Some(pending) = runtime.pending_login.borrow().clone() else {
                    return;
                };
                let callback_input = callback_entry_for_action.text().trim().to_string();
                if callback_input.is_empty() {
                    *runtime.auth_state.borrow_mut() =
                        SlackAuthStatus::Error("Paste the full redirected Slack URL first.".into());
                    refresh_ui(&handles, &runtime);
                    return;
                }

                *runtime.auth_state.borrow_mut() =
                    SlackAuthStatus::Connecting("Exchanging Slack OAuth code.".into());
                refresh_ui(&handles, &runtime);

                let auth_controller = runtime.auth_controller.clone();
                let auth_tx = runtime.auth_tx.clone();
                let pending = pending.clone();
                std::thread::spawn(move || {
                    let result = auth_controller
                        .finish_login(&pending.workspace_key, &pending.login, &callback_input)
                        .map_err(|error| error.to_string());
                    let _ = auth_tx.send(AuthEvent::Completed {
                        workspace_key: pending.workspace_key,
                        result,
                    });
                });
            });
        }
        column.append(&callback_entry);
        column.append(&complete_auth_button);
    }

    column.append(&gtk::Separator::new(Orientation::Horizontal));

    let shortcuts_title = Label::new(Some("Shortcuts"));
    shortcuts_title.add_css_class("meta");
    shortcuts_title.set_xalign(0.0);
    column.append(&shortcuts_title);

    let shortcuts_summary = Label::new(Some(
        &shortcut_summary_lines(&runtime.state.borrow().settings.shortcuts).join("\n"),
    ));
    shortcuts_summary.add_css_class("meta");
    shortcuts_summary.set_wrap(true);
    shortcuts_summary.set_xalign(0.0);
    column.append(&shortcuts_summary);

    let shortcuts_hint = Label::new(Some(
        "Override shortcuts in XDG_CONFIG_HOME/slaxide/config.toml under [shortcuts].",
    ));
    shortcuts_hint.add_css_class("meta");
    shortcuts_hint.set_wrap(true);
    shortcuts_hint.set_xalign(0.0);
    column.append(&shortcuts_hint);

    column.append(&gtk::Separator::new(Orientation::Horizontal));

    let cache_title = Label::new(Some("Local cache"));
    cache_title.add_css_class("meta");
    cache_title.set_xalign(0.0);
    column.append(&cache_title);

    let status_text = handles.startup_status.text().to_string();
    let local_status = Label::new(Some(&status_text));
    local_status.add_css_class("meta");
    local_status.set_wrap(true);
    local_status.set_xalign(0.0);
    column.append(&local_status);

    column
}

fn build_admin_column(index: usize, handles: &UiHandles, runtime: &UiRuntime) -> GtkBox {
    let column = GtkBox::new(Orientation::Vertical, 12);
    column.add_css_class("deck-column");
    column.add_css_class("settings-column");
    column.set_width_request(380);

    let header = GtkBox::new(Orientation::Horizontal, 8);
    let title_box = GtkBox::new(Orientation::Vertical, 4);
    let title = Label::new(Some("Admin"));
    title.add_css_class("title-3");
    title.set_xalign(0.0);
    let subtitle = Label::new(Some("Channel management and member operations"));
    subtitle.add_css_class("meta");
    subtitle.set_xalign(0.0);
    title_box.append(&title);
    title_box.append(&subtitle);
    let spacer = GtkBox::new(Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    let close_button = build_icon_button("window-close-symbolic", "Close this column");
    close_button.remove_css_class("nav-button");
    close_button.add_css_class("close-button");
    {
        let handles = handles.clone();
        let runtime = runtime.clone();
        close_button.connect_clicked(move |_| {
            runtime.deck_state.borrow_mut().close(index);
            refresh_ui(&handles, &runtime);
        });
    }
    header.append(&title_box);
    header.append(&spacer);
    header.append(&close_button);
    column.append(&header);

    let config_button = Button::with_label("Open config editor");
    config_button.add_css_class("suggested-action");
    column.append(&config_button);

    let config_hint = Label::new(Some(
        "Edit config.toml-style search profiles, notification rules, regex matchers, and channel access from a dedicated column.",
    ));
    config_hint.add_css_class("meta");
    config_hint.set_wrap(true);
    config_hint.set_xalign(0.0);
    column.append(&config_hint);

    {
        let handles = handles.clone();
        let runtime = runtime.clone();
        config_button.connect_clicked(move |_| {
            runtime.deck_state.borrow_mut().open_config_editor();
            refresh_ui(&handles, &runtime);
        });
    }

    column.append(&gtk::Separator::new(Orientation::Horizontal));

    let connected = matches!(&*runtime.auth_state.borrow(), SlackAuthStatus::Connected(_));
    let available_channels = runtime.state.borrow().available_channels();
    let available_members = runtime.state.borrow().available_members();
    let selected_channel = available_channels.first().map(|(id, _)| id.as_str());
    let selected_member = available_members.first().map(|(id, _)| id.as_str());

    let channel_title = Label::new(Some("Channels"));
    channel_title.add_css_class("meta");
    channel_title.set_xalign(0.0);
    column.append(&channel_title);

    let create_name = Entry::builder()
        .placeholder_text("new-channel-name")
        .build();
    let create_private = gtk::CheckButton::with_label("Private channel");
    let create_button = Button::with_label("Create channel");
    create_button.add_css_class("suggested-action");
    create_button.set_sensitive(connected);
    column.append(&create_name);
    column.append(&create_private);
    column.append(&create_button);

    let manage_channel = ComboBoxText::new();
    sync_filter_picker(
        &manage_channel,
        "Select channel",
        &available_channels,
        selected_channel,
    );
    let rename_entry = Entry::builder()
        .placeholder_text("rename-channel-to")
        .build();
    let rename_button = Button::with_label("Rename channel");
    let archive_button = Button::with_label("Archive channel");
    rename_button.set_sensitive(connected && selected_channel.is_some());
    archive_button.set_sensitive(connected && selected_channel.is_some());
    column.append(&manage_channel);
    column.append(&rename_entry);
    column.append(&rename_button);
    column.append(&archive_button);

    column.append(&gtk::Separator::new(Orientation::Horizontal));

    let member_title = Label::new(Some("Members"));
    member_title.add_css_class("meta");
    member_title.set_xalign(0.0);
    column.append(&member_title);

    let member_channel = ComboBoxText::new();
    sync_filter_picker(
        &member_channel,
        "Select channel",
        &available_channels,
        selected_channel,
    );
    let member_user = ComboBoxText::new();
    sync_filter_picker(
        &member_user,
        "Select member",
        &available_members,
        selected_member,
    );
    let invite_button = Button::with_label("Invite to channel");
    let kick_button = Button::with_label("Remove from channel");
    invite_button
        .set_sensitive(connected && selected_channel.is_some() && selected_member.is_some());
    kick_button.set_sensitive(connected && selected_channel.is_some() && selected_member.is_some());
    column.append(&member_channel);
    column.append(&member_user);
    column.append(&invite_button);
    column.append(&kick_button);

    let hint = Label::new(Some(
        "These actions use Slack Web API scopes. If Slack returns missing_scope, reconnect the room once so the expanded scope set is granted.",
    ));
    hint.add_css_class("meta");
    hint.set_wrap(true);
    hint.set_xalign(0.0);
    column.append(&hint);

    {
        let handles = handles.clone();
        let runtime = runtime.clone();
        let create_name = create_name.clone();
        let create_private = create_private.clone();
        create_button.connect_clicked(move |_| {
            start_create_channel(
                create_name.text().to_string(),
                create_private.is_active(),
                &handles,
                &runtime,
            );
        });
    }

    {
        let handles = handles.clone();
        let runtime = runtime.clone();
        let manage_channel = manage_channel.clone();
        let rename_entry = rename_entry.clone();
        rename_button.connect_clicked(move |_| {
            let channel_id = manage_channel.active_id().map(|value| value.to_string());
            start_rename_channel(
                channel_id,
                rename_entry.text().to_string(),
                &handles,
                &runtime,
            );
        });
    }

    {
        let handles = handles.clone();
        let runtime = runtime.clone();
        let manage_channel = manage_channel.clone();
        archive_button.connect_clicked(move |_| {
            let channel_id = manage_channel.active_id().map(|value| value.to_string());
            start_archive_channel(channel_id, &handles, &runtime);
        });
    }

    {
        let handles = handles.clone();
        let runtime = runtime.clone();
        let member_channel = member_channel.clone();
        let member_user = member_user.clone();
        invite_button.connect_clicked(move |_| {
            start_invite_member(
                member_channel.active_id().map(|value| value.to_string()),
                member_user.active_id().map(|value| value.to_string()),
                &handles,
                &runtime,
            );
        });
    }

    {
        let handles = handles.clone();
        let runtime = runtime.clone();
        let member_channel = member_channel.clone();
        let member_user = member_user.clone();
        kick_button.connect_clicked(move |_| {
            start_kick_member(
                member_channel.active_id().map(|value| value.to_string()),
                member_user.active_id().map(|value| value.to_string()),
                &handles,
                &runtime,
            );
        });
    }

    column
}

fn build_config_editor_column(handles: &UiHandles, runtime: &UiRuntime) -> GtkBox {
    let column = GtkBox::new(Orientation::Vertical, 12);
    column.add_css_class("deck-column");
    column.add_css_class("settings-column");
    column.set_width_request(520);

    let header = GtkBox::new(Orientation::Horizontal, 8);
    let title_box = GtkBox::new(Orientation::Vertical, 4);
    let title = Label::new(Some("Config Editor"));
    title.add_css_class("title-3");
    title.set_xalign(0.0);
    let subtitle = Label::new(Some("Profiles, notification rules, and channel access"));
    subtitle.add_css_class("meta");
    subtitle.set_xalign(0.0);
    title_box.append(&title);
    title_box.append(&subtitle);
    let spacer = GtkBox::new(Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    let close_button = build_icon_button("window-close-symbolic", "Close this column");
    close_button.remove_css_class("nav-button");
    close_button.add_css_class("close-button");
    {
        let handles = handles.clone();
        let runtime = runtime.clone();
        close_button.connect_clicked(move |_| {
            runtime.deck_state.borrow_mut().close_config_editor();
            refresh_ui(&handles, &runtime);
        });
    }
    header.append(&title_box);
    header.append(&spacer);
    header.append(&close_button);
    column.append(&header);

    let overlay_hint = Label::new(Some(
        "These edits persist in SQLite. If XDG_CONFIG_HOME/slaxide/config.toml defines the same values, the TOML overlay will still win again on next launch.",
    ));
    overlay_hint.add_css_class("meta");
    overlay_hint.set_wrap(true);
    overlay_hint.set_xalign(0.0);
    column.append(&overlay_hint);

    let scroll = ScrolledWindow::new();
    scroll.set_vexpand(true);
    scroll.set_hexpand(true);

    let body = GtkBox::new(Orientation::Vertical, 16);
    body.set_margin_top(4);
    body.set_margin_bottom(8);
    body.set_margin_start(2);
    body.set_margin_end(2);
    scroll.set_child(Some(&body));
    column.append(&scroll);

    let settings = runtime.state.borrow().settings.clone();
    let available_channels = runtime.state.borrow().available_channels();
    let available_members = runtime.state.borrow().available_members();

    let channel_access_box = GtkBox::new(Orientation::Vertical, 8);
    let access_hint = Label::new(Some(
        "Use this section for config.toml-style channel permission overrides. Hidden channels disappear entirely, read-only channels stay visible but disable posting.",
    ));
    access_hint.add_css_class("meta");
    access_hint.set_wrap(true);
    access_hint.set_xalign(0.0);
    channel_access_box.append(&access_hint);

    let default_permission_label = Label::new(Some("Default permission"));
    default_permission_label.add_css_class("meta");
    default_permission_label.set_xalign(0.0);
    channel_access_box.append(&default_permission_label);
    let default_permission_picker = build_channel_permission_picker();
    set_channel_permission_picker(
        &default_permission_picker,
        settings.default_channel_permission,
    );
    channel_access_box.append(&default_permission_picker);
    let save_default_permission = Button::with_label("Save default permission");
    channel_access_box.append(&save_default_permission);
    {
        let handles = handles.clone();
        let runtime = runtime.clone();
        let default_permission_picker = default_permission_picker.clone();
        save_default_permission.connect_clicked(move |_| {
            let default_permission = channel_permission_from_picker(&default_permission_picker);
            let settings = {
                let mut state = runtime.state.borrow_mut();
                state.settings.default_channel_permission = default_permission;
                state.apply_filters(true);
                state.settings.clone()
            };
            runtime.bootstrap.save_settings(&settings);
            handles
                .startup_status
                .set_text("Saved default channel permission.");
            refresh_ui_resetting_config_editor(&handles, &runtime);
        });
    }

    channel_access_box.append(&gtk::Separator::new(Orientation::Horizontal));

    let override_label = Label::new(Some("Channel override"));
    override_label.add_css_class("meta");
    override_label.set_xalign(0.0);
    channel_access_box.append(&override_label);
    let override_channel_picker = ComboBoxText::new();
    let selected_override_channel = settings
        .channel_permissions
        .keys()
        .next()
        .cloned()
        .or_else(|| available_channels.first().map(|(id, _)| id.clone()));
    sync_filter_picker(
        &override_channel_picker,
        "Select channel",
        &available_channels,
        selected_override_channel.as_deref(),
    );
    channel_access_box.append(&override_channel_picker);
    let override_permission_picker = build_channel_permission_picker();
    if let Some(channel_id) = selected_override_channel.as_deref() {
        let permission = settings
            .channel_permissions
            .get(channel_id)
            .copied()
            .unwrap_or(settings.default_channel_permission);
        set_channel_permission_picker(&override_permission_picker, permission);
    }
    channel_access_box.append(&override_permission_picker);
    {
        let runtime = runtime.clone();
        let override_permission_picker = override_permission_picker.clone();
        override_channel_picker.connect_changed(move |picker| {
            let Some(channel_id) = picker.active_id().filter(|value| !value.is_empty()) else {
                return;
            };
            let permission = runtime
                .state
                .borrow()
                .settings
                .channel_permissions
                .get(channel_id.as_str())
                .copied()
                .unwrap_or(runtime.state.borrow().settings.default_channel_permission);
            set_channel_permission_picker(&override_permission_picker, permission);
        });
    }
    let override_actions = GtkBox::new(Orientation::Horizontal, 8);
    let save_override_button = Button::with_label("Save override");
    let clear_override_button = Button::with_label("Clear override");
    override_actions.append(&save_override_button);
    override_actions.append(&clear_override_button);
    channel_access_box.append(&override_actions);
    {
        let handles = handles.clone();
        let runtime = runtime.clone();
        let override_channel_picker = override_channel_picker.clone();
        let override_permission_picker = override_permission_picker.clone();
        save_override_button.connect_clicked(move |_| {
            let Some(channel_id) = override_channel_picker
                .active_id()
                .map(|value| value.to_string())
                .filter(|value| !value.is_empty())
            else {
                handles
                    .startup_status
                    .set_text("Pick a channel for the override.");
                return;
            };
            let permission = channel_permission_from_picker(&override_permission_picker);
            let settings = {
                let mut state = runtime.state.borrow_mut();
                state
                    .settings
                    .channel_permissions
                    .insert(channel_id, permission);
                state.apply_filters(true);
                state.settings.clone()
            };
            runtime.bootstrap.save_settings(&settings);
            handles
                .startup_status
                .set_text("Saved channel permission override.");
            refresh_ui_resetting_config_editor(&handles, &runtime);
        });
    }
    {
        let handles = handles.clone();
        let runtime = runtime.clone();
        let override_channel_picker = override_channel_picker.clone();
        clear_override_button.connect_clicked(move |_| {
            let Some(channel_id) = override_channel_picker
                .active_id()
                .map(|value| value.to_string())
                .filter(|value| !value.is_empty())
            else {
                handles
                    .startup_status
                    .set_text("Pick a channel override to clear.");
                return;
            };
            let settings = {
                let mut state = runtime.state.borrow_mut();
                state.settings.channel_permissions.remove(&channel_id);
                state.apply_filters(true);
                state.settings.clone()
            };
            runtime.bootstrap.save_settings(&settings);
            handles
                .startup_status
                .set_text("Cleared channel permission override.");
            refresh_ui_resetting_config_editor(&handles, &runtime);
        });
    }
    let override_summary = if settings.channel_permissions.is_empty() {
        "No channel overrides yet.".to_string()
    } else {
        settings
            .channel_permissions
            .iter()
            .map(|(channel_id, permission)| {
                let label = runtime
                    .state
                    .borrow()
                    .channel_name_for(channel_id)
                    .unwrap_or_else(|| channel_id.clone());
                let permission = match permission {
                    ChannelPermission::ReadWrite => "read_write",
                    ChannelPermission::ReadOnly => "read_only",
                    ChannelPermission::Hidden => "hidden",
                };
                format!("{label} ({channel_id}) = {permission}")
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let override_summary_label = Label::new(Some(&override_summary));
    override_summary_label.add_css_class("meta");
    override_summary_label.set_wrap(true);
    override_summary_label.set_xalign(0.0);
    channel_access_box.append(&override_summary_label);
    let channel_access_expander = gtk::Expander::builder()
        .label("Channel access")
        .expanded(false)
        .build();
    channel_access_expander.set_child(Some(&channel_access_box));
    body.append(&channel_access_expander);

    let keyword_profiles = {
        let mut profiles = settings.keyword_profiles.clone();
        profiles.sort_by(|left, right| left.label.cmp(&right.label).then(left.id.cmp(&right.id)));
        profiles
    };
    let keyword_box = GtkBox::new(Orientation::Vertical, 8);
    let keyword_help = Label::new(Some(
        "Add one matcher per row. Choose Text or Regex, then enter the expression.",
    ));
    keyword_help.add_css_class("meta");
    keyword_help.set_wrap(true);
    keyword_help.set_xalign(0.0);
    keyword_box.append(&keyword_help);
    let keyword_picker = ComboBoxText::new();
    let keyword_options = keyword_profiles
        .iter()
        .map(|profile| {
            (
                profile.id.clone(),
                format!("{} ({})", profile.label, profile.id),
            )
        })
        .collect::<Vec<_>>();
    sync_filter_picker(
        &keyword_picker,
        "New keyword profile",
        &keyword_options,
        None,
    );
    keyword_box.append(&keyword_picker);
    let keyword_id_label = Label::new(Some("Profile ID"));
    keyword_id_label.add_css_class("meta");
    keyword_id_label.set_xalign(0.0);
    keyword_box.append(&keyword_id_label);
    let keyword_id_entry = Entry::builder().placeholder_text("shipping_terms").build();
    keyword_box.append(&keyword_id_entry);
    let keyword_label_label = Label::new(Some("Label"));
    keyword_label_label.add_css_class("meta");
    keyword_label_label.set_xalign(0.0);
    keyword_box.append(&keyword_label_label);
    let keyword_label_entry = Entry::builder().placeholder_text("Shipping terms").build();
    keyword_box.append(&keyword_label_entry);
    let keyword_mode_label = Label::new(Some("Mode"));
    keyword_mode_label.add_css_class("meta");
    keyword_mode_label.set_xalign(0.0);
    keyword_box.append(&keyword_mode_label);
    let keyword_mode_picker = build_profile_mode_picker();
    keyword_box.append(&keyword_mode_picker);
    let keyword_matchers_label = Label::new(Some("Matchers"));
    keyword_matchers_label.add_css_class("meta");
    keyword_matchers_label.set_xalign(0.0);
    keyword_box.append(&keyword_matchers_label);
    let keyword_matchers = PatternMatcherEditor::new("release");
    keyword_box.append(&keyword_matchers.widget());
    let keyword_actions = GtkBox::new(Orientation::Horizontal, 8);
    let keyword_new = Button::with_label("New");
    let keyword_save = Button::with_label("Save");
    let keyword_delete = Button::with_label("Delete");
    keyword_actions.append(&keyword_new);
    keyword_actions.append(&keyword_save);
    keyword_actions.append(&keyword_delete);
    keyword_box.append(&keyword_actions);
    let keyword_summary = Label::new(Some("Pick a keyword profile to preview cached hits."));
    keyword_summary.add_css_class("meta");
    keyword_summary.set_wrap(true);
    keyword_summary.set_xalign(0.0);
    keyword_box.append(&keyword_summary);
    apply_keyword_profile_form(
        None,
        &keyword_id_entry,
        &keyword_label_entry,
        &keyword_mode_picker,
        &keyword_matchers,
    );
    {
        let keyword_profiles = keyword_profiles.clone();
        let keyword_id_entry = keyword_id_entry.clone();
        let keyword_label_entry = keyword_label_entry.clone();
        let keyword_mode_picker = keyword_mode_picker.clone();
        let keyword_matchers = keyword_matchers.clone();
        let runtime = runtime.clone();
        let keyword_summary = keyword_summary.clone();
        keyword_picker.connect_changed(move |picker| {
            let profile = picker.active_id().and_then(|selected| {
                if selected.is_empty() {
                    None
                } else {
                    keyword_profiles
                        .iter()
                        .find(|profile| profile.id == selected.as_str())
                }
            });
            apply_keyword_profile_form(
                profile,
                &keyword_id_entry,
                &keyword_label_entry,
                &keyword_mode_picker,
                &keyword_matchers,
            );
            keyword_summary.set_text(&keyword_profile_preview_summary(
                &runtime.state.borrow(),
                profile,
            ));
        });
    }
    {
        let keyword_picker = keyword_picker.clone();
        let keyword_id_entry = keyword_id_entry.clone();
        let keyword_label_entry = keyword_label_entry.clone();
        let keyword_mode_picker = keyword_mode_picker.clone();
        let keyword_matchers = keyword_matchers.clone();
        let keyword_summary = keyword_summary.clone();
        keyword_new.connect_clicked(move |_| {
            keyword_picker.set_active(Some(0));
            apply_keyword_profile_form(
                None,
                &keyword_id_entry,
                &keyword_label_entry,
                &keyword_mode_picker,
                &keyword_matchers,
            );
            keyword_summary.set_text("Pick a keyword profile to preview cached hits.");
        });
    }
    {
        let handles = handles.clone();
        let runtime = runtime.clone();
        let keyword_id_entry = keyword_id_entry.clone();
        let keyword_label_entry = keyword_label_entry.clone();
        let keyword_mode_picker = keyword_mode_picker.clone();
        let keyword_matchers = keyword_matchers.clone();
        keyword_save.connect_clicked(move |_| {
            let Some(id) = normalized_entry_value(keyword_id_entry.text().to_string()) else {
                handles
                    .startup_status
                    .set_text("Keyword profile ID is required.");
                return;
            };
            let label = normalized_entry_value(keyword_label_entry.text().to_string())
                .unwrap_or_else(|| id.clone());
            let matchers = match keyword_matchers.matchers() {
                Ok(matchers) => matchers,
                Err(error) => {
                    handles.startup_status.set_text(&error);
                    return;
                }
            };
            let profile = KeywordProfile {
                id: id.clone(),
                label,
                mode: profile_mode_from_picker(&keyword_mode_picker),
                matchers,
            };
            let settings = {
                let mut state = runtime.state.borrow_mut();
                if let Some(existing) = state
                    .settings
                    .keyword_profiles
                    .iter_mut()
                    .find(|existing| existing.id == id)
                {
                    *existing = profile;
                } else {
                    state.settings.keyword_profiles.push(profile);
                }
                state.apply_filters(true);
                state.settings.clone()
            };
            runtime.bootstrap.save_settings(&settings);
            handles.startup_status.set_text("Saved keyword profile.");
            refresh_ui_resetting_config_editor(&handles, &runtime);
        });
    }
    {
        let handles = handles.clone();
        let runtime = runtime.clone();
        let keyword_picker = keyword_picker.clone();
        keyword_delete.connect_clicked(move |_| {
            let Some(profile_id) = keyword_picker
                .active_id()
                .map(|value| value.to_string())
                .filter(|value| !value.is_empty())
            else {
                handles
                    .startup_status
                    .set_text("Pick a keyword profile to delete.");
                return;
            };
            let settings = {
                let mut state = runtime.state.borrow_mut();
                state
                    .settings
                    .keyword_profiles
                    .retain(|profile| profile.id != profile_id);
                for search_profile in &mut state.settings.search_profiles {
                    search_profile
                        .keyword_profiles
                        .retain(|existing| existing != &profile_id);
                }
                for rule in &mut state.settings.notification_rules {
                    rule.keyword_profile_ids
                        .retain(|existing| existing != &profile_id);
                }
                state.apply_filters(true);
                state.settings.clone()
            };
            runtime.bootstrap.save_settings(&settings);
            handles.startup_status.set_text("Deleted keyword profile.");
            refresh_ui_resetting_config_editor(&handles, &runtime);
        });
    }
    let keyword_expander = gtk::Expander::builder()
        .label("Keyword profiles")
        .expanded(true)
        .build();
    keyword_expander.set_child(Some(&keyword_box));
    body.append(&keyword_expander);

    let section_profiles = {
        let mut profiles = settings.section_profiles.clone();
        profiles.sort_by(|left, right| left.label.cmp(&right.label).then(left.id.cmp(&right.id)));
        profiles
    };
    let section_box = GtkBox::new(Orientation::Vertical, 8);
    let section_help = Label::new(Some(
        "Sections are reusable channel groups. They show up in the timeline filter and can also be referenced from search profiles.",
    ));
    section_help.add_css_class("meta");
    section_help.set_wrap(true);
    section_help.set_xalign(0.0);
    section_box.append(&section_help);
    let section_picker = ComboBoxText::new();
    let section_options = section_profiles
        .iter()
        .map(|profile| {
            (
                profile.id.clone(),
                format!("{} ({})", profile.label, profile.id),
            )
        })
        .collect::<Vec<_>>();
    sync_filter_picker(&section_picker, "New section", &section_options, None);
    section_box.append(&section_picker);
    let section_id_label = Label::new(Some("Section ID"));
    section_id_label.add_css_class("meta");
    section_id_label.set_xalign(0.0);
    section_box.append(&section_id_label);
    let section_id_entry = Entry::builder().placeholder_text("release_rooms").build();
    section_box.append(&section_id_entry);
    let section_label_label = Label::new(Some("Label"));
    section_label_label.add_css_class("meta");
    section_label_label.set_xalign(0.0);
    section_box.append(&section_label_label);
    let section_label_entry = Entry::builder().placeholder_text("Release rooms").build();
    section_box.append(&section_label_entry);
    let section_channels_label = Label::new(Some("Channels"));
    section_channels_label.add_css_class("meta");
    section_channels_label.set_xalign(0.0);
    section_box.append(&section_channels_label);
    let section_channels_picker = MultiSelectPicker::new(
        "Select channels",
        "No cached channels yet.",
        &available_channels,
    );
    section_box.append(&section_channels_picker.widget());
    let section_matchers_label = Label::new(Some("Channel name matchers"));
    section_matchers_label.add_css_class("meta");
    section_matchers_label.set_xalign(0.0);
    section_box.append(&section_matchers_label);
    let section_matchers_help = Label::new(Some(
        "Optional. Add one matcher per row. Choose Text or Regex, then enter the expression.",
    ));
    section_matchers_help.add_css_class("meta");
    section_matchers_help.set_wrap(true);
    section_matchers_help.set_xalign(0.0);
    section_box.append(&section_matchers_help);
    let section_matchers = PatternMatcherEditor::new("times_");
    section_box.append(&section_matchers.widget());
    let section_actions = GtkBox::new(Orientation::Horizontal, 8);
    let section_new = Button::with_label("New");
    let section_save = Button::with_label("Save");
    let section_delete = Button::with_label("Delete");
    section_actions.append(&section_new);
    section_actions.append(&section_save);
    section_actions.append(&section_delete);
    section_box.append(&section_actions);
    let section_summary = Label::new(Some("Pick a section to preview cached hits."));
    section_summary.add_css_class("meta");
    section_summary.set_wrap(true);
    section_summary.set_xalign(0.0);
    section_box.append(&section_summary);
    apply_section_profile_form(
        None,
        &section_id_entry,
        &section_label_entry,
        &section_channels_picker,
        &section_matchers,
    );
    {
        let section_profiles = section_profiles.clone();
        let section_id_entry = section_id_entry.clone();
        let section_label_entry = section_label_entry.clone();
        let section_channels_picker = section_channels_picker.clone();
        let section_matchers = section_matchers.clone();
        let runtime = runtime.clone();
        let section_summary = section_summary.clone();
        section_picker.connect_changed(move |picker| {
            let profile = picker.active_id().and_then(|selected| {
                if selected.is_empty() {
                    None
                } else {
                    section_profiles
                        .iter()
                        .find(|profile| profile.id == selected.as_str())
                }
            });
            apply_section_profile_form(
                profile,
                &section_id_entry,
                &section_label_entry,
                &section_channels_picker,
                &section_matchers,
            );
            section_summary.set_text(&section_profile_preview_summary(
                &runtime.state.borrow(),
                profile,
            ));
        });
    }
    {
        let section_picker = section_picker.clone();
        let section_id_entry = section_id_entry.clone();
        let section_label_entry = section_label_entry.clone();
        let section_channels_picker = section_channels_picker.clone();
        let section_matchers = section_matchers.clone();
        let section_summary = section_summary.clone();
        section_new.connect_clicked(move |_| {
            section_picker.set_active(Some(0));
            apply_section_profile_form(
                None,
                &section_id_entry,
                &section_label_entry,
                &section_channels_picker,
                &section_matchers,
            );
            section_summary.set_text("Pick a section to preview cached hits.");
        });
    }
    {
        let handles = handles.clone();
        let runtime = runtime.clone();
        let section_id_entry = section_id_entry.clone();
        let section_label_entry = section_label_entry.clone();
        let section_channels_picker = section_channels_picker.clone();
        let section_matchers = section_matchers.clone();
        section_save.connect_clicked(move |_| {
            let Some(id) = normalized_entry_value(section_id_entry.text().to_string()) else {
                handles.startup_status.set_text("Section ID is required.");
                return;
            };
            let label = normalized_entry_value(section_label_entry.text().to_string())
                .unwrap_or_else(|| id.clone());
            let channel_name_matchers = match section_matchers.matchers() {
                Ok(matchers) => matchers,
                Err(error) => {
                    handles.startup_status.set_text(&error);
                    return;
                }
            };
            let profile = SectionProfile {
                id: id.clone(),
                label,
                channels: section_channels_picker.selected_set(),
                channel_name_matchers,
            };
            let settings = {
                let mut state = runtime.state.borrow_mut();
                if let Some(existing) = state
                    .settings
                    .section_profiles
                    .iter_mut()
                    .find(|existing| existing.id == id)
                {
                    *existing = profile;
                } else {
                    state.settings.section_profiles.push(profile);
                }
                state.apply_filters(true);
                state.settings.clone()
            };
            runtime.bootstrap.save_settings(&settings);
            handles.startup_status.set_text("Saved section.");
            refresh_ui_resetting_config_editor(&handles, &runtime);
        });
    }
    {
        let handles = handles.clone();
        let runtime = runtime.clone();
        let section_picker = section_picker.clone();
        section_delete.connect_clicked(move |_| {
            let Some(profile_id) = section_picker
                .active_id()
                .map(|value| value.to_string())
                .filter(|value| !value.is_empty())
            else {
                handles.startup_status.set_text("Pick a section to delete.");
                return;
            };
            let settings = {
                let mut state = runtime.state.borrow_mut();
                state
                    .settings
                    .section_profiles
                    .retain(|profile| profile.id != profile_id);
                for search_profile in &mut state.settings.search_profiles {
                    search_profile
                        .section_profiles
                        .retain(|existing| existing != &profile_id);
                }
                for rule in &mut state.settings.notification_rules {
                    rule.section_profile_ids
                        .retain(|existing| existing != &profile_id);
                }
                if state.section_filter() == Some(profile_id.as_str()) {
                    state.section_filter = None;
                }
                state.apply_filters(true);
                state.settings.clone()
            };
            runtime.bootstrap.save_settings(&settings);
            handles.startup_status.set_text("Deleted section.");
            refresh_ui_resetting_config_editor(&handles, &runtime);
        });
    }
    let section_expander = gtk::Expander::builder()
        .label("Sections")
        .expanded(true)
        .build();
    section_expander.set_child(Some(&section_box));
    body.append(&section_expander);

    let channel_profiles = {
        let mut profiles = settings.channel_profiles.clone();
        profiles.sort_by(|left, right| left.label.cmp(&right.label).then(left.id.cmp(&right.id)));
        profiles
    };
    let channel_box = GtkBox::new(Orientation::Vertical, 8);
    let channel_help = Label::new(Some(
        "Pick channels from the cached list. Use allow to restrict a search profile to specific channels, or deny to exclude them.",
    ));
    channel_help.add_css_class("meta");
    channel_help.set_wrap(true);
    channel_help.set_xalign(0.0);
    channel_box.append(&channel_help);
    let channel_picker = ComboBoxText::new();
    let channel_options = channel_profiles
        .iter()
        .map(|profile| {
            (
                profile.id.clone(),
                format!("{} ({})", profile.label, profile.id),
            )
        })
        .collect::<Vec<_>>();
    sync_filter_picker(
        &channel_picker,
        "New channel profile",
        &channel_options,
        None,
    );
    channel_box.append(&channel_picker);
    let channel_id_label = Label::new(Some("Profile ID"));
    channel_id_label.add_css_class("meta");
    channel_id_label.set_xalign(0.0);
    channel_box.append(&channel_id_label);
    let channel_id_entry = Entry::builder()
        .placeholder_text("release_channels")
        .build();
    channel_box.append(&channel_id_entry);
    let channel_label_label = Label::new(Some("Label"));
    channel_label_label.add_css_class("meta");
    channel_label_label.set_xalign(0.0);
    channel_box.append(&channel_label_label);
    let channel_label_entry = Entry::builder()
        .placeholder_text("Release channels")
        .build();
    channel_box.append(&channel_label_entry);
    let channel_mode_label = Label::new(Some("Mode"));
    channel_mode_label.add_css_class("meta");
    channel_mode_label.set_xalign(0.0);
    channel_box.append(&channel_mode_label);
    let channel_mode_picker = build_profile_mode_picker();
    channel_box.append(&channel_mode_picker);
    let channels_entry_label = Label::new(Some("Channels"));
    channels_entry_label.add_css_class("meta");
    channels_entry_label.set_xalign(0.0);
    channel_box.append(&channels_entry_label);
    let channels_picker = MultiSelectPicker::new(
        "Select channels",
        "No cached channels yet.",
        &available_channels,
    );
    channel_box.append(&channels_picker.widget());
    let channel_matchers_label = Label::new(Some("Channel name matchers"));
    channel_matchers_label.add_css_class("meta");
    channel_matchers_label.set_xalign(0.0);
    channel_box.append(&channel_matchers_label);
    let channel_matchers_help = Label::new(Some(
        "Optional. Add one matcher per row. Choose Text or Regex, then enter the expression.",
    ));
    channel_matchers_help.add_css_class("meta");
    channel_matchers_help.set_wrap(true);
    channel_matchers_help.set_xalign(0.0);
    channel_box.append(&channel_matchers_help);
    let channel_matchers = PatternMatcherEditor::new("times_");
    channel_box.append(&channel_matchers.widget());
    let channel_actions = GtkBox::new(Orientation::Horizontal, 8);
    let channel_new = Button::with_label("New");
    let channel_save = Button::with_label("Save");
    let channel_delete = Button::with_label("Delete");
    channel_actions.append(&channel_new);
    channel_actions.append(&channel_save);
    channel_actions.append(&channel_delete);
    channel_box.append(&channel_actions);
    let channel_summary = Label::new(Some("Pick a channel profile to preview cached hits."));
    channel_summary.add_css_class("meta");
    channel_summary.set_wrap(true);
    channel_summary.set_xalign(0.0);
    channel_box.append(&channel_summary);
    apply_channel_profile_form(
        None,
        &channel_id_entry,
        &channel_label_entry,
        &channel_mode_picker,
        &channels_picker,
        &channel_matchers,
    );
    {
        let channel_profiles = channel_profiles.clone();
        let channel_id_entry = channel_id_entry.clone();
        let channel_label_entry = channel_label_entry.clone();
        let channel_mode_picker = channel_mode_picker.clone();
        let channels_picker = channels_picker.clone();
        let channel_matchers = channel_matchers.clone();
        let runtime = runtime.clone();
        let channel_summary = channel_summary.clone();
        channel_picker.connect_changed(move |picker| {
            let profile = picker.active_id().and_then(|selected| {
                if selected.is_empty() {
                    None
                } else {
                    channel_profiles
                        .iter()
                        .find(|profile| profile.id == selected.as_str())
                }
            });
            apply_channel_profile_form(
                profile,
                &channel_id_entry,
                &channel_label_entry,
                &channel_mode_picker,
                &channels_picker,
                &channel_matchers,
            );
            channel_summary.set_text(&channel_profile_preview_summary(
                &runtime.state.borrow(),
                profile,
            ));
        });
    }
    {
        let channel_picker = channel_picker.clone();
        let channel_id_entry = channel_id_entry.clone();
        let channel_label_entry = channel_label_entry.clone();
        let channel_mode_picker = channel_mode_picker.clone();
        let channels_picker = channels_picker.clone();
        let channel_matchers = channel_matchers.clone();
        let channel_summary = channel_summary.clone();
        channel_new.connect_clicked(move |_| {
            channel_picker.set_active(Some(0));
            apply_channel_profile_form(
                None,
                &channel_id_entry,
                &channel_label_entry,
                &channel_mode_picker,
                &channels_picker,
                &channel_matchers,
            );
            channel_summary.set_text("Pick a channel profile to preview cached hits.");
        });
    }
    {
        let handles = handles.clone();
        let runtime = runtime.clone();
        let channel_id_entry = channel_id_entry.clone();
        let channel_label_entry = channel_label_entry.clone();
        let channel_mode_picker = channel_mode_picker.clone();
        let channels_picker = channels_picker.clone();
        let channel_matchers = channel_matchers.clone();
        channel_save.connect_clicked(move |_| {
            let Some(id) = normalized_entry_value(channel_id_entry.text().to_string()) else {
                handles
                    .startup_status
                    .set_text("Channel profile ID is required.");
                return;
            };
            let label = normalized_entry_value(channel_label_entry.text().to_string())
                .unwrap_or_else(|| id.clone());
            let channel_name_matchers = match channel_matchers.matchers() {
                Ok(matchers) => matchers,
                Err(error) => {
                    handles.startup_status.set_text(&error);
                    return;
                }
            };
            let profile = ChannelProfile {
                id: id.clone(),
                label,
                mode: profile_mode_from_picker(&channel_mode_picker),
                channels: channels_picker.selected_set(),
                channel_name_matchers,
            };
            let settings = {
                let mut state = runtime.state.borrow_mut();
                if let Some(existing) = state
                    .settings
                    .channel_profiles
                    .iter_mut()
                    .find(|existing| existing.id == id)
                {
                    *existing = profile;
                } else {
                    state.settings.channel_profiles.push(profile);
                }
                state.apply_filters(true);
                state.settings.clone()
            };
            runtime.bootstrap.save_settings(&settings);
            handles.startup_status.set_text("Saved channel profile.");
            refresh_ui_resetting_config_editor(&handles, &runtime);
        });
    }
    {
        let handles = handles.clone();
        let runtime = runtime.clone();
        let channel_picker = channel_picker.clone();
        channel_delete.connect_clicked(move |_| {
            let Some(profile_id) = channel_picker
                .active_id()
                .map(|value| value.to_string())
                .filter(|value| !value.is_empty())
            else {
                handles
                    .startup_status
                    .set_text("Pick a channel profile to delete.");
                return;
            };
            let settings = {
                let mut state = runtime.state.borrow_mut();
                state
                    .settings
                    .channel_profiles
                    .retain(|profile| profile.id != profile_id);
                for search_profile in &mut state.settings.search_profiles {
                    search_profile
                        .channel_profiles
                        .retain(|existing| existing != &profile_id);
                }
                for rule in &mut state.settings.notification_rules {
                    rule.channel_profile_ids
                        .retain(|existing| existing != &profile_id);
                }
                state.apply_filters(true);
                state.settings.clone()
            };
            runtime.bootstrap.save_settings(&settings);
            handles.startup_status.set_text("Deleted channel profile.");
            refresh_ui_resetting_config_editor(&handles, &runtime);
        });
    }
    let channel_expander = gtk::Expander::builder()
        .label("Channel profiles")
        .expanded(false)
        .build();
    channel_expander.set_child(Some(&channel_box));
    body.append(&channel_expander);

    let author_profiles = {
        let mut profiles = settings.author_profiles.clone();
        profiles.sort_by(|left, right| left.label.cmp(&right.label).then(left.id.cmp(&right.id)));
        profiles
    };
    let author_box = GtkBox::new(Orientation::Vertical, 8);
    let author_help = Label::new(Some(
        "Pick authors from the cached member list. These profiles can then be reused from search profiles and notification rules.",
    ));
    author_help.add_css_class("meta");
    author_help.set_wrap(true);
    author_help.set_xalign(0.0);
    author_box.append(&author_help);
    let author_picker = ComboBoxText::new();
    let author_options = author_profiles
        .iter()
        .map(|profile| {
            (
                profile.id.clone(),
                format!("{} ({})", profile.label, profile.id),
            )
        })
        .collect::<Vec<_>>();
    sync_filter_picker(&author_picker, "New author profile", &author_options, None);
    author_box.append(&author_picker);
    let author_id_label = Label::new(Some("Profile ID"));
    author_id_label.add_css_class("meta");
    author_id_label.set_xalign(0.0);
    author_box.append(&author_id_label);
    let author_id_entry = Entry::builder().placeholder_text("leadership").build();
    author_box.append(&author_id_entry);
    let author_label_label = Label::new(Some("Label"));
    author_label_label.add_css_class("meta");
    author_label_label.set_xalign(0.0);
    author_box.append(&author_label_label);
    let author_label_entry = Entry::builder().placeholder_text("Leadership").build();
    author_box.append(&author_label_entry);
    let author_mode_label = Label::new(Some("Mode"));
    author_mode_label.add_css_class("meta");
    author_mode_label.set_xalign(0.0);
    author_box.append(&author_mode_label);
    let author_mode_picker = build_profile_mode_picker();
    author_box.append(&author_mode_picker);
    let authors_entry_label = Label::new(Some("Authors"));
    authors_entry_label.add_css_class("meta");
    authors_entry_label.set_xalign(0.0);
    author_box.append(&authors_entry_label);
    let authors_picker = MultiSelectPicker::new(
        "Select authors",
        "No cached members yet.",
        &available_members,
    );
    author_box.append(&authors_picker.widget());
    let author_actions = GtkBox::new(Orientation::Horizontal, 8);
    let author_new = Button::with_label("New");
    let author_save = Button::with_label("Save");
    let author_delete = Button::with_label("Delete");
    author_actions.append(&author_new);
    author_actions.append(&author_save);
    author_actions.append(&author_delete);
    author_box.append(&author_actions);
    let author_summary = Label::new(Some("Pick an author profile to preview cached hits."));
    author_summary.add_css_class("meta");
    author_summary.set_wrap(true);
    author_summary.set_xalign(0.0);
    author_box.append(&author_summary);
    apply_author_profile_form(
        None,
        &author_id_entry,
        &author_label_entry,
        &author_mode_picker,
        &authors_picker,
    );
    {
        let author_profiles = author_profiles.clone();
        let author_id_entry = author_id_entry.clone();
        let author_label_entry = author_label_entry.clone();
        let author_mode_picker = author_mode_picker.clone();
        let authors_picker = authors_picker.clone();
        let runtime = runtime.clone();
        let author_summary = author_summary.clone();
        author_picker.connect_changed(move |picker| {
            let profile = picker.active_id().and_then(|selected| {
                if selected.is_empty() {
                    None
                } else {
                    author_profiles
                        .iter()
                        .find(|profile| profile.id == selected.as_str())
                }
            });
            apply_author_profile_form(
                profile,
                &author_id_entry,
                &author_label_entry,
                &author_mode_picker,
                &authors_picker,
            );
            author_summary.set_text(&author_profile_preview_summary(
                &runtime.state.borrow(),
                profile,
            ));
        });
    }
    {
        let author_picker = author_picker.clone();
        let author_id_entry = author_id_entry.clone();
        let author_label_entry = author_label_entry.clone();
        let author_mode_picker = author_mode_picker.clone();
        let authors_picker = authors_picker.clone();
        let author_summary = author_summary.clone();
        author_new.connect_clicked(move |_| {
            author_picker.set_active(Some(0));
            apply_author_profile_form(
                None,
                &author_id_entry,
                &author_label_entry,
                &author_mode_picker,
                &authors_picker,
            );
            author_summary.set_text("Pick an author profile to preview cached hits.");
        });
    }
    {
        let handles = handles.clone();
        let runtime = runtime.clone();
        let author_id_entry = author_id_entry.clone();
        let author_label_entry = author_label_entry.clone();
        let author_mode_picker = author_mode_picker.clone();
        let authors_picker = authors_picker.clone();
        author_save.connect_clicked(move |_| {
            let Some(id) = normalized_entry_value(author_id_entry.text().to_string()) else {
                handles
                    .startup_status
                    .set_text("Author profile ID is required.");
                return;
            };
            let label = normalized_entry_value(author_label_entry.text().to_string())
                .unwrap_or_else(|| id.clone());
            let profile = AuthorProfile {
                id: id.clone(),
                label,
                mode: profile_mode_from_picker(&author_mode_picker),
                authors: authors_picker.selected_set(),
            };
            let settings = {
                let mut state = runtime.state.borrow_mut();
                if let Some(existing) = state
                    .settings
                    .author_profiles
                    .iter_mut()
                    .find(|existing| existing.id == id)
                {
                    *existing = profile;
                } else {
                    state.settings.author_profiles.push(profile);
                }
                state.apply_filters(true);
                state.settings.clone()
            };
            runtime.bootstrap.save_settings(&settings);
            handles.startup_status.set_text("Saved author profile.");
            refresh_ui_resetting_config_editor(&handles, &runtime);
        });
    }
    {
        let handles = handles.clone();
        let runtime = runtime.clone();
        let author_picker = author_picker.clone();
        author_delete.connect_clicked(move |_| {
            let Some(profile_id) = author_picker
                .active_id()
                .map(|value| value.to_string())
                .filter(|value| !value.is_empty())
            else {
                handles
                    .startup_status
                    .set_text("Pick an author profile to delete.");
                return;
            };
            let settings = {
                let mut state = runtime.state.borrow_mut();
                state
                    .settings
                    .author_profiles
                    .retain(|profile| profile.id != profile_id);
                for search_profile in &mut state.settings.search_profiles {
                    search_profile
                        .author_profiles
                        .retain(|existing| existing != &profile_id);
                }
                for rule in &mut state.settings.notification_rules {
                    rule.author_profile_ids
                        .retain(|existing| existing != &profile_id);
                }
                state.apply_filters(true);
                state.settings.clone()
            };
            runtime.bootstrap.save_settings(&settings);
            handles.startup_status.set_text("Deleted author profile.");
            refresh_ui_resetting_config_editor(&handles, &runtime);
        });
    }
    let author_expander = gtk::Expander::builder()
        .label("Author profiles")
        .expanded(false)
        .build();
    author_expander.set_child(Some(&author_box));
    body.append(&author_expander);

    let search_profiles = {
        let mut profiles = settings.search_profiles.clone();
        profiles.sort_by(|left, right| left.label.cmp(&right.label).then(left.id.cmp(&right.id)));
        profiles
    };
    let search_box = GtkBox::new(Orientation::Vertical, 8);
    let search_help = Label::new(Some(
        "Search profiles combine free-text query with reusable keyword, section, channel, and author profiles. They appear in the top timeline filter immediately after saving.",
    ));
    search_help.add_css_class("meta");
    search_help.set_wrap(true);
    search_help.set_xalign(0.0);
    search_box.append(&search_help);
    let search_picker = ComboBoxText::new();
    let search_options = search_profiles
        .iter()
        .map(|profile| {
            (
                profile.id.clone(),
                format!("{} ({})", profile.label, profile.id),
            )
        })
        .collect::<Vec<_>>();
    sync_filter_picker(&search_picker, "New search profile", &search_options, None);
    search_box.append(&search_picker);
    let search_id_label = Label::new(Some("Profile ID"));
    search_id_label.add_css_class("meta");
    search_id_label.set_xalign(0.0);
    search_box.append(&search_id_label);
    let search_id_entry = Entry::builder().placeholder_text("release_focus").build();
    search_box.append(&search_id_entry);
    let search_label_label = Label::new(Some("Label"));
    search_label_label.add_css_class("meta");
    search_label_label.set_xalign(0.0);
    search_box.append(&search_label_label);
    let search_label_entry = Entry::builder().placeholder_text("Release focus").build();
    search_box.append(&search_label_entry);
    let query_label = Label::new(Some("Text query"));
    query_label.add_css_class("meta");
    query_label.set_xalign(0.0);
    search_box.append(&query_label);
    let search_query_entry = Entry::builder().placeholder_text("ship").build();
    search_box.append(&search_query_entry);
    let search_keyword_label = Label::new(Some("Keyword profiles"));
    search_keyword_label.add_css_class("meta");
    search_keyword_label.set_xalign(0.0);
    search_box.append(&search_keyword_label);
    let search_keyword_picker = MultiSelectPicker::new(
        "Select keyword profiles",
        "No keyword profiles yet.",
        &keyword_options,
    );
    search_box.append(&search_keyword_picker.widget());
    let search_section_label = Label::new(Some("Sections"));
    search_section_label.add_css_class("meta");
    search_section_label.set_xalign(0.0);
    search_box.append(&search_section_label);
    let search_section_picker =
        MultiSelectPicker::new("Select sections", "No sections yet.", &section_options);
    search_box.append(&search_section_picker.widget());
    let search_channel_label = Label::new(Some("Channel profiles"));
    search_channel_label.add_css_class("meta");
    search_channel_label.set_xalign(0.0);
    search_box.append(&search_channel_label);
    let search_channel_picker = MultiSelectPicker::new(
        "Select channel profiles",
        "No channel profiles yet.",
        &channel_options,
    );
    search_box.append(&search_channel_picker.widget());
    let search_author_label = Label::new(Some("Author profiles"));
    search_author_label.add_css_class("meta");
    search_author_label.set_xalign(0.0);
    search_box.append(&search_author_label);
    let search_author_picker = MultiSelectPicker::new(
        "Select author profiles",
        "No author profiles yet.",
        &author_options,
    );
    search_box.append(&search_author_picker.widget());
    let search_actions = GtkBox::new(Orientation::Horizontal, 8);
    let search_new = Button::with_label("New");
    let search_save = Button::with_label("Save");
    let search_delete = Button::with_label("Delete");
    search_actions.append(&search_new);
    search_actions.append(&search_save);
    search_actions.append(&search_delete);
    search_box.append(&search_actions);
    let search_summary = Label::new(Some("Pick a search profile to preview cached hits."));
    search_summary.add_css_class("meta");
    search_summary.set_wrap(true);
    search_summary.set_xalign(0.0);
    search_box.append(&search_summary);
    apply_search_profile_form(
        None,
        &search_id_entry,
        &search_label_entry,
        &search_query_entry,
        &search_keyword_picker,
        &search_section_picker,
        &search_channel_picker,
        &search_author_picker,
    );
    {
        let search_profiles = search_profiles.clone();
        let search_id_entry = search_id_entry.clone();
        let search_label_entry = search_label_entry.clone();
        let search_query_entry = search_query_entry.clone();
        let search_keyword_picker = search_keyword_picker.clone();
        let search_section_picker = search_section_picker.clone();
        let search_channel_picker = search_channel_picker.clone();
        let search_author_picker = search_author_picker.clone();
        let runtime = runtime.clone();
        let search_summary = search_summary.clone();
        search_picker.connect_changed(move |picker| {
            let profile = picker.active_id().and_then(|selected| {
                if selected.is_empty() {
                    None
                } else {
                    search_profiles
                        .iter()
                        .find(|profile| profile.id == selected.as_str())
                }
            });
            apply_search_profile_form(
                profile,
                &search_id_entry,
                &search_label_entry,
                &search_query_entry,
                &search_keyword_picker,
                &search_section_picker,
                &search_channel_picker,
                &search_author_picker,
            );
            search_summary.set_text(&search_profile_preview_summary(
                &runtime.state.borrow(),
                profile,
            ));
        });
    }
    {
        let search_picker = search_picker.clone();
        let search_id_entry = search_id_entry.clone();
        let search_label_entry = search_label_entry.clone();
        let search_query_entry = search_query_entry.clone();
        let search_keyword_picker = search_keyword_picker.clone();
        let search_section_picker = search_section_picker.clone();
        let search_channel_picker = search_channel_picker.clone();
        let search_author_picker = search_author_picker.clone();
        let search_summary = search_summary.clone();
        search_new.connect_clicked(move |_| {
            search_picker.set_active(Some(0));
            apply_search_profile_form(
                None,
                &search_id_entry,
                &search_label_entry,
                &search_query_entry,
                &search_keyword_picker,
                &search_section_picker,
                &search_channel_picker,
                &search_author_picker,
            );
            search_summary.set_text("Pick a search profile to preview cached hits.");
        });
    }
    {
        let handles = handles.clone();
        let runtime = runtime.clone();
        let search_id_entry = search_id_entry.clone();
        let search_label_entry = search_label_entry.clone();
        let search_query_entry = search_query_entry.clone();
        let search_keyword_picker = search_keyword_picker.clone();
        let search_section_picker = search_section_picker.clone();
        let search_channel_picker = search_channel_picker.clone();
        let search_author_picker = search_author_picker.clone();
        search_save.connect_clicked(move |_| {
            let Some(id) = normalized_entry_value(search_id_entry.text().to_string()) else {
                handles
                    .startup_status
                    .set_text("Search profile ID is required.");
                return;
            };
            let label = normalized_entry_value(search_label_entry.text().to_string())
                .unwrap_or_else(|| id.clone());
            let keyword_profiles = search_keyword_picker.selected_ids();
            let section_profiles = search_section_picker.selected_ids();
            let channel_profiles = search_channel_picker.selected_ids();
            let author_profiles = search_author_picker.selected_ids();
            {
                let state = runtime.state.borrow();
                for profile_id in &keyword_profiles {
                    if !state
                        .settings
                        .keyword_profiles
                        .iter()
                        .any(|profile| &profile.id == profile_id)
                    {
                        handles
                            .startup_status
                            .set_text(&format!("Unknown keyword profile reference: {profile_id}"));
                        return;
                    }
                }
                for profile_id in &section_profiles {
                    if !state
                        .settings
                        .section_profiles
                        .iter()
                        .any(|profile| &profile.id == profile_id)
                    {
                        handles
                            .startup_status
                            .set_text(&format!("Unknown section reference: {profile_id}"));
                        return;
                    }
                }
                for profile_id in &channel_profiles {
                    if !state
                        .settings
                        .channel_profiles
                        .iter()
                        .any(|profile| &profile.id == profile_id)
                    {
                        handles
                            .startup_status
                            .set_text(&format!("Unknown channel profile reference: {profile_id}"));
                        return;
                    }
                }
                for profile_id in &author_profiles {
                    if !state
                        .settings
                        .author_profiles
                        .iter()
                        .any(|profile| &profile.id == profile_id)
                    {
                        handles
                            .startup_status
                            .set_text(&format!("Unknown author profile reference: {profile_id}"));
                        return;
                    }
                }
            }
            let profile = SearchProfile {
                id: id.clone(),
                label,
                query: normalized_entry_value(search_query_entry.text().to_string()),
                keyword_profiles,
                section_profiles,
                channel_profiles,
                author_profiles,
                channels: BTreeSet::new(),
                authors: BTreeSet::new(),
            };
            let settings = {
                let mut state = runtime.state.borrow_mut();
                if let Some(existing) = state
                    .settings
                    .search_profiles
                    .iter_mut()
                    .find(|existing| existing.id == id)
                {
                    *existing = profile;
                } else {
                    state.settings.search_profiles.push(profile);
                }
                state.apply_filters(true);
                state.settings.clone()
            };
            runtime.bootstrap.save_settings(&settings);
            handles.startup_status.set_text("Saved search profile.");
            refresh_ui_resetting_config_editor(&handles, &runtime);
        });
    }
    {
        let handles = handles.clone();
        let runtime = runtime.clone();
        let search_picker = search_picker.clone();
        search_delete.connect_clicked(move |_| {
            let Some(profile_id) = search_picker
                .active_id()
                .map(|value| value.to_string())
                .filter(|value| !value.is_empty())
            else {
                handles
                    .startup_status
                    .set_text("Pick a search profile to delete.");
                return;
            };
            let settings = {
                let mut state = runtime.state.borrow_mut();
                state
                    .settings
                    .search_profiles
                    .retain(|profile| profile.id != profile_id);
                if state.settings.active_search_profile_id.as_deref() == Some(profile_id.as_str()) {
                    state.settings.active_search_profile_id = None;
                }
                for rule in &mut state.settings.notification_rules {
                    rule.search_profile_ids
                        .retain(|existing| existing != &profile_id);
                }
                state.apply_filters(true);
                state.settings.clone()
            };
            runtime.bootstrap.save_settings(&settings);
            handles.startup_status.set_text("Deleted search profile.");
            refresh_ui_resetting_config_editor(&handles, &runtime);
        });
    }
    let search_expander = gtk::Expander::builder()
        .label("Search profiles")
        .expanded(true)
        .build();
    search_expander.set_child(Some(&search_box));
    body.append(&search_expander);

    let notification_rules = settings.notification_rules.clone();
    let notification_box = GtkBox::new(Orientation::Vertical, 8);
    let notification_help = Label::new(Some(
        "Notification rules reuse keyword, section, channel, author, and search profiles. Inline channel, author, or matcher input is intentionally not exposed here.",
    ));
    notification_help.add_css_class("meta");
    notification_help.set_wrap(true);
    notification_help.set_xalign(0.0);
    notification_box.append(&notification_help);
    let rule_picker = ComboBoxText::new();
    let rule_options = notification_rules
        .iter()
        .map(|rule| (rule.id.to_string(), format!("{} ({})", rule.label, rule.id)))
        .collect::<Vec<_>>();
    sync_filter_picker(&rule_picker, "New notification rule", &rule_options, None);
    notification_box.append(&rule_picker);
    let rule_id_label = Label::new(Some("Rule ID: new"));
    rule_id_label.add_css_class("meta");
    rule_id_label.set_xalign(0.0);
    notification_box.append(&rule_id_label);
    let rule_label_label = Label::new(Some("Label"));
    rule_label_label.add_css_class("meta");
    rule_label_label.set_xalign(0.0);
    notification_box.append(&rule_label_label);
    let rule_label_entry = Entry::builder().placeholder_text("Release profile").build();
    notification_box.append(&rule_label_entry);
    let rule_enabled = gtk::CheckButton::with_label("Enabled");
    rule_enabled.set_active(true);
    notification_box.append(&rule_enabled);
    let rule_action_label = Label::new(Some("Action"));
    rule_action_label.add_css_class("meta");
    rule_action_label.set_xalign(0.0);
    notification_box.append(&rule_action_label);
    let rule_action_picker = build_notification_action_picker();
    notification_box.append(&rule_action_picker);
    let rule_keyword_profiles_label = Label::new(Some("Keyword profiles"));
    rule_keyword_profiles_label.add_css_class("meta");
    rule_keyword_profiles_label.set_xalign(0.0);
    notification_box.append(&rule_keyword_profiles_label);
    let rule_keyword_profiles_picker = MultiSelectPicker::new(
        "Select keyword profiles",
        "No keyword profiles yet.",
        &keyword_options,
    );
    notification_box.append(&rule_keyword_profiles_picker.widget());
    let rule_section_profiles_label = Label::new(Some("Sections"));
    rule_section_profiles_label.add_css_class("meta");
    rule_section_profiles_label.set_xalign(0.0);
    notification_box.append(&rule_section_profiles_label);
    let rule_section_profiles_picker =
        MultiSelectPicker::new("Select sections", "No sections yet.", &section_options);
    notification_box.append(&rule_section_profiles_picker.widget());
    let rule_channel_profiles_label = Label::new(Some("Channel profiles"));
    rule_channel_profiles_label.add_css_class("meta");
    rule_channel_profiles_label.set_xalign(0.0);
    notification_box.append(&rule_channel_profiles_label);
    let rule_channel_profiles_picker = MultiSelectPicker::new(
        "Select channel profiles",
        "No channel profiles yet.",
        &channel_options,
    );
    notification_box.append(&rule_channel_profiles_picker.widget());
    let rule_author_profiles_label = Label::new(Some("Author profiles"));
    rule_author_profiles_label.add_css_class("meta");
    rule_author_profiles_label.set_xalign(0.0);
    notification_box.append(&rule_author_profiles_label);
    let rule_author_profiles_picker = MultiSelectPicker::new(
        "Select author profiles",
        "No author profiles yet.",
        &author_options,
    );
    notification_box.append(&rule_author_profiles_picker.widget());
    let rule_search_profiles_label = Label::new(Some("Search profiles"));
    rule_search_profiles_label.add_css_class("meta");
    rule_search_profiles_label.set_xalign(0.0);
    notification_box.append(&rule_search_profiles_label);
    let rule_search_profiles_picker = MultiSelectPicker::new(
        "Select search profiles",
        "No search profiles yet.",
        &search_options,
    );
    notification_box.append(&rule_search_profiles_picker.widget());
    let thread_only_check = gtk::CheckButton::with_label("Only threads I participate in");
    notification_box.append(&thread_only_check);
    let quiet_enabled_check = gtk::CheckButton::with_label("Use quiet hours");
    notification_box.append(&quiet_enabled_check);
    let quiet_row = GtkBox::new(Orientation::Horizontal, 8);
    let quiet_from_label = Label::new(Some("From"));
    quiet_from_label.add_css_class("meta");
    let quiet_start_picker = build_hour_picker("Start");
    quiet_start_picker.set_hexpand(true);
    let quiet_to_label = Label::new(Some("To"));
    quiet_to_label.add_css_class("meta");
    let quiet_end_picker = build_hour_picker("End");
    quiet_end_picker.set_hexpand(true);
    quiet_row.append(&quiet_from_label);
    quiet_row.append(&quiet_start_picker);
    quiet_row.append(&quiet_to_label);
    quiet_row.append(&quiet_end_picker);
    notification_box.append(&quiet_row);
    {
        let quiet_enabled_check = quiet_enabled_check.clone();
        let quiet_start_picker = quiet_start_picker.clone();
        let quiet_end_picker = quiet_end_picker.clone();
        quiet_enabled_check.connect_toggled(move |check| {
            sync_quiet_hours_inputs(check, &quiet_start_picker, &quiet_end_picker);
        });
    }
    let notification_actions = GtkBox::new(Orientation::Horizontal, 8);
    let notification_new = Button::with_label("New");
    let notification_save = Button::with_label("Save");
    let notification_delete = Button::with_label("Delete");
    notification_actions.append(&notification_new);
    notification_actions.append(&notification_save);
    notification_actions.append(&notification_delete);
    notification_box.append(&notification_actions);
    apply_notification_rule_form(
        None,
        &rule_label_entry,
        &rule_enabled,
        &rule_action_picker,
        &rule_keyword_profiles_picker,
        &rule_section_profiles_picker,
        &rule_channel_profiles_picker,
        &rule_author_profiles_picker,
        &rule_search_profiles_picker,
        &thread_only_check,
        &quiet_enabled_check,
        &quiet_start_picker,
        &quiet_end_picker,
        &rule_id_label,
    );
    {
        let notification_rules = notification_rules.clone();
        let rule_label_entry = rule_label_entry.clone();
        let rule_enabled = rule_enabled.clone();
        let rule_action_picker = rule_action_picker.clone();
        let rule_keyword_profiles_picker = rule_keyword_profiles_picker.clone();
        let rule_section_profiles_picker = rule_section_profiles_picker.clone();
        let rule_channel_profiles_picker = rule_channel_profiles_picker.clone();
        let rule_author_profiles_picker = rule_author_profiles_picker.clone();
        let rule_search_profiles_picker = rule_search_profiles_picker.clone();
        let thread_only_check = thread_only_check.clone();
        let quiet_enabled_check = quiet_enabled_check.clone();
        let quiet_start_picker = quiet_start_picker.clone();
        let quiet_end_picker = quiet_end_picker.clone();
        let rule_id_label = rule_id_label.clone();
        rule_picker.connect_changed(move |picker| {
            let rule = picker.active_id().and_then(|selected| {
                if selected.is_empty() {
                    None
                } else {
                    notification_rules
                        .iter()
                        .find(|rule| rule.id.to_string() == selected.as_str())
                }
            });
            apply_notification_rule_form(
                rule,
                &rule_label_entry,
                &rule_enabled,
                &rule_action_picker,
                &rule_keyword_profiles_picker,
                &rule_section_profiles_picker,
                &rule_channel_profiles_picker,
                &rule_author_profiles_picker,
                &rule_search_profiles_picker,
                &thread_only_check,
                &quiet_enabled_check,
                &quiet_start_picker,
                &quiet_end_picker,
                &rule_id_label,
            );
        });
    }
    {
        let rule_picker = rule_picker.clone();
        let rule_label_entry = rule_label_entry.clone();
        let rule_enabled = rule_enabled.clone();
        let rule_action_picker = rule_action_picker.clone();
        let rule_keyword_profiles_picker = rule_keyword_profiles_picker.clone();
        let rule_section_profiles_picker = rule_section_profiles_picker.clone();
        let rule_channel_profiles_picker = rule_channel_profiles_picker.clone();
        let rule_author_profiles_picker = rule_author_profiles_picker.clone();
        let rule_search_profiles_picker = rule_search_profiles_picker.clone();
        let thread_only_check = thread_only_check.clone();
        let quiet_enabled_check = quiet_enabled_check.clone();
        let quiet_start_picker = quiet_start_picker.clone();
        let quiet_end_picker = quiet_end_picker.clone();
        let rule_id_label = rule_id_label.clone();
        notification_new.connect_clicked(move |_| {
            rule_picker.set_active(Some(0));
            apply_notification_rule_form(
                None,
                &rule_label_entry,
                &rule_enabled,
                &rule_action_picker,
                &rule_keyword_profiles_picker,
                &rule_section_profiles_picker,
                &rule_channel_profiles_picker,
                &rule_author_profiles_picker,
                &rule_search_profiles_picker,
                &thread_only_check,
                &quiet_enabled_check,
                &quiet_start_picker,
                &quiet_end_picker,
                &rule_id_label,
            );
        });
    }
    {
        let handles = handles.clone();
        let runtime = runtime.clone();
        let rule_picker = rule_picker.clone();
        let rule_label_entry = rule_label_entry.clone();
        let rule_enabled = rule_enabled.clone();
        let rule_action_picker = rule_action_picker.clone();
        let rule_keyword_profiles_picker = rule_keyword_profiles_picker.clone();
        let rule_section_profiles_picker = rule_section_profiles_picker.clone();
        let rule_channel_profiles_picker = rule_channel_profiles_picker.clone();
        let rule_author_profiles_picker = rule_author_profiles_picker.clone();
        let rule_search_profiles_picker = rule_search_profiles_picker.clone();
        let thread_only_check = thread_only_check.clone();
        let quiet_enabled_check = quiet_enabled_check.clone();
        let quiet_start_picker = quiet_start_picker.clone();
        let quiet_end_picker = quiet_end_picker.clone();
        notification_save.connect_clicked(move |_| {
            let keyword_profile_ids = rule_keyword_profiles_picker.selected_ids();
            let section_profile_ids = rule_section_profiles_picker.selected_ids();
            let channel_profile_ids = rule_channel_profiles_picker.selected_ids();
            let author_profile_ids = rule_author_profiles_picker.selected_ids();
            let search_profile_ids = rule_search_profiles_picker.selected_ids();
            {
                let state = runtime.state.borrow();
                for profile_id in &keyword_profile_ids {
                    if !state
                        .settings
                        .keyword_profiles
                        .iter()
                        .any(|profile| &profile.id == profile_id)
                    {
                        handles
                            .startup_status
                            .set_text(&format!("Unknown keyword profile reference: {profile_id}"));
                        return;
                    }
                }
                for profile_id in &section_profile_ids {
                    if !state
                        .settings
                        .section_profiles
                        .iter()
                        .any(|profile| &profile.id == profile_id)
                    {
                        handles
                            .startup_status
                            .set_text(&format!("Unknown section reference: {profile_id}"));
                        return;
                    }
                }
                for profile_id in &channel_profile_ids {
                    if !state
                        .settings
                        .channel_profiles
                        .iter()
                        .any(|profile| &profile.id == profile_id)
                    {
                        handles
                            .startup_status
                            .set_text(&format!("Unknown channel profile reference: {profile_id}"));
                        return;
                    }
                }
                for profile_id in &author_profile_ids {
                    if !state
                        .settings
                        .author_profiles
                        .iter()
                        .any(|profile| &profile.id == profile_id)
                    {
                        handles
                            .startup_status
                            .set_text(&format!("Unknown author profile reference: {profile_id}"));
                        return;
                    }
                }
                for profile_id in &search_profile_ids {
                    if state.settings.search_profile(profile_id).is_none() {
                        handles
                            .startup_status
                            .set_text(&format!("Unknown search profile reference: {profile_id}"));
                        return;
                    }
                }
            }
            let quiet_hours = match parse_quiet_hours(
                quiet_enabled_check.is_active(),
                hour_from_picker(&quiet_start_picker),
                hour_from_picker(&quiet_end_picker),
            ) {
                Ok(hours) => hours,
                Err(error) => {
                    handles.startup_status.set_text(&error);
                    return;
                }
            };
            let rule_id = rule_picker
                .active_id()
                .and_then(|value| {
                    if value.is_empty() {
                        None
                    } else {
                        Uuid::parse_str(value.as_str()).ok()
                    }
                })
                .unwrap_or_else(Uuid::new_v4);
            let label = normalized_entry_value(rule_label_entry.text().to_string())
                .unwrap_or_else(|| format!("Rule {}", &rule_id.to_string()[..8]));
            let rule = NotificationRule {
                id: rule_id,
                label,
                enabled: rule_enabled.is_active(),
                channels: BTreeSet::new(),
                authors: BTreeSet::new(),
                include: Vec::new(),
                exclude: Vec::new(),
                keyword_profile_ids,
                section_profile_ids,
                channel_profile_ids,
                author_profile_ids,
                search_profile_ids,
                thread_participation_only: thread_only_check.is_active(),
                quiet_hours,
                action: notification_action_from_picker(&rule_action_picker),
            };
            if let Err(error) = rule.validate() {
                handles.startup_status.set_text(&error.to_string());
                return;
            }
            let settings = {
                let mut state = runtime.state.borrow_mut();
                if let Some(existing) = state
                    .settings
                    .notification_rules
                    .iter_mut()
                    .find(|existing| existing.id == rule.id)
                {
                    *existing = rule;
                } else {
                    state.settings.notification_rules.push(rule);
                }
                state.settings.clone()
            };
            runtime.bootstrap.save_settings(&settings);
            handles.startup_status.set_text("Saved notification rule.");
            refresh_ui_resetting_config_editor(&handles, &runtime);
        });
    }
    {
        let handles = handles.clone();
        let runtime = runtime.clone();
        let rule_picker = rule_picker.clone();
        notification_delete.connect_clicked(move |_| {
            let Some(rule_id) = rule_picker.active_id().and_then(|value| {
                if value.is_empty() {
                    None
                } else {
                    Uuid::parse_str(value.as_str()).ok()
                }
            }) else {
                handles
                    .startup_status
                    .set_text("Pick a notification rule to delete.");
                return;
            };
            let settings = {
                let mut state = runtime.state.borrow_mut();
                state
                    .settings
                    .notification_rules
                    .retain(|rule| rule.id != rule_id);
                state.settings.clone()
            };
            runtime.bootstrap.save_settings(&settings);
            handles
                .startup_status
                .set_text("Deleted notification rule.");
            refresh_ui_resetting_config_editor(&handles, &runtime);
        });
    }
    let notification_expander = gtk::Expander::builder()
        .label("Notification rules")
        .expanded(true)
        .build();
    notification_expander.set_child(Some(&notification_box));
    body.append(&notification_expander);

    column
}

fn build_thread_column(
    index: usize,
    thread_ts: &str,
    handles: &UiHandles,
    runtime: &UiRuntime,
) -> GtkBox {
    let column = GtkBox::new(Orientation::Vertical, 12);
    column.add_css_class("deck-column");
    column.add_css_class("thread-column");
    column.set_width_request(420);

    let (root, ranked, items) = {
        let borrowed = runtime.state.borrow();
        (
            borrowed.root_item(thread_ts),
            borrowed.ranked_item_for_thread(thread_ts),
            borrowed.thread_items(thread_ts),
        )
    };

    let title_text = root
        .as_ref()
        .map(|item| item.channel_name.clone())
        .unwrap_or_else(|| "Thread".to_string());
    let subtitle_text = if ranked.is_some() {
        format!("{} messages", items.len())
    } else if items.is_empty() {
        "Thread is no longer cached.".to_string()
    } else {
        format!("{} messages", items.len())
    };

    let header = GtkBox::new(Orientation::Horizontal, 8);
    let title_box = GtkBox::new(Orientation::Vertical, 4);
    let title = Label::new(Some(&title_text));
    title.add_css_class("title-3");
    title.set_xalign(0.0);
    let subtitle = Label::new(Some(&subtitle_text));
    subtitle.add_css_class("meta");
    subtitle.set_xalign(0.0);
    title_box.append(&title);
    title_box.append(&subtitle);
    let spacer = GtkBox::new(Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    let close_button = build_icon_button("window-close-symbolic", "Close this column");
    close_button.remove_css_class("nav-button");
    close_button.add_css_class("close-button");
    {
        let handles = handles.clone();
        let runtime = runtime.clone();
        close_button.connect_clicked(move |_| {
            runtime.deck_state.borrow_mut().close(index);
            refresh_ui(&handles, &runtime);
        });
    }
    header.append(&title_box);
    header.append(&spacer);
    header.append(&close_button);
    column.append(&header);

    let focused_message_ts = runtime
        .state
        .borrow()
        .focused_thread_message_ts()
        .map(str::to_string);
    let thread_list = GtkBox::new(Orientation::Vertical, 10);
    thread_list.set_vexpand(true);
    let mut scroll_target = None::<GtkBox>;
    if items.is_empty() {
        let empty = Label::new(Some("Thread is no longer available in the cache."));
        empty.add_css_class("meta");
        empty.set_wrap(true);
        empty.set_xalign(0.0);
        thread_list.append(&empty);
    } else {
        for item in &items {
            let card = build_thread_message_card(
                item,
                item.message_ts == item.thread_ts,
                focused_message_ts.as_deref() == Some(item.message_ts.as_str()),
                handles,
                runtime,
            );
            if focused_message_ts.as_deref() == Some(item.message_ts.as_str()) {
                scroll_target = Some(card.clone());
            }
            thread_list.append(&card);
        }
    }

    let thread_scroll = ScrolledWindow::new();
    thread_scroll.set_vexpand(true);
    thread_scroll.set_hexpand(true);
    thread_scroll.set_child(Some(&thread_list));
    column.append(&thread_scroll);
    if let Some(scroll_target) = scroll_target {
        let thread_scroll = thread_scroll.clone();
        glib::idle_add_local_once(move || {
            scroll_target.grab_focus();
            let allocation = scroll_target.allocation();
            let adjustment = thread_scroll.vadjustment();
            let page_size = adjustment.page_size();
            let target_top = (allocation.y() as f64 - 24.0).max(adjustment.lower());
            let max_top = (adjustment.upper() - page_size).max(adjustment.lower());
            adjustment.set_value(target_top.min(max_top));
        });
    }

    if let Some(channel_id) = root.as_ref().map(|item| item.channel_id.clone())
        && let Some(typing_summary) = runtime
            .state
            .borrow()
            .typing_summary_for_channel(&channel_id)
    {
        let typing_label = Label::new(Some(&typing_summary));
        typing_label.add_css_class("meta");
        typing_label.set_xalign(0.0);
        column.append(&typing_label);
    }

    let composer = TextView::new();
    composer.set_wrap_mode(WrapMode::WordChar);
    composer.set_size_request(-1, 120);
    composer.set_sensitive(root.is_some());
    composer.set_tooltip_text(Some("Reply in-thread without leaving the timeline."));
    column.append(&composer);

    let composer_actions = GtkBox::new(Orientation::Horizontal, 8);
    let composer_hint = Label::new(Some("Reply"));
    composer_hint.add_css_class("meta");
    composer_hint.set_xalign(0.0);
    let composer_spacer = GtkBox::new(Orientation::Horizontal, 0);
    composer_spacer.set_hexpand(true);
    let send_button = Button::with_label("Send reply");
    send_button.add_css_class("suggested-action");
    send_button.set_sensitive(
        root.as_ref().is_some_and(|item| {
            runtime
                .state
                .borrow()
                .settings
                .can_write_channel(&item.channel_id)
        }) && matches!(&*runtime.auth_state.borrow(), SlackAuthStatus::Connected(_)),
    );
    composer_actions.append(&composer_hint);
    composer_actions.append(&composer_spacer);
    composer_actions.append(&send_button);
    column.append(&composer_actions);

    {
        let handles = handles.clone();
        let runtime = runtime.clone();
        let thread_ts = thread_ts.to_string();
        let buffer = composer.buffer();
        let composer = composer.clone();
        send_button.connect_clicked(move |button| {
            let body = buffer_text(&buffer);
            if body.trim().is_empty() {
                handles.startup_status.set_text("Reply body is empty.");
                return;
            }

            let selected = {
                let borrowed = runtime.state.borrow();
                borrowed.root_item(&thread_ts)
            };
            let Some(selected) = selected else {
                handles
                    .startup_status
                    .set_text("The thread is no longer available in the cache.");
                return;
            };
            if !runtime
                .state
                .borrow()
                .settings
                .can_write_channel(&selected.channel_id)
            {
                handles
                    .startup_status
                    .set_text("This channel is configured as read-only. Replies are disabled.");
                return;
            }

            let session = match &*runtime.auth_state.borrow() {
                SlackAuthStatus::Connected(session) => session.clone(),
                _ => {
                    handles
                        .startup_status
                        .set_text("Connect Slack before sending a reply.");
                    return;
                }
            };
            let (settings, self_avatar_path) = {
                let state = runtime.state.borrow();
                let self_avatar_path = session
                    .user_id
                    .as_deref()
                    .or(session.bot_user_id.as_deref())
                    .and_then(|author_id| state.author_avatar_path_for(author_id));
                (state.settings.clone(), self_avatar_path)
            };

            handles.startup_status.set_text("Sending reply to Slack...");
            composer.set_sensitive(false);
            button.set_sensitive(false);

            let auth_tx = runtime.auth_tx.clone();
            let workspace_key = runtime.state.borrow().active_workspace_key().to_string();
            std::thread::spawn(move || {
                let result =
                    send_thread_reply(&session, &selected, &body, &settings, self_avatar_path)
                        .map_err(|error| error.to_string());
                let _ = auth_tx.send(AuthEvent::ReplySent {
                    workspace_key,
                    result,
                });
            });
        });
    }

    column
}

fn build_thread_message_card(
    item: &TimelineItem,
    is_root: bool,
    is_focused: bool,
    handles: &UiHandles,
    runtime: &UiRuntime,
) -> GtkBox {
    let card = GtkBox::new(Orientation::Horizontal, 12);
    card.add_css_class("card");
    card.add_css_class("thread-message");
    card.set_focusable(true);
    if is_root {
        card.add_css_class("thread-root");
    }
    if is_focused {
        card.add_css_class("card-active");
    }

    let avatar = build_avatar_widget(
        item.author_avatar_path.as_deref(),
        &item.author_name,
        AVATAR_SIZE_PX,
    );

    let content = GtkBox::new(Orientation::Vertical, 8);
    content.set_hexpand(true);

    let header_row = GtkBox::new(Orientation::Horizontal, 8);
    let title = Label::new(Some(&format!(
        "{} • {}",
        item.author_name, item.channel_name
    )));
    title.add_css_class("meta");
    title.set_xalign(0.0);
    let spacer = GtkBox::new(Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    let timestamp = slack_ts_to_datetime(&item.message_ts).unwrap_or(item.last_activity_at);
    let time = Label::new(Some(&relative_timestamp_text(timestamp)));
    time.add_css_class("timestamp");
    time.set_tooltip_text(Some(&absolute_local_timestamp_text(timestamp)));
    header_row.append(&title);
    header_row.append(&spacer);
    header_row.append(&time);

    content.append(&header_row);
    if let Some(body) = build_slack_body_widget(item, runtime) {
        content.append(&body);
    }
    if let Some(previews) = build_shared_message_previews(&item.body, runtime) {
        content.append(&previews);
    }
    if !item.attachments.is_empty() {
        content.append(&build_attachment_strip(
            &item.attachments,
            THREAD_ATTACHMENT_WIDTH_PX,
        ));
    }
    if let Some(reaction_bar) = build_reaction_bar(item, handles, runtime) {
        content.append(&reaction_bar);
    }
    let actions = GtkBox::new(Orientation::Horizontal, 8);
    let copy_button = build_inline_icon_button("edit-copy-symbolic", "Copy only the message text");
    let share_button =
        build_inline_icon_button("insert-link-symbolic", "Copy the Slack message URL");
    copy_button.set_sensitive(!item.body.trim().is_empty());
    {
        let handles = handles.clone();
        let message_text = render_slack_text(&item.body);
        copy_button.connect_clicked(move |_| {
            let _ = copy_text_to_clipboard(
                &handles,
                &message_text,
                "Copied message text to clipboard.",
            );
        });
    }
    {
        let handles = handles.clone();
        let runtime = runtime.clone();
        let item = item.clone();
        share_button.connect_clicked(move |_| {
            start_share_link_copy(item.clone(), &handles, &runtime);
        });
    }
    actions.append(&copy_button);
    actions.append(&share_button);
    if item_owned_by_connected_user(item, runtime) {
        let edit_button = build_edit_message_menu_button(item, handles, runtime);
        let delete_button = build_delete_message_menu_button(item, handles, runtime);
        actions.append(&edit_button);
        actions.append(&delete_button);
    }
    content.append(&actions);
    card.append(&avatar);
    card.append(&content);
    card
}

fn buffer_text(buffer: &TextBuffer) -> String {
    let start = buffer.start_iter();
    let end = buffer.end_iter();
    buffer.text(&start, &end, false).to_string()
}

fn apply_theme(provider: &gtk::CssProvider, theme_id: ThemeId) {
    provider.load_from_data(&theme_id.css());
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use chrono::Utc;
    use slaxide_core::sample::{sample_settings, sample_timeline};
    use slaxide_slack::SlackConversation;
    use uuid::Uuid;

    use super::{
        SlackRenderBlock, SlackRenderLookup, SlackTableAlignment, UiState,
        extract_slack_permalink_targets, is_syncable_conversation, normalize_reaction_name,
        notification_action_for_item, parse_fallback_slack_blocks, parse_rich_text_blocks,
        permalink_token_to_slack_ts, prioritize_initial_sync_conversations, render_segments_markup,
        seed_default_notification_rules,
    };
    use slaxide_core::{
        AppSettings, ChannelPermission, NotificationAction, NotificationRule, ReactionSummary,
        ReplyState, SearchProfile, TimelineItem,
    };

    #[test]
    fn ui_state_stays_empty_without_cached_items() {
        let state = UiState::new(
            Default::default(),
            Vec::new(),
            BTreeMap::new(),
            BTreeMap::new(),
            BTreeMap::new(),
            BTreeMap::new(),
        );

        assert!(state.visible_items().is_empty());
        assert!(state.selected_message_ts.is_none());
    }

    #[test]
    fn ui_state_selects_the_top_ranked_item() {
        let state = UiState::new(
            sample_settings(),
            sample_timeline(),
            BTreeMap::new(),
            BTreeMap::new(),
            BTreeMap::new(),
            BTreeMap::new(),
        );

        assert!(!state.visible_items().is_empty());
        assert_eq!(
            state.selected_message_ts.as_deref(),
            Some(state.visible_items()[0].item.message_ts.as_str())
        );
    }

    #[test]
    fn applying_reply_item_updates_thread_activity() {
        let mut state = UiState::new(
            sample_settings(),
            sample_timeline(),
            BTreeMap::new(),
            BTreeMap::new(),
            BTreeMap::new(),
            BTreeMap::new(),
        );
        let selected = state.selected_item().unwrap().item;
        let now = Utc::now();

        state.apply_reply_item(slaxide_core::TimelineItem {
            workspace_id: selected.workspace_id.clone(),
            channel_id: selected.channel_id.clone(),
            channel_name: selected.channel_name.clone(),
            message_ts: "9999999999.123456".into(),
            thread_ts: selected.thread_ts.clone(),
            author_id: "U-me".into(),
            author_name: "you".into(),
            author_avatar_path: None,
            body: "reply".into(),
            rich_text_blocks: vec![],
            reactions: vec![],
            attachments: vec![],
            unread: false,
            participant: true,
            direct_mention: false,
            focus_keyword_hits: vec![],
            watch_weight: selected.watch_weight,
            last_activity_at: now,
            reply_state: slaxide_core::ReplyState::Idle,
        });

        assert!(
            state
                .source_items
                .iter()
                .any(|item| item.message_ts == "9999999999.123456")
        );
        assert!(
            state
                .source_items
                .iter()
                .filter(|item| item.thread_ts == selected.thread_ts)
                .all(|item| item.last_activity_at == now && item.participant)
        );
    }

    #[test]
    fn replies_do_not_create_extra_visible_threads() {
        let mut state = UiState::new(
            sample_settings(),
            sample_timeline(),
            BTreeMap::new(),
            BTreeMap::new(),
            BTreeMap::new(),
            BTreeMap::new(),
        );
        let selected = state.selected_item().unwrap().item;
        let visible_before = state.visible_items().len();

        state.apply_reply_item(slaxide_core::TimelineItem {
            workspace_id: selected.workspace_id.clone(),
            channel_id: selected.channel_id.clone(),
            channel_name: selected.channel_name.clone(),
            message_ts: "9999999999.123456".into(),
            thread_ts: selected.thread_ts.clone(),
            author_id: "U-me".into(),
            author_name: "you".into(),
            author_avatar_path: None,
            body: "reply".into(),
            rich_text_blocks: vec![],
            reactions: vec![],
            attachments: vec![],
            unread: false,
            participant: true,
            direct_mention: false,
            focus_keyword_hits: vec![],
            watch_weight: selected.watch_weight,
            last_activity_at: Utc::now(),
            reply_state: slaxide_core::ReplyState::Idle,
        });

        assert_eq!(state.visible_items().len(), visible_before);
        assert!(
            state
                .visible_items()
                .iter()
                .all(|item| item.item.message_ts == item.item.thread_ts)
        );
    }

    #[test]
    fn default_notification_rules_seed_global_notify() {
        let mut settings = AppSettings::default();

        assert!(seed_default_notification_rules(&mut settings));
        assert_eq!(settings.notification_rules.len(), 1);
        assert_eq!(
            settings.notification_rules[0].label,
            "All incoming activity"
        );
        assert_eq!(
            settings.notification_rules[0].action,
            NotificationAction::Notify
        );
        assert!(settings.notification_rules[0].channels.is_empty());
    }

    #[test]
    fn legacy_watched_channel_rule_migrates_to_global_notify() {
        let mut settings = AppSettings::default();
        settings.notification_rules.push(NotificationRule {
            id: Uuid::new_v4(),
            label: "Watched channel activity".into(),
            enabled: true,
            channels: BTreeSet::from(["C-eng".to_string(), "C-release".to_string()]),
            authors: BTreeSet::new(),
            include: vec![],
            exclude: vec![],
            keyword_profile_ids: vec![],
            section_profile_ids: vec![],
            channel_profile_ids: vec![],
            author_profile_ids: vec![],
            search_profile_ids: vec![],
            thread_participation_only: false,
            quiet_hours: None,
            action: NotificationAction::Notify,
        });

        assert!(seed_default_notification_rules(&mut settings));
        assert_eq!(settings.notification_rules.len(), 1);
        assert_eq!(
            settings.notification_rules[0].label,
            "All incoming activity"
        );
        assert!(settings.notification_rules[0].channels.is_empty());
    }

    #[test]
    fn incoming_posts_notify_without_rules() {
        let settings = AppSettings::default();
        let item = TimelineItem {
            workspace_id: "W1".into(),
            channel_id: "C-random".into(),
            channel_name: "#random".into(),
            message_ts: "1".into(),
            thread_ts: "1".into(),
            author_id: "U2".into(),
            author_name: "teammate".into(),
            author_avatar_path: None,
            body: "shipping now".into(),
            rich_text_blocks: vec![],
            reactions: vec![],
            attachments: vec![],
            unread: true,
            participant: false,
            direct_mention: false,
            focus_keyword_hits: vec![],
            watch_weight: 1,
            last_activity_at: Utc::now(),
            reply_state: ReplyState::Idle,
        };

        assert_eq!(
            notification_action_for_item(&settings, &item),
            Some(NotificationAction::Notify)
        );
    }

    #[test]
    fn active_search_profile_filters_visible_threads() {
        let mut settings = sample_settings();
        settings.active_search_profile_id = Some("incident-only".into());
        settings.search_profiles = vec![SearchProfile {
            id: "incident-only".into(),
            label: "Incident only".into(),
            query: None,
            keyword_profiles: vec![],
            section_profiles: vec![],
            channel_profiles: vec![],
            author_profiles: vec![],
            channels: BTreeSet::from(["C-incident".into()]),
            authors: BTreeSet::new(),
        }];

        let state = UiState::new(
            settings,
            sample_timeline(),
            BTreeMap::new(),
            BTreeMap::new(),
            BTreeMap::new(),
            BTreeMap::new(),
        );

        assert!(!state.visible_items().is_empty());
        assert!(
            state
                .visible_items()
                .iter()
                .all(|ranked| ranked.item.channel_id == "C-incident")
        );
    }

    #[test]
    fn hidden_and_read_only_channel_permissions_shape_ui_options() {
        let mut settings = sample_settings();
        settings
            .channel_permissions
            .insert("C-incident".into(), ChannelPermission::Hidden);
        settings
            .channel_permissions
            .insert("C-release".into(), ChannelPermission::ReadOnly);

        let state = UiState::new(
            settings,
            sample_timeline(),
            BTreeMap::new(),
            BTreeMap::new(),
            BTreeMap::new(),
            BTreeMap::new(),
        );

        assert!(
            state
                .visible_items()
                .iter()
                .all(|ranked| ranked.item.channel_id != "C-incident")
        );
        assert!(
            state
                .available_post_channels()
                .iter()
                .all(|(channel_id, _)| channel_id != "C-release")
        );
    }

    #[test]
    fn public_channels_without_membership_still_sync() {
        let public_channel = SlackConversation {
            id: "C-public".into(),
            name: Some("times_alice".into()),
            name_normalized: Some("times_alice".into()),
            creator: None,
            is_member: Some(false),
            is_private: Some(false),
            is_archived: Some(false),
        };
        let private_channel = SlackConversation {
            id: "G-private".into(),
            name: Some("secret".into()),
            name_normalized: Some("secret".into()),
            creator: None,
            is_member: Some(false),
            is_private: Some(true),
            is_archived: Some(false),
        };

        assert!(is_syncable_conversation(&public_channel));
        assert!(!is_syncable_conversation(&private_channel));
    }

    #[test]
    fn active_profile_prioritizes_matching_channels_for_initial_sync() {
        let mut settings = AppSettings::default();
        settings.active_search_profile_id = Some("times".into());
        settings.channel_profiles = vec![slaxide_core::ChannelProfile {
            id: "times-only".into(),
            label: "Times only".into(),
            mode: slaxide_core::ProfileMode::Allow,
            channels: BTreeSet::new(),
            channel_name_matchers: vec![slaxide_core::PatternMatcher::Regex("^times_".into())],
        }];
        settings.search_profiles = vec![SearchProfile {
            id: "times".into(),
            label: "Times".into(),
            query: None,
            keyword_profiles: vec![],
            section_profiles: vec![],
            channel_profiles: vec!["times-only".into()],
            author_profiles: vec![],
            channels: BTreeSet::new(),
            authors: BTreeSet::new(),
        }];

        let channels = prioritize_initial_sync_conversations(
            vec![
                SlackConversation {
                    id: "C-random".into(),
                    name: Some("random".into()),
                    name_normalized: Some("random".into()),
                    creator: None,
                    is_member: Some(true),
                    is_private: Some(false),
                    is_archived: Some(false),
                },
                SlackConversation {
                    id: "C-times".into(),
                    name: Some("times_alice".into()),
                    name_normalized: Some("times_alice".into()),
                    creator: Some("U-alice".into()),
                    is_member: Some(false),
                    is_private: Some(false),
                    is_archived: Some(false),
                },
            ],
            &settings,
        );

        assert_eq!(channels[0].id, "C-times");
    }

    #[test]
    fn normalize_reaction_name_accepts_shortcode_aliases_and_unicode() {
        assert_eq!(normalize_reaction_name(":thumbsup:"), Some("+1".into()));
        assert_eq!(normalize_reaction_name("thumbsup"), Some("+1".into()));
        assert_eq!(normalize_reaction_name("👍"), Some("+1".into()));
        assert_eq!(
            normalize_reaction_name("white-check-mark"),
            Some("white_check_mark".into())
        );
    }

    #[test]
    fn normalize_reaction_name_preserves_custom_workspace_names() {
        assert_eq!(
            normalize_reaction_name(":party-parrot:"),
            Some("party-parrot".into())
        );
        assert_eq!(normalize_reaction_name(""), None);
        assert_eq!(normalize_reaction_name("party parrot"), None);
    }

    #[test]
    fn live_reaction_events_update_counts_without_double_counting_self() {
        let mut state = UiState::new(
            Default::default(),
            vec![TimelineItem {
                workspace_id: "W1".into(),
                channel_id: "C1".into(),
                channel_name: "#general".into(),
                message_ts: "1".into(),
                thread_ts: "1".into(),
                author_id: "U-author".into(),
                author_name: "author".into(),
                author_avatar_path: None,
                body: "hello".into(),
                rich_text_blocks: vec![],
                reactions: vec![ReactionSummary {
                    name: "+1".into(),
                    emoji: "👍".into(),
                    count: 1,
                    me: true,
                }],
                attachments: vec![],
                unread: false,
                participant: false,
                direct_mention: false,
                focus_keyword_hits: vec![],
                watch_weight: 1,
                last_activity_at: Utc::now(),
                reply_state: ReplyState::Idle,
            }],
            BTreeMap::new(),
            BTreeMap::new(),
            BTreeMap::new(),
            BTreeMap::new(),
        );

        assert!(state.apply_reaction_added("1", "+1", Some("U-me"), Some("U-me")));
        assert!(state.apply_reaction_added("1", "+1", Some("U-other"), Some("U-me")));

        let reactions = &state.source_items[0].reactions;
        assert_eq!(reactions.len(), 1);
        assert_eq!(reactions[0].count, 2);
        assert!(reactions[0].me);
    }

    #[test]
    fn live_reaction_remove_drops_empty_reaction() {
        let mut state = UiState::new(
            Default::default(),
            vec![TimelineItem {
                workspace_id: "W1".into(),
                channel_id: "C1".into(),
                channel_name: "#general".into(),
                message_ts: "1".into(),
                thread_ts: "1".into(),
                author_id: "U-author".into(),
                author_name: "author".into(),
                author_avatar_path: None,
                body: "hello".into(),
                rich_text_blocks: vec![],
                reactions: vec![ReactionSummary {
                    name: "tada".into(),
                    emoji: "🎉".into(),
                    count: 1,
                    me: false,
                }],
                attachments: vec![],
                unread: false,
                participant: false,
                direct_mention: false,
                focus_keyword_hits: vec![],
                watch_weight: 1,
                last_activity_at: Utc::now(),
                reply_state: ReplyState::Idle,
            }],
            BTreeMap::new(),
            BTreeMap::new(),
            BTreeMap::new(),
            BTreeMap::new(),
        );

        assert!(state.apply_reaction_removed("1", "tada", Some("U-other"), Some("U-me")));
        assert!(state.source_items[0].reactions.is_empty());
    }

    #[test]
    fn remote_message_snapshot_overwrites_reactions() {
        let mut state = UiState::new(
            Default::default(),
            vec![TimelineItem {
                workspace_id: "W1".into(),
                channel_id: "C1".into(),
                channel_name: "#general".into(),
                message_ts: "1".into(),
                thread_ts: "1".into(),
                author_id: "U-author".into(),
                author_name: "author".into(),
                author_avatar_path: None,
                body: "hello".into(),
                rich_text_blocks: vec![],
                reactions: vec![],
                attachments: vec![],
                unread: false,
                participant: false,
                direct_mention: false,
                focus_keyword_hits: vec![],
                watch_weight: 1,
                last_activity_at: Utc::now(),
                reply_state: ReplyState::Idle,
            }],
            BTreeMap::new(),
            BTreeMap::new(),
            BTreeMap::new(),
            BTreeMap::new(),
        );

        assert!(state.apply_remote_message_snapshot(
            "1",
            "hello",
            vec![],
            vec![ReactionSummary {
                name: "+1".into(),
                emoji: "👍".into(),
                count: 3,
                me: false,
            }],
            vec![],
        ));

        assert_eq!(state.source_items[0].reactions.len(), 1);
        assert_eq!(state.source_items[0].reactions[0].count, 3);
    }

    #[test]
    fn permalink_token_round_trips_to_slack_timestamp() {
        assert_eq!(
            permalink_token_to_slack_ts("p1710112233445566"),
            Some("1710112233.445566".into())
        );
        assert_eq!(permalink_token_to_slack_ts("not-a-permalink"), None);
    }

    #[test]
    fn shared_message_links_extract_from_plain_and_slack_markup_urls() {
        let text = "plain https://acme.slack.com/archives/C123/p1710112233445566 and <https://acme.slack.com/archives/C999/p1700000000000123|Slack link>";
        let targets = extract_slack_permalink_targets(text);

        assert_eq!(targets.len(), 2);
        assert!(targets.iter().any(|target| {
            target.channel_id == "C123" && target.message_ts == "1710112233.445566"
        }));
        assert!(targets.iter().any(|target| {
            target.channel_id == "C999" && target.message_ts == "1700000000.000123"
        }));
    }

    #[test]
    fn fallback_slack_blocks_parse_basic_formatting() {
        let blocks = parse_fallback_slack_blocks(
            "*bold* _italic_ __underline__ ~strike~\n- first\n1. ordered\n> quoted\n```rs\nlet x = 1;\n```",
        );

        assert!(matches!(blocks[0], SlackRenderBlock::Paragraph(_)));
        assert!(matches!(blocks[1], SlackRenderBlock::UnorderedList(_)));
        assert!(matches!(blocks[2], SlackRenderBlock::OrderedList(_)));
        assert!(matches!(blocks[3], SlackRenderBlock::Quote(_)));
        assert!(matches!(
            blocks[4],
            SlackRenderBlock::CodeBlock {
                code: _,
                language_hint: _
            }
        ));

        let paragraph = match &blocks[0] {
            SlackRenderBlock::Paragraph(segments) => render_segments_markup(segments),
            _ => String::new(),
        };
        assert!(paragraph.contains("<b>bold</b>"));
        assert!(paragraph.contains("<i>italic</i>"));
        assert!(paragraph.contains("<u>underline</u>"));
        assert!(paragraph.contains("<s>strike</s>"));
    }

    #[test]
    fn rich_text_blocks_render_lists_and_links() {
        let lookup = SlackRenderLookup {
            user_names: BTreeMap::from([("U123".to_string(), "Alice".to_string())]),
            channel_names: BTreeMap::from([("C123".to_string(), "#times_alice".to_string())]),
        };
        let blocks = parse_rich_text_blocks(
            &[serde_json::json!({
                "type": "rich_text",
                "elements": [
                    {
                        "type": "rich_text_section",
                        "elements": [
                            { "type": "text", "text": "hello ", "style": { "bold": true } },
                            { "type": "link", "url": "https://example.com", "text": "docs" },
                            { "type": "text", "text": " " },
                            { "type": "user", "user_id": "U123" }
                        ]
                    },
                    {
                        "type": "rich_text_list",
                        "style": "ordered",
                        "elements": [
                            { "type": "rich_text_section", "elements": [{ "type": "text", "text": "first" }]},
                            { "type": "rich_text_section", "elements": [{ "type": "channel", "channel_id": "C123" }]}
                        ]
                    }
                ]
            })],
            &lookup,
        );

        let first = match &blocks[0] {
            SlackRenderBlock::Paragraph(segments) => render_segments_markup(segments),
            _ => String::new(),
        };
        assert!(first.contains("<b>hello </b>"));
        assert!(first.contains("href=\"https://example.com\""));
        assert!(first.contains("@Alice"));

        match &blocks[1] {
            SlackRenderBlock::OrderedList(items) => {
                assert_eq!(items.len(), 2);
                assert_eq!(
                    items[1]
                        .1
                        .iter()
                        .map(|segment| segment.text.as_str())
                        .collect::<String>(),
                    "#times_alice"
                );
            }
            _ => panic!("expected ordered list"),
        }
    }

    #[test]
    fn fallback_parser_detects_gfm_tables() {
        let blocks = parse_fallback_slack_blocks(
            "| name | score |\n| :--- | ---: |\n| alice | 10 |\n| bob | 20 |",
        );

        match &blocks[0] {
            SlackRenderBlock::Table {
                headers,
                rows,
                alignments,
            } => {
                assert_eq!(headers.len(), 2);
                assert_eq!(rows.len(), 2);
                assert_eq!(alignments[0], SlackTableAlignment::Left);
                assert_eq!(alignments[1], SlackTableAlignment::Right);
            }
            _ => panic!("expected table"),
        }
    }

    #[test]
    fn user_directory_names_override_raw_ids() {
        let state = UiState::new(
            Default::default(),
            Vec::new(),
            BTreeMap::from([("U-alice".to_string(), "Alice".to_string())]),
            BTreeMap::new(),
            BTreeMap::new(),
            BTreeMap::new(),
        );

        assert_eq!(state.author_name_for("U-alice", false), "Alice");
        assert_eq!(state.author_name_for("U-me", true), "you");
        assert_eq!(state.author_name_for("U-unknown", false), "U-unknown");
    }
}
