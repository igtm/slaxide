use std::{cell::RefCell, rc::Rc};

use chrono::Utc;
use gtk::{
    Application, ApplicationWindow, Box as GtkBox, Button, ComboBoxText, Label, ListBox,
    ListBoxRow, Orientation, ScrolledWindow, SearchEntry, SelectionMode, Separator, TextBuffer,
    TextView, gdk, prelude::*,
};
use relm4::gtk;
use slaxide_core::{
    AppSettings, RankedTimelineItem, ReplyState, ThemeId, TimelineItem, TimelineMode,
    TimelineRanker,
    sample::{sample_settings, sample_timeline},
};

fn main() {
    let app = Application::builder()
        .application_id("dev.slaxide.Slaxide")
        .build();
    app.connect_activate(build_ui);
    app.run();
}

#[derive(Clone)]
struct UiState {
    settings: AppSettings,
    theme_id: ThemeId,
    mode: TimelineMode,
    source_items: Vec<TimelineItem>,
    ranked_items: Vec<RankedTimelineItem>,
    selected_message_ts: Option<String>,
}

impl UiState {
    fn new() -> Self {
        let settings = sample_settings();
        let source_items = sample_timeline();
        let ranker = TimelineRanker::default();
        let ranked_items = ranker.visible_items(
            TimelineMode::Focus,
            &settings.timeline,
            source_items.clone(),
        );

        Self {
            theme_id: settings.theme_id,
            settings,
            mode: TimelineMode::Focus,
            source_items,
            ranked_items,
            selected_message_ts: None,
        }
    }

    fn visible_items(&self) -> &[RankedTimelineItem] {
        &self.ranked_items
    }

    fn set_mode(&mut self, mode: TimelineMode) {
        self.mode = mode;
        self.rerank();
    }

    fn set_theme(&mut self, theme_id: ThemeId) {
        self.theme_id = theme_id;
        self.settings.theme_id = theme_id;
    }

    fn rerank(&mut self) {
        let ranker = TimelineRanker::new(Utc::now());
        self.ranked_items = ranker.visible_items(
            self.mode,
            &self.settings.timeline,
            self.source_items.clone(),
        );
        if self.selected_message_ts.as_ref().is_some_and(|message_ts| {
            !self
                .ranked_items
                .iter()
                .any(|item| &item.item.message_ts == message_ts)
        }) {
            self.selected_message_ts = self
                .ranked_items
                .first()
                .map(|item| item.item.message_ts.clone());
        }
    }

    fn select(&mut self, message_ts: Option<String>) {
        self.selected_message_ts = message_ts;
    }

    fn selected_item(&self) -> Option<RankedTimelineItem> {
        self.selected_message_ts.as_ref().and_then(|selected| {
            self.ranked_items
                .iter()
                .find(|item| &item.item.message_ts == selected)
                .cloned()
        })
    }

    fn send_local_reply(&mut self, body: String) {
        let Some(selected) = self.selected_item() else {
            return;
        };
        if body.trim().is_empty() {
            return;
        }

        let now = Utc::now();
        self.source_items.push(TimelineItem {
            workspace_id: selected.item.workspace_id.clone(),
            channel_id: selected.item.channel_id.clone(),
            channel_name: selected.item.channel_name.clone(),
            message_ts: format!("{}.{}", now.timestamp(), now.timestamp_subsec_millis()),
            thread_ts: selected.item.thread_ts.clone(),
            author_id: "U-me".into(),
            author_name: "you".into(),
            body,
            attachments: vec![],
            unread: false,
            participant: true,
            direct_mention: false,
            focus_keyword_hits: vec![],
            watch_weight: selected.item.watch_weight,
            last_activity_at: now,
            reply_state: ReplyState::Idle,
        });

        for item in &mut self.source_items {
            if item.thread_ts == selected.item.thread_ts {
                item.last_activity_at = now;
                item.participant = true;
            }
        }

        self.rerank();
    }
}

