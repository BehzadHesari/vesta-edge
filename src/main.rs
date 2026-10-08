#![windows_subsystem = "windows"]

//! Vesta Edge — a liquid side panel that lives on the right edge of the screen.

use std::{
    fs,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Mutex,
    },
    thread,
    time::{Duration, Instant},
};

mod sys;
mod util;

use serde_json::{json, Value};
use tao::{
    dpi::{PhysicalPosition, PhysicalSize},
    event::{Event, StartCause, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy},
    window::WindowBuilder,
};
use wry::{WebContext, WebViewBuilder};

const UI: &str = include_str!("ui.html");
const VERSION: &str = env!("CARGO_PKG_VERSION");
const REPO: &str = "BehzadHesari/vesta-edge";

static OPEN: AtomicBool = AtomicBool::new(false);
static PICKING: AtomicBool = AtomicBool::new(false);
/// Recent clipboard images, newest last. Thumbnails go to the UI; the pixels stay here.
static IMAGES: Mutex<Vec<(u64, u32, u32, Vec<u8>)>> = Mutex::new(Vec::new());
/// Pinned images are also kept on disk, so they come back after a restart.
static PINNED: Mutex<Vec<(u64, u32, u32, Vec<u8>)>> = Mutex::new(Vec::new());
static LAST_IMAGE: AtomicU64 = AtomicU64::new(0);
/// Styling that the source app kept in its own clipboard formats, by text hash (newest last).
static PRIVATE: Mutex<Vec<(u64, Vec<(String, Vec<u8>)>)>> = Mutex::new(Vec::new());
/// The clipboard sequence number of the last copy made by the panel itself.
static OWN_SEQ: AtomicU64 = AtomicU64::new(u64::MAX);

