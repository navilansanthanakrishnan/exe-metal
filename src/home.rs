//! The home page: everything installed, as tiles you can launch.
//!
//! This is the window that opens when someone double-clicks the app. It is a
//! library first and a file-opener second, because the common case is coming
//! back to something you already installed.
//!
//! Nothing slow runs on the UI thread. An installer can take minutes and a
//! first prefix boot several seconds, so both go to a worker and report back
//! over a channel.

use crate::downloads::{self, Found, Option_};
use crate::flow;
use crate::library::{Backend, Library, Title};
use crate::paths::Layout;
use crate::scan;
use eframe::egui;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};

pub fn launch(fullscreen: bool) -> anyhow::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([960.0, 720.0])
            .with_min_inner_size([620.0, 460.0])
            .with_fullscreen(fullscreen)
            .with_title("EXE Metal"),
        ..Default::default()
    };
    eframe::run_native(
        "EXE Metal",
        options,
        Box::new(|cc| Ok(Box::new(Home::new(cc)))),
    )
    .map_err(|e| anyhow::anyhow!("could not open the window: {e}"))
}

/// Open the window with an `.exe` already queued — what a double-click in
/// Finder, or `open -a "EXE Metal" game.exe`, ends up calling.
pub fn launch_with(exe: PathBuf) -> anyhow::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([960.0, 720.0])
            .with_min_inner_size([620.0, 460.0])
            .with_title("EXE Metal"),
        ..Default::default()
    };
    eframe::run_native(
        "EXE Metal",
        options,
        Box::new(move |cc| {
            let mut h = Home::new(cc);
            h.pending_open = Some(exe);
            Ok(Box::new(h))
        }),
    )
    .map_err(|e| anyhow::anyhow!("could not open the window: {e}"))
}

enum Msg {
    Status(String),
    Done(String),
    Failed(String),
    /// Library changed on disk; reload it.
    Reload,
}

#[derive(PartialEq)]
enum State {
    Idle,
    Working,
}

struct Home {
    layout: Option<Layout>,
    lib: Library,
    /// Title id.
    selected: Option<String>,
    parts: Vec<scan::Candidate>,
    search: String,
    state: State,
    status: String,
    log: Vec<String>,
    rx: Option<Receiver<Msg>>,
    pending_open: Option<PathBuf>,
    /// Confirmation gate for Forget, so one stray click cannot clear a tile.
    confirm_forget: Option<String>,
    /// `.exe` files sitting in Downloads/Desktop/Documents.
    downloads: Vec<Found>,
    show_downloads: bool,
    /// An install waiting on a destination choice: (exe, options, chosen).
    choosing: Option<(PathBuf, Vec<Option_>, usize)>,
}

