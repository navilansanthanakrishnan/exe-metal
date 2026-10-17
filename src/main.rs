//! exe-metal — run Windows games and applications on Apple Silicon macOS.
//!
//! Everything exe-metal does from a terminal, plus a library: open any `.exe`
//! the way you would open a downloaded file, and the applications it installs
//! show up on a home page you can launch them from.
//!
//! Composes a Wine runtime with a DirectX-to-Metal renderer into an "engine",
//! then runs executables inside per-game "bottles" (Wine prefixes) with the
//! environment tuned for framerate.

mod auto;
mod bottle;
mod catalog;
mod crossover;
mod diagnose;
mod doctor;
mod downloads;
mod engine;
mod env;
mod flow;
#[cfg(feature = "ui")]
mod home;
mod hw;
mod install;
mod library;
mod lifecycle;
mod paths;
mod probe;
mod prune;
mod recent;
mod registry;
mod run;
mod scan;
mod setup;
mod shortcut;
mod startmenu;
mod status;
mod steam;
#[cfg(feature = "ui")]
mod ui;
mod util;
mod window;

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use paths::Layout;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "exe-metal",
    about = "Run Windows games and applications on Apple Silicon macOS",
    version
)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Everything, in one command: check the machine, install, build a bottle.
    Setup {
        /// Name of the bottle to create.
        #[arg(long, default_value = "default")]
        bottle: String,
        #[arg(long, default_value = catalog::DEFAULT_RUNTIME)]
        runtime: String,
        #[arg(long, default_value = catalog::DEFAULT_RENDERER)]
        renderer: String,
        /// Recreate the bottle if it already exists.
        #[arg(long)]
        force: bool,
        /// Print the plan without downloading anything.
        #[arg(long)]
        dry_run: bool,
    },

    /// Run an executable, choosing the runtime and renderer from the file.
    Play {
        /// Path to the .exe.
        exe: PathBuf,
        /// Arguments passed through to the executable.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
        /// Use this bottle instead of choosing one.
        #[arg(long)]
        bottle: Option<String>,
        /// Return immediately instead of waiting for the program to exit.
        #[arg(long)]
        detach: bool,
        /// Show the Metal performance overlay.
        #[arg(long)]
        hud: bool,
    },

    /// Inspect an executable and show what would be used to run it.
    Probe { exe: PathBuf },

    /// Is a bottle healthy right now? Live processes, throughput, load.
    Status {
        /// Bottle to inspect. Omit for every bottle.
        bottle: Option<String>,
    },

    /// Bring a bottle up to current recommended settings.
    ///
    /// Fixes the things that are frozen at creation time and silently drift:
    /// thread synchronisation, missing wine-mono, window behaviour.
    Repair {
        /// Bottle to repair. Omit to repair every bottle.
        bottle: Option<String>,
    },

    /// Change window, pointer and display behaviour for a bottle.
    Window {
        bottle: String,
        /// Render into a resizable macOS window. Fixes a trapped pointer and
        /// lets the window move between monitors.
        #[arg(long)]
        virtual_desktop: bool,
        /// Go back to native windows.
        #[arg(long, conflicts_with = "virtual_desktop")]
        native: bool,
        /// Size of the window, e.g. 1920x1080, or `auto` for this display.
        #[arg(long)]
        size: Option<String>,
        /// Let Wine seize the whole display in fullscreen (traps the pointer).
        #[arg(long)]
        capture: bool,
        /// Stop Wine seizing the display. This is the default.
        #[arg(long, conflicts_with = "capture")]
        no_capture: bool,
        /// Windows DPI scale as a percentage: 100, 125, 150, 200, 250, 300.
        /// This is what makes Steam and games render at a sensible size on a
        /// Retina display rather than half of one.
        #[arg(long)]
        scale: Option<u32>,
    },

    /// Look for known failure signatures in a bottle's logs and configuration.
    Diagnose {
        /// Bottle to check. Omit to check every bottle.
        bottle: Option<String>,
    },

    /// List recently launched executables.
    Recent {
        /// Drop entries whose file no longer exists.
        #[arg(long)]
        prune: bool,
        /// Remove one entry by path.
        #[arg(long)]
        forget: Option<PathBuf>,
    },

    /// Open the window.
    Ui {
        /// Start full screen. Also toggleable with F11 once open.
        #[arg(long)]
        fullscreen: bool,
    },

    /// Download the default runtime and every renderer.
    Install {
        /// Runtime to install (defaults to the recommended one).
        #[arg(long)]
        runtime: Option<String>,
    },
    /// Open an .exe the way Finder would: install it, or launch it if it is
    /// already a known title. This is what a double-click calls.
    Open {
        /// Path to the .exe.
        exe: PathBuf,
        /// Put it in this bottle instead of choosing one.
        #[arg(long)]
        bottle: Option<String>,
        /// Return immediately instead of waiting for it to exit.
        #[arg(long)]
        detach: bool,
    },

    /// What is sitting in Downloads that could be installed, and where each
    /// one would go.
    Downloads {
        /// Also print the destination options for the top candidate.
        #[arg(long)]
        where_to: bool,
    },

    /// The installed applications this knows about.
    Library {
        /// Include helper binaries that are hidden from the home page.
        #[arg(long)]
        all: bool,
    },

    /// Launch something from the library by id.
    Launch {
        /// Title id, as shown by `library`.
        id: String,
        /// Return immediately instead of waiting for it to exit.
        #[arg(long)]
        detach: bool,
    },

    /// Look through every bottle for applications not yet in the library.
    Rescan {
        /// Only this bottle.
        #[arg(long)]
        bottle: Option<String>,
    },

    /// Show what an installed title is made of — its other executables.
    Parts {
        /// Title id.
        id: String,
    },

    /// Uninstall a Windows program, using its own uninstaller when it has one.
    Uninstall {
        /// Title id.
        id: String,
        /// If it has no uninstaller, delete its install directory instead.
        #[arg(long)]
        delete_files: bool,
    },

    /// Duplicate a bottle. Uses APFS copy-on-write: a 75 GB bottle clones in
    /// well under a second and costs no additional disk.
    Clone {
        /// Bottle to copy.
        source: String,
        /// Name for the copy.
        dest: String,
        /// Clone a CrossOver bottle instead of an exe-metal one.
        #[arg(long)]
        crossover: bool,
    },

    /// Pack a bottle into a .tar.gz.
    Archive {
        bottle: String,
        /// Destination file.
        out: PathBuf,
        #[arg(long)]
        crossover: bool,
    },

    /// Restore a bottle from a .tar.gz.
    Restore {
        /// Archive file.
        file: PathBuf,
        /// Restore under a different name.
        #[arg(long)]
        as_name: Option<String>,
        #[arg(long)]
        force: bool,
        #[arg(long)]
        crossover: bool,
    },

    /// Reboot a bottle's virtual Windows.
    Reboot {
        bottle: String,
        #[arg(long)]
        crossover: bool,
    },

    /// Make a native .app for a title, so it launches from the Dock,
    /// Spotlight or the Desktop like any Mac application.
    Shortcut {
        /// Title id.
        id: String,
        /// Also drop a link on the Desktop.
        #[arg(long)]
        desktop: bool,
        /// Remove the shortcut instead of creating it.
        #[arg(long)]
        remove: bool,
    },

    /// Publish a bottle's Windows Start Menu as native .app shortcuts —
    /// the equivalent of CrossOver's cxmenu.
    Menu {
        /// Bottle name.
        bottle: String,
        #[arg(long)]
        crossover: bool,
        /// Create the shortcuts, rather than only listing what was found.
        #[arg(long)]
        publish: bool,
        /// Also link them on the Desktop.
        #[arg(long)]
        desktop: bool,
    },

    /// File types a bottle knows how to open.
    Assoc {
        bottle: String,
        #[arg(long)]
        crossover: bool,
    },

    /// Read a bottle's Wine registry.
    Reg {
        bottle: String,
        /// Key, e.g. 'HKCU\Software\Wine\Mac Driver'.
        key: String,
        /// Value name. Omit to list every value under the key.
        value: Option<String>,
        #[arg(long)]
        crossover: bool,
    },

    /// The Windows version a bottle reports, and set it.
    Winver {
        bottle: String,
        /// New version, e.g. win10, win11, winxp. Omit to read.
        set: Option<String>,
        #[arg(long)]
        crossover: bool,
    },

    /// Everything a bug report needs: machine, backends, bottles, settings.
    Sysinfo,

    /// Close a running title gracefully so it saves its settings.
    /// A kill discards them; this posts WM_CLOSE.
    Quit {
        /// Title id.
        id: String,
    },

    /// Remove a title from the library. Does not delete any files.
    Forget {
        /// Title id.
        id: String,
    },

    /// Open the library window. This is also what running it with no
    /// arguments does, so the .app bundle needs no flags.
    Home {
        #[arg(long)]
        fullscreen: bool,
    },

    /// Open the library window with an .exe queued. What the .app bundle calls
    /// when Finder hands it a file.
    #[command(hide = true)]
    GuiOpen { exe: PathBuf },

    /// Show what can be installed and what each piece is good for.
    Catalog,
    /// Check the system and report anything that would stop a game running.
    Doctor,
    /// Find and terminate Wine processes left behind by a crashed bottle.
    Prune {
        /// Actually terminate them, rather than just listing.
        #[arg(long)]
        force: bool,
    },

    /// Create a bottle.
    Create {
        name: String,
        #[arg(long, default_value = catalog::DEFAULT_RUNTIME)]
        runtime: String,
        #[arg(long, default_value = catalog::DEFAULT_RENDERER)]
        renderer: String,
        /// Delete and recreate an existing bottle.
        #[arg(long)]
        force: bool,
    },
    /// List bottles.
    List,
    /// Show a bottle's configuration and resolved environment.
    Show { name: String },
    /// Delete a bottle and its shader cache.
    Remove { name: String },

    /// Run a Windows executable inside a bottle.
    Run {
        bottle: String,
        /// Path to the .exe — host path, path under drive_c, or C:\... form.
        exe: PathBuf,
        /// Arguments passed through to the executable. Game flags start with a
        /// hyphen, so they are taken verbatim rather than parsed here.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
        /// Return immediately instead of waiting for the program to exit.
        #[arg(long)]
        detach: bool,
        /// Show the Metal performance overlay for this run.
        #[arg(long)]
        hud: bool,
    },
    /// Stop everything running in a bottle.
    Kill { bottle: String },
    /// Open Wine's configuration UI for a bottle.
    Winecfg { bottle: String },
    /// Print the environment a bottle runs with.
    Env { bottle: String },

    /// Inspect or rebuild composed runtime+renderer engines.
    Engine {
        #[command(subcommand)]
        cmd: EngineCmd,
    },

    /// Steam: install the Windows client into a bottle, or launch it.
    Steam {
        #[command(subcommand)]
        cmd: SteamCmd,
    },
}