enum Ev {
    Script(String),
    Ipc(String),
    Open,
    /// open the panel from the tray, optionally straight into a widget
    Show(&'static str),
    Menu(String),
}

/* ---------------- clipboard ---------------- */
pub fn set_clip(text: &str) {
    if let Ok(mut c) = arboard::Clipboard::new() {
        let _ = c.set_text(text.to_owned());
    }
}

pub fn get_clip() -> Option<String> {
    arboard::Clipboard::new().ok()?.get_text().ok()
}

/// Puts an item back on the clipboard with every format it had (plain, HTML, RTF and the source
/// app's own formats), or as plain text only when the item says so.
fn put_clip(item: &Value) {
    let text = item["text"].as_str().unwrap_or_default();
    let plain = item["plain"].as_bool().unwrap_or(false);
    let rich = |k: &str| item[k].as_str().filter(|s| !s.is_empty() && !plain);
    let private = if plain { Vec::new() } else { private_formats(text) };
    // plain text also goes through here: an empty clipboard first, so no styled format from the last copy is left behind
    if !sys::set_clip_rich(text, rich("html"), rich("rtf"), &private) {
        set_clip(text);
    }
    // the watcher leaves our own copy alone: the UI already has the item, and the target app gets the clipboard to itself
    OWN_SEQ.store(sys::clip_seq(), Ordering::Relaxed);
}

/// Remembers the private formats of a copy, by its text. Like the history, a later plain copy of the
/// same text keeps them.
fn keep_private(text: &str, formats: Vec<(String, Vec<u8>)>) {
    if formats.is_empty() {
        return;
    }
    let id = util::hash(text.as_bytes());
    if let Ok(mut all) = PRIVATE.lock() {
        all.retain(|(other, _)| *other != id);
        all.push((id, formats));
        if all.len() > 50 {
            all.remove(0);
        }
    }
}

fn private_formats(text: &str) -> Vec<(String, Vec<u8>)> {
    let id = util::hash(text.as_bytes());
    PRIVATE.lock().ok().and_then(|all| all.iter().find(|(other, _)| *other == id).map(|(_, f)| f.clone())).unwrap_or_default()
}

fn transform(text: &str, mode: &str) -> String {
    match mode {
        "upper" => text.to_uppercase(),
        "lower" => text.to_lowercase(),
        "title" => text
            .split(' ')
            .map(|w| {
                let mut c = w.chars();
                c.next().map(|f| f.to_uppercase().collect::<String>() + &c.as_str().to_lowercase()).unwrap_or_default()
            })
            .collect::<Vec<_>>()
            .join(" "),
        "sentence" => {
            let lower = text.to_lowercase();
            let mut c = lower.chars();
            c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
        }
        "trim" => text.lines().map(|l| l.split_whitespace().collect::<Vec<_>>().join(" ")).collect::<Vec<_>>().join("\n").trim().to_string(),
        "oneline" | "plain" => text.split_whitespace().collect::<Vec<_>>().join(" "),
        "reverse" => text.chars().rev().collect(),
        "slug" => {
            let s: String = text.to_lowercase().chars().map(|c| if c.is_alphanumeric() { c } else { '-' }).collect();
            s.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-")
        }
        _ => text.to_string(),
    }
}

fn spawn_clip_watch(proxy: EventLoopProxy<Ev>) {
    thread::spawn(move || {
        let mut seq = 0;
        loop {
            let s = sys::clip_seq();
            if s != seq && s != OWN_SEQ.load(Ordering::Relaxed) {
                seq = s;
                match get_clip() {
                    Some(t) if !t.trim().is_empty() => {
                        if t.len() < 100_000 {
                            let (html, rtf) = sys::clip_rich();
                            let private = sys::clip_private();
                            let item = json!({ "text": t, "html": html, "rtf": rtf, "own": !private.is_empty() });
                            keep_private(&t, private);
                            if proxy.send_event(Ev::Script(format!("app.clip({item})"))).is_err() {
                                return;
                            }
                        }
                    }
                    _ => {
                        if let Some(script) = take_clip_image() {
                            if proxy.send_event(Ev::Script(script)).is_err() {
                                return;
                            }
                        }
                    }
                }
            }
            thread::sleep(Duration::from_millis(if cfg!(windows) { 400 } else { 800 }));
        }
    });
}

/// Copies a new clipboard image into the history and returns the call that shows its thumbnail.
fn take_clip_image() -> Option<String> {
    let (w, h, rgba) = sys::clip_image()?;
    let id = util::hash(&rgba) ^ ((w as u64) << 32) ^ h as u64;
    if LAST_IMAGE.swap(id, Ordering::Relaxed) == id {
        return None;
    }
    let call = image_call(id, w, h, &rgba, false);
    let mut images = IMAGES.lock().ok()?;
    images.retain(|(other, ..)| *other != id);
    images.push((id, w, h, rgba));
    if images.len() > 8 {
        images.remove(0);
    }
    Some(call)
}

/* ---------------- pinned clipboard images ---------------- */
fn pins_dir() -> std::path::PathBuf {
    let dir = sys::data_dir().join("pinned");
    let _ = fs::create_dir_all(&dir);
    dir
}

fn image_call(id: u64, w: u32, h: u32, rgba: &[u8], pinned: bool) -> String {
    let (tw, th, thumb) = util::thumbnail(w, h, rgba, 240);
    let uri = format!("data:image/png;base64,{}", util::base64(&util::png(tw, th, &thumb)));
    format!("app.clipImage({})", json!({ "id": id.to_string(), "w": w, "h": h, "thumb": uri, "pinned": pinned }))
}

fn pin_image(id: u64, on: bool) -> bool {
    let file = pins_dir().join(format!("{id}.bin"));
    if !on {
        let _ = fs::remove_file(file);
        return PINNED.lock().map(|mut v| v.retain(|(i, ..)| *i != id)).is_ok();
    }
    let Some((_, w, h, px)) = IMAGES.lock().ok().and_then(|v| v.iter().find(|(i, ..)| *i == id).cloned()) else { return false };
    let (w, h, px) = util::thumbnail(w, h, &px, 1600); // keep the saved copy reasonable
    let mut body = Vec::with_capacity(px.len() + 8);
    body.extend_from_slice(&w.to_le_bytes());
    body.extend_from_slice(&h.to_le_bytes());
    body.extend_from_slice(&px);
    if fs::write(file, body).is_err() {
        return false;
    }
    PINNED.lock().map(|mut v| v.push((id, w, h, px))).is_ok()
}

/// Shows pinned images again when the app starts.
fn restore_pins(proxy: EventLoopProxy<Ev>) {
    thread::spawn(move || {
        let Ok(entries) = fs::read_dir(pins_dir()) else { return };
        for entry in entries.flatten().take(12) {
            let Some(id) = entry.path().file_stem().and_then(|s| s.to_str()).and_then(|s| s.parse::<u64>().ok()) else { continue };
            let Ok(body) = fs::read(entry.path()) else { continue };
            if body.len() < 8 {
                continue;
            }
            let w = u32::from_le_bytes(body[0..4].try_into().unwrap());
            let h = u32::from_le_bytes(body[4..8].try_into().unwrap());
            let px = body[8..].to_vec();
            if px.len() as u32 != w * h * 4 {
                continue;
            }
            let _ = proxy.send_event(Ev::Script(image_call(id, w, h, &px, true)));
            if let Ok(mut v) = PINNED.lock() {
                v.push((id, w, h, px));
            }
        }
    });
}

fn log(msg: &str) {
    use std::io::Write;
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(sys::data_dir().join("log.txt")) {
        let _ = writeln!(f, "{msg}");
    }
}

/// File picker runs on its own thread so the panel keeps animating.
fn add_app(proxy: EventLoopProxy<Ev>, folder: bool) {
    thread::spawn(move || {
        if let Some(path) = if folder { sys::pick_folder() } else { sys::pick_file() } {
            let item = json!({ "path": path, "name": util::display_name(&path), "icon": sys::icon_data_uri(&path) });
            let _ = proxy.send_event(Ev::Script(format!("app.appAdded({item})")));
            let _ = proxy.send_event(Ev::Show("settings"));
        }
    });
}

/* ---------------- "Add to Vesta Edge" from the file manager ---------------- */
/// A second launch (right-click → Add to Vesta Edge) drops the path here and exits;
/// the running copy picks it up.
fn inbox() -> std::path::PathBuf {
    sys::data_dir().join("inbox.txt")
}

fn queue_path(path: &str) {
    use std::io::Write;
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(inbox()) {
        let _ = writeln!(f, "{path}");
    }
}

fn spawn_inbox_watch(proxy: EventLoopProxy<Ev>) {
    thread::spawn(move || loop {
        let file = inbox();
        if let Ok(text) = fs::read_to_string(&file) {
            let _ = fs::remove_file(&file);
            for path in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
                let item = json!({ "path": path, "name": util::display_name(path), "icon": sys::icon_data_uri(path) });
                if proxy.send_event(Ev::Script(format!("app.appAdded({item})"))).is_err() {
                    return;
                }
            }
        }
        thread::sleep(Duration::from_millis(700));
    });
}

/* ---------------- updates ---------------- */
fn newer(latest: &str, current: &str) -> bool {
    let parts = |v: &str| -> Vec<u32> { v.trim_start_matches('v').split('.').map(|x| x.parse().unwrap_or(0)).collect() };
    parts(latest) > parts(current)
}

/// Asks GitHub for the newest release (through curl, which ships with every supported OS).
fn spawn_update_check(proxy: EventLoopProxy<Ev>, manual: bool) {
    thread::spawn(move || {
        let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
        let out = std::process::Command::new("curl")
            .args(["-sL", "--max-time", "15", "-H", "Accept: application/vnd.github+json", "-H", "User-Agent: VestaEdge", &url])
            .output();
        let latest = out.ok().and_then(|o| serde_json::from_slice::<Value>(&o.stdout).ok()).and_then(|v| v["tag_name"].as_str().map(str::to_owned));
        let info = match latest {
            Some(tag) => json!({ "latest": tag, "newer": newer(&tag, VERSION), "manual": manual }),
            None => json!({ "latest": Value::Null, "newer": false, "manual": manual }),
        };
        let _ = proxy.send_event(Ev::Script(format!("app.updateInfo({info})")));
    });
}