impl Home {
    fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let layout = Layout::discover().ok();
        let mut lib = Library::load().unwrap_or_default();
        if lib.prune_missing() > 0 {
            let _ = lib.save();
        }
        Self {
            layout,
            lib,
            selected: None,
            parts: Vec::new(),
            search: String::new(),
            state: State::Idle,
            status: String::new(),
            log: Vec::new(),
            rx: None,
            pending_open: None,
            confirm_forget: None,
            downloads: downloads::scan(),
            show_downloads: true,
            choosing: None,
        }
    }

    /// Ask where an executable should go, rather than guessing silently.
    /// A bottle is a whole Windows install; putting a game in the wrong one is
    /// annoying to undo, so this is a question worth asking once.
    fn begin_install(&mut self, exe: PathBuf) {
        let Some(layout) = self.layout.as_ref() else {
            return;
        };
        let opts = downloads::options(layout, &exe);
        let pick = opts.iter().position(|o| o.recommended).unwrap_or(0);
        self.choosing = Some((exe, opts, pick));
    }

    fn confirm_install(&mut self) {
        let Some((exe, opts, pick)) = self.choosing.take() else {
            return;
        };
        let Some(layout) = self.layout.as_ref().map(|l| Layout {
            root: l.root.clone(),
        }) else {
            return;
        };
        let Some(chosen) = opts.get(pick).map(|o| o.destination.clone()) else {
            return;
        };
        let name = exe
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        self.spawn(&format!("installing {name}…"), move |tx| {
            let _ = tx.send(Msg::Status(format!(
                "preparing bottle '{}'…",
                chosen.bottle_name()
            )));
            match downloads::realise(&layout, &chosen) {
                Ok(b) => {
                    let _ = tx.send(Msg::Status(format!(
                        "running the installer in '{}'…",
                        b.name
                    )));
                    match flow::open_exe(&layout, &exe, Some(&b.name), false) {
                        Ok(o) => {
                            let _ = tx.send(Msg::Reload);
                            let _ = tx.send(Msg::Done(flow::describe_outcome(&o)));
                        }
                        Err(e) => {
                            let _ = tx.send(Msg::Failed(format!("{e:#}")));
                        }
                    }
                }
                Err(e) => {
                    let _ = tx.send(Msg::Failed(format!("could not prepare the bottle: {e:#}")));
                }
            }
        });
    }

    fn reload(&mut self) {
        self.lib = Library::load().unwrap_or_default();
        if let (Some(id), Some(layout)) = (self.selected.clone(), self.layout.as_ref()) {
            self.parts = flow::parts_of(layout, &id).unwrap_or_default();
        }
    }

    fn select(&mut self, id: &str) {
        self.selected = Some(id.to_string());
        self.parts = self
            .layout
            .as_ref()
            .and_then(|l| flow::parts_of(l, id).ok())
            .unwrap_or_default();
        self.confirm_forget = None;
    }

    fn spawn<F>(&mut self, what: &str, job: F)
    where
        F: FnOnce(Sender<Msg>) + Send + 'static,
    {
        if self.state == State::Working {
            return;
        }
        let (tx, rx) = channel();
        self.rx = Some(rx);
        self.state = State::Working;
        self.status = what.to_string();
        self.log.clear();
        std::thread::spawn(move || job(tx));
    }

    fn open_exe(&mut self, exe: PathBuf) {
        let Some(layout) = self.layout.as_ref().map(|l| Layout {
            root: l.root.clone(),
        }) else {
            self.status = "could not find the exe-metal support directory".into();
            return;
        };
        let name = exe
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        self.spawn(&format!("opening {name}…"), move |tx| {
            let _ = tx.send(Msg::Status(
                "working out what this needs and preparing a bottle…".into(),
            ));
            match flow::open_exe(&layout, &exe, None, false) {
                Ok(outcome) => {
                    let _ = tx.send(Msg::Reload);
                    let _ = tx.send(Msg::Done(flow::describe_outcome(&outcome)));
                }
                Err(e) => {
                    let _ = tx.send(Msg::Failed(format!("{e:#}")));
                }
            }
        });
    }

    fn play(&mut self, id: String) {
        let Some(layout) = self.layout.as_ref().map(|l| Layout {
            root: l.root.clone(),
        }) else {
            return;
        };
        let name = self
            .lib
            .get(&id)
            .map(|t| t.name.clone())
            .unwrap_or_else(|| id.clone());
        self.spawn(&format!("launching {name}…"), move |tx| {
            // Detached: the window should come back immediately, and the game
            // outliving this process is the point.
            match flow::launch_title(&layout, &id, true) {
                Ok(_) => {
                    let _ = tx.send(Msg::Reload);
                    let _ = tx.send(Msg::Done(format!("{name} is running")));
                }
                Err(e) => {
                    let _ = tx.send(Msg::Failed(format!("{e:#}")));
                }
            }
        });
    }

    /// Ask a running title to close gracefully. Distinct from killing it:
    /// many Windows games only write their settings on a clean shutdown.
    fn quit_title(&mut self, id: String) {
        let Some(layout) = self.layout.as_ref().map(|l| Layout {
            root: l.root.clone(),
        }) else {
            return;
        };
        let name = self
            .lib
            .get(&id)
            .map(|t| t.name.clone())
            .unwrap_or_else(|| id.clone());
        self.spawn(
            &format!("closing {name}…"),
            move |tx| match flow::quit_title(&layout, &id) {
                Ok(()) => {
                    let _ = tx.send(Msg::Done(format!(
                        "asked {name} to close — its settings will be saved"
                    )));
                }
                Err(e) => {
                    let _ = tx.send(Msg::Failed(format!("{e:#}")));
                }
            },
        );
    }

    fn rescan(&mut self) {
        let Some(layout) = self.layout.as_ref().map(|l| Layout {
            root: l.root.clone(),
        }) else {
            return;
        };
        self.spawn(
            "looking through every bottle…",
            move |tx| match flow::rescan(&layout, None) {
                Ok((added, pruned)) => {
                    let _ = tx.send(Msg::Reload);
                    let msg = match (added.len(), pruned) {
                        (0, 0) => "nothing new".to_string(),
                        (0, p) => format!("dropped {p} that were gone"),
                        (a, 0) => format!("found {a}"),
                        (a, p) => format!("found {a}, dropped {p} that were gone"),
                    };
                    let _ = tx.send(Msg::Done(msg));
                }
                Err(e) => {
                    let _ = tx.send(Msg::Failed(format!("{e:#}")));
                }
            },
        );
    }

    fn pump(&mut self) {
        let mut reload = false;
        let mut finished = false;
        if let Some(rx) = &self.rx {
            while let Ok(msg) = rx.try_recv() {
                match msg {
                    Msg::Status(s) => {
                        self.log.push(s.clone());
                        self.status = s;
                    }
                    Msg::Reload => reload = true,
                    Msg::Done(s) => {
                        self.status = s;
                        finished = true;
                    }
                    Msg::Failed(e) => {
                        self.status = format!("failed: {e}");
                        // Also to disk. A GUI that shows a failure only in a
                        // status line loses it the moment anything else
                        // happens, and there is then no way to find out what
                        // went wrong after the fact.
                        log_failure(&e);
                        self.log.push(e);
                        finished = true;
                    }
                }
            }
        }
        if reload {
            self.reload();
            self.downloads = downloads::scan();
        }
        if finished {
            self.state = State::Idle;
            self.rx = None;
        }
    }

    fn visible(&self) -> Vec<Title> {
        let q = self.search.trim().to_lowercase();
        self.lib
            .shelf()
            .into_iter()
            .filter(|t| q.is_empty() || t.name.to_lowercase().contains(&q))
            .cloned()
            .collect()
    }
}