fn build_ui(app: &Application) {
    let state = Rc::new(RefCell::new(UiState::new()));
    let provider = gtk::CssProvider::new();
    apply_theme(&provider, state.borrow().theme_id);

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
        .default_width(1440)
        .default_height(900)
        .build();

    let root = GtkBox::new(Orientation::Horizontal, 16);
    root.set_margin_start(16);
    root.set_margin_end(16);
    root.set_margin_top(16);
    root.set_margin_bottom(16);

    let rail = GtkBox::new(Orientation::Vertical, 12);
    rail.add_css_class("rail");
    rail.set_width_request(240);

    let brand = Label::new(Some("Slaxide"));
    brand.add_css_class("title-2");
    brand.set_xalign(0.0);
    rail.append(&brand);

    let search = SearchEntry::builder()
        .placeholder_text("Ctrl+K to jump")
        .build();
    rail.append(&search);

    let focus_button = Button::with_label("Focus");
    focus_button.add_css_class("suggested-action");
    let recent_button = Button::with_label("Recent");
    let theme_picker = ComboBoxText::new();
    for theme in ThemeId::ALL {
        theme_picker.append(Some(theme.slug()), theme.label());
    }
    theme_picker.set_active_id(Some(state.borrow().theme_id.slug()));

    rail.append(&focus_button);
    rail.append(&recent_button);
    rail.append(&Separator::new(Orientation::Horizontal));
    rail.append(&Label::new(Some("Theme")));
    rail.append(&theme_picker);
    rail.append(&Separator::new(Orientation::Horizontal));
    rail.append(&Label::new(Some("Scope")));
    rail.append(&Label::new(Some("Public + Private channels")));
    rail.append(&Label::new(Some("7-day cache + live")));

    let center = GtkBox::new(Orientation::Vertical, 12);
    center.set_hexpand(true);
    let timeline_header = Label::new(Some("Unified timeline"));
    timeline_header.add_css_class("title-2");
    timeline_header.set_xalign(0.0);
    center.append(&timeline_header);

    let list = ListBox::new();
    list.set_selection_mode(SelectionMode::Single);
    let list_scroll = ScrolledWindow::new();
    list_scroll.set_vexpand(true);
    list_scroll.set_hexpand(true);
    list_scroll.set_child(Some(&list));
    center.append(&list_scroll);

    let drawer = GtkBox::new(Orientation::Vertical, 12);
    drawer.add_css_class("drawer");
    drawer.set_width_request(360);

    let thread_title = Label::new(Some("Select a thread"));
    thread_title.add_css_class("title-3");
    thread_title.set_xalign(0.0);
    let thread_meta = Label::new(Some("Thread replies stay here"));
    thread_meta.add_css_class("meta");
    thread_meta.set_xalign(0.0);
    let thread_body = Label::new(Some("No thread selected"));
    thread_body.set_xalign(0.0);
    thread_body.set_wrap(true);
    let composer = TextView::new();
    composer.set_vexpand(true);
    composer.set_wrap_mode(gtk::WrapMode::WordChar);
    composer
        .buffer()
        .set_text("Reply in-thread without changing channels.");

    let composer_actions = GtkBox::new(Orientation::Horizontal, 8);
    let attach_button = Button::with_label("Attach");
    let paste_image_button = Button::with_label("Paste image");
    let send_button = Button::with_label("Send local demo reply");
    send_button.add_css_class("suggested-action");
    composer_actions.append(&attach_button);
    composer_actions.append(&paste_image_button);
    composer_actions.append(&send_button);

    drawer.append(&thread_title);
    drawer.append(&thread_meta);
    drawer.append(&thread_body);
    drawer.append(&composer);
    drawer.append(&composer_actions);

    root.append(&rail);
    root.append(&center);
    root.append(&drawer);
    window.set_child(Some(&root));

    rebuild_list(&list, &state);
    refresh_drawer(&state, &thread_title, &thread_meta, &thread_body);

    {
        let list = list.clone();
        let state = state.clone();
        let thread_title = thread_title.clone();
        let thread_meta = thread_meta.clone();
        let thread_body = thread_body.clone();
        focus_button.connect_clicked(move |_| {
            state.borrow_mut().set_mode(TimelineMode::Focus);
            rebuild_list(&list, &state);
            refresh_drawer(&state, &thread_title, &thread_meta, &thread_body);
        });
    }

    {
        let list = list.clone();
        let state = state.clone();
        let thread_title = thread_title.clone();
        let thread_meta = thread_meta.clone();
        let thread_body = thread_body.clone();
        recent_button.connect_clicked(move |_| {
            state.borrow_mut().set_mode(TimelineMode::Recent);
            rebuild_list(&list, &state);
            refresh_drawer(&state, &thread_title, &thread_meta, &thread_body);
        });
    }

    {
        let state = state.clone();
        let provider = provider.clone();
        theme_picker.connect_changed(move |picker| {
            let Some(theme_slug) = picker.active_id() else {
                return;
            };
            let Some(theme_id) = ThemeId::from_slug(theme_slug.as_str()) else {
                return;
            };

            state.borrow_mut().set_theme(theme_id);
            apply_theme(&provider, theme_id);
        });
    }

    {
        let state = state.clone();
        let thread_title = thread_title.clone();
        let thread_meta = thread_meta.clone();
        let thread_body = thread_body.clone();
        list.connect_row_selected(move |_, row| {
            let selected = row.map(|row| row.widget_name().to_string());
            state.borrow_mut().select(selected);
            refresh_drawer(&state, &thread_title, &thread_meta, &thread_body);
        });
    }

    {
        let list = list.clone();
        let state = state.clone();
        let thread_title = thread_title.clone();
        let thread_meta = thread_meta.clone();
        let thread_body = thread_body.clone();
        let buffer = composer.buffer();
        send_button.connect_clicked(move |_| {
            let text = buffer_text(&buffer);
            state.borrow_mut().send_local_reply(text);
            buffer.set_text("");
            rebuild_list(&list, &state);
            refresh_drawer(&state, &thread_title, &thread_meta, &thread_body);
        });
    }

    attach_button.connect_clicked(|button| {
        button.set_tooltip_text(Some("GTK file dialog integration is the next step."));
    });
    paste_image_button.connect_clicked(|button| {
        button.set_tooltip_text(Some(
            "Clipboard image upload will use the same upload service.",
        ));
    });

    window.present();
}

