//! shaderwarm desktop UI: game list with cache status, replay, cleanup and settings.

mod model;

use iced::font::{self, Font};
use iced::widget::{
    button, checkbox, column, container, horizontal_space, progress_bar, row, scrollable, text, text_input, toggler,
    Column,
};
use iced::{Alignment, Border, Color, Element, Length, Task, Theme};

use model::{JobEvent, Row, Snapshot};
use shaderwarm_core::settings::Settings;
use shaderwarm_core::state::Staleness;

const BOLD: Font = Font { weight: font::Weight::Bold, ..Font::DEFAULT };
const MUTED: Color = Color::from_rgb(0.62, 0.64, 0.70);
const GOOD: Color = Color::from_rgb(0.40, 0.80, 0.50);
const WARN: Color = Color::from_rgb(0.95, 0.70, 0.30);
const BAD: Color = Color::from_rgb(0.95, 0.42, 0.40);
const CARD: Color = Color::from_rgb(0.17, 0.18, 0.21);

/// Darker than the theme's rounded box so muted text stays readable.
fn card(_: &Theme) -> container::Style {
    container::Style {
        background: Some(CARD.into()),
        border: Border { radius: 8.0.into(), ..Border::default() },
        ..container::Style::default()
    }
}

pub fn main() -> iced::Result {
    iced::application("shaderwarm", App::update, App::view)
        .theme(|_| Theme::Dark)
        .window_size(iced::Size::new(1180.0, 760.0))
        .run_with(App::new)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Games,
    Settings,
}

#[derive(Debug, Clone)]
enum Dialog {
    Purge { appid: u32, name: String, recordings: bool, backup: bool, preview: Option<(usize, u64)> },
    SelfCompiling { appid: u32, name: String, reason: String },
    Video { appid: u32, name: String, bytes: u64 },
}

struct JobState {
    name: String,
    index: usize,
    count: usize,
    done: u64,
    total: u64,
}

struct App {
    snapshot: Option<Snapshot>,
    load_error: Option<String>,
    loading: bool,
    tab: Tab,
    filter: String,
    job: Option<JobState>,
    log: Vec<(bool, String)>,
    dialog: Option<Dialog>,
    settings: Settings,
    threads_input: String,
}

#[derive(Debug, Clone)]
enum Message {
    Refresh,
    Loaded(Result<Snapshot, String>),
    Tab(Tab),
    Filter(String),
    Replay(u32),
    ReplayConfirmed(u32),
    ReplayStale,
    Job(JobEvent),
    CopyLaunchOption,
    OpenPurge(u32),
    PurgeRecordings(bool),
    PurgeBackup(bool),
    PurgePreview(Result<(usize, u64), String>),
    PurgeConfirmed,
    OpenVideo(u32),
    VideoConfirmed,
    Restore(u32),
    ActionDone(Result<String, String>),
    CloseDialog,
    Threads(String),
    IncludeSelfCompiling(bool),
    AutoReplay(bool),
}

impl App {
    fn new() -> (Self, Task<Message>) {
        let settings = Settings::load();
        let app = Self {
            snapshot: None,
            load_error: None,
            loading: true,
            tab: Tab::Games,
            filter: String::new(),
            job: None,
            log: Vec::new(),
            dialog: None,
            threads_input: settings.threads.map(|n| n.to_string()).unwrap_or_default(),
            settings,
        };
        (app, load())
    }

    fn row(&self, appid: u32) -> Option<&Row> {
        self.snapshot.as_ref()?.rows.iter().find(|r| r.game.appid == appid)
    }

    fn busy(&self) -> bool {
        self.job.is_some()
    }

    fn note(&mut self, result: Result<String, String>) {
        match result {
            Ok(m) => self.log.push((true, m)),
            Err(e) => self.log.push((false, e)),
        }
        let excess = self.log.len().saturating_sub(6);
        self.log.drain(..excess);
    }