/// Append a failure to a log beside the library, with a timestamp.
fn log_failure(msg: &str) {
    let Ok(dir) = crate::library::library_root() else {
        return;
    };
    let _ = std::fs::create_dir_all(&dir);
    let line = format!(
        "[{}] {}\n",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        msg
    );
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("gui.log"))
    {
        let _ = f.write_all(line.as_bytes());
    }
}

fn human_size(bytes: u64) -> String {
    const MB: f64 = 1_048_576.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.1} GB", b / GB)
    } else if b >= MB {
        format!("{:.0} MB", b / MB)
    } else {
        format!("{bytes} B")
    }
}

/// A stable colour per title, so tiles are visually distinct without icons.
/// Extracting real icons out of PE resources is a project of its own; a
/// deterministic hue is honest about being a placeholder and still makes the
/// grid scannable.
fn tile_colour(name: &str) -> egui::Color32 {
    let mut h: u32 = 2166136261;
    for b in name.as_bytes() {
        h ^= *b as u32;
        h = h.wrapping_mul(16777619);
    }
    let hue = (h % 360) as f32 / 360.0;
    let (r, g, b) = hsv_rgb(hue, 0.45, 0.55);
    egui::Color32::from_rgb(r, g, b)
}

fn hsv_rgb(h: f32, s: f32, v: f32) -> (u8, u8, u8) {
    let i = (h * 6.0).floor();
    let f = h * 6.0 - i;
    let p = v * (1.0 - s);
    let q = v * (1.0 - f * s);
    let t = v * (1.0 - (1.0 - f) * s);
    let (r, g, b) = match (i as i32) % 6 {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    ((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8)
}

impl eframe::App for Home {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.pump();
        let ctx = ui.ctx().clone();
        if self.state == State::Working {
            // Keep the spinner and status line moving while the worker runs.
            ctx.request_repaint_after(std::time::Duration::from_millis(120));
        } else {
            // egui only repaints on demand, and AccessKit publishes the widget
            // tree as part of a repaint. Left idle, the window stops publishing
            // and an accessibility client sees nothing but window chrome — so a
            // read succeeds and the click a moment later finds no elements.
            // A slow steady tick keeps the tree alive at negligible cost.
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }

        // A file handed over at launch, or dropped onto the window.
        if let Some(exe) = self.pending_open.take() {
            self.open_exe(exe);
        }
        let dropped: Option<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .find(|p| {
                    p.extension()
                        .map(|e| e.eq_ignore_ascii_case("exe"))
                        .unwrap_or(false)
                })
        });
        if let Some(exe) = dropped {
            self.begin_install(exe);
        }

        egui::CentralPanel::default().show(ui, |ui| {
            self.top_bar(ui);
            ui.add_space(4.0);
            self.status_bar(ui);
            ui.separator();

            if self.choosing.is_some() {
                self.chooser(ui);
                return;
            }
            self.downloads_section(ui);

            let detail = self
                .selected
                .clone()
                .and_then(|id| self.lib.get(&id).cloned());

            ui.horizontal_top(|ui| {
                let total = ui.available_width();
                let detail_w = if detail.is_some() {
                    340.0_f32.min(total * 0.42)
                } else {
                    0.0
                };
                let grid_w = (total - detail_w - 16.0).max(200.0);

                ui.vertical(|ui| {
                    ui.set_width(grid_w);
                    self.grid(ui);
                });

                if let Some(t) = detail {
                    ui.separator();
                    ui.vertical(|ui| {
                        ui.set_width(detail_w);
                        egui::ScrollArea::vertical()
                            .id_salt("detail-scroll")
                            .show(ui, |ui| {
                                self.detail(ui, &t);
                            });
                    });
                }
            });
        });
    }
}