#[derive(Subcommand)]
enum EngineCmd {
    /// List engines that have been composed.
    List,
    /// Compose an engine now instead of on first use.
    Build { runtime: String, renderer: String },
    /// Delete an engine. It is rebuilt automatically when next needed.
    Remove { runtime: String, renderer: String },
}

#[derive(Subcommand)]
enum SteamCmd {
    /// Install the Windows Steam client into a bottle.
    Install { bottle: String },
    /// Launch Steam.
    Launch {
        bottle: String,
        #[arg(long)]
        detach: bool,
    },
    /// Create a bottle, install Steam and launch it, in one step.
    Setup {
        #[arg(default_value = "steam")]
        bottle: String,
        #[arg(long)]
        force: bool,
    },
    /// Tail Steam's own logs, which explain far more than Wine's.
    Logs { bottle: String },
}

fn main() {
    // Rust ignores SIGPIPE, so `exe-metal doctor | head` panicked on the first
    // write after `head` exited. A command-line tool should just stop.
    // SAFETY: called before any other thread exists.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    if let Err(e) = real_main() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn real_main() -> Result<()> {
    let cli = Cli::parse();
    let layout = Layout::discover()?;
    layout.ensure()?;

    // No subcommand from a terminal is someone asking what this does. From
    // anywhere else it was double-clicked, and the library window is the
    // right thing to show.
    let Some(cmd) = cli.cmd else {
        use std::io::IsTerminal;
        if std::io::stdout().is_terminal() {
            use clap::CommandFactory;
            Cli::command().print_help()?;
            return Ok(());
        }
        #[cfg(feature = "ui")]
        {
            return home::launch(false);
        }
        #[cfg(not(feature = "ui"))]
        {
            bail!("built without the ui feature — try `exe-metal --help`");
        }
    };

    match cmd {
        Cmd::Setup {
            bottle: name,
            runtime,
            renderer,
            force,
            dry_run,
        } => {
            setup::run(
                &layout,
                &setup::Options {
                    bottle: name,
                    runtime,
                    renderer,
                    force,
                    dry_run,
                },
            )?;
        }

        Cmd::Play {
            exe,
            args,
            bottle: name,
            detach,
            hud,
        } => {
            let code = auto::play(
                &layout,
                &auto::Request {
                    exe,
                    args,
                    detach,
                    hud,
                    bottle: name,
                },
            )?;
            if let Err(e) = flow::remember_played(&layout) {
                eprintln!("warn: could not add it to the library: {e:#}");
            }
            if code != 0 {
                std::process::exit(code);
            }
        }

        Cmd::Probe { exe } => {
            let d = auto::decide(&layout, &exe, None)?;
            auto::print_decision(&d);
        }

        Cmd::Status { bottle: name } => {
            let targets = match name {
                Some(n) => vec![bottle::Bottle::load(&layout, &n)?],
                None => bottle::list(&layout),
            };
            for (i, b) in targets.iter().enumerate() {
                if i > 0 {
                    println!();
                }
                status::run(&layout, b)?;
            }
        }

        Cmd::Repair { bottle: name } => {
            let targets = match name {
                Some(n) => vec![bottle::Bottle::load(&layout, &n)?],
                None => bottle::list(&layout),
            };
            if targets.is_empty() {
                println!("no bottles to repair");
            }
            for mut b in targets {
                println!("\n=== {} ===", b.name);
                let mut changed = false;

                // Thread sync: what a bottle created today would use.
                if let Some(rt) = catalog::runtime(&b.runtime) {
                    if rt.msync && !b.perf.msync {
                        b.perf.msync = true;
                        b.perf.esync = false;
                        changed = true;
                        println!("  sync     esync -> msync (measured 4x less CPU here)");
                    }
                }

                // Retina on with 96 DPI renders every Windows app at half
                // size. Bottles created before dpi existed have exactly that.
                if b.window.retina && window::dpi_percent(b.window.dpi) < 150 {
                    b.window.dpi = 192;
                    changed = true;
                    println!("  scale    Windows DPI -> 200% (was {}%, which renders half size on Retina)",
                        window::dpi_percent(96));
                }

                // Give existing bottles a native macOS window too.
                if !b.window.virtual_desktop {
                    b.window.virtual_desktop = true;
                    changed = true;
                    println!("  window   native macOS window on (traffic lights, resize, green-button full screen)");
                }
                if let Some(size) = window::detect_display_size() {
                    if b.window.desktop_size != size {
                        println!("  window   sized to this display: {size}");
                        b.window.desktop_size = size;
                        changed = true;
                    }
                }

                // Window behaviour: stop Wine seizing the display.
                if b.window.capture_displays {
                    b.window.capture_displays = false;
                    changed = true;
                    println!("  window   display capture off (frees the pointer)");
                }

                if changed {
                    b.save(&layout)?;
                } else {
                    println!("  config   already current");
                }

                // .NET, which needs the prefix rather than the config.
                let engine = layout.engine(&b.runtime, &b.renderer);
                if engine.exists() && install::bundles_mono(&engine) {
                    if env::mono_installed(&layout, &b) {
                        println!("  .NET     wine-mono present");
                    } else {
                        println!("  .NET     installing wine-mono (bundled, no download)");
                        install::mono_into_prefix(&layout, &b)?;
                        if env::mono_installed(&layout, &b) {
                            println!("  .NET     installed — .NET games will now run");
                        } else {
                            eprintln!("  warn     wine-mono still missing");
                        }
                    }
                }

                if let Err(e) = window::apply(&layout, &b) {
                    eprintln!("  warn     window settings: {e:#}");
                } else {
                    println!("  window   {}", window::describe(&b.window));
                }
            }
        }

        Cmd::Window {
            bottle: name,
            virtual_desktop,
            native,
            size,
            capture,
            no_capture,
            scale,
        } => {
            let mut b = bottle::Bottle::load(&layout, &name)?;
            if let Some(pct) = scale {
                b.window.dpi = window::snap_dpi((pct as f32 * 0.96).round() as u32);
            }
            if virtual_desktop {
                b.window.virtual_desktop = true;
            }
            if native {
                b.window.virtual_desktop = false;
            }
            if let Some(s) = size {
                b.window.desktop_size = if s == "auto" {
                    window::detect_display_size().unwrap_or(s)
                } else {
                    s
                };
                b.window.virtual_desktop = true;
            }
            if capture {
                b.window.capture_displays = true;
            }
            if no_capture {
                b.window.capture_displays = false;
            }
            b.save(&layout)?;
            window::apply(&layout, &b)?;
            println!("{}: {}", b.name, window::describe(&b.window));
            if b.window.capture_displays {
                println!("  note  the pointer will be confined to the game while fullscreen");
            }
        }

        Cmd::Diagnose { bottle: name } => {
            let targets = match name {
                Some(n) => vec![bottle::Bottle::load(&layout, &n)?],
                None => bottle::list(&layout),
            };
            if targets.is_empty() {
                println!("no bottles to check — create one with: exe-metal setup");
            }
            let mut worst_clean = true;
            for b in targets {
                println!("\n=== {} [{} + {}] ===", b.name, b.runtime, b.renderer);
                let report = diagnose::bottle(&layout, &b)?;
                report.print();
                worst_clean &= report.is_clean();
            }
            if !worst_clean {
                std::process::exit(1);
            }
        }

        Cmd::Recent { prune, forget } => {
            let mut r = recent::Recent::load(&layout);
            if let Some(p) = forget {
                r.forget(&p);
                r.save(&layout)?;
                println!("forgot {}", p.display());
            }
            if prune {
                let n = r.forget_missing();
                r.save(&layout)?;
                println!(
                    "forgot {n} missing entr{}",
                    if n == 1 { "y" } else { "ies" }
                );
            }
            let entries = r.list();
            if entries.is_empty() {
                println!("nothing yet — run: exe-metal play <exe>");
            }
            for e in entries {
                println!(
                    "{:<28} {:<16} {:<16} {}",
                    e.label(),
                    e.bottle,
                    recent::ago(e.last_run),
                    if e.exists() {
                        e.path.display().to_string()
                    } else {
                        format!("{} (missing)", e.path.display())
                    }
                );
            }
        }

        Cmd::Ui { fullscreen } => {
            #[cfg(feature = "ui")]
            {
                ui::launch(fullscreen)?;
            }
            #[cfg(not(feature = "ui"))]
            {
                let _ = fullscreen;
                bail!(
                    "this build has no window — it was compiled with --no-default-features.\n\
                     Rebuild with: cargo build --release"
                );
            }
        }

        Cmd::Install { runtime } => {
            util::require_apple_silicon()?;
            if !util::rosetta_present() {
                bail!(
                    "Rosetta 2 is required to execute x86-64 Windows code.\n\
                     Install it with: softwareupdate --install-rosetta --agree-to-license"
                );
            }
            let rt = runtime.unwrap_or_else(|| catalog::DEFAULT_RUNTIME.to_string());
            install::runtime(&layout, &rt)?;
            install::renderers_and_frameworks(&layout)?;
            println!("\nready. create a bottle with:\n  exe-metal create mygame");
        }

        Cmd::Open {
            exe,
            bottle: b,
            detach,
        } => {
            let outcome = flow::open_exe(&layout, &exe, b.as_deref(), detach)?;
            println!("\n{}", flow::describe_outcome(&outcome));
            if !outcome.registered.is_empty() {
                let lib = library::Library::load()?;
                for id in &outcome.registered {
                    if let Some(t) = lib.get(id) {
                        println!("  {}  {}", t.id, t.name);
                    }
                }
                println!("\nlaunch one with:  exe-metal launch <id>");
            }
            if outcome.support_found > 0 {
                println!(
                    "  ({} helper binaries were found and left out of the library)",
                    outcome.support_found
                );
            }
        }

        Cmd::Downloads { where_to } => {
            let found = downloads::scan();
            if found.is_empty() {
                println!("no .exe files in Downloads, Desktop or Documents.");
            } else {
                for f in &found {
                    println!(
                        "{:<44}  {:>8}  {}",
                        f.name,
                        format!("{:.0} MB", f.size as f64 / 1_048_576.0),
                        f.describe()
                    );
                }
                if where_to {
                    let top = &found[0];
                    println!("\nwhere '{}' would go:", top.name);
                    for o in downloads::options(&layout, &top.path) {
                        println!(
                            "  {} {:<24} {}",
                            if o.recommended { "*" } else { " " },
                            o.label,
                            o.reason
                        );
                    }
                    println!(
                        "\n  free space: {:.1} GB",
                        downloads::free_space(&layout) as f64 / 1_073_741_824.0
                    );
                }
            }
        }

        Cmd::Library { all } => {
            let mut lib = library::Library::load()?;
            let pruned = lib.prune_missing();
            if pruned > 0 {
                lib.save()?;
            }
            let rows: Vec<_> = if all {
                lib.titles.iter().collect()
            } else {
                lib.shelf()
            };
            if crossover::is_installed() {
                println!(
                    "backends: exe-metal + CrossOver{}\n",
                    crossover::version()
                        .map(|v| format!(" {v}"))
                        .unwrap_or_default()
                );
            }
            if rows.is_empty() {
                println!("nothing installed yet.");
                println!("open an installer with:  exe-metal open <path to .exe>");
            } else {
                for t in rows {
                    let played = match t.play_count {
                        0 => "never played".to_string(),
                        1 => "played once".to_string(),
                        n => format!("played {n} times"),
                    };
                    println!("{:<24}  {:<28}  {:<10}  {}", t.id, t.name, t.bottle, played);
                }
            }
            if pruned > 0 {
                println!("\n({pruned} entries dropped — their files were gone)");
            }
        }

        Cmd::Launch { id, detach } => {
            if let Some(client) = flow::launcher_for(&id) {
                println!(
                    "'{id}' is started by {client}, not by path, so {client} opens first and \
                     is asked to launch it.\nIf {client} shows Update instead of Play, press it; \
                     the game binary started directly fails with \"unable to authenticate login\"."
                );
            }
            let status = flow::launch_title(&layout, &id, detach)?;
            if status != 0 {
                bail!("exited with status {status}");
            }
        }

        Cmd::Rescan { bottle: b } => {
            let (added, pruned) = flow::rescan(&layout, b.as_deref())?;
            let lib = library::Library::load()?;
            if added.is_empty() {
                println!("no new applications found.");
            } else {
                println!("added {}:", added.len());
                for id in &added {
                    if let Some(t) = lib.get(id) {
                        println!("  {}  {}  [{}]", t.id, t.name, t.bottle);
                    }
                }
            }
            if pruned > 0 {
                println!("dropped {pruned} entries whose files were gone");
            }
        }

        Cmd::Parts { id } => {
            let lib = library::Library::load()?;
            let t = lib
                .get(&id)
                .ok_or_else(|| anyhow::anyhow!("no title '{id}' in the library"))?;
            println!("{}  [{}]", t.name, t.bottle);
            println!("  main    {}", t.exe.display());
            let parts = flow::parts_of(&layout, &id)?;
            if parts.is_empty() {
                println!("  (no other executables alongside it)");
            } else {
                for p in parts {
                    println!(
                        "  {:<7} {}  ({:.1} MB)",
                        match p.kind {
                            scan::Kind::App => "app",
                            scan::Kind::Support => "helper",
                            scan::Kind::Installer => "setup",
                        },
                        p.path.display(),
                        p.size as f64 / 1_048_576.0
                    );
                }
            }
        }

        Cmd::Uninstall { id, delete_files } => match lifecycle::uninstall(&layout, &id)? {
            lifecycle::Outcome::Ran {
                uninstaller,
                status,
                dropped,
            } => {
                println!("ran {}", uninstaller.exe.display());
                println!("  exit status {status}, {dropped} library entries dropped");
            }
            lifecycle::Outcome::NoUninstaller {
                install_root,
                bytes,
            } => {
                println!(
                    "no uninstaller found for '{id}'.\n  {} holds {}",
                    install_root.display(),
                    lifecycle::human(bytes)
                );
                if delete_files {
                    let r = lifecycle::remove_files(&layout, &id)?;
                    println!(
                        "deleted {} ({}), dropped {} library entries",
                        r.install_root.display(),
                        lifecycle::human(r.bytes),
                        r.titles_dropped
                    );
                } else {
                    println!("  re-run with --delete-files to remove it");
                }
            }
        },

        Cmd::Clone {
            source,
            dest,
            crossover: cx,
        } => {
            let backend = if cx {
                library::Backend::CrossOver
            } else {
                library::Backend::ExeMetal
            };
            let c = lifecycle::clone(&layout, backend, &source, &dest)?;
            println!(
                "cloned '{source}' -> '{dest}' in {:.2}s ({} top-level entries){}",
                c.took.as_secs_f64(),
                c.entries,
                if c.cow {
                    ", copy-on-write — no extra disk used"
                } else {
                    ""
                }
            );
        }

        Cmd::Archive {
            bottle: b,
            out,
            crossover: cx,
        } => {
            let backend = if cx {
                library::Backend::CrossOver
            } else {
                library::Backend::ExeMetal
            };
            let a = lifecycle::archive(&layout, backend, &b, &out)?;
            println!(
                "wrote {} ({}) in {:.1}s",
                a.file.display(),
                lifecycle::human(a.bytes),
                a.took.as_secs_f64()
            );
        }

        Cmd::Restore {
            file,
            as_name,
            force,
            crossover: cx,
        } => {
            let backend = if cx {
                library::Backend::CrossOver
            } else {
                library::Backend::ExeMetal
            };
            let b = lifecycle::restore(&layout, backend, &file, as_name.as_deref(), force)?;
            println!("restored bottle '{}' at {}", b.name, b.dir.display());
        }

        Cmd::Reboot {
            bottle: b,
            crossover: cx,
        } => {
            let backend = if cx {
                library::Backend::CrossOver
            } else {
                library::Backend::ExeMetal
            };
            lifecycle::reboot(&layout, backend, &b)?;
            println!("rebooted '{b}'");
        }

        Cmd::Shortcut {
            id,
            desktop,
            remove,
        } => {
            let lib = library::Library::load()?;
            let t = lib
                .get(&id)
                .ok_or_else(|| anyhow::anyhow!("no title '{id}' in the library"))?;
            if remove {
                if shortcut::remove(t)? {
                    println!("removed the shortcut for '{}'", t.name);
                } else {
                    println!("'{}' had no shortcut", t.name);
                }
            } else {
                shortcut::check_launchable(t)?;
                let p = shortcut::create(t, desktop)?;
                println!("created {}", p.display());
                if desktop {
                    println!("  and linked it on the Desktop");
                }
                println!(
                    "  launches: {} in bottle '{}' via {}",
                    t.name,
                    t.bottle,
                    t.backend.label()
                );
            }
        }

        Cmd::Menu {
            bottle: b,
            crossover: cx,
            publish,
            desktop,
        } => {
            let backend = if cx {
                library::Backend::CrossOver
            } else {
                library::Backend::ExeMetal
            };
            let prefix = startmenu::bottle_prefix(backend, &b)?;
            let entries = startmenu::scan_start_menu(&prefix);
            if entries.is_empty() {
                println!("no Start Menu shortcuts in '{b}'.");
            } else {
                for e in &entries {
                    println!(
                        "{:<30} {:<26} {}",
                        e.name,
                        e.menu_path,
                        if e.exists() { "ok" } else { "target missing" }
                    );
                }
                if publish {
                    let r = startmenu::publish(&b, backend, &entries, desktop)?;
                    println!("\npublished {}:", r.published.len());
                    for p in &r.published {
                        println!("  {}", p.bundle.display());
                    }
                    for s in &r.skipped {
                        println!("  skipped {} — {}", s.name, s.why);
                    }
                }
            }
        }

        Cmd::Assoc {
            bottle: b,
            crossover: cx,
        } => {
            let backend = if cx {
                library::Backend::CrossOver
            } else {
                library::Backend::ExeMetal
            };
            let prefix = startmenu::bottle_prefix(backend, &b)?;
            let a = startmenu::file_associations(&prefix);
            if a.is_empty() {
                println!("no file associations in '{b}'.");
            } else {
                for x in &a {
                    println!(
                        "{:<10} {:<18} {}",
                        x.extension,
                        x.prog_id,
                        x.program
                            .as_ref()
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|| "-".into())
                    );
                }
            }
        }

        Cmd::Reg {
            bottle: b,
            key,
            value,
            crossover: cx,
        } => {
            let backend = if cx {
                library::Backend::CrossOver
            } else {
                library::Backend::ExeMetal
            };
            let prefix = registry::prefix_of(&layout, backend, &b)?;
            match value {
                Some(v) => match registry::read(&prefix, &key, &v) {
                    Some(val) => println!("{v} = {}  [{}]", val.display(), val.type_name()),
                    None => println!("(no value '{v}' under {key})"),
                },
                None => {
                    let vals = registry::list(&prefix, &key);
                    if vals.is_empty() {
                        println!("(no values under {key})");
                    }
                    for (n, v) in vals {
                        println!("{n} = {v}");
                    }
                    for sk in registry::subkeys(&prefix, &key) {
                        println!("  [subkey] {sk}");
                    }
                }
            }
        }

        Cmd::Winver {
            bottle: b,
            set,
            crossover: cx,
        } => {
            let backend = if cx {
                library::Backend::CrossOver
            } else {
                library::Backend::ExeMetal
            };
            match set {
                Some(v) => {
                    registry::set_windows_version(&layout, backend, &b, &v)?;
                    println!("'{b}' now reports {v}");
                }
                None => {
                    let prefix = registry::prefix_of(&layout, backend, &b)?;
                    println!("{}", registry::windows_version_detail(&prefix));
                }
            }
        }

        Cmd::Sysinfo => println!("{}", registry::sysinfo(&layout)),

        Cmd::Quit { id } => {
            flow::quit_title(&layout, &id)?;
            println!("asked '{id}' to close (settings will be saved)");
        }

        Cmd::Forget { id } => {
            let mut lib = library::Library::load()?;
            if lib.remove(&id) {
                lib.save()?;
                println!("removed '{id}' from the library (no files were deleted)");
            } else {
                bail!("no title '{id}' in the library");
            }
        }

        Cmd::Home { fullscreen } => {
            #[cfg(feature = "ui")]
            {
                home::launch(fullscreen)?;
            }
            #[cfg(not(feature = "ui"))]
            {
                let _ = fullscreen;
                bail!("built without the ui feature");
            }
        }

        Cmd::GuiOpen { exe } => {
            #[cfg(feature = "ui")]
            {
                home::launch_with(exe)?;
            }
            #[cfg(not(feature = "ui"))]
            {
                let _ = exe;
                bail!("built without the ui feature");
            }
        }

        Cmd::Catalog => print_catalog(),

        Cmd::Doctor => doctor::run(&layout)?,

        Cmd::Prune { force } => prune::run(&layout, force)?,

        Cmd::Create {
            name,
            runtime,
            renderer,
            force,
        } => {
            util::require_apple_silicon()?;
            bottle::create(&layout, &name, &runtime, &renderer, force)?;
        }

        Cmd::List => {
            let bottles = bottle::list(&layout);
            if bottles.is_empty() {
                println!("no bottles yet — create one with: exe-metal create <name>");
            }
            for b in bottles {
                println!(
                    "{:<16} {} + {:<10} {}",
                    b.name,
                    b.runtime,
                    b.renderer,
                    util::human_size(&b.prefix(&layout))
                );
            }
        }

        Cmd::Show { name } => {
            let b = bottle::Bottle::load(&layout, &name)?;
            println!("{}", toml::to_string_pretty(&b)?);
            println!("prefix: {}", b.prefix(&layout).display());
            if let Ok(w) = b.wine(&layout) {
                println!("wine:   {}", w.display());
            }
            println!("\nenvironment:");
            println!("{}", env::as_shell(&env::build(&layout, &b)));
        }

        Cmd::Remove { name } => bottle::remove(&layout, &name)?,

        Cmd::Run {
            bottle: name,
            exe,
            args,
            detach,
            hud,
        } => {
            let mut b = bottle::Bottle::load(&layout, &name)?;
            if hud {
                b.perf.hud = true;
            }
            // Only capture to a log when detaching. In the foreground the
            // caller is watching the terminal, and swallowing the program's
            // own output there makes a working run look like a silent failure.
            let log = b.dir(&layout).join("logs/run.log");
            let sink = if detach { Some(log.as_path()) } else { None };
            let code = run::exec(&layout, &b, &exe, &args, detach, sink)?;
            if code != 0 {
                eprintln!("exited with {code}");
                // A bare exit code is rarely the useful part. Say what it means
                // if we recognise it.
                auto::explain_failure(&layout, &b, &log);
                std::process::exit(code);
            }
        }

        Cmd::Kill { bottle: name } => {
            let b = bottle::Bottle::load(&layout, &name)?;
            run::kill(&layout, &b)?;
        }

        Cmd::Winecfg { bottle: name } => {
            let b = bottle::Bottle::load(&layout, &name)?;
            run::winecfg(&layout, &b)?;
        }

        Cmd::Env { bottle: name } => {
            let b = bottle::Bottle::load(&layout, &name)?;
            println!("{}", env::as_shell(&env::build(&layout, &b)));
        }

        Cmd::Engine { cmd } => match cmd {
            EngineCmd::List => {
                let engines = engine::list(&layout);
                if engines.is_empty() {
                    println!("no engines composed yet — they are built on first use");
                }
                for e in engines {
                    println!("{:<32} {}", e, util::human_size(&layout.engines().join(&e)));
                }
            }
            EngineCmd::Build { runtime, renderer } => {
                let p = engine::ensure(&layout, &runtime, &renderer)?;
                println!("{}", p.display());
            }
            EngineCmd::Remove { runtime, renderer } => {
                engine::remove(&layout, &runtime, &renderer)?;
            }
        },

        Cmd::Steam { cmd } => match cmd {
            SteamCmd::Install { bottle: name } => {
                let b = bottle::Bottle::load(&layout, &name)?;
                steam::install(&layout, &b)?;
            }
            SteamCmd::Launch {
                bottle: name,
                detach,
            } => {
                let b = bottle::Bottle::load(&layout, &name)?;
                steam::launch(&layout, &b, detach)?;
            }
            SteamCmd::Setup {
                bottle: name,
                force,
            } => {
                util::require_apple_silicon()?;
                let b = if force || bottle::Bottle::load(&layout, &name).is_err() {
                    bottle::create(
                        &layout,
                        &name,
                        catalog::DEFAULT_RUNTIME,
                        catalog::DEFAULT_RENDERER,
                        force,
                    )?
                } else {
                    bottle::Bottle::load(&layout, &name)?
                };
                steam::install(&layout, &b)?;
                steam::launch(&layout, &b, true)?;
            }
            SteamCmd::Logs { bottle: name } => {
                let b = bottle::Bottle::load(&layout, &name)?;
                let dir = steam::logs_dir(&layout, &b);
                if !dir.is_dir() {
                    bail!("no Steam logs in bottle '{name}' yet");
                }
                println!("{}", dir.display());
                for f in ["bootstrap_log.txt", "connection_log.txt", "webhelper.txt"] {
                    let p = dir.join(f);
                    if p.is_file() {
                        println!("\n--- {f} ---");
                        let text = std::fs::read_to_string(&p).unwrap_or_default();
                        for l in text.lines().rev().take(15).collect::<Vec<_>>().iter().rev() {
                            println!("{}", l.trim_end_matches('\r'));
                        }
                    }
                }
            }
        },
    }
    Ok(())
}

fn print_catalog() {
    println!("runtimes (the Wine build that provides the Windows API)\n");
    for r in catalog::RUNTIMES {
        let dflt = if r.id == catalog::DEFAULT_RUNTIME {
            "  [default]"
        } else {
            ""
        };
        println!("  {}{}", r.id, dflt);
        println!(
            "    wine {}   32-bit: {}   winemetal: {}",
            r.wine_version,
            yn(r.win32),
            yn(r.winemetal)
        );
        println!("    {}\n", r.notes);
    }

    println!("renderers (DirectX translated to Metal)\n");
    for r in catalog::RENDERERS {
        let dflt = if r.id == catalog::DEFAULT_RENDERER {
            "  [default]"
        } else {
            ""
        };
        println!("  {}{}", r.id, dflt);
        println!("    {}", r.apis);
        println!("    {}\n", r.notes);
    }
}

fn yn(b: bool) -> &'static str {
    if b {
        "yes"
    } else {
        "no"
    }
}
