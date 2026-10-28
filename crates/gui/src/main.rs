//! shaderwarm desktop UI: a cover library of your Steam games with cache status, a
//! detail panel per game, replay, cleanup and settings.

mod model;
mod style;

use iced::alignment::{Horizontal, Vertical};
use iced::font::{self, Font};
use iced::widget::{
    button, checkbox, column, container, horizontal_space, image, progress_bar, responsive, row, scrollable, stack,
    svg, text, text_input, toggler, Column, Row as WRow, Space,
};
use iced::{window, Alignment, Color, ContentFit, Element, Length, Size, Task, Theme};

use model::{JobEvent, Row, Snapshot};
use shaderwarm_core::settings::Settings;
use shaderwarm_core::state::Staleness;
use style::{ACCENT, GOLD, GOOD, MUTED, ROSE, SKY, TEXT, VIOLET, WARN};

const BOLD: Font = Font { weight: font::Weight::Bold, ..Font::DEFAULT };
const ICON_SVG: &[u8] = include_bytes!("../../../assets/shaderwarm.svg");
const ICON_PNG: &[u8] = include_bytes!("../../../assets/shaderwarm-256.png");
const TILE_W: f32 = 156.0;
const TILE_H: f32 = 234.0;
const GAP: f32 = 18.0;
const PANEL_W: f32 = 440.0;

pub fn main() -> iced::Result {
    let window = window::Settings {
        size: Size::new(1320.0, 840.0),
        min_size: Some(Size::new(980.0, 640.0)),
        icon: window::icon::from_file_data(ICON_PNG, None).ok(),
        // Matches shaderwarm.desktop so the taskbar shows the right icon on Wayland.
        platform_specific: window::settings::PlatformSpecific { application_id: "shaderwarm".into(), ..Default::default() },
        ..window::Settings::default()
    };
    iced::application("shaderwarm", App::update, App::view)
        .theme(|_| style::theme())
        .window(window)
        .run_with(App::new)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Library,
    Settings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Filter {
    All,
    NeedsReplay,
    Ready,
    NotRecorded,
}

/// What a game's tile and panel say about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Ready,
    NeedsReplay,
    NotRecorded,
    SelfCompiles,
    NoGpu,
}

impl Status {
    fn of(r: &Row) -> Self {
        if r.verdict.self_compiles {
            return Status::SelfCompiles;
        }
        match r.staleness {
            Some(Staleness::Fresh) => Status::Ready,
            Some(Staleness::NoInputs) => Status::NotRecorded,
            Some(_) => Status::NeedsReplay,
            None => Status::NoGpu,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Status::Ready => "READY",
            Status::NeedsReplay => "NEEDS REPLAY",
            Status::NotRecorded => "NOT RECORDED",
            Status::SelfCompiles => "SELF-COMPILES",
            Status::NoGpu => "NO GPU",
        }
    }

    fn color(self) -> Color {
        match self {
            Status::Ready => GOOD,
            Status::NeedsReplay => WARN,
            Status::NotRecorded => MUTED,
            Status::SelfCompiles => VIOLET,
            Status::NoGpu => ROSE,
        }
    }
}