impl Home {
    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.heading("EXE Metal");
            ui.add_space(12.0);
            let busy = self.state == State::Working;
            if ui
                .add_enabled(!busy, egui::Button::new("Open .exe…"))
                .on_hover_text("Run a Windows installer or program")
                .clicked()
            {
                if let Some(p) = rfd::FileDialog::new()
                    .add_filter("Windows executable", &["exe"])
                    .pick_file()
                {
                    self.begin_install(p);
                }
            }
            if ui
                .add_enabled(!busy, egui::Button::new("Rescan"))
                .on_hover_text("Look through every bottle for programs not listed yet")
                .clicked()
            {
                self.rescan();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.search)
                        .hint_text("Search")
                        .desired_width(170.0),
                );
            });
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if self.state == State::Working {
                ui.spinner();
            }
            let text = if self.status.is_empty() {
                "Drop an .exe anywhere in this window to install it.".to_string()
            } else {
                self.status.clone()
            };
            ui.label(egui::RichText::new(text).small().weak());
        });
    }

    /// Newly downloaded executables, so installing is one click from the home
    /// page rather than a trip through a file dialog.
    fn downloads_section(&mut self, ui: &mut egui::Ui) {
        if self.downloads.is_empty() {
            return;
        }
        let n = self.downloads.len();
        ui.horizontal(|ui| {
            let arrow = if self.show_downloads { "▾" } else { "▸" };
            if ui
                .button(format!("{arrow} Downloads ({n})"))
                .on_hover_text("Windows executables found in Downloads, Desktop and Documents")
                .clicked()
            {
                self.show_downloads = !self.show_downloads;
            }
            if ui.small_button("Refresh").clicked() {
                self.downloads = downloads::scan();
            }
        });
        if !self.show_downloads {
            ui.add_space(4.0);
            return;
        }
        ui.add_space(4.0);
        let busy = self.state == State::Working;
        let items = self.downloads.clone();
        let mut install: Option<PathBuf> = None;
        egui::Frame::group(ui.style())
            .corner_radius(6.0)
            .show(ui, |ui| {
                for f in items.iter().take(6) {
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(!busy, egui::Button::new("Install…"))
                            .on_hover_text(f.path.to_string_lossy())
                            .clicked()
                        {
                            install = Some(f.path.clone());
                        }
                        ui.label(egui::RichText::new(&f.name).strong());
                        ui.label(
                            egui::RichText::new(format!(
                                "{} · {}",
                                human_size(f.size),
                                f.describe()
                            ))
                            .small()
                            .weak(),
                        );
                    });
                }
            });
        if let Some(p) = install {
            self.begin_install(p);
        }
        ui.add_space(8.0);
    }

    /// Where should this go? Asked once, with the reasoning shown, because a
    /// bottle is a whole Windows install and the wrong one is tedious to undo.
    fn chooser(&mut self, ui: &mut egui::Ui) {
        let Some((exe, opts, pick)) = self.choosing.clone() else {
            return;
        };
        let name = exe
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        ui.add_space(8.0);
        ui.heading(format!("Install {name}"));
        ui.label(egui::RichText::new(exe.to_string_lossy()).small().weak());
        ui.add_space(10.0);
        ui.label(egui::RichText::new("Where should it go?").strong());
        ui.add_space(4.0);

        let mut chosen = pick;
        egui::ScrollArea::vertical()
            .id_salt("chooser-scroll")
            .max_height(320.0)
            .show(ui, |ui| {
                for (i, o) in opts.iter().enumerate() {
                    ui.horizontal(|ui| {
                        if ui.radio(i == pick, "").clicked() {
                            chosen = i;
                        }
                        ui.vertical(|ui| {
                            let mut label = egui::RichText::new(&o.label);
                            if o.recommended {
                                label = label.strong();
                            }
                            ui.horizontal(|ui| {
                                if ui.selectable_label(i == pick, label).clicked() {
                                    chosen = i;
                                }
                                if o.recommended {
                                    ui.label(
                                        egui::RichText::new("recommended")
                                            .small()
                                            .color(ui.visuals().hyperlink_color),
                                    );
                                }
                            });
                            ui.label(egui::RichText::new(&o.reason).small().weak());
                        });
                    });
                    ui.add_space(4.0);
                }
            });

        if chosen != pick {
            if let Some(c) = self.choosing.as_mut() {
                c.2 = chosen;
            }
        }

        ui.add_space(10.0);
        if let Some(layout) = self.layout.as_ref() {
            let free = downloads::free_space(layout);
            ui.label(
                egui::RichText::new(format!("{} free on this volume", human_size(free)))
                    .small()
                    .weak(),
            );
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button("Install").clicked() {
                self.confirm_install();
            }
            if ui.button("Cancel").clicked() {
                self.choosing = None;
            }
        });
    }

    fn grid(&mut self, ui: &mut egui::Ui) {
        let titles = self.visible();
        if titles.is_empty() {
            ui.add_space(50.0);
            ui.vertical_centered(|ui| {
                if self.lib.titles.is_empty() {
                    ui.label("Nothing installed yet.");
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new("Open a Windows installer and it will appear here.")
                            .weak(),
                    );
                } else {
                    ui.label("Nothing matches that search.");
                }
            });
            return;
        }

        egui::ScrollArea::vertical()
            .id_salt("grid-scroll")
            .show(ui, |ui| {
                ui.add_space(8.0);
                let tile_w = 180.0;
                let avail = ui.available_width();
                let per_row = ((avail / (tile_w + 24.0)).floor() as usize).max(1);
                let mut clicked: Option<String> = None;
                let mut play: Option<String> = None;

                for chunk in titles.chunks(per_row) {
                    ui.horizontal(|ui| {
                        for t in chunk {
                            let selected = self.selected.as_deref() == Some(t.id.as_str());
                            let (c, p) = tile(ui, t, tile_w, selected);
                            if c {
                                clicked = Some(t.id.clone());
                            }
                            if p {
                                play = Some(t.id.clone());
                            }
                        }
                    });
                    ui.add_space(8.0);
                }
                if let Some(id) = clicked {
                    self.select(&id);
                }
                if let Some(id) = play {
                    self.play(id);
                }
            });
    }
}