/// Downloads the installer for this platform and hands over to it (the user confirms in the wizard).
fn install_update(proxy: EventLoopProxy<Ev>) {
    thread::spawn(move || {
        let page = format!("https://github.com/{REPO}/releases/latest");
        if !cfg!(windows) {
            sys::launch(&page); // macOS / Linux: packages are installed by the system tools
            return;
        }
        let file = std::env::temp_dir().join("VestaEdge-Setup.exe");
        let url = format!("https://github.com/{REPO}/releases/latest/download/VestaEdge-Setup.exe");
        let ok = std::process::Command::new("curl")
            .args(["-sL", "--max-time", "300", "-o", &file.display().to_string(), &url])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok && fs::metadata(&file).map(|m| m.len() > 1_000_000).unwrap_or(false) {
            let _ = std::process::Command::new(&file).spawn();
            let _ = proxy.send_event(Ev::Script("app.quitForUpdate()".into()));
        } else {
            sys::launch(&page);
            let _ = proxy.send_event(Ev::Script("app.toast('Could not download the update, opening the page')".into()));
        }
    });
}

/* ---------------- the dock: it sits on an edge, and follows the pointer while it is dragged ---------------- */
// The window is a square and the dock is always in its middle: moving the window moves the dock.
const BOX: f64 = 760.0; // window side, logical px
const RAIL: f64 = 22.0; // collapsed dock: thickness, logical px
const RAIL_LEN: f64 = 120.0; // collapsed dock: length, logical px
const GAP: f64 = 0.0; // a docked dock sits flush against its screen edge
const SNAP: f64 = 150.0; // how close the dock has to come before an edge starts to pull it in
const HYSTERESIS: f64 = 60.0; // how much nearer another edge has to be before the dragged dock turns to it

#[derive(Clone, Copy, PartialEq, Debug)]
enum Edge {
    Left,
    Right,
    Top,
    Bottom,
    Free,
}

impl Edge {
    fn parse(s: &str) -> Option<Edge> {
        match s {
            "left" => Some(Edge::Left),
            "right" => Some(Edge::Right),
            "top" => Some(Edge::Top),
            "bottom" => Some(Edge::Bottom),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Edge::Left => "left",
            Edge::Right => "right",
            Edge::Top => "top",
            Edge::Bottom => "bottom",
            Edge::Free => "free",
        }
    }
}

/// One monitor: its full size, and the part that the taskbar does not cover.
#[derive(Clone)]
struct Screen {
    name: String,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    wx: i32,
    wy: i32,
    ww: i32,
    wh: i32,
}

fn screens(event_loop: &tao::event_loop::EventLoop<Ev>) -> Vec<Screen> {
    event_loop
        .available_monitors()
        .enumerate()
        .map(|(i, m)| {
            let (p, s) = (m.position(), m.size());
            let full = (p.x, p.y, s.width as i32, s.height as i32);
            // the taskbar position is only known for the primary monitor
            let work = if p.x == 0 && p.y == 0 { sys::work_area().unwrap_or(full) } else { full };
            Screen {
                name: m.name().unwrap_or_else(|| format!("Monitor {}", i + 1)),
                x: full.0,
                y: full.1,
                w: full.2,
                h: full.3,
                wx: work.0,
                wy: work.1,
                ww: work.2,
                wh: work.3,
            }
        })
        .collect()
}

fn clamp_to(v: f64, lo: f64, hi: f64) -> f64 {
    if hi < lo { (lo + hi) / 2.0 } else { v.clamp(lo, hi) }
}

fn is_row(edge: Edge) -> bool {
    matches!(edge, Edge::Top | Edge::Bottom)
}

/// The work-area edge of `s` nearest to `p`. The edge the dock already has wins unless another one
/// is nearer by more than `margin`, so the dock does not flip back and forth near a corner.
fn nearest_edge(s: &Screen, p: (f64, f64), current: Option<Edge>, margin: f64) -> Edge {
    let dists = [
        (Edge::Right, (s.wx + s.ww) as f64 - p.0),
        (Edge::Left, p.0 - s.wx as f64),
        (Edge::Top, p.1 - s.wy as f64),
        (Edge::Bottom, (s.wy + s.wh) as f64 - p.1),
    ];
    let (best, d) = dists.into_iter().min_by(|a, b| a.1.total_cmp(&b.1)).unwrap();
    match current.and_then(|c| dists.iter().find(|(e, _)| *e == c)) {
        Some(&(c, dc)) if dc <= d + margin => c,
        _ => best,
    }
}

/// Distance from a point to a monitor (0 when the point is on it).
fn dist_to(s: &Screen, p: (f64, f64)) -> f64 {
    let dx = (s.x as f64 - p.0).max(0.0).max(p.0 - (s.x + s.w) as f64);
    let dy = (s.y as f64 - p.1).max(0.0).max(p.1 - (s.y + s.h) as f64);
    dx.hypot(dy)
}

/// The dock's position and motion. Physical px for positions, logical px for sizes.
struct Dock {
    edge: Edge,
    along: f64, // where it sits along its edge: 0 = top / left end, 1 = bottom / right end
    screen: usize,
    center: (f64, f64), // dock centre on the screen; the window is centred on it
    target: (f64, f64),
    vel: (f64, f64),
    size: (f64, f64), // dock width x height, as the UI reports it
    scale: f64,
    grab: (f64, f64), // cursor minus centre, from the moment the drag began
    dragging: bool,
    stuck: Option<Edge>, // the edge the dock is being held against while it is dragged
    hint: Option<Edge>,  // the edge the dock is close to while it is dragged: it already takes that shape
    press: Option<(i32, i32)>, // where the grip was pressed, until the pointer has moved far enough to be a drag
    sent: (Edge, bool, &'static str),
    last: Instant,
    moving: bool,
}

/// What one animation step changed, for the window and the UI.
struct Step {
    moved: bool,
    layout: Option<(Edge, bool, &'static str)>,
    magnet: f64,
}

impl Dock {
    fn new(scale: f64, screens: &[Screen], screen: usize, edge: Edge, along: f64) -> Dock {
        let mut dock = Dock {
            edge,
            along,
            screen,
            center: (0.0, 0.0),
            target: (0.0, 0.0),
            vel: (0.0, 0.0),
            size: (RAIL, RAIL_LEN),
            scale,
            grab: (0.0, 0.0),
            dragging: false,
            stuck: None,
            hint: None,
            press: None,
            sent: (Edge::Right, true, "left"),
            last: Instant::now(),
            moving: false,
        };
        let s = &screens[screen];
        dock.center = dock.edge_center(edge, s, dock.along_point(edge, s));
        dock.target = dock.center;
        dock.sent = dock.layout(screens);
        dock
    }

