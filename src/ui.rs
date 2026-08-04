//! The window.
//!
//! Deliberately one screen: pick a file, look at what exe-metal worked out
//! about it, boot it. The recent list is the only navigation, because the only
//! thing anyone comes back for is the game they played yesterday.
//!
//! Nothing slow happens on the UI thread. Composing an engine clones ~700 MB
//! and re-signs it, and a first prefix boot takes several seconds — both would
//! freeze the window, so all of it runs on a worker and reports back over a
//! channel.

use crate::auto;
use crate::bottle::{self, Bottle};
use crate::diagnose;
use crate::hw::Machine;
use crate::paths::Layout;

use crate::recent::{self, Recent};
use crate::run;
use eframe::egui;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};

pub fn launch() -> anyhow::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([620.0, 560.0])
            .with_min_inner_size([480.0, 420.0])
            .with_title("exe-metal"),
        ..Default::default()
    };
    eframe::run_native(
        "exe-metal",
        options,
        Box::new(|_cc| Ok(Box::new(App::new()))),
    )
    .map_err(|e| anyhow::anyhow!("could not open the window: {e}"))
}

/// Messages from the worker thread.
enum Msg {
    Status(String),
    Failed(String),
    Launched(String),
}

#[derive(PartialEq)]
enum State {
    Idle,
    Working,
}

struct App {
    machine: Machine,
    selected: Option<PathBuf>,
    decision: Option<auto::Decision>,
    /// Set when inspection itself failed — a 0-byte file, or not a PE.
    select_error: Option<String>,
    recent: Vec<recent::Entry>,
    state: State,
    log: Vec<String>,
    rx: Option<Receiver<Msg>>,
}

impl App {
    fn new() -> Self {
        let layout = Layout::discover().ok();
        let recent = layout
            .as_ref()
            .map(|l| {
                let mut r = Recent::load(l);
                // Games get moved and deleted; a recent list full of dead paths
                // is worse than a short one.
                if r.forget_missing() > 0 {
                    let _ = r.save(l);
                }
                r.list()
            })
            .unwrap_or_default();

        Self {
            machine: Machine::detect(),
            selected: None,
            decision: None,
            select_error: None,
            recent,
            state: State::Idle,
            log: Vec::new(),
            rx: None,
        }
    }

    fn select(&mut self, path: PathBuf) {
        self.select_in(path, None);
    }

    /// Replaying something from the recent list must reuse the bottle it ran in
    /// last time. Re-deciding from scratch would name a fresh bottle and
    /// compose a second engine for a game that already has one — and throw away
    /// its warm shader cache in the process.
    fn select_in(&mut self, path: PathBuf, bottle: Option<&str>) {
        self.selected = Some(path.clone());
        self.decision = None;
        self.select_error = None;
        self.log.clear();

        let Ok(layout) = Layout::discover() else {
            self.select_error = Some("cannot find the exe-metal directory".into());
            return;
        };
        match auto::decide(&layout, &path, bottle) {
            Ok(d) => self.decision = Some(d),
            Err(e) => self.select_error = Some(format!("{e:#}")),
        }
    }

    fn boot(&mut self) {
        let Some(d) = &self.decision else { return };
        let exe = d.info.path.clone();
        let bottle_name = d.bottle.clone();
        let runtime = d.plan.runtime.to_string();
        let renderer = d.plan.renderer.to_string();
        let created = d.created;

        let (tx, rx) = channel();
        self.rx = Some(rx);
        self.state = State::Working;
        self.log.clear();

        std::thread::spawn(move || {
            let send = |t: &Sender<Msg>, m: Msg| {
                let _ = t.send(m);
            };
            let layout = match Layout::discover() {
                Ok(l) => l,
                Err(e) => {
                    send(&tx, Msg::Failed(format!("{e:#}")));
                    return;
                }
            };

            let b = if created {
                send(
                    &tx,
                    Msg::Status(format!("creating bottle '{bottle_name}' — first run composes the engine, this takes a minute")),
                );
                match bottle::create(&layout, &bottle_name, &runtime, &renderer, false) {
                    Ok(b) => b,
                    Err(e) => {
                        send(&tx, Msg::Failed(format!("{e:#}")));
                        return;
                    }
                }
            } else {
                match Bottle::load(&layout, &bottle_name) {
                    Ok(b) => b,
                    Err(e) => {
                        send(&tx, Msg::Failed(format!("{e:#}")));
                        return;
                    }
                }
            };

            send(&tx, Msg::Status("checking the bottle".into()));
            match diagnose::bottle(&layout, &b) {
                Ok(report) => {
                    let fatal: Vec<_> = report
                        .findings
                        .iter()
                        .filter(|f| f.severity == diagnose::Severity::Fatal)
                        .collect();
                    if !fatal.is_empty() {
                        let mut msg = String::from("this will not run yet:\n");
                        for f in fatal {
                            msg.push_str(&format!("\n• {}\n  fix: {}\n", f.problem, f.fix.trim()));
                        }
                        send(&tx, Msg::Failed(msg));
                        return;
                    }
                    for f in report.findings.iter() {
                        send(&tx, Msg::Status(format!("warning: {}", f.problem)));
                    }
                }
                Err(e) => send(&tx, Msg::Status(format!("could not run checks: {e:#}"))),
            }

            send(&tx, Msg::Status("launching".into()));
            let log = b.dir(&layout).join("logs/run.log");
            // Detached: the game must outlive this window.
            match run::exec(&layout, &b, &exe, &[], true, Some(log.as_path())) {
                Ok(_) => {
                    let mut r = Recent::load(&layout);
                    r.record(&exe, &b.name);
                    let _ = r.save(&layout);
                    send(
                        &tx,
                        Msg::Launched(format!(
                            "running in bottle '{}'. If nothing appears, the log is at {}",
                            b.name,
                            log.display()
                        )),
                    );
                }
                Err(e) => send(&tx, Msg::Failed(format!("{e:#}"))),
            }
        });
    }