/// One tile. Returns (clicked, play_clicked).
fn tile(ui: &mut egui::Ui, t: &Title, w: f32, selected: bool) -> (bool, bool) {
    let mut clicked = false;
    let mut play = false;
    let frame = egui::Frame::group(ui.style())
        .fill(if selected {
            ui.visuals().selection.bg_fill.linear_multiply(0.35)
        } else {
            ui.visuals().faint_bg_color
        })
        .corner_radius(8.0)
        .inner_margin(egui::Margin::same(10));

    frame.show(ui, |ui| {
        ui.set_width(w);
        ui.vertical(|ui| {
            // Colour block stands in for an icon.
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 74.0), egui::Sense::click());
            ui.painter().rect_filled(rect, 6.0, tile_colour(&t.name));
            let initial: String = t.name.chars().take(1).collect::<String>().to_uppercase();
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                initial,
                egui::FontId::proportional(34.0),
                egui::Color32::from_white_alpha(210),
            );
            if resp.clicked() {
                clicked = true;
            }

            ui.add_space(6.0);
            ui.label(egui::RichText::new(&t.name).strong());
            ui.label(
                egui::RichText::new(format!(
                    "{} · {} · {}",
                    t.bottle,
                    human_size(t.size()),
                    t.backend.label()
                ))
                .weak()
                .small(),
            );
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.button("Play").clicked() {
                    play = true;
                }
                if ui.small_button("Details").clicked() {
                    clicked = true;
                }
            });
        });
    });
    (clicked, play)
}

