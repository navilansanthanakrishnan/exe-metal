//! exe-metal — run Windows games and applications on Apple Silicon macOS.
//!
//! Composes a Wine runtime with a DirectX-to-Metal renderer into an "engine",
//! then runs executables inside per-game "bottles" (Wine prefixes) with the
//! environment tuned for framerate.

mod auto;
mod bottle;
mod catalog;
mod diagnose;
mod doctor;
mod engine;
mod env;
mod hw;
mod install;
mod paths;
mod probe;
mod prune;
mod recent;
mod run;
mod setup;
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
    cmd: Cmd,
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
        /// Size of the virtual desktop, e.g. 1920x1080.
        #[arg(long)]
        size: Option<String>,
        /// Let Wine seize the whole display in fullscreen (traps the pointer).
        #[arg(long)]
        capture: bool,
        /// Stop Wine seizing the display. This is the default.
        #[arg(long, conflicts_with = "capture")]
        no_capture: bool,
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
    Ui,

    /// Download the default runtime and every renderer.
    Install {
        /// Runtime to install (defaults to the recommended one).
        #[arg(long)]
        runtime: Option<String>,
    },
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
    if let Err(e) = real_main() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn real_main() -> Result<()> {
    let cli = Cli::parse();
    let layout = Layout::discover()?;
    layout.ensure()?;

    match cli.cmd {
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
        } => {
            let mut b = bottle::Bottle::load(&layout, &name)?;
            if virtual_desktop {
                b.window.virtual_desktop = true;
            }
            if native {
                b.window.virtual_desktop = false;
            }
            if let Some(s) = size {
                b.window.desktop_size = s;
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
                println!("forgot {n} missing entr{}", if n == 1 { "y" } else { "ies" });
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

        Cmd::Ui => {
            #[cfg(feature = "ui")]
            {
                ui::launch()?;
            }
            #[cfg(not(feature = "ui"))]
            {
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
            SteamCmd::Launch { bottle: name, detach } => {
                let b = bottle::Bottle::load(&layout, &name)?;
                steam::launch(&layout, &b, detach)?;
            }
            SteamCmd::Setup { bottle: name, force } => {
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
        println!("    wine {}   32-bit: {}   winemetal: {}", r.wine_version, yn(r.win32), yn(r.winemetal));
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