#[derive(Debug, Clone)]
enum Dialog {
    Purge { appid: u32, name: String, recordings: bool, backup: bool, preview: Option<(usize, u64)> },
    SelfCompiling { appid: u32, name: String, reason: String },
    /// `appid: None` cleans every game.
    Video { appid: Option<u32>, name: String, bytes: u64 },
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
    search: String,
    filter: Filter,
    selected: Option<u32>,
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
    Search(String),
    Filter(Filter),
    Select(u32),
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
    OpenVideo(Option<u32>),
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
            tab: Tab::Library,
            search: String::new(),
            filter: Filter::All,
            selected: None,
            job: None,
            log: Vec::new(),
            dialog: None,
            threads_input: settings.threads.map(|n| n.to_string()).unwrap_or_default(),
            settings,
        };
        (app, load())
    }

    fn rows(&self) -> &[Row] {
        self.snapshot.as_ref().map_or(&[], |s| &s.rows)
    }

    fn row(&self, appid: u32) -> Option<&Row> {
        self.rows().iter().find(|r| r.game.appid == appid)
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

    fn visible(&self) -> Vec<&Row> {
        let needle = self.search.to_lowercase();
        self.rows()
            .iter()
            .filter(|r| needle.is_empty() || r.game.name.to_lowercase().contains(&needle))
            .filter(|r| match self.filter {
                Filter::All => true,
                Filter::NeedsReplay => Status::of(r) == Status::NeedsReplay,
                Filter::Ready => Status::of(r) == Status::Ready,
                Filter::NotRecorded => Status::of(r) == Status::NotRecorded,
            })
            .collect()
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
                        let keep = self.selected.filter(|id| s.rows.iter().any(|r| r.game.appid == *id));
                        self.selected = keep.or_else(|| s.rows.first().map(|r| r.game.appid));
                        self.snapshot = Some(s);
                        self.load_error = None;
                    }
                    Err(e) => self.load_error = Some(e),
                }
            }
            Message::Tab(tab) => self.tab = tab,
            Message::Search(s) => self.search = s,
            Message::Filter(f) => self.filter = f,
            Message::Select(appid) => self.selected = Some(appid),
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
                let ids = self
                    .rows()
                    .iter()
                    .filter(|r| r.staleness.is_some_and(Staleness::needs_replay))
                    .filter(|r| include || !r.verdict.self_compiles)
                    .map(|r| r.game.appid)
                    .collect();
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
                let (name, bytes) = match appid {
                    Some(id) => self.row(id).map_or((String::new(), 0), |r| (r.game.name.clone(), r.video)),
                    None => ("all games".into(), self.rows().iter().map(|r| r.video).sum()),
                };
                self.dialog = Some(Dialog::Video { appid, name, bytes });
            }
            Message::VideoConfirmed => {
                if let Some(Dialog::Video { appid, .. }) = self.dialog.take() {
                    return match appid {
                        Some(id) => action(move || model::clean_video(id)),
                        None => action(model::clean_all_video),
                    };
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

    // ---- view ---------------------------------------------------------------

    fn view(&self) -> Element<'_, Message> {
        let mut page = column![self.header()].spacing(18);
        if let Some(e) = &self.load_error {
            page = page.push(text(format!("Could not read the Steam library: {e}")).color(ROSE));
        }
        page = page
            .push_maybe(self.warnings())
            .push_maybe(self.job_bar())
            .push_maybe(self.dialog_panel())
            .push(match self.tab {
                Tab::Library => self.library(),
                Tab::Settings => self.settings_page(),
            })
            .push_maybe(self.toasts());
        container(page).padding([20, 24]).width(Length::Fill).height(Length::Fill).into()
    }

    fn header(&self) -> Element<'_, Message> {
        let icon = svg(svg::Handle::from_memory(ICON_SVG)).width(44).height(44);
        let gpu = self.snapshot.as_ref().and_then(|s| s.gpu.clone()).unwrap_or_else(|| "Detecting GPU…".into());
        let title = column![
            text("shaderwarm").size(26).font(BOLD),
            text(gpu).size(13).color(MUTED),
        ]
        .spacing(2);
        let tab = |label, t| button(text(label)).padding([8, 18]).style(style::toggle(self.tab == t)).on_press(Message::Tab(t));
        row![
            icon,
            title,
            horizontal_space(),
            tab("Library", Tab::Library),
            tab("Settings", Tab::Settings),
            button(text(if self.loading { "Refreshing…" } else { "Refresh" }))
                .padding([8, 16])
                .style(style::secondary)
                .on_press_maybe((!self.loading).then_some(Message::Refresh)),
        ]
        .spacing(14)
        .align_y(Alignment::Center)
        .into()
    }

    fn stats(&self) -> Element<'_, Message> {
        let rows = self.rows();
        let count = |s: Status| rows.iter().filter(|r| Status::of(r) == s).count();
        let shaders: u64 = rows.iter().map(|r| r.shaders).sum();
        let video: u64 = rows.iter().map(|r| r.video).sum();
        let stat = |value: String, label: &'static str, color: Color, extra: Option<Element<'static, Message>>| {
            let body = column![text(value).size(28).font(BOLD).color(color), text(label).size(13).color(MUTED)].spacing(2);
            let mut line = row![body, horizontal_space()].align_y(Alignment::Center);
            if let Some(e) = extra {
                line = line.push(e);
            }
            container(line).padding([14, 18]).width(Length::Fill).style(style::stat_card)
        };
        let free = (video > 1 << 20).then(|| {
            button(text("Free").size(13))
                .padding([6, 14])
                .style(style::primary)
                .on_press_maybe((!self.busy()).then_some(Message::OpenVideo(None)))
                .into()
        });
        row![
            stat(format!("{}/{}", count(Status::Ready), rows.len()), "games ready", GOOD, None),
            stat(count(Status::NeedsReplay).to_string(), "need replay", WARN, None),
            stat(model::size(shaders), "shader caches", SKY, None),
            stat(model::size(video), "Steam video you can free", ACCENT, free),
        ]
        .spacing(14)
        .into()
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
        (!lines.is_empty()).then(|| container(Column::with_children(lines).spacing(4)).padding([10, 16]).width(Length::Fill).style(style::card).into())
    }

    fn job_bar(&self) -> Option<Element<'_, Message>> {
        let j = self.job.as_ref()?;
        let pct = if j.total > 0 { j.done as f32 * 100.0 / j.total as f32 } else { 0.0 };
        let label = if j.name.is_empty() {
            "Preparing replay…".to_string()
        } else {
            format!("Warming {}  ·  game {}/{}  ·  {} / {} pipelines", j.name, j.index + 1, j.count, j.done, j.total)
        };
        let bar = progress_bar(0.0..=100.0, pct).height(8).style(|_: &Theme| progress_bar::Style {
            background: style::SURFACE_HI.into(),
            bar: style::warm_gradient(1.0),
            border: iced::Border { radius: 4.0.into(), ..Default::default() },
        });
        let head = row![text(label).size(14), horizontal_space(), text(format!("{pct:.0}%")).font(BOLD).color(ACCENT)];
        Some(container(column![head, bar].spacing(8)).padding([12, 16]).width(Length::Fill).style(style::stat_card).into())
    }

    fn toasts(&self) -> Option<Element<'_, Message>> {
        (!self.log.is_empty()).then(|| {
            let lines = self.log.iter().rev().take(2).map(|(ok, m)| {
                row![text(if *ok { "●" } else { "▲" }).color(if *ok { GOOD } else { ROSE }), text(m.clone()).size(13)]
                    .spacing(8)
                    .into()
            });
            Column::with_children(lines).spacing(4).into()
        })
    }

    fn dialog_panel(&self) -> Option<Element<'_, Message>> {
        let cancel = || button(text("Cancel")).padding([8, 16]).style(style::secondary).on_press(Message::CloseDialog);
        let body: Element<_> = match self.dialog.as_ref()? {
            Dialog::Purge { name, recordings, backup, preview, .. } => {
                let summary = match preview {
                    Some((n, bytes)) => format!("{n} item(s), {}", model::size(*bytes)),
                    None => "measuring…".into(),
                };
                let verb = if *backup { "Move to backup" } else { "Delete" };
                column![
                    text(format!("Clear the shader cache of {name}?")).size(18).font(BOLD),
                    text(format!("Driver caches (Mesa, NVIDIA, DXVK, vkd3d-proton): {summary}")).color(MUTED),
                    checkbox("Also delete shaderwarm's recordings (they rebuild the cache)", *recordings)
                        .on_toggle(Message::PurgeRecordings),
                    checkbox("Keep a backup so it can be restored (for cold/warm testing)", *backup)
                        .on_toggle(Message::PurgeBackup),
                    row![
                        button(text(verb)).padding([8, 16]).style(style::danger).on_press_maybe(preview.map(|_| Message::PurgeConfirmed)),
                        cancel(),
                    ]
                    .spacing(10),
                ]
                .spacing(10)
                .into()
            }
            Dialog::SelfCompiling { appid, name, reason } => column![
                text(format!("{name} compiles its own shaders")).size(18).font(BOLD),
                text(format!("{reason}. Replaying is probably unnecessary.")).color(MUTED),
                row![
                    button(text("Replay anyway")).padding([8, 16]).style(style::primary).on_press(Message::ReplayConfirmed(*appid)),
                    cancel(),
                ]
                .spacing(10),
            ]
            .spacing(10)
            .into(),
            Dialog::Video { name, bytes, .. } => column![
                text(format!("Delete Steam's transcoded video for {name}?")).size(18).font(BOLD),
                text(format!("{} that Proton-CachyOS/GE don't need.", model::size(*bytes))).color(MUTED),
                row![
                    button(text("Delete")).padding([8, 16]).style(style::danger).on_press(Message::VideoConfirmed),
                    cancel(),
                ]
                .spacing(10),
            ]
            .spacing(10)
            .into(),
        };
        Some(container(body).padding(20).width(Length::Fill).style(style::dialog).into())
    }

    fn library(&self) -> Element<'_, Message> {
        if self.snapshot.is_none() {
            return container(text("Reading your Steam library…").color(MUTED)).center(Length::Fill).into();
        }
        let chip = |label: &'static str, f: Filter| {
            button(text(label).size(13)).padding([6, 14]).style(style::toggle(self.filter == f)).on_press(Message::Filter(f))
        };
        let stale = self.rows().iter().filter(|r| Status::of(r) == Status::NeedsReplay).count();
        let toolbar = row![
            text_input("Search games", &self.search).on_input(Message::Search).padding([8, 12]).width(240),
            chip("All", Filter::All),
            chip("Needs replay", Filter::NeedsReplay),
            chip("Ready", Filter::Ready),
            chip("Not recorded", Filter::NotRecorded),
            horizontal_space(),
            button(text("Copy launch option")).padding([8, 14]).style(style::secondary).on_press(Message::CopyLaunchOption),
            button(text(format!("Warm all stale ({stale})")))
                .padding([8, 16])
                .style(style::primary)
                .on_press_maybe((stale > 0 && !self.busy()).then_some(Message::ReplayStale)),
        ]
        .spacing(10)
        .align_y(Alignment::Center);

        let visible = self.visible();
        let grid = responsive(move |size| {
            // Tiles are TILE_W plus 12 of button padding; 12 more is left for the scrollbar.
            let usable = size.width - 12.0;
            let cell = TILE_W + 12.0;
            let cols = (((usable + GAP) / (cell + GAP)).floor() as usize).max(1);
            // Spread leftover width into the gaps so the grid fills its column.
            let gap = if cols > 1 { ((usable - cols as f32 * cell) / (cols - 1) as f32).clamp(GAP, 3.0 * GAP) } else { GAP };
            let rows = visible.chunks(cols).map(|chunk| {
                WRow::with_children(chunk.iter().map(|r| self.tile(r))).spacing(gap).into()
            });
            let content: Element<_> = if visible.is_empty() {
                container(text("No games match.").color(MUTED)).padding(40).into()
            } else {
                Column::with_children(rows).spacing(GAP).padding(iced::Padding { top: 4.0, right: 12.0, bottom: 4.0, left: 0.0 }).into()
            };
            scrollable(content).height(Length::Fill).into()
        });

        let detail = self.selected.and_then(|id| self.row(id)).map(|r| self.detail(r));
        let body = row![container(grid).width(Length::Fill).height(Length::Fill)]
            .push_maybe(detail)
            .spacing(20)
            .height(Length::Fill);
        column![self.stats(), toolbar, body].spacing(16).into()
    }

    fn cover(&self, r: &Row, width: f32, height: f32) -> Element<'_, Message> {
        match r.art.cover.as_ref().or(r.art.header.as_ref()) {
            Some(path) => image(image::Handle::from_path(path))
                .width(width)
                .height(height)
                .content_fit(ContentFit::Cover)
                .into(),
            None => container(text(r.game.name.clone()).size(16).font(BOLD).align_x(Horizontal::Center))
                .padding(12)
                .width(width)
                .height(height)
                .center(Length::Fixed(width))
                .style(style::cover_placeholder)
                .into(),
        }
    }

    fn tile<'a>(&'a self, r: &'a Row) -> Element<'a, Message> {
        let status = Status::of(r);
        let badge = container(text(status.label()).size(10).font(BOLD))
            .padding([3, 9])
            .style(style::pill(status.color()));
        let overlay = container(badge)
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(8)
            .align_x(Horizontal::Center)
            .align_y(Vertical::Bottom);
        let art = stack![self.cover(r, TILE_W, TILE_H), overlay];
        let name = container(text(r.game.name.clone()).size(13).color(TEXT).align_x(Horizontal::Center))
            .width(TILE_W)
            .height(34)
            .align_x(Horizontal::Center);
        let selected = self.selected == Some(r.game.appid);
        button(column![art, name].spacing(8).width(TILE_W))
            .padding(6)
            .style(style::tile(selected))
            .on_press(Message::Select(r.game.appid))
            .into()
    }

    fn detail<'a>(&'a self, r: &'a Row) -> Element<'a, Message> {
        const HERO_H: f32 = 190.0;
        let status = Status::of(r);

        // Hero banner with the game's logo laid over a fade into the panel.
        let hero: Element<_> = match &r.art.hero {
            Some(p) => image(image::Handle::from_path(p)).width(Length::Fill).height(HERO_H).content_fit(ContentFit::Cover).into(),
            None => container(Space::new(Length::Fill, HERO_H)).style(style::cover_placeholder).into(),
        };
        let logo: Element<_> = match &r.art.logo {
            Some(p) => image(image::Handle::from_path(p)).height(86).content_fit(ContentFit::Contain).into(),
            None => text(r.game.name.clone()).size(24).font(BOLD).into(),
        };
        let banner = stack![
            hero,
            container(Space::new(Length::Fill, HERO_H)).style(style::hero_fade),
            container(logo).width(Length::Fill).height(HERO_H).padding(14).align_x(Horizontal::Center).align_y(Vertical::Bottom),
        ];

        let chip = |label: String| container(text(label).size(12)).padding([4, 10]).style(style::chip);
        let chips = row![chip(r.engine.clone()), chip(r.proton.clone()), chip(format!("app {}", r.game.appid))].spacing(6);

        let (headline, explain) = match status {
            Status::Ready => ("Cache is warm", "Pipelines you've played are compiled for this driver.".to_string()),
            Status::NeedsReplay => (
                "Needs a replay",
                r.staleness.map_or_else(String::new, |s| format!("{}.", capitalize(s.describe()))),
            ),
            Status::NotRecorded => ("Nothing recorded yet", "Play once with the launch option to record its pipelines.".into()),
            Status::SelfCompiles => ("Compiles its own shaders", format!("{}. shaderwarm is probably unnecessary.", r.verdict.reason())),
            Status::NoGpu => ("No GPU found", "Vulkan reported no hardware device.".into()),
        };
        let status_block = row![
            container(Space::new(4, 40)).style(style::swatch(status.color())),
            column![text(headline).size(18).font(BOLD).color(status.color()), text(explain).size(13).color(MUTED)].spacing(2),
        ]
        .spacing(12)
        .align_y(Alignment::Center);

        let b = &r.breakdown;
        let parts = [
            ("Mesa", b.mesa, GOOD),
            ("Fossilize", b.fossilize, SKY),
            ("DXVK", b.dxvk, VIOLET),
            ("vkd3d", b.vkd3d, GOLD),
            ("NVIDIA/AMDVLK", b.nvidia + b.amdvlk, ACCENT),
            ("Video", b.video, ROSE),
        ];
        let total: u64 = parts.iter().map(|p| p.1).sum();
        let bar: Element<_> = if total == 0 {
            container(Space::new(Length::Fill, 10)).style(style::swatch(style::SURFACE_HI)).into()
        } else {
            WRow::with_children(parts.iter().filter(|p| p.1 > 0).map(|(_, size, color)| {
                let portion = ((size * 1000 / total).max(4)) as u16;
                container(Space::new(Length::Fill, 10)).width(Length::FillPortion(portion)).style(style::swatch(*color)).into()
            }))
            .spacing(2)
            .into()
        };
        let legend = WRow::with_children(parts.iter().filter(|p| p.1 > 0).map(|(name, size, color)| {
            row![
                container(Space::new(10, 10)).style(style::swatch(*color)),
                text(format!("{name} {}", model::size(*size))).size(12).color(MUTED),
            ]
            .spacing(6)
            .align_y(Alignment::Center)
            .into()
        }))
        .spacing(12);
        let cache_block = column![
            row![text("Shader cache").font(BOLD), horizontal_space(), text(model::size(total)).color(MUTED)],
            bar,
            legend,
        ]
        .spacing(8);

        let fact = |k: &'static str, v: String| row![text(k).size(13).color(MUTED).width(130), text(v).size(13)];
        let facts = column![
            fact("Recordings", if r.recordings > 0 { model::size(r.recordings) } else { "none".into() }),
            fact("Last recorded", r.last_launch.map_or_else(|| "never through shaderwarm".into(), age)),
            fact("Backups", r.backups.to_string()),
        ]
        .spacing(6);

        let idle = !self.busy();
        let can_replay = idle && r.staleness.is_some_and(|s| s != Staleness::NoInputs);
        let mut secondary = row![
            button(text("Clear cache…")).padding([8, 14]).style(style::secondary).on_press_maybe(idle.then_some(Message::OpenPurge(r.game.appid))),
        ]
        .spacing(8);
        if r.backups > 0 {
            secondary = secondary.push(
                button(text("Restore")).padding([8, 14]).style(style::secondary).on_press_maybe(idle.then_some(Message::Restore(r.game.appid))),
            );
        }
        if r.video > 1 << 20 {
            secondary = secondary.push(
                button(text(format!("Free video {}", model::size(r.video))))
                    .padding([8, 14])
                    .style(style::secondary)
                    .on_press_maybe(idle.then_some(Message::OpenVideo(Some(r.game.appid)))),
            );
        }
        let warm = button(container(text(if status == Status::Ready { "Re-warm cache" } else { "Warm cache now" }).font(BOLD)).center_x(Length::Fill))
            .padding([12, 16])
            .width(Length::Fill)
            .style(style::primary)
            .on_press_maybe(can_replay.then_some(Message::Replay(r.game.appid)));

        let mut info = column![
            column![text(r.game.name.clone()).size(22).font(BOLD), chips].spacing(8),
            status_block,
            container(cache_block).padding(14).style(style::card_hi),
            facts,
            warm,
            secondary,
        ]
        .spacing(16);
        if r.last_launch.is_none() && status != Status::SelfCompiles {
            info = info.push(
                container(
                    column![
                        text("Record it: set this Steam launch option").size(13).color(MUTED),
                        row![
                            text(model::LAUNCH_OPTION).font(Font::MONOSPACE).color(GOLD),
                            horizontal_space(),
                            button(text("Copy").size(12)).padding([4, 10]).style(style::secondary).on_press(Message::CopyLaunchOption),
                        ]
                        .align_y(Alignment::Center),
                    ]
                    .spacing(6),
                )
                .padding(12)
                .style(style::card_hi),
            );
        }
        let panel = column![banner, scrollable(container(info).padding(iced::Padding { top: 4.0, right: 20.0, bottom: 20.0, left: 20.0 })).height(Length::Fill)];
        container(panel).width(PANEL_W).height(Length::Fill).style(style::card).clip(true).into()
    }

    fn settings_page(&self) -> Element<'_, Message> {
        let auto = self.snapshot.as_ref().is_some_and(|s| s.auto_replay);
        let layer = self.snapshot.as_ref().is_some_and(|s| s.layer_installed);
        let recording = column![
            text("Recording and auto-replay").size(18).font(BOLD),
            toggler(auto)
                .label("Warm stale caches automatically after GPU driver or Proton updates")
                .on_toggle_maybe((!self.busy()).then_some(Message::AutoReplay)),
            text(if layer { "Recording layer installed." } else { "Recording layer not installed: turning auto-replay on installs it." })
                .size(13)
                .color(MUTED),
            text(format!("Launch option for each game: {}", model::LAUNCH_OPTION)).size(13).color(MUTED),
        ]
        .spacing(12);
        let replay = column![
            text("Replay").size(18).font(BOLD),
            row![
                text("Compile threads"),
                text_input("all cores", &self.threads_input).on_input(Message::Threads).width(120).padding([6, 10]),
                text(format!("using {}", self.settings.effective_threads())).color(MUTED),
            ]
            .spacing(12)
            .align_y(Alignment::Center),
            checkbox("Include games that compile their own shaders in \"Warm all stale\" and auto-replay", self.settings.include_self_compiling)
                .on_toggle(Message::IncludeSelfCompiling),
        ]
        .spacing(12);
        let section = |body| container(body).padding(20).width(Length::Fill).style(style::card);
        scrollable(column![section(recording), section(replay)].spacing(16).max_width(900))
            .height(Length::Fill)
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

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

fn age(secs: u64) -> String {
    match secs {
        s if s < 120 => "just now".into(),
        s if s < 7200 => format!("{} min ago", s / 60),
        s if s < 172_800 => format!("{} h ago", s / 3600),
        s => format!("{} days ago", s / 86_400),
    }
}
