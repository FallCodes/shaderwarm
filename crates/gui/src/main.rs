//! shaderwarm desktop UI: a cover library of your Steam games with cache status, a
//! detail panel per game, replay, cleanup and settings. Slanted red/black/paper look
//! with snappy, interruptible transitions (all skippable via "Reduce motion").

mod anim;
mod model;
mod shapes;
mod style;

use std::cell::Cell;
use std::collections::HashMap;
use std::time::{Duration, Instant};

use iced::alignment::{Horizontal, Vertical};
use iced::keyboard::{self, key::Named, Key};
use iced::widget::{
    canvas, checkbox, column, container, horizontal_space, image, mouse_area, responsive, row, scrollable, stack,
    svg, text, text_input, toggler, Column, Row as WRow, Space,
};
use iced::{event, mouse, window, Alignment, Color, ContentFit, Element, Event, Length, Padding, Size, Subscription, Task};

use anim::Fade;
use model::{JobEvent, Row, Snapshot};
use shaderwarm_core::settings::Settings;
use shaderwarm_core::state::Staleness;
use shapes::{Kind, Slant, DISPLAY};
use style::{alpha, ACCENT, ALERT, BLACK, GRAPHITE, GREY, INK, PAPER, SMOKE};

const ICON_SVG: &[u8] = include_bytes!("../../../assets/shaderwarm.svg");
const ICON_PNG: &[u8] = include_bytes!("../../../assets/shaderwarm-256.png");
const ANTON: &[u8] = include_bytes!("../../../assets/fonts/Anton-Regular.ttf");
const TILE_W: f32 = 150.0;
const TILE_H: f32 = 225.0;
const INSET: f32 = 12.0;
const GAP: f32 = 10.0;
const PANEL_W: f32 = 450.0;
const HERO_H: f32 = 200.0;
// Motion tokens (ms).
const STAGGER: u64 = 35;
const TILE_IN: u64 = 260;
const PANEL_IN: u64 = 380;
const WIPE: u64 = 520;
const COUNT_UP: u64 = 650;
const TAB_WIPE: u64 = 560;