fn rebuild_list(list: &ListBox, state: &Rc<RefCell<UiState>>) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }

    for ranked in state.borrow().visible_items() {
        let row = build_row(ranked);
        if state
            .borrow()
            .selected_message_ts
            .as_ref()
            .is_some_and(|selected| selected == &ranked.item.message_ts)
        {
            list.select_row(Some(&row));
        }
        list.append(&row);
    }
}

fn build_row(ranked: &RankedTimelineItem) -> ListBoxRow {
    let row = ListBoxRow::new();
    row.set_widget_name(&ranked.item.message_ts);
    row.add_css_class("card");

    let container = GtkBox::new(Orientation::Vertical, 8);
    let header = Label::new(Some(&format!(
        "{}  •  {}  •  {:.0}",
        ranked.item.channel_name, ranked.item.author_name, ranked.score
    )));
    header.add_css_class("meta");
    header.set_xalign(0.0);

    let body = Label::new(Some(&ranked.item.body));
    body.set_wrap(true);
    body.set_xalign(0.0);

    let reasons = ranked
        .reasons
        .iter()
        .map(|reason| format!("{reason:?}"))
        .collect::<Vec<_>>()
        .join(" · ");
    let meta = Label::new(Some(&reasons));
    meta.add_css_class(if ranked.item.direct_mention {
        "mention"
    } else if ranked.item.unread {
        "unread"
    } else {
        "meta"
    });
    meta.set_xalign(0.0);

    container.append(&header);
    container.append(&body);
    container.append(&meta);
    row.set_child(Some(&container));
    row
}

fn refresh_drawer(
    state: &Rc<RefCell<UiState>>,
    thread_title: &Label,
    thread_meta: &Label,
    thread_body: &Label,
) {
    if let Some(selected) = state.borrow().selected_item() {
        thread_title.set_text(&selected.item.channel_name);
        thread_meta.set_text(&format!(
            "{} • thread {} • {} reasons",
            selected.item.author_name,
            selected.item.thread_ts,
            selected.reasons.len()
        ));
        thread_body.set_text(&selected.item.body);
    } else {
        thread_title.set_text("Select a thread");
        thread_meta.set_text("Thread replies stay here");
        thread_body.set_text("No thread selected");
    }
}

fn buffer_text(buffer: &TextBuffer) -> String {
    let start = buffer.start_iter();
    let end = buffer.end_iter();
    buffer.text(&start, &end, false).to_string()
}

fn apply_theme(provider: &gtk::CssProvider, theme_id: ThemeId) {
    provider.load_from_data(theme_id.css().as_bytes());
}