impl Home {
    fn detail(&mut self, ui: &mut egui::Ui, t: &Title) {
        ui.add_space(8.0);
        ui.heading(&t.name);
        ui.label(
            egui::RichText::new(format!(
                "{} in bottle “{}” · runs on {}",
                flow::kind_label(t.kind),
                t.bottle,
                t.backend.label()
            ))
            .weak(),
        );
        if t.backend == Backend::CrossOver {
            ui.label(
                egui::RichText::new(
                    "CrossOver bottle — some titles only run here, because its Wine \
                     patches are not in the open tree exe-metal builds from.",
                )
                .small()
                .weak(),
            );
        }
        ui.add_space(8.0);

        ui.horizontal(|ui| {
            let busy = self.state == State::Working;
            if ui.add_enabled(!busy, egui::Button::new("Play")).clicked() {
                let id = t.id.clone();
                self.play(id);
            }
            if ui
                .add_enabled(!busy, egui::Button::new("Quit"))
                .on_hover_text("Close it properly so it saves its settings (a kill discards them)")
                .clicked()
            {
                let id = t.id.clone();
                self.quit_title(id);
            }
            if ui.button("Show in Finder").clicked() {
                let target = flow::reveal_target(&t.exe);
                let _ = std::process::Command::new("/usr/bin/open")
                    .arg("-R")
                    .arg(&t.exe)
                    .spawn()
                    .or_else(|_| {
                        std::process::Command::new("/usr/bin/open")
                            .arg(target)
                            .spawn()
                    });
            }
        });

        ui.add_space(10.0);
        ui.separator();
        ui.add_space(6.0);

        ui.label(egui::RichText::new("Executable").strong());
        ui.label(egui::RichText::new(t.exe.to_string_lossy()).small().weak());
        ui.add_space(4.0);
        if t.play_count > 0 {
            ui.label(
                egui::RichText::new(match t.play_count {
                    1 => "Played once".to_string(),
                    n => format!("Played {n} times"),
                })
                .small()
                .weak(),
            );
        }

        ui.add_space(10.0);
        ui.label(egui::RichText::new("Also in this folder").strong());
        if self.parts.is_empty() {
            ui.label(
                egui::RichText::new("Nothing else — this program is a single executable.")
                    .small()
                    .weak(),
            );
        } else {
            egui::ScrollArea::vertical()
                .max_height(200.0)
                .show(ui, |ui| {
                    let parts = self.parts.clone();
                    for p in &parts {
                        ui.horizontal(|ui| {
                            if ui
                                .small_button("Run")
                                .on_hover_text(p.path.to_string_lossy())
                                .clicked()
                            {
                                self.run_loose(&t.bottle, p.path.clone());
                            }
                            ui.label(
                                egui::RichText::new(format!(
                                    "{}  ({})",
                                    p.stem,
                                    human_size(p.size)
                                ))
                                .small(),
                            );
                        });
                    }
                });
        }

        ui.add_space(12.0);
        ui.separator();
        ui.add_space(6.0);
        if self.confirm_forget.as_deref() == Some(t.id.as_str()) {
            ui.label(egui::RichText::new("Remove from the library? No files are deleted.").small());
            ui.horizontal(|ui| {
                if ui.button("Remove").clicked() {
                    self.lib.remove(&t.id);
                    let _ = self.lib.save();
                    self.selected = None;
                    self.confirm_forget = None;
                    self.status = "removed from the library".into();
                }
                if ui.button("Cancel").clicked() {
                    self.confirm_forget = None;
                }
            });
        } else if ui.small_button("Remove from library").clicked() {
            self.confirm_forget = Some(t.id.clone());
        }
    }

    /// Run an executable that is not itself a registered title — a settings
    /// tool or server binary sitting beside the game.
    fn run_loose(&mut self, bottle: &str, exe: PathBuf) {
        let Some(layout) = self.layout.as_ref().map(|l| Layout {
            root: l.root.clone(),
        }) else {
            return;
        };
        let bottle = bottle.to_string();
        let name = exe
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        self.spawn(&format!("running {name}…"), move |tx| {
            let r = crate::bottle::Bottle::load(&layout, &bottle)
                .and_then(|b| crate::run::exec(&layout, &b, &exe, &[], true, None));
            match r {
                Ok(_) => {
                    let _ = tx.send(Msg::Done(format!("{name} is running")));
                }
                Err(e) => {
                    let _ = tx.send(Msg::Failed(format!("{e:#}")));
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_the_way_people_expect() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(5 * 1_048_576), "5 MB");
        assert_eq!(human_size(3 * 1_048_576 * 1024), "3.0 GB");
    }

    #[test]
    fn tile_colour_is_stable_per_name_and_differs_between_names() {
        assert_eq!(tile_colour("Celeste"), tile_colour("Celeste"));
        assert_ne!(tile_colour("Celeste"), tile_colour("Overwatch"));
    }
}