    fn half(&self) -> (f64, f64) {
        (self.size.0 * self.scale / 2.0, self.size.1 * self.scale / 2.0)
    }

    /// Half size as the dock will be on edge. A top or bottom dock is a row, so the reported size is
    /// turned around when it is currently a column (or the other way round).
    fn half_for(&self, edge: Edge) -> (f64, f64) {
        if is_row(edge) == (self.size.0 > self.size.1) { self.half() } else { (self.size.1 * self.scale / 2.0, self.size.0 * self.scale / 2.0) }
    }

    /// A point on the edge `edge` of screen `s` that keeps the dock at its place along that edge.
    fn along_point(&self, edge: Edge, s: &Screen) -> (f64, f64) {
        match edge {
            Edge::Top | Edge::Bottom => (s.wx as f64 + self.along * s.ww as f64, 0.0),
            _ => (0.0, s.wy as f64 + self.along * s.wh as f64),
        }
    }

    /// Where the dock centre sits when it is on `edge` of screen `s`, following `p` along that edge.
    fn edge_center(&self, edge: Edge, s: &Screen, p: (f64, f64)) -> (f64, f64) {
        let (hw, hh) = self.half_for(edge);
        let gap = GAP * self.scale;
        let (l, t) = (s.wx as f64, s.wy as f64);
        let (r, b) = ((s.wx + s.ww) as f64, (s.wy + s.wh) as f64);
        match edge {
            Edge::Right => (r - gap - hw, clamp_to(p.1, t + hh + gap, b - hh - gap)),
            Edge::Left => (l + gap + hw, clamp_to(p.1, t + hh + gap, b - hh - gap)),
            Edge::Top => (clamp_to(p.0, l + hw + gap, r - hw - gap), t + gap + hh),
            Edge::Bottom => (clamp_to(p.0, l + hw + gap, r - hw - gap), b - gap - hh),
            Edge::Free => p,
        }
    }

    /// Which along-edge position a point on that edge stands for (0..1).
    fn along_of(edge: Edge, s: &Screen, p: (f64, f64)) -> f64 {
        match edge {
            Edge::Top | Edge::Bottom => ((p.0 - s.wx as f64) / s.ww.max(1) as f64).clamp(0.0, 1.0),
            _ => ((p.1 - s.wy as f64) / s.wh.max(1) as f64).clamp(0.0, 1.0),
        }
    }

    /// The index of the monitor that holds `p`, or the nearest one.
    fn screen_at(screens: &[Screen], p: (f64, f64)) -> usize {
        screens
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| dist_to(a, p).total_cmp(&dist_to(b, p)))
            .map(|(i, _)| i)
            .unwrap_or(0)
    }

    /// What the UI shows: a column or a row, and the side the flyout opens to.
    /// While it is dragged the dock already has the shape of the edge it will land on; it only looks
    /// docked (flat side, fillets) while that edge holds it.
    fn layout(&self, screens: &[Screen]) -> (Edge, bool, &'static str) {
        let shape = if self.dragging { self.hint.unwrap_or(Edge::Free) } else { self.edge };
        let shown = if self.dragging && self.stuck != Some(shape) { Edge::Free } else { shape };
        match shape {
            Edge::Right => (shown, true, "left"),
            Edge::Left => (shown, true, "right"),
            Edge::Top => (shown, false, "down"),
            Edge::Bottom => (shown, false, "up"),
            Edge::Free => {
                let s = &screens[Self::screen_at(screens, self.center)];
                (Edge::Free, true, if self.center.0 > (s.x + s.w / 2) as f64 { "left" } else { "right" })
            }
        }
    }

    /// Top-left corner of the window: it is always centred on the dock.
    fn box_origin(&self) -> (i32, i32) {
        let half = BOX * self.scale / 2.0;
        ((self.center.0 - half).round() as i32, (self.center.1 - half).round() as i32)
    }

    fn begin_drag(&mut self, cursor: (i32, i32)) {
        self.grab = (cursor.0 as f64 - self.center.0, cursor.1 as f64 - self.center.1);
        self.dragging = true;
        self.stuck = None;
        self.hint = (self.edge != Edge::Free).then_some(self.edge);
        self.edge = Edge::Free;
        self.moving = true;
        self.last = Instant::now();
    }

    /// The settings picked an edge: move the dock there, keeping its place along the edge.
    fn snap(&mut self, edge: Edge, screen: usize) {
        self.edge = edge;
        self.screen = screen;
        self.dragging = false;
        self.stuck = None;
        self.moving = true;
        save_dock(edge, self.along);
    }