    fn pump(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.rx else { return };
        let mut finished = false;
        while let Ok(msg) = rx.try_recv() {
            match msg {
                Msg::Status(s) => self.log.push(s),
                Msg::Failed(s) => {
                    self.log.push(s);
                    finished = true;
                }
                Msg::Launched(s) => {
                    self.log.push(s);
                    finished = true;
                    if let Ok(l) = Layout::discover() {
                        self.recent = Recent::load(&l).list();
                    }
                }
            }
        }
        if finished {
            self.state = State::Idle;
            self.rx = None;
        }
        if self.state == State::Working {
            // The worker has no way to wake the UI, so keep it ticking while
            // anything is in flight.
            ctx.request_repaint_after(std::time::Duration::from_millis(120));
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.pump(ui.ctx());

        egui::CentralPanel::default().show(ui, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.heading("exe-metal");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.weak(self.machine.summary());
                });
            });
            ui.separator();

            // A machine that cannot run anything should say so before the user
            // picks a file and waits.
            let blockers = self.machine.blocking_issues();
            if !blockers.is_empty() {
                for (problem, fix) in &blockers {
                    ui.colored_label(egui::Color32::from_rgb(220, 90, 90), format!("⚠ {problem}"));
                    ui.weak(fix);
                }
                ui.separator();
            }

            ui.add_space(4.0);
            let picking = self.state == State::Working;
            ui.add_enabled_ui(!picking, |ui| {
                if ui
                    .add(egui::Button::new("  Select .exe file…  ").min_size([0.0, 30.0].into()))
                    .clicked()
                {
                    if let Some(p) = rfd::FileDialog::new()
                        .add_filter("Windows executable", &["exe"])
                        .pick_file()
                    {
                        self.select(p);
                    }
                }
            });

            if let Some(err) = &self.select_error {
                ui.add_space(6.0);
                ui.colored_label(egui::Color32::from_rgb(220, 90, 90), err);
            }

            if let Some(d) = &self.decision {
                ui.add_space(8.0);
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.label(
                        egui::RichText::new(
                            d.info
                                .path
                                .file_name()
                                .map(|s| s.to_string_lossy().to_string())
                                .unwrap_or_default(),
                        )
                        .strong(),
                    );
                    ui.weak(d.info.path.display().to_string());
                    ui.add_space(4.0);
                    row(ui, "binary", d.info.arch.label());
                    let apis = if d.info.apis.is_empty() {
                        "none named in the binary".to_string()
                    } else {
                        d.info
                            .apis
                            .iter()
                            .map(|a| a.label())
                            .collect::<Vec<_>>()
                            .join(", ")
                    };
                    row(ui, "graphics", &apis);
                    row(ui, "renderer", &format!("{} — {}", d.plan.renderer, d.plan.why));
                    row(
                        ui,
                        "bottle",
                        &format!(
                            "{}{}",
                            d.bottle,
                            if d.created { "  (will be created)" } else { "" }
                        ),
                    );
                    for w in &d.plan.warnings {
                        ui.add_space(2.0);
                        ui.colored_label(egui::Color32::from_rgb(210, 160, 60), format!("⚠ {w}"));
                    }
                });

                ui.add_space(8.0);
                ui.add_enabled_ui(self.state == State::Idle && blockers.is_empty(), |ui| {
                    if ui
                        .add(egui::Button::new("  Boot it up  ").min_size([0.0, 34.0].into()))
                        .clicked()
                    {
                        self.boot();
                    }
                });
            }

            if self.state == State::Working {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("working…");
                });
            }

            if !self.log.is_empty() {
                ui.add_space(8.0);
                egui::ScrollArea::vertical()
                    .max_height(140.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for line in &self.log {
                            ui.label(line);
                        }
                    });
            }

            ui.add_space(10.0);
            ui.separator();
            ui.label(egui::RichText::new("Recent").strong());
            if self.recent.is_empty() {
                ui.weak("nothing yet — pick an .exe above");
            } else {
                let entries = self.recent.clone();
                egui::ScrollArea::vertical()
                    .id_salt("recent")
                    .max_height(170.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for e in entries {
                            ui.horizontal(|ui| {
                                let enabled = self.state == State::Idle;
                                ui.add_enabled_ui(enabled, |ui| {
                                    if ui.button("▶").on_hover_text("Boot this").clicked() {
                                        self.select_in(e.path.clone(), Some(&e.bottle));
                                        if self.decision.is_some() {
                                            self.boot();
                                        }
                                    }
                                });
                                ui.label(e.label());
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        ui.weak(recent::ago(e.last_run));
                                        ui.weak(format!("· {}", e.bottle));
                                    },
                                );
                            });
                        }
                    });
            }
        });
    }
}

fn row(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.horizontal_top(|ui| {
        ui.add_sized([70.0, 16.0], egui::Label::new(egui::RichText::new(label).weak()));
        // The renderer explanation is a full sentence and will not fit on one
        // line in a narrow window; without this it is silently clipped at the
        // window edge.
        ui.add(egui::Label::new(value).wrap());
    });
}