    fn start_replay(&mut self, appids: Vec<u32>) -> Task<Message> {
        if appids.is_empty() || self.busy() {
            return Task::none();
        }
        self.job = Some(JobState { name: String::new(), index: 0, count: appids.len(), done: 0, total: 0 });
        Task::run(model::spawn_replay(appids, self.settings.effective_threads()), Message::Job)
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Refresh => {
                self.loading = true;
                return load();
            }
            Message::Loaded(result) => {
                self.loading = false;
                match result {
                    Ok(s) => {
                        self.snapshot = Some(s);
                        self.load_error = None;
                    }
                    Err(e) => self.load_error = Some(e),
                }
            }
            Message::Tab(tab) => self.tab = tab,
            Message::Filter(f) => self.filter = f,
            Message::Replay(appid) => {
                if let Some(r) = self.row(appid).filter(|r| r.verdict.self_compiles) {
                    self.dialog = Some(Dialog::SelfCompiling {
                        appid,
                        name: r.game.name.clone(),
                        reason: r.verdict.reason(),
                    });
                    return Task::none();
                }
                return self.start_replay(vec![appid]);
            }
            Message::ReplayConfirmed(appid) => {
                self.dialog = None;
                return self.start_replay(vec![appid]);
            }
            Message::ReplayStale => {
                let include = self.settings.include_self_compiling;
                let ids = self.snapshot.as_ref().map_or_else(Vec::new, |s| {
                    s.rows
                        .iter()
                        .filter(|r| r.staleness.is_some_and(Staleness::needs_replay))
                        .filter(|r| include || !r.verdict.self_compiles)
                        .map(|r| r.game.appid)
                        .collect()
                });
                return self.start_replay(ids);
            }
            Message::Job(event) => match event {
                JobEvent::Started { name, index, count } => {
                    self.job = Some(JobState { name, index, count, done: 0, total: 0 });
                }
                JobEvent::Progress { done, total } => {
                    if let Some(j) = &mut self.job {
                        j.done = done;
                        j.total = total;
                    }
                }
                JobEvent::Finished { name, result } => {
                    self.note(result.map(|()| format!("{name}: cache replayed")));
                }
                JobEvent::AllDone => {
                    self.job = None;
                    return load();
                }
            },
            Message::CopyLaunchOption => {
                self.note(Ok(format!("Copied launch option: {}", model::LAUNCH_OPTION)));
                return iced::clipboard::write(model::LAUNCH_OPTION.to_string());
            }
            Message::OpenPurge(appid) => {
                let name = self.row(appid).map(|r| r.game.name.clone()).unwrap_or_default();
                self.dialog = Some(Dialog::Purge { appid, name, recordings: false, backup: true, preview: None });
                return preview(appid, false);
            }
            Message::PurgeRecordings(on) => {
                if let Some(Dialog::Purge { appid, recordings, preview: p, .. }) = &mut self.dialog {
                    *recordings = on;
                    *p = None;
                    return preview(*appid, on);
                }
            }
            Message::PurgeBackup(on) => {
                if let Some(Dialog::Purge { backup, .. }) = &mut self.dialog {
                    *backup = on;
                }
            }
            Message::PurgePreview(result) => match (result, &mut self.dialog) {
                (Ok(p), Some(Dialog::Purge { preview, .. })) => *preview = Some(p),
                (Err(e), _) => self.note(Err(e)),
                _ => {}
            },
            Message::PurgeConfirmed => {
                if let Some(Dialog::Purge { appid, recordings, backup, .. }) = self.dialog.take() {
                    return action(move || model::purge(appid, recordings, backup));
                }
            }
            Message::OpenVideo(appid) => {
                if let Some(r) = self.row(appid) {
                    self.dialog = Some(Dialog::Video { appid, name: r.game.name.clone(), bytes: r.video });
                }
            }
            Message::VideoConfirmed => {
                if let Some(Dialog::Video { appid, .. }) = self.dialog.take() {
                    return action(move || model::clean_video(appid));
                }
            }
            Message::Restore(appid) => return action(move || model::restore_latest(appid)),
            Message::ActionDone(result) => {
                self.note(result);
                return load();
            }
            Message::CloseDialog => self.dialog = None,
            Message::Threads(input) => {
                self.threads_input = input.chars().filter(char::is_ascii_digit).collect();
                self.settings.threads = self.threads_input.parse().ok().filter(|&n| n > 0);
                self.save_settings();
            }
            Message::IncludeSelfCompiling(on) => {
                self.settings.include_self_compiling = on;
                self.save_settings();
            }
            Message::AutoReplay(on) => return action(move || model::set_auto_replay(on)),
        }
        Task::none()
    }

    fn save_settings(&mut self) {
        if let Err(e) = self.settings.save() {
            self.note(Err(format!("saving settings: {e:#}")));
        }
    }

    fn view(&self) -> Element<'_, Message> {
        let tab_button = |label, tab| {
            let style = if self.tab == tab { button::primary } else { button::secondary };
            button(text(label)).style(style).on_press(Message::Tab(tab))
        };
        let header = row![
            text("shaderwarm").size(24).font(BOLD),
            text(self.snapshot.as_ref().and_then(|s| s.gpu.clone()).unwrap_or_default()).color(MUTED),
            horizontal_space(),
            tab_button("Games", Tab::Games),
            tab_button("Settings", Tab::Settings),
            button(text(if self.loading { "Refreshing…" } else { "Refresh" }))
                .style(button::secondary)
                .on_press_maybe((!self.loading).then_some(Message::Refresh)),
        ]
        .spacing(12)
        .align_y(Alignment::Center);

        let mut page = column![header].spacing(14);
        if let Some(e) = &self.load_error {
            page = page.push(text(format!("Could not read the Steam library: {e}")).color(BAD));
        }
        page = page.push_maybe(self.warnings());
        page = page.push_maybe(self.job_panel());
        page = page.push_maybe(self.dialog_panel());
        page = page.push(match self.tab {
            Tab::Games => self.games(),
            Tab::Settings => self.settings_page(),
        });
        if !self.log.is_empty() {
            let lines = self.log.iter().rev().take(3).map(|(ok, m)| text(m.clone()).size(13).color(if *ok { GOOD } else { BAD }).into());
            page = page.push(Column::with_children(lines).spacing(2));
        }
        container(page).padding(18).width(Length::Fill).height(Length::Fill).into()
    }

    fn warnings(&self) -> Option<Element<'_, Message>> {
        let s = self.snapshot.as_ref()?;
        let mut lines: Vec<Element<_>> = Vec::new();
        if s.precaching == Some(true) {
            lines.push(text("Steam Shader Pre-Caching is on: it downloads shaders and video you don't need. Turn it off in Steam > Settings > Downloads.").color(WARN).into());
        }
        if !s.layer_installed {
            lines.push(text("Recording layer not installed: enable it in Settings.").color(WARN).into());
        }
        (!lines.is_empty()).then(|| Column::with_children(lines).spacing(4).into())
    }

    fn job_panel(&self) -> Option<Element<'_, Message>> {
        let j = self.job.as_ref()?;
        let label = if j.name.is_empty() {
            "Preparing replay…".to_string()
        } else {
            format!("Replaying {} ({}/{}): {} / {} pipelines", j.name, j.index + 1, j.count, j.done, j.total)
        };
        let bar = progress_bar(0.0..=j.total.max(1) as f32, j.done as f32).height(10);
        Some(container(column![text(label), bar].spacing(6)).padding(10).style(card).into())
    }

    fn dialog_panel(&self) -> Option<Element<'_, Message>> {
        let body: Element<_> = match self.dialog.as_ref()? {
            Dialog::Purge { name, recordings, backup, preview, .. } => {
                let summary = match preview {
                    Some((n, bytes)) => format!("{n} item(s), {}", model::size(*bytes)),
                    None => "measuring…".into(),
                };
                let verb = if *backup { "Move to backup" } else { "Delete" };
                column![
                    text(format!("Clear the shader cache of {name}?")).font(BOLD),
                    text(format!("Driver caches (Mesa, NVIDIA, DXVK, vkd3d-proton): {summary}")).color(MUTED),
                    checkbox("Also delete shaderwarm's recordings (they rebuild the cache)", *recordings)
                        .on_toggle(Message::PurgeRecordings),
                    checkbox("Keep a backup so it can be restored (for cold/warm testing)", *backup)
                        .on_toggle(Message::PurgeBackup),
                    row![
                        button(text(verb)).style(button::danger).on_press_maybe(preview.map(|_| Message::PurgeConfirmed)),
                        button(text("Cancel")).style(button::secondary).on_press(Message::CloseDialog),
                    ]
                    .spacing(10),
                ]
                .spacing(8)
                .into()
            }
            Dialog::SelfCompiling { appid, name, reason } => column![
                text(format!("{name} compiles its own shaders")).font(BOLD),
                text(format!("{reason}. Replaying is probably unnecessary.")).color(MUTED),
                row![
                    button(text("Replay anyway")).style(button::primary).on_press(Message::ReplayConfirmed(*appid)),
                    button(text("Cancel")).style(button::secondary).on_press(Message::CloseDialog),
                ]
                .spacing(10),
            ]
            .spacing(8)
            .into(),
            Dialog::Video { name, bytes, .. } => column![
                text(format!("Delete Steam's transcoded video for {name}?")).font(BOLD),
                text(format!("{} that Proton-CachyOS/GE don't need.", model::size(*bytes))).color(MUTED),
                row![
                    button(text("Delete")).style(button::danger).on_press(Message::VideoConfirmed),
                    button(text("Cancel")).style(button::secondary).on_press(Message::CloseDialog),
                ]
                .spacing(10),
            ]
            .spacing(8)
            .into(),
        };
        Some(container(body).padding(14).width(Length::Fill).style(container::bordered_box).into())
    }

    fn games(&self) -> Element<'_, Message> {
        let Some(s) = &self.snapshot else {
            return text("Reading the Steam library…").color(MUTED).into();
        };
        let stale = s.rows.iter().filter(|r| r.staleness.is_some_and(Staleness::needs_replay)).count();
        let toolbar = row![
            text_input("Filter games", &self.filter).on_input(Message::Filter).width(260),
            horizontal_space(),
            button(text("Copy launch option")).style(button::secondary).on_press(Message::CopyLaunchOption),
            button(text(format!("Replay stale ({stale})")))
                .style(button::primary)
                .on_press_maybe((stale > 0 && !self.busy()).then_some(Message::ReplayStale)),
        ]
        .spacing(10)
        .align_y(Alignment::Center);

        let needle = self.filter.to_lowercase();
        let rows = s
            .rows
            .iter()
            .filter(|r| needle.is_empty() || r.game.name.to_lowercase().contains(&needle))
            .map(|r| self.game_row(r));
        column![toolbar, scrollable(Column::with_children(rows).spacing(8).padding([0, 12])).height(Length::Fill)]
            .spacing(10)
            .into()
    }

    fn game_row<'a>(&self, r: &'a Row) -> Element<'a, Message> {
        let (status, color) = if r.verdict.self_compiles {
            (format!("Compiles its own shaders: shaderwarm probably unnecessary ({})", r.verdict.reason()), WARN)
        } else {
            match r.staleness {
                Some(Staleness::Fresh) => ("Cache ready".to_string(), GOOD),
                Some(Staleness::NoInputs) => ("Nothing recorded yet: play it once with the launch option".to_string(), MUTED),
                Some(s) => (format!("Needs replay ({})", s.describe()), WARN),
                None => ("No GPU found".to_string(), BAD),
            }
        };
        let launched = match r.last_launch {
            Some(secs) => format!("last recorded launch {}", age(secs)),
            None => "never launched through shaderwarm".into(),
        };
        let info = column![
            text(&r.game.name).font(BOLD),
            text(format!("{} · {} · shaders {} · {launched}", r.engine, r.proton, model::size(r.shaders))).size(13).color(MUTED),
            text(status).size(13).color(color),
        ]
        .spacing(3)
        .width(Length::Fill);

        let idle = !self.busy();
        let can_replay = idle && r.staleness.is_some_and(|s| s != Staleness::NoInputs);
        let mut actions = row![
            button(text("Replay")).style(button::primary).on_press_maybe(can_replay.then_some(Message::Replay(r.game.appid))),
            button(text("Clear cache…")).style(button::secondary).on_press_maybe(idle.then_some(Message::OpenPurge(r.game.appid))),
        ]
        .spacing(8);
        if r.backups > 0 {
            actions = actions.push(
                button(text("Restore backup")).style(button::secondary).on_press_maybe(idle.then_some(Message::Restore(r.game.appid))),
            );
        }
        if r.video > 1 << 20 {
            actions = actions.push(
                button(text(format!("Delete video ({})", model::size(r.video))))
                    .style(button::secondary)
                    .on_press_maybe(idle.then_some(Message::OpenVideo(r.game.appid))),
            );
        }
        container(row![info, actions].spacing(12).align_y(Alignment::Center))
            .padding(12)
            .width(Length::Fill)
            .style(card)
            .into()
    }

    fn settings_page(&self) -> Element<'_, Message> {
        let auto = self.snapshot.as_ref().is_some_and(|s| s.auto_replay);
        let layer = self.snapshot.as_ref().is_some_and(|s| s.layer_installed);
        column![
            text("Recording and auto-replay").font(BOLD),
            toggler(auto)
                .label("Replay stale caches automatically after GPU driver or Proton updates (systemd user units)")
                .on_toggle_maybe((!self.busy()).then_some(Message::AutoReplay)),
            text(if layer { "Recording layer installed." } else { "Recording layer not installed: turning auto-replay on installs it." })
                .size(13)
                .color(MUTED),
            text(format!("Steam launch option for each game: {}", model::LAUNCH_OPTION)).size(13).color(MUTED),
            text("Replay").font(BOLD),
            row![
                text("Compile threads"),
                text_input("all cores", &self.threads_input).on_input(Message::Threads).width(120),
                text(format!("using {}", self.settings.effective_threads())).color(MUTED),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
            checkbox("Include games that compile their own shaders in \"Replay stale\" and auto-replay", self.settings.include_self_compiling)
                .on_toggle(Message::IncludeSelfCompiling),
        ]
        .spacing(12)
        .into()
    }
}

fn load() -> Task<Message> {
    Task::perform(model::blocking(model::load), Message::Loaded)
}

fn preview(appid: u32, recordings: bool) -> Task<Message> {
    Task::perform(model::blocking(move || model::purge_preview(appid, recordings)), Message::PurgePreview)
}

fn action(f: impl FnOnce() -> Result<String, String> + Send + 'static) -> Task<Message> {
    Task::perform(model::blocking(f), Message::ActionDone)
}

fn age(secs: u64) -> String {
    match secs {
        s if s < 120 => "just now".into(),
        s if s < 7200 => format!("{} min ago", s / 60),
        s if s < 172_800 => format!("{} h ago", s / 3600),
        s => format!("{} days ago", s / 86_400),
    }
}