    /// One animation step. While dragging, the dock follows the pointer and is pulled towards the
    /// nearest edge when it comes close; once it is let go it settles on that edge.
    fn step(&mut self, screens: &[Screen], cursor: Option<(i32, i32)>, held: bool) -> Step {
        let now = Instant::now();
        let dt = (now - self.last).as_secs_f64().min(1.0 / 30.0);
        self.last = now;
        let mut magnet = 0.0;

        if let Some(p) = self.press {
            match cursor.filter(|_| held) {
                Some(c) if !self.dragging && ((c.0 - p.0).abs().max((c.1 - p.1).abs()) > 4) => {
                    self.press = None;
                    self.begin_drag(p);
                }
                Some(_) => {}
                None => self.press = None,
            }
        }

        if self.dragging {
            match cursor.filter(|_| held) {
                Some(c) => {
                    let pointer = (c.0 as f64, c.1 as f64);
                    self.screen = Self::screen_at(screens, pointer);
                    let s = &screens[self.screen];
                    // the edge it will land on, and so its shape: measured from the pointer, so the shape
                    // turning around cannot change the answer
                    let edge = nearest_edge(s, pointer, self.hint, HYSTERESIS * self.scale);
                    // a column and a row list the same things top-down and left-right: turning the grab
                    // around keeps the same spot of the dock under the pointer
                    if self.hint.is_some_and(|h| is_row(h) != is_row(edge)) {
                        self.grab = (self.grab.1, self.grab.0);
                    }
                    self.hint = Some(edge);
                    let free = (pointer.0 - self.grab.0, pointer.1 - self.grab.1);
                    // how far the dock's outer side is from that edge, with the size it has on that edge
                    let (hw, hh) = self.half_for(edge);
                    let dist = match edge {
                        Edge::Right => (s.wx + s.ww) as f64 - (free.0 + hw),
                        Edge::Left => (free.0 - hw) - s.wx as f64,
                        Edge::Top => (free.1 - hh) - s.wy as f64,
                        _ => (s.wy + s.wh) as f64 - (free.1 + hh),
                    };
                    let reach = SNAP * self.scale;
                    // holding on to an edge is easier than catching one: it takes a bigger pull to let go
                    let hold = if self.stuck == Some(edge) { reach * 0.9 } else { reach * 0.35 };
                    self.stuck = (dist < hold).then_some(edge);
                    let pull = if self.stuck.is_some() { 1.0 } else if dist < reach { (1.0 - dist / reach).powi(2) * 0.85 } else { 0.0 };
                    let on_edge = self.edge_center(edge, s, free);
                    self.target = (free.0 + (on_edge.0 - free.0) * pull, free.1 + (on_edge.1 - free.1) * pull);
                    magnet = pull;
                    self.edge = Edge::Free;
                }
                None => {
                    // button let go: the dock always lands on the edge it was nearest to, at its place along it
                    self.dragging = false;
                    self.stuck = None;
                    let s = &screens[self.screen];
                    let e = self.hint.take().unwrap_or_else(|| nearest_edge(s, self.center, None, 0.0));
                    self.along = Self::along_of(e, s, self.target);
                    self.edge = e;
                    self.target = self.edge_center(e, s, self.along_point(e, s));
                    save_dock(e, self.along);
                }
            }
        } else if self.edge != Edge::Free {
            let s = &screens[self.screen];
            self.target = self.edge_center(self.edge, s, self.along_point(self.edge, s));
        }

        // a spring towards the target; stiff while dragging so the dock sticks to the pointer
        let k = if self.dragging { 1400.0 } else { 260.0 };
        let c = 2.0 * f64::sqrt(k) * if self.dragging { 0.9 } else { 0.95 };
        let before = self.center;
        let n = ((dt / 0.002).ceil() as usize).max(1);
        let h = dt / n as f64;
        for _ in 0..n {
            self.vel.0 += (-k * (self.center.0 - self.target.0) - c * self.vel.0) * h;
            self.vel.1 += (-k * (self.center.1 - self.target.1) - c * self.vel.1) * h;
            self.center.0 += self.vel.0 * h;
            self.center.1 += self.vel.1 * h;
        }

        let gap = (self.center.0 - self.target.0).hypot(self.center.1 - self.target.1);
        let speed = self.vel.0.hypot(self.vel.1);
        self.moving = self.dragging || gap > 0.5 || speed > 0.5;
        let moved = (self.center.0 - before.0).abs() > 0.01 || (self.center.1 - before.1).abs() > 0.01;
        let layout = self.layout(screens);
        let changed = (layout != self.sent).then_some(layout);
        self.sent = layout;
        Step { moved, layout: changed, magnet }
    }
}

fn layout_json(dock: &Dock, screens: &[Screen]) -> Value {
    let (edge, vertical, flyout) = dock.layout(screens);
    json!({ "edge": edge.name(), "vertical": vertical, "flyout": flyout })
}

/// Remembers the edge and the place along it, so the dock comes back where it was left.
fn save_dock(edge: Edge, along: f64) {
    let _ = fs::write(sys::data_dir().join("dock.json"), json!({ "edge": edge.name(), "along": along }).to_string());
}

/* ---------------- click-through while closed ---------------- */
/// Closed panel lets clicks through to the apps below. On Linux we instead shrink the input
/// area to a thin strip at the screen edge: its mouse events open the panel even where the
/// compositor won't report the global pointer position (Wayland / XWayland).
// Windows shapes the window with regions instead (sys::set_regions); elsewhere it can be made click-through.
#[cfg(windows)]
fn set_passthrough(_window: &tao::window::Window, _on: bool) {}

#[cfg(all(not(windows), not(target_os = "linux")))]
fn set_passthrough(window: &tao::window::Window, on: bool) {
    window.set_ignore_cursor_events(on).ok();
}

#[cfg(target_os = "linux")]
fn set_passthrough(window: &tao::window::Window, on: bool) {
    use gtk::prelude::*;
    use tao::platform::unix::WindowExtUnix;
    let gw = window.gtk_window();
    if let Some(gdk) = gw.window() {
        let (w, h) = (gw.allocated_width().max(1), gw.allocated_height().max(1));
        let strip = 4;
        let rect = if on {
            gtk::cairo::RectangleInt::new((w - strip).max(0), 0, strip, h)
        } else {
            gtk::cairo::RectangleInt::new(0, 0, w, h)
        };
        gdk.input_shape_combine_region(&gtk::cairo::Region::create_rectangle(&rect), 0, 0);
    }
}

/* ---------------- tray ---------------- */
mod tray {
    use super::{util, Ev};
    use tao::event_loop::EventLoopProxy;
    use tray_icon::{
        menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem},
        Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    };