pub fn main() -> iced::Result {
    let window = window::Settings {
        size: Size::new(1360.0, 880.0),
        min_size: Some(Size::new(1040.0, 680.0)),
        icon: window::icon::from_file_data(ICON_PNG, None).ok(),
        // Matches shaderwarm.desktop so the taskbar shows the right icon on Wayland.
        platform_specific: window::settings::PlatformSpecific { application_id: "shaderwarm".into(), ..Default::default() },
        ..window::Settings::default()
    };
    iced::application("shaderwarm", App::update, App::view)
        .theme(|_| style::theme())
        // MSAA: without it the slanted canvas edges are visibly stair-stepped.
        .antialiasing(true)
        .font(ANTON)
        .subscription(App::subscription)
        .window(window)
        .run_with(App::new)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Tab {
    Library,
    Settings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Filter {
    All,
    NeedsReplay,
    Ready,
    NotRecorded,
}

/// Every hoverable thing, so each gets its own interruptible fade.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Hot {
    Tile(u32),
    Tab(Tab),
    Chip(Filter),
    Btn(Btn),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Btn {
    Refresh,
    Copy,
    WarmStale,
    Warm,
    Clear,
    Restore,
    Video,
    FreeAll,
    Confirm,
    Cancel,
    CopyHint,
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
            Status::Ready => "Ready",
            Status::NeedsReplay => "Needs replay",
            Status::NotRecorded => "Not recorded",
            Status::SelfCompiles => "Self-compiles",
            Status::NoGpu => "No GPU",
        }
    }

    /// Fill and text colour of the status tag. Each status also has its own word, so
    /// colour is never the only signal.
    fn colors(self) -> (Color, Color) {
        match self {
            Status::Ready => (PAPER, BLACK),
            Status::NeedsReplay => (ACCENT, BLACK),
            Status::NoGpu => (ALERT, PAPER),
            Status::NotRecorded => (SMOKE, PAPER),
            Status::SelfCompiles => (BLACK, ACCENT),
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
    /// The tab on screen; `tab_target` is where a transition is heading.
    tab: Tab,
    tab_target: Tab,
    tab_since: Instant,
    /// +1 when moving right (to Settings), -1 when moving left.
    tab_dir: f32,
    focused: bool,
    search: String,
    filter: Filter,
    selected: Option<u32>,
    job: Option<JobState>,
    log: Vec<(bool, String)>,
    dialog: Option<Dialog>,
    settings: Settings,
    threads_input: String,
    // Animation state.
    now: Instant,
    booted: Instant,
    hover: HashMap<Hot, Fade>,
    grid_since: Instant,
    panel_since: Instant,
    stats_since: Instant,
    /// Grid columns from the last layout, for arrow-key navigation.
    cols: Cell<usize>,
    backdrop: canvas::Cache,
}

#[derive(Debug, Clone)]
enum Message {
    Tick(Instant),
    Focus(bool),
    Hover(Hot, bool),
    Nav(i32, i32),
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
    ReduceMotion(bool),
    AutoReplay(bool),
}

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

impl App {
    fn new() -> (Self, Task<Message>) {
        let settings = Settings::load();
        let now = Instant::now();
        let app = Self {
            snapshot: None,
            load_error: None,
            loading: true,
            tab: Tab::Library,
            tab_target: Tab::Library,
            tab_since: now,
            tab_dir: 1.0,
            focused: true,
            search: String::new(),
            filter: Filter::All,
            selected: None,
            job: None,
            log: Vec::new(),
            dialog: None,
            threads_input: settings.threads.map(|n| n.to_string()).unwrap_or_default(),
            settings,
            now,
            booted: now,
            hover: HashMap::new(),
            grid_since: now,
            panel_since: now,
            stats_since: now,
            cols: Cell::new(1),
            backdrop: canvas::Cache::new(),
        };
        (app, load())
    }

    // ---- animation helpers ----------------------------------------------------

    fn reduce(&self) -> bool {
        self.settings.reduce_motion
    }

    /// Eased 0..1 progress of a timeline, or 1 with reduced motion.
    fn eased(&self, start: Instant, delay: u64, dur: u64) -> f32 {
        if self.reduce() {
            1.0
        } else {
            anim::ease_out(anim::progress(self.now, start, ms(delay), ms(dur)))
        }
    }

    fn hov(&self, hot: Hot) -> f32 {
        match self.hover.get(&hot) {
            Some(f) if self.reduce() => f32::from(u8::from(f.is_on())),
            Some(f) => f.value(self.now),
            None => 0.0,
        }
    }

    fn tab_progress(&self) -> f32 {
        if self.reduce() {
            1.0
        } else {
            anim::progress(self.now, self.tab_since, Duration::ZERO, ms(TAB_WIPE))
        }
    }

    /// Whether a transition needs every frame. The drifting backdrop alone doesn't: it
    /// ticks at 30 fps (see `subscription`), and not at all when unfocused.
    fn animating(&self) -> bool {
        if self.reduce() {
            return false;
        }
        if self.job.is_some() || self.tab != self.tab_target || self.tab_progress() < 1.0 {
            return true;
        }
        let now = Instant::now();
        let grid_len = self.visible().len() as u64;
        let busy_until = |start: Instant, total: u64| now < start + ms(total);
        self.hover.values().any(|f| !f.settled(now))
            || busy_until(self.grid_since, grid_len * STAGGER + TILE_IN)
            || busy_until(self.panel_since, WIPE.max(PANEL_IN))
            || busy_until(self.stats_since, COUNT_UP)
            || busy_until(self.booted, 700)
    }

    fn subscription(&self) -> Subscription<Message> {
        let keys = keyboard::on_key_press(|key, modifiers| {
            if modifiers.control() || modifiers.alt() {
                return None;
            }
            match key.as_ref() {
                Key::Named(Named::ArrowLeft) => Some(Message::Nav(-1, 0)),
                Key::Named(Named::ArrowRight) => Some(Message::Nav(1, 0)),
                Key::Named(Named::ArrowUp) => Some(Message::Nav(0, -1)),
                Key::Named(Named::ArrowDown) => Some(Message::Nav(0, 1)),
                Key::Named(Named::F5) => Some(Message::Refresh),
                _ => None,
            }
        });
        let focus = event::listen_with(|event, _, _| match event {
            Event::Window(window::Event::Focused) => Some(Message::Focus(true)),
            Event::Window(window::Event::Unfocused) => Some(Message::Focus(false)),
            _ => None,
        });
        if self.animating() {
            Subscription::batch([keys, focus, window::frames().map(Message::Tick)])
        } else if self.focused && !self.reduce() {
            let drift = iced::time::every(ms(33)).map(Message::Tick);
            Subscription::batch([keys, focus, drift])
        } else {
            Subscription::batch([keys, focus])
        }
    }

    // ---- state ----------------------------------------------------------------

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

    fn select(&mut self, appid: u32) {
        if self.selected != Some(appid) {
            self.selected = Some(appid);
            self.panel_since = self.now;
        }
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
        self.now = Instant::now();
        match message {
            Message::Tick(frame) => {
                self.now = frame;
                self.hover.retain(|_, f| f.is_on() || !f.settled(frame));
                // Swap pages while the wipe covers them.
                if self.tab != self.tab_target && self.tab_progress() >= 0.45 {
                    self.tab = self.tab_target;
                    self.grid_since = self.now;
                }
            }
            Message::Focus(on) => self.focused = on,
            Message::Hover(hot, on) => {
                let now = self.now;
                let fade = self.hover.get(&hot).map_or_else(|| Fade::new(on, now), |f| f.retarget(on, now));
                self.hover.insert(hot, fade);
            }
            Message::Nav(dx, dy) => {
                if self.tab_target != Tab::Library || self.dialog.is_some() {
                    return Task::none();
                }
                let ids: Vec<u32> = self.visible().iter().map(|r| r.game.appid).collect();
                if ids.is_empty() {
                    return Task::none();
                }
                let at = self.selected.and_then(|id| ids.iter().position(|&i| i == id)).unwrap_or(0) as i32;
                let cols = self.cols.get().max(1) as i32;
                let next = (at + dx + dy * cols).clamp(0, ids.len() as i32 - 1);
                self.select(ids[next as usize]);
            }
            Message::Refresh => {
                self.loading = true;
                return load();
            }
            Message::Loaded(result) => {
                self.loading = false;
                match result {
                    Ok(s) => {
                        let first = self.snapshot.is_none();
                        let keep = self.selected.filter(|id| s.rows.iter().any(|r| r.game.appid == *id));
                        let pick = keep.or_else(|| s.rows.first().map(|r| r.game.appid));
                        self.snapshot = Some(s);
                        self.load_error = None;
                        self.stats_since = self.now;
                        if first {
                            self.grid_since = self.now;
                        }
                        if let Some(id) = pick {
                            self.select(id);
                        }
                    }
                    Err(e) => self.load_error = Some(e),
                }
            }
            Message::Tab(tab) => {
                if self.tab_target != tab {
                    self.tab_dir = if tab == Tab::Settings { 1.0 } else { -1.0 };
                    self.tab_target = tab;
                    self.tab_since = self.now;
                    if self.reduce() {
                        self.tab = tab;
                        self.grid_since = self.now;
                    }
                }
            }
            Message::Search(s) => {
                self.search = s;
                self.grid_since = self.now;
            }
            Message::Filter(f) => {
                if self.filter != f {
                    self.filter = f;
                    self.grid_since = self.now;
                }
            }
            Message::Select(appid) => self.select(appid),
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
            Message::ReduceMotion(on) => {
                self.settings.reduce_motion = on;
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

    // ---- building blocks --------------------------------------------------------

    /// Slanted canvas button with its own hover fade. Disabled when `on_press` is None.
    fn button(&self, hot: Hot, label: &str, kind: Kind, size: f32, width: Option<Length>, on_press: Option<Message>) -> Element<'static, Message> {
        let height = (size * 2.3).round() + 5.0;
        let width = width.unwrap_or(Length::Fixed(shapes::button_width(label, size)));
        let enabled = on_press.is_some();
        let shape = canvas(Slant { label: label.to_string(), kind, hover: self.hov(hot), enabled, size }).width(width).height(height);
        let mut area = mouse_area(shape).on_enter(Message::Hover(hot, true)).on_exit(Message::Hover(hot, false));
        if let Some(msg) = on_press {
            area = area.on_press(msg).interaction(mouse::Interaction::Pointer);
        }
        area.into()
    }

    fn tag(label: &str, fill: Color, ink: Color, size: f32) -> Element<'static, Message> {
        let height = (size * 2.0).round() + 5.0;
        canvas(Slant { label: label.to_string(), kind: Kind::Tag(fill, ink), hover: 0.0, enabled: true, size })
            .width(shapes::button_width(label, size) - size * 0.6)
            .height(height)
            .into()
    }

    fn heading(label: &str, size: f32) -> iced::widget::Text<'static> {
        text(label.to_uppercase()).size(size).font(DISPLAY).color(PAPER)
    }

    // ---- view -------------------------------------------------------------------

    fn view(&self) -> Element<'_, Message> {
        let mut page = column![self.header()].spacing(16);
        if let Some(e) = &self.load_error {
            page = page.push(text(format!("Could not read the Steam library: {e}")).color(PAPER));
        }
        page = page
            .push_maybe(self.warnings())
            .push_maybe(self.job_bar())
            .push_maybe(self.dialog_panel())
            .push(self.page())
            .push_maybe(self.toasts());
        let content = container(page).padding([18, 24]).width(Length::Fill).height(Length::Fill);
        let phase = if self.reduce() { 0.0 } else { self.now.duration_since(self.booted).as_secs_f32() };
        let backdrop = canvas(shapes::Backdrop { cache: &self.backdrop, phase }).width(Length::Fill).height(Length::Fill);
        stack![backdrop, content].into()
    }

    /// The current tab, with a lateral wipe over it while switching.
    fn page(&self) -> Element<'_, Message> {
        let body = match self.tab {
            Tab::Library => self.library(),
            Tab::Settings => self.settings_page(),
        };
        let t = self.tab_progress();
        if t >= 1.0 {
            return body;
        }
        let wipe = canvas(shapes::Wipe::page(t, self.tab_dir)).width(Length::Fill).height(Length::Fill);
        stack![body, wipe].into()
    }

    fn header(&self) -> Element<'_, Message> {
        let icon = svg(svg::Handle::from_memory(ICON_SVG)).width(52).height(52);
        let intro = self.eased(self.booted, 0, 520);
        let title = canvas(shapes::Title { intro }).width(270).height(62);
        let gpu = self.snapshot.as_ref().and_then(|s| s.gpu.clone()).unwrap_or_else(|| "Detecting GPU…".into());
        let sub = column![text("SHADER CACHE CONTROL").size(14).font(DISPLAY).color(GREY), text(gpu).size(13).color(GREY)].spacing(2);
        let tab = |label: &str, t: Tab| {
            let kind = if self.tab_target == t { Kind::Active } else { Kind::Ghost };
            self.button(Hot::Tab(t), label, kind, 17.0, None, Some(Message::Tab(t)))
        };
        row![
            icon,
            title,
            sub,
            horizontal_space(),
            tab("Library", Tab::Library),
            tab("Settings", Tab::Settings),
            self.button(
                Hot::Btn(Btn::Refresh),
                if self.loading { "Refreshing" } else { "Refresh" },
                Kind::Secondary,
                17.0,
                None,
                (!self.loading).then_some(Message::Refresh),
            ),
        ]
        .spacing(14)
        .align_y(Alignment::Center)
        .into()
    }

    fn stats(&self) -> Element<'_, Message> {
        let rows = self.rows();
        let count = |s: Status| rows.iter().filter(|r| Status::of(r) == s).count();
        let k = self.eased(self.stats_since, 0, COUNT_UP);
        let up = |n: f32| (n * k).round();
        let shaders: u64 = rows.iter().map(|r| r.shaders).sum();
        let video: u64 = rows.iter().map(|r| r.video).sum();
        let card = |value: String, label: &str, color: Color, hot: bool| {
            canvas(shapes::StatCard { value, label: label.to_string(), color, hot }).width(Length::Fill).height(84)
        };
        let free: Element<_> = if video > 1 << 20 {
            let button = self.button(Hot::Btn(Btn::FreeAll), "Free it", Kind::Primary, 15.0, None, (!self.busy()).then_some(Message::OpenVideo(None)));
            stack![
                card(model::size((video as f32 * k) as u64), "Steam video you can free", PAPER, true),
                container(button).width(Length::Fill).height(Length::Fill).align_x(Horizontal::Right).align_y(Vertical::Center).padding([0, 22]),
            ]
            .into()
        } else {
            card("0 B".into(), "Steam video to free", GREY, false).into()
        };
        row![
            card(format!("{}/{}", up(count(Status::Ready) as f32), rows.len()), "Games ready", PAPER, false),
            card(format!("{}", up(count(Status::NeedsReplay) as f32)), "Need replay", if count(Status::NeedsReplay) > 0 { ACCENT } else { GREY }, false),
            card(model::size((shaders as f32 * k) as u64), "Shader caches", PAPER, false),
            free,
        ]
        .spacing(12)
        .into()
    }

    fn warnings(&self) -> Option<Element<'_, Message>> {
        let s = self.snapshot.as_ref()?;
        let mut lines: Vec<Element<_>> = Vec::new();
        if s.precaching == Some(true) {
            lines.push(text("Steam Shader Pre-Caching is on: it downloads shaders and video you don't need. Turn it off in Steam > Settings > Downloads.").into());
        }
        if !s.layer_installed {
            lines.push(text("Recording layer not installed: enable it in Settings.").into());
        }
        (!lines.is_empty()).then(|| {
            container(row![Self::tag("Heads up", ACCENT, BLACK, 14.0), Column::with_children(lines).spacing(4)].spacing(14).align_y(Alignment::Center))
                .padding([10, 16])
                .width(Length::Fill)
                .style(style::alert)
                .into()
        })
    }

    fn job_bar(&self) -> Option<Element<'_, Message>> {
        let j = self.job.as_ref()?;
        let frac = if j.total > 0 { j.done as f32 / j.total as f32 } else { 0.0 };
        let label = if j.name.is_empty() {
            "Preparing replay".to_string()
        } else {
            format!("Warming {}  /  game {} of {}  /  {} of {} pipelines", j.name, j.index + 1, j.count, j.done, j.total)
        };
        let phase = if self.reduce() { 0.0 } else { self.now.duration_since(self.booted).as_secs_f32() * 42.0 };
        let bar = canvas(shapes::Stripes { frac, phase }).width(Length::Fill).height(16);
        let head = row![
            text(label.to_uppercase()).size(16).font(DISPLAY),
            horizontal_space(),
            text(format!("{:.0}%", frac * 100.0)).size(22).font(DISPLAY),
        ]
        .align_y(Alignment::Center);
        Some(container(column![head, bar].spacing(8)).padding([12, 16]).width(Length::Fill).style(style::panel).into())
    }

    fn toasts(&self) -> Option<Element<'_, Message>> {
        (!self.log.is_empty()).then(|| {
            let lines = self.log.iter().rev().take(2).map(|(ok, m)| {
                let tag = if *ok { Self::tag("Done", PAPER, BLACK, 12.0) } else { Self::tag("Error", ALERT, PAPER, 12.0) };
                row![tag, text(m.clone()).size(13)].spacing(10).align_y(Alignment::Center).into()
            });
            Column::with_children(lines).spacing(4).into()
        })
    }

    fn dialog_panel(&self) -> Option<Element<'_, Message>> {
        let cancel = self.button(Hot::Btn(Btn::Cancel), "Cancel", Kind::Secondary, 16.0, None, Some(Message::CloseDialog));
        let confirm = |label: &str, msg: Option<Message>| self.button(Hot::Btn(Btn::Confirm), label, Kind::Primary, 16.0, None, msg);
        let body: Element<_> = match self.dialog.as_ref()? {
            Dialog::Purge { name, recordings, backup, preview, .. } => {
                let summary = match preview {
                    Some((n, bytes)) => format!("{n} item(s), {}", model::size(*bytes)),
                    None => "measuring…".into(),
                };
                let verb = if *backup { "Move to backup" } else { "Delete" };
                column![
                    Self::heading(&format!("Clear the shader cache of {name}?"), 24.0),
                    text(format!("Driver caches (Mesa, NVIDIA, DXVK, vkd3d-proton): {summary}")).color(GREY),
                    checkbox("Also delete shaderwarm's recordings (they rebuild the cache)", *recordings).on_toggle(Message::PurgeRecordings),
                    checkbox("Keep a backup so it can be restored (for cold/warm testing)", *backup).on_toggle(Message::PurgeBackup),
                    row![confirm(verb, preview.map(|_| Message::PurgeConfirmed)), cancel].spacing(12),
                ]
                .spacing(12)
                .into()
            }
            Dialog::SelfCompiling { appid, name, reason } => column![
                Self::heading(&format!("{name} compiles its own shaders"), 24.0),
                text(format!("{reason}. Replaying is probably unnecessary.")).color(GREY),
                row![confirm("Replay anyway", Some(Message::ReplayConfirmed(*appid))), cancel].spacing(12),
            ]
            .spacing(12)
            .into(),
            Dialog::Video { name, bytes, .. } => column![
                Self::heading(&format!("Delete Steam's video for {name}?"), 24.0),
                text(format!("{} of transcoded video that Proton-CachyOS/GE don't need.", model::size(*bytes))).color(GREY),
                row![confirm("Delete", Some(Message::VideoConfirmed)), cancel].spacing(12),
            ]
            .spacing(12)
            .into(),
        };
        Some(container(body).padding(22).width(Length::Fill).style(style::alert).into())
    }

    fn library(&self) -> Element<'_, Message> {
        if self.snapshot.is_none() {
            return container(Self::heading("Reading your Steam library", 28.0).color(GREY)).center(Length::Fill).into();
        }
        let chip = |label: &str, f: Filter| {
            let kind = if self.filter == f { Kind::Active } else { Kind::Ghost };
            self.button(Hot::Chip(f), label, kind, 14.0, None, Some(Message::Filter(f)))
        };
        let stale = self.rows().iter().filter(|r| Status::of(r) == Status::NeedsReplay).count();
        let toolbar = row![
            text_input("Search games", &self.search).on_input(Message::Search).padding([9, 12]).width(230).style(style::search),
            chip("All", Filter::All),
            chip("Needs replay", Filter::NeedsReplay),
            chip("Ready", Filter::Ready),
            chip("Not recorded", Filter::NotRecorded),
            horizontal_space(),
            self.button(Hot::Btn(Btn::Copy), "Copy launch option", Kind::Secondary, 15.0, None, Some(Message::CopyLaunchOption)),
            self.button(
                Hot::Btn(Btn::WarmStale),
                &format!("Warm all stale ({stale})"),
                Kind::Primary,
                15.0,
                None,
                (stale > 0 && !self.busy()).then_some(Message::ReplayStale),
            ),
        ]
        .spacing(10)
        .align_y(Alignment::Center);

        let visible = self.visible();
        let grid = responsive(move |size| {
            let cell = TILE_W + 2.0 * INSET + 6.0;
            let usable = size.width - 14.0;
            let cols = (((usable + GAP) / (cell + GAP)).floor() as usize).max(1);
            self.cols.set(cols);
            // Spread leftover width into the gaps so the grid fills its column.
            let gap = if cols > 1 { ((usable - cols as f32 * cell) / (cols - 1) as f32).clamp(GAP, 4.0 * GAP) } else { GAP };
            let content: Element<_> = if visible.is_empty() {
                container(Self::heading("No games match", 24.0).color(GREY)).padding(40).into()
            } else {
                let rows = visible.chunks(cols).enumerate().map(|(ri, chunk)| {
                    WRow::with_children(chunk.iter().enumerate().map(|(ci, r)| self.tile(r, ri * cols + ci))).spacing(gap).into()
                });
                Column::with_children(rows).spacing(GAP).padding(Padding { top: 6.0, right: 14.0, bottom: 6.0, left: 0.0 }).into()
            };
            scrollable(content).height(Length::Fill).into()
        });

        let detail = self.selected.and_then(|id| self.row(id)).map(|r| self.detail(r));
        let body = row![container(grid).width(Length::Fill).height(Length::Fill)].push_maybe(detail).spacing(18).height(Length::Fill);
        column![self.stats(), toolbar, body].spacing(14).into()
    }

    fn cover(r: &Row, width: f32, height: f32) -> Element<'_, Message> {
        match r.art.cover.as_ref().or(r.art.header.as_ref()) {
            Some(path) => image(image::Handle::from_path(path)).width(width).height(height).content_fit(ContentFit::Cover).into(),
            None => container(text(r.game.name.to_uppercase()).size(22).font(DISPLAY).align_x(Horizontal::Center))
                .padding(12)
                .width(width)
                .height(height)
                .center(Length::Fixed(width))
                .style(style::fill(GRAPHITE))
                .into(),
        }
    }

    fn tile<'a>(&'a self, r: &'a Row, index: usize) -> Element<'a, Message> {
        let id = r.game.appid;
        let reveal = self.eased(self.grid_since, STAGGER * index as u64, TILE_IN);
        let hover = self.hov(Hot::Tile(id));
        let selected = if self.selected == Some(id) { self.eased(self.panel_since, 0, 160) } else { 0.0 };
        let (w, h) = (TILE_W + 2.0 * INSET + 6.0, TILE_H + INSET + 10.0 + 26.0 + 10.0);
        let deco = canvas(shapes::TileDeco { name: r.game.name.clone(), hover, selected, inset: INSET, cover: Size::new(TILE_W, TILE_H) })
            .width(w)
            .height(h);
        // The cover lifts toward the slab on hover and drops in from below on reveal.
        let lift = 4.0 * hover.max(selected);
        let drop = (1.0 - reveal) * 26.0;
        let at = |extra_top: f32, extra_left: f32| Padding { top: INSET - lift + drop + extra_top, left: INSET - lift * 0.5 + extra_left, ..Padding::ZERO };
        let status = Status::of(r);
        let (fill, ink) = status.colors();
        let mut layers = stack![deco, container(Self::cover(r, TILE_W, TILE_H)).padding(at(0.0, 0.0)), container(Self::tag(status.label(), fill, ink, 11.0)).padding(at(8.0, -6.0))];
        if reveal < 1.0 {
            layers = layers.push(container(Space::new(Length::Fill, Length::Fill)).style(style::fill(alpha(BLACK, 1.0 - reveal))));
        }
        mouse_area(layers)
            .on_enter(Message::Hover(Hot::Tile(id), true))
            .on_exit(Message::Hover(Hot::Tile(id), false))
            .on_press(Message::Select(id))
            .interaction(mouse::Interaction::Pointer)
            .into()
    }

    fn detail<'a>(&'a self, r: &'a Row) -> Element<'a, Message> {
        let status = Status::of(r);
        let slide = self.eased(self.panel_since, 0, PANEL_IN);
        let wipe = if self.reduce() { 1.0 } else { anim::progress(self.now, self.panel_since, Duration::ZERO, ms(WIPE)) };

        let hero: Element<_> = match &r.art.hero {
            Some(p) => image(image::Handle::from_path(p)).width(Length::Fill).height(HERO_H).content_fit(ContentFit::Cover).into(),
            None => container(Space::new(Length::Fill, HERO_H)).style(style::fill(GRAPHITE)).into(),
        };
        let logo: Element<_> = match &r.art.logo {
            Some(p) => image(image::Handle::from_path(p)).height(84).content_fit(ContentFit::Contain).into(),
            None => Self::heading(&r.game.name, 34.0).into(),
        };
        let mut banner = stack![
            hero,
            container(Space::new(Length::Fill, HERO_H)).style(style::hero_fade),
            container(logo).width(Length::Fill).height(HERO_H).padding(14).align_x(Horizontal::Center).align_y(Vertical::Bottom),
        ];
        if wipe < 1.0 {
            banner = banner.push(canvas(shapes::Wipe::hero(wipe)).width(Length::Fill).height(HERO_H));
        }

        let chips = row![
            Self::tag(&r.engine, GRAPHITE, PAPER, 12.0),
            Self::tag(&r.proton, GRAPHITE, PAPER, 12.0),
            Self::tag(&format!("App {}", r.game.appid), GRAPHITE, PAPER, 12.0),
        ]
        .spacing(4);

        let (fill, ink) = status.colors();
        let explain = match status {
            Status::Ready => "Pipelines you've played are compiled for this driver.".to_string(),
            Status::NeedsReplay => r.staleness.map_or_else(String::new, |s| format!("{}.", capitalize(s.describe()))),
            Status::NotRecorded => "Play once with the launch option to record its pipelines.".into(),
            Status::SelfCompiles => format!("{}. shaderwarm is probably unnecessary.", r.verdict.reason()),
            Status::NoGpu => "Vulkan reported no hardware device.".into(),
        };
        let status_block = column![Self::tag(status.label(), fill, ink, 18.0), text(explain).size(13).color(GREY)].spacing(6);

        let b = &r.breakdown;
        let parts = [
            ("Mesa", b.mesa, PAPER),
            ("Fossilize", b.fossilize, ACCENT),
            ("DXVK", b.dxvk, GREY),
            ("vkd3d", b.vkd3d, style::ACCENT_HI),
            ("NVIDIA/AMDVLK", b.nvidia + b.amdvlk, SMOKE),
            ("Video", b.video, ALERT),
        ];
        let total: u64 = parts.iter().map(|p| p.1).sum();
        let segs = parts.iter().filter(|p| p.1 > 0).map(|(_, size, c)| (*size as f32 / total.max(1) as f32, *c)).collect();
        let legend = WRow::with_children(parts.iter().filter(|p| p.1 > 0).map(|(name, size, color)| {
            row![
                container(Space::new(10, 10)).style(style::fill(*color)),
                text(format!("{name} {}", model::size(*size))).size(12).color(GREY),
            ]
            .spacing(6)
            .align_y(Alignment::Center)
            .into()
        }))
        .spacing(12);
        let cache_block = column![
            row![Self::heading("Shader cache", 18.0), horizontal_space(), text(model::size(total)).font(DISPLAY).size(18)].align_y(Alignment::Center),
            canvas(shapes::SegBar { parts: segs }).width(Length::Fill).height(14),
            legend,
        ]
        .spacing(10);

        let fact = |k: &str, v: String| row![text(k.to_uppercase()).size(13).font(DISPLAY).color(GREY).width(130), text(v).size(13)];
        let facts = column![
            fact("Recordings", if r.recordings > 0 { model::size(r.recordings) } else { "none".into() }),
            fact("Last recorded", r.last_launch.map_or_else(|| "never through shaderwarm".into(), age)),
            fact("Backups", r.backups.to_string()),
        ]
        .spacing(6);

        let idle = !self.busy();
        let can_replay = idle && r.staleness.is_some_and(|s| s != Staleness::NoInputs);
        let warm_label = if status == Status::Ready { "Re-warm cache" } else { "Warm cache now" };
        let warm = self.button(Hot::Btn(Btn::Warm), warm_label, Kind::Primary, 22.0, Some(Length::Fill), can_replay.then_some(Message::Replay(r.game.appid)));
        let mut actions = row![self.button(Hot::Btn(Btn::Clear), "Clear cache", Kind::Secondary, 14.0, None, idle.then_some(Message::OpenPurge(r.game.appid)))].spacing(6);
        if r.backups > 0 {
            actions = actions.push(self.button(Hot::Btn(Btn::Restore), "Restore", Kind::Secondary, 14.0, None, idle.then_some(Message::Restore(r.game.appid))));
        }
        if r.video > 1 << 20 {
            actions = actions.push(self.button(
                Hot::Btn(Btn::Video),
                &format!("Free video {}", model::size(r.video)),
                Kind::Secondary,
                14.0,
                None,
                idle.then_some(Message::OpenVideo(Some(r.game.appid))),
            ));
        }

        let mut info = column![
            column![Self::heading(&r.game.name, 32.0), chips].spacing(8),
            status_block,
            container(cache_block).padding(14).style(style::well),
            facts,
            warm,
            actions,
        ]
        .spacing(16);
        if r.last_launch.is_none() && status != Status::SelfCompiles {
            info = info.push(
                container(
                    column![
                        text("RECORD IT: SET THIS STEAM LAUNCH OPTION").size(13).font(DISPLAY).color(GREY),
                        row![
                            text(model::LAUNCH_OPTION).font(iced::Font::MONOSPACE).color(PAPER),
                            horizontal_space(),
                            self.button(Hot::Btn(Btn::CopyHint), "Copy", Kind::Secondary, 12.0, None, Some(Message::CopyLaunchOption)),
                        ]
                        .align_y(Alignment::Center),
                    ]
                    .spacing(6),
                )
                .padding(12)
                .style(style::well),
            );
        }
        // The content slides in from the right under a fading curtain.
        let content = container(info).padding(Padding { top: 6.0, right: 20.0, bottom: 20.0, left: 20.0 + (1.0 - slide) * 48.0 });
        let mut body = stack![scrollable(content).height(Length::Fill)];
        if slide < 1.0 {
            body = body.push(container(Space::new(Length::Fill, Length::Fill)).style(style::fill(alpha(INK, 1.0 - slide))));
        }
        container(column![banner, body]).width(PANEL_W).height(Length::Fill).style(style::panel).clip(true).into()
    }

    fn settings_page(&self) -> Element<'_, Message> {
        let auto = self.snapshot.as_ref().is_some_and(|s| s.auto_replay);
        let layer = self.snapshot.as_ref().is_some_and(|s| s.layer_installed);
        let recording = column![
            Self::heading("Recording and auto-replay", 24.0),
            toggler(auto)
                .label("Warm stale caches automatically after GPU driver or Proton updates")
                .on_toggle_maybe((!self.busy()).then_some(Message::AutoReplay)),
            text(if layer { "Recording layer installed." } else { "Recording layer not installed: turning auto-replay on installs it." }).size(13).color(GREY),
            text(format!("Launch option for each game: {}", model::LAUNCH_OPTION)).size(13).color(GREY),
        ]
        .spacing(12);
        let replay = column![
            Self::heading("Replay", 24.0),
            row![
                text("Compile threads"),
                text_input("all cores", &self.threads_input).on_input(Message::Threads).width(120).padding([6, 10]).style(style::search),
                text(format!("using {}", self.settings.effective_threads())).color(GREY),
            ]
            .spacing(12)
            .align_y(Alignment::Center),
            checkbox("Include games that compile their own shaders in \"Warm all stale\" and auto-replay", self.settings.include_self_compiling)
                .on_toggle(Message::IncludeSelfCompiling),
        ]
        .spacing(12);
        let interface = column![
            Self::heading("Interface", 24.0),
            toggler(self.settings.reduce_motion).label("Reduce motion (no wipes, reveals or count-ups)").on_toggle(Message::ReduceMotion),
            text("Keyboard: arrow keys move through the library, F5 refreshes.").size(13).color(GREY),
        ]
        .spacing(12);
        let section = |body| container(body).padding(22).width(Length::Fill).style(style::panel);
        scrollable(column![section(recording), section(replay), section(interface)].spacing(14).max_width(920)).height(Length::Fill).into()
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