    pub struct Tray {
        _icon: TrayIcon,
        startup: CheckMenuItem,
    }

    impl Tray {
        pub fn new(proxy: &EventLoopProxy<Ev>, startup_on: bool) -> Option<Tray> {
            let login = if cfg!(windows) { "Start with Windows" } else { "Open at login" };
            let startup = CheckMenuItem::with_id("startup", login, true, startup_on, None);
            let menu = Menu::new();
            let _ = menu.append_items(&[
                &MenuItem::with_id("open", "Open Vesta Edge", true, None),
                &MenuItem::with_id("settings", "Widgets && settings…", true, None),
                &MenuItem::with_id("addapp", "Add app…", true, None),
                &PredefinedMenuItem::separator(),
                &startup,
                &PredefinedMenuItem::separator(),
                &MenuItem::with_id("quit", "Quit Vesta Edge", true, None),
            ]);
            let icon = TrayIconBuilder::new()
                .with_tooltip("Vesta Edge")
                .with_icon(Icon::from_rgba(util::tray_rgba(), 32, 32).ok()?)
                .with_menu(Box::new(menu))
                .with_menu_on_left_click(false)
                .build()
                .ok()?;
            let mp = proxy.clone();
            MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
                let _ = mp.send_event(Ev::Menu(e.id.0));
            }));
            let tp = proxy.clone();
            TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
                if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
                    let _ = tp.send_event(Ev::Show(""));
                }
            }));
            Some(Tray { _icon: icon, startup })
        }

        pub fn set_startup(&self, on: bool) {
            self.startup.set_checked(on);
        }
    }
}


fn main() {
    // right-click → "Add to Vesta Edge" launches us with --add <path>
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(path) = args.iter().position(|a| a == "--add").and_then(|i| args.get(i + 1)) {
        queue_path(path);
    }
    // debug helper: write whatever image is on the clipboard to a PNG
    if let Some(out) = args.iter().position(|a| a == "--dump-clip-image").and_then(|i| args.get(i + 1)) {
        match sys::clip_image() {
            Some((w, h, px)) => {
                let _ = fs::write(out, util::png(w, h, &px));
                println!("{w}x{h} written to {out}");
            }
            None => println!("no image on the clipboard"),
        }
        return;
    }

    // lets the installer / a script turn the Explorer menu entry on or off
    if let Some(v) = args.iter().position(|a| a == "--context-menu").and_then(|i| args.get(i + 1)) {
        sys::set_context_menu(v == "on");
        return;
    }
    if !sys::single_instance() {
        return; // the copy already running picks the path up from the inbox
    }
    let _ = fs::remove_file(inbox()); // stale entries from a previous session

    #[cfg(target_os = "linux")]
    {
        // The panel needs edge placement + input shaping, which Wayland (xdg-shell) forbids, so it
        // runs through XWayland. VESTA_EDGE_BACKEND=wayland opts out (for testing on compositors
        // where that works, or systems without XWayland).
        let want = std::env::var("VESTA_EDGE_BACKEND").unwrap_or_else(|_| "x11".into());
        unsafe { std::env::set_var("GDK_BACKEND", &want) };
    }
    let dir = sys::data_dir();
    let settings_path = dir.join("settings.json");
    let notes_path = dir.join("notes.txt");

    let mut settings: Value = fs::read_to_string(&settings_path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_else(|| json!({}));
    settings["startup"] = json!(sys::startup_enabled());
    let notes_json = dir.join("notes.json");
    let notes: Value = fs::read_to_string(&notes_json)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| match fs::read_to_string(&notes_path) {
            // carry the single note from older versions into the new list
            Ok(old) if !old.trim().is_empty() => json!([{ "id": "note-1", "title": "Note", "text": old }]),
            _ => json!([]),
        });

    #[allow(unused_mut)]
    let mut event_loop = EventLoopBuilder::<Ev>::with_user_event().build();
    #[cfg(target_os = "macos")]
    {
        use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
        event_loop.set_activation_policy(ActivationPolicy::Accessory); // no Dock icon
    }
    let proxy = event_loop.create_proxy();

    let monitor = event_loop.primary_monitor().or_else(|| event_loop.available_monitors().next()).expect("no monitor");
    let scale = monitor.scale_factor();
    sys::set_scale(scale);
    let all_screens = screens(&event_loop);
    let mut screen_idx = settings["monitor"].as_u64().unwrap_or(0) as usize;
    if screen_idx >= all_screens.len() {
        screen_idx = 0;
    }
    let saved = fs::read_to_string(dir.join("dock.json")).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok()).unwrap_or_else(|| json!({}));
    let edge = Edge::parse(saved["edge"].as_str().unwrap_or("right")).unwrap_or(Edge::Right);
    let mut dock = Dock::new(scale, &all_screens, screen_idx, edge, saved["along"].as_f64().unwrap_or(0.5).clamp(0.0, 1.0));
    let side = (BOX * scale).round() as i32;
    let (x, y) = dock.box_origin();
    let builder = WindowBuilder::new()
        .with_title("Vesta Edge")
        .with_decorations(false)
        .with_transparent(true)
        .with_always_on_top(true)
        .with_resizable(false)
        .with_inner_size(PhysicalSize::new(side, side))
        .with_position(PhysicalPosition::new(x, y));
    #[cfg(windows)]
    let builder = {
        use tao::platform::windows::WindowBuilderExtWindows;
        builder.with_skip_taskbar(true).with_undecorated_shadow(false)
    };
    #[cfg(target_os = "linux")]
    let builder = {
        use tao::platform::unix::WindowBuilderExtUnix;
        builder.with_skip_taskbar(true).with_visible(false)
    };
    let window = builder.build(&event_loop).expect("window");
    #[cfg(target_os = "linux")]
    {
        use gtk::prelude::*;
        use tao::platform::unix::WindowExtUnix;
        let gw = window.gtk_window();
        gw.set_type_hint(gtk::gdk::WindowTypeHint::Dock);
        gw.set_keep_above(true);
        gw.set_accept_focus(true); // dock-type windows still need the keyboard for search / notes
        gw.move_(x, y);
    }
    #[cfg(not(target_os = "linux"))]
    set_passthrough(&window, true);
    #[cfg(windows)]
    {
        use tao::platform::windows::WindowExtWindows;
        sys::set_self_window(window.hwnd() as isize);
    }

    // some platforms ignore a negative start position, so place the window again once it exists
    window.set_outer_position(PhysicalPosition::new(x, y));

    let mut ctx = WebContext::new(Some(dir.join("webview")));
    let init = format!(
        "window.onerror=(m,s,l)=>window.ipc.postMessage(JSON.stringify({{t:\"log\",text:m+\" @\"+l}}));window.__init = {};",
        json!({ "settings": settings, "notes": notes, "platform": sys::PLATFORM, "home": sys::home(),
                "version": VERSION, "contextMenu": sys::context_menu_enabled(),
                "screens": all_screens.iter().map(|s| json!({ "name": s.name, "w": s.w, "h": s.h })).collect::<Vec<_>>(), "edge": dock.edge.name(), "layout": layout_json(&dock, &all_screens) })
    );
    let ipc_proxy = proxy.clone();
    let drop_proxy = proxy.clone();
    let wv = WebViewBuilder::new_with_web_context(&mut ctx)
        // dropping a file or folder on the panel pins it (this replaces WebView2's own
        // drag handling on Windows, so the UI reorders with pointer events instead)
        .with_drag_drop_handler(move |e| {
            match e {
                wry::DragDropEvent::Enter { .. } => {
                    let _ = drop_proxy.send_event(Ev::Script("app.dropHint(true)".into()));
                }
                wry::DragDropEvent::Leave => {
                    let _ = drop_proxy.send_event(Ev::Script("app.dropHint(false)".into()));
                }
                wry::DragDropEvent::Drop { paths, .. } => {
                    let _ = drop_proxy.send_event(Ev::Script("app.dropHint(false)".into()));
                    for path in paths {
                        let p = path.display().to_string();
                        let item = json!({ "path": p, "name": util::display_name(&p), "icon": sys::icon_data_uri(&p) });
                        let _ = drop_proxy.send_event(Ev::Script(format!("app.appAdded({item})")));
                    }
                }
                _ => {}
            }
            true
        })
        .with_transparent(true)
        .with_background_color((0, 0, 0, 0))
        .with_initialization_script(&init)
        .with_html(UI)
        .with_ipc_handler(move |req| {
            let _ = ipc_proxy.send_event(Ev::Ipc(req.body().clone()));
        });
    #[cfg(not(target_os = "linux"))]
    let webview = wv.build(&window).expect("webview");
    #[cfg(target_os = "linux")]
    let webview = {
        use gtk::prelude::*;
        use tao::platform::unix::WindowExtUnix;
        use wry::WebViewBuilderExtUnix;
        let wv = wv.build_gtk(window.default_vbox().expect("gtk box")).expect("webview");
        let gw = window.gtk_window();
        gw.show_all();
        gw.move_(x, y);
        set_passthrough(&window, true);
        wv
    };

    spawn_clip_watch(proxy.clone());
    spawn_inbox_watch(proxy.clone());
    restore_pins(proxy.clone());
    let mut tray: Option<tray::Tray> = None;

    event_loop.run(move |event, _, flow| {
        *flow = ControlFlow::Wait;
        match event {
            // macOS needs the tray created once the app is running
            Event::NewEvents(StartCause::Init) => {
                tray = tray::Tray::new(&proxy, sys::startup_enabled());
                if !OPEN.load(Ordering::Relaxed) {
                    set_passthrough(&window, true);
                }
            }
            Event::UserEvent(Ev::Script(js)) => {
                let _ = webview.evaluate_script(&js);
            }
            Event::UserEvent(Ev::Open) => {
                set_passthrough(&window, false);
                let _ = webview.evaluate_script("app.open()");
            }
            Event::UserEvent(Ev::Show(panel)) => {
                sys::remember_foreground();
                OPEN.store(true, Ordering::Relaxed);
                set_passthrough(&window, false);
                let _ = webview.evaluate_script(&format!("app.show('{panel}')"));
            }
            Event::UserEvent(Ev::Menu(id)) => match id.as_str() {
                "open" => {
                    let _ = proxy.send_event(Ev::Show(""));
                }
                "settings" => {
                    let _ = proxy.send_event(Ev::Show("settings"));
                }
                "addapp" => add_app(proxy.clone(), false),
                "startup" => {
                    let on = !sys::startup_enabled();
                    sys::set_startup(on);
                    if let Some(t) = &tray {
                        t.set_startup(on);
                    }
                    let _ = webview.evaluate_script(&format!("app.syncStartup({on})"));
                }
                "quit" => *flow = ControlFlow::Exit,
                _ => {}
            },
            Event::UserEvent(Ev::Ipc(body)) => {
                let Ok(m) = serde_json::from_str::<Value>(&body) else { return };
                let s = |k: &str| m[k].as_str().unwrap_or_default().to_string();
                match m["t"].as_str().unwrap_or_default() {
                    "edge" => sys::remember_foreground(),
                    "regions" => {
                        // the parts of the window that take the pointer, from the UI (logical px, window coordinates)
                        let rects: Vec<(i32, i32, i32, i32)> = m["rects"]
                            .as_array()
                            .map(|list| {
                                list.iter()
                                    .filter_map(|r| {
                                        let v: Vec<f64> = r.as_array()?.iter().filter_map(Value::as_f64).collect();
                                        (v.len() == 4).then(|| ((v[0] * scale) as i32, (v[1] * scale) as i32, (v[2] * scale).ceil() as i32, (v[3] * scale).ceil() as i32))
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        sys::set_regions(&rects);
                    }
                    "dockSize" => {
                        dock.size = (m["w"].as_f64().unwrap_or(RAIL), m["h"].as_f64().unwrap_or(RAIL_LEN));
                        dock.moving = true;
                    }
                    "gripDown" => {
                        dock.press = sys::cursor_pos();
                        dock.moving = true;
                    }
                    "dragStart" => {
                        // measured from where the button went down, so the dock keeps its place under the pointer
                        if let Some(c) = dock.press.take().or_else(sys::cursor_pos).filter(|_| !dock.dragging) {
                            dock.begin_drag(c);
                        }
                    }
                    "copy" => put_clip(&m),
                    "paste" => {
                        put_clip(&m);
                        sys::paste_into_previous();
                    }
                    "transform" => {
                        if let Some(t) = get_clip() {
                            set_clip(&transform(&t, &s("mode")));
                            if m["paste"].as_bool().unwrap_or(false) {
                                sys::paste_into_previous();
                            }
                        }
                    }
                    "note" => {
                        let _ = fs::write(&notes_json, m["notes"].to_string());
                    }
                    "settings" => {
                        let st = &m["settings"];
                        let want = st["startup"].as_bool().unwrap_or(false);
                        if want != sys::startup_enabled() {
                            sys::set_startup(want);
                        }
                        if let Some(t) = &tray {
                            t.set_startup(want);
                        }
                        let _ = fs::write(&settings_path, st.to_string());
                    }
                    "screenshot" => sys::screenshot(),
                    "pickColor" => {
                        if !PICKING.swap(true, Ordering::Relaxed) {
                            let p = proxy.clone();
                            sys::pick_color(move |hex| {
                                if let Some(hex) = hex {
                                    set_clip(&hex);
                                    OPEN.store(true, Ordering::Relaxed);
                                    let _ = p.send_event(Ev::Open);
                                    let _ = p.send_event(Ev::Script(format!("app.picked('{hex}')")));
                                }
                                PICKING.store(false, Ordering::Relaxed);
                            });
                        }
                    }
                    "lock" => sys::lock(),
                    "alarm" => {
                        sys::beep();
                        OPEN.store(true, Ordering::Relaxed);
                        set_passthrough(&window, false);
                    }
                    "log" => log(&s("text")),
                    "addApp" => add_app(proxy.clone(), m["folder"].as_bool().unwrap_or(false)),
                    "launch" | "open" => sys::launch(&s(if m["t"] == "open" { "target" } else { "path" })),
                    "key" => sys::press(&s("name")),
                    "screenoff" => sys::screen_off(),
                    "pinwin" => {
                        let msg = sys::toggle_topmost();
                        let _ = webview.evaluate_script(&format!("app.toast({})", json!(msg)));
                    }
                    "awake" => sys::keep_awake(m["on"].as_bool().unwrap_or(false)),
                    "refreshIcon" => {
                        let (id, path, p2) = (s("id"), s("path"), proxy.clone());
                        thread::spawn(move || {
                            if let Some(icon) = sys::icon_data_uri(&path) {
                                let _ = p2.send_event(Ev::Script(format!("app.iconFor({},{})", json!(id), json!(icon))));
                            }
                        });
                    }
                    "contextMenu" => sys::set_context_menu(m["on"].as_bool().unwrap_or(false)),
                    "placement" => {
                        let idx = (m["monitor"].as_u64().unwrap_or(0) as usize).min(all_screens.len() - 1);
                        if let Some(edge) = Edge::parse(m["edge"].as_str().unwrap_or_default()) {
                            dock.snap(edge, idx);
                            let _ = webview.evaluate_script(&format!("app.placed({})", json!({ "edge": edge.name() })));
                        }
                    }
                    "pinImage" => {
                        let id: u64 = s("id").parse().unwrap_or(0);
                        let on = m["on"].as_bool().unwrap_or(false);
                        let ok = pin_image(id, on);
                        let msg = if !ok { "Could not pin that image" } else if on { "Image pinned" } else { "Image unpinned" };
                        let _ = webview.evaluate_script(&format!("app.toast({})", json!(msg)));
                    }
                    "clipImage" => {
                        let id: u64 = s("id").parse().unwrap_or(0);
                        let found = IMAGES
                            .lock()
                            .ok()
                            .and_then(|v| v.iter().find(|(i, ..)| *i == id).cloned())
                            .or_else(|| PINNED.lock().ok().and_then(|v| v.iter().find(|(i, ..)| *i == id).cloned()));
                        let ok = found.map(|(_, w, h, px)| sys::set_clip_image(w, h, &px)).unwrap_or(false);
                        LAST_IMAGE.store(id, Ordering::Relaxed); // don't re-announce our own copy
                        let _ = webview.evaluate_script(if ok { "app.toast('Image copied')" } else { "app.toast('Could not copy that image')" });
                    }
                    "checkUpdate" => spawn_update_check(proxy.clone(), m["manual"].as_bool().unwrap_or(false)),
                    "installUpdate" => install_update(proxy.clone()),
                    "quit" => *flow = ControlFlow::Exit,
                    _ => {}
                }
            }
            Event::WindowEvent { event: WindowEvent::Focused(false), .. } => {
                let _ = webview.evaluate_script("app.blur()");
            }
            _ => {}
        }

        // the dock moves on its own while it is dragged or settling onto an edge
        if dock.moving {
            let st = dock.step(&all_screens, sys::cursor_pos(), sys::left_down());
            if st.moved {
                let (x, y) = dock.box_origin();
                window.set_outer_position(PhysicalPosition::new(x, y));
            }
            if let Some((edge, vertical, flyout)) = st.layout {
                let _ = webview.evaluate_script(&format!("app.layout({})", json!({ "edge": edge.name(), "vertical": vertical, "flyout": flyout })));
            }
            let _ = webview.evaluate_script(&format!("app.magnet({:.3})", st.magnet));
            if !matches!(*flow, ControlFlow::Exit) {
                *flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(8));
            }
        }
    });
}
