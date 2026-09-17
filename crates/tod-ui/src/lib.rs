mod app;
mod cli;
mod drafting;
mod interview;
mod ui;
mod views;

#[cfg(feature = "agent-socket")]
mod agent_socket;

fn verify_process_bundle() -> anyhow::Result<()> {
    use tod_core::process_bundle::{ProcessManifest, TodInstallPaths};

    let install = TodInstallPaths::discover()?;
    let manifest = ProcessManifest::load(&install)?;
    let root = install.process_root();
    let phases = manifest.phase_count();
    println!("ok process_root={}", root.display());
    println!("ok manifest_phases={phases}");
    Ok(())
}

pub fn run() {
    let opts = match cli::LaunchOptions::from_args(std::env::args()) {
        Ok(opts) => opts,
        Err(err) => {
            eprintln!("tod: {err}");
            eprintln!(
                "usage: tod [--width PX] [--height PX] [--agent-socket HOST:PORT] \
                 [--agent-socket-port PORT] [--data-root PATH] [--agent mock|cursor|claude] \
                 [--log-level error|info|debug|trace] [--no-focus] [--verify-process-bundle]\n\
                 Data root: --data-root PATH overrides TOD_DATA_ROOT and install.toml (see README)."
            );
            std::process::exit(2);
        }
    };
    app::no_focus::set_enabled(opts.no_focus);
    if let Some(root) = interview::paths::resolve_startup_data_root(opts.data_root.as_deref()) {
        if let Err(err) = std::fs::create_dir_all(&root) {
            eprintln!("tod: failed to create data root {}: {err}", root.display());
            std::process::exit(2);
        }
        interview::set_data_root(root);
    }

    if opts.verify_process_bundle {
        if let Err(err) = verify_process_bundle() {
            eprintln!("tod: {err:#}");
            std::process::exit(1);
        }
        return;
    }

    let needs_data_root_setup = !interview::paths::is_data_root_configured();
    if !needs_data_root_setup {
        if let Err(err) = init_logging(&opts) {
            eprintln!("tod: {err:#}");
            std::process::exit(1);
        }
    }

    app::App::run(opts, needs_data_root_setup);
}

pub(crate) fn init_logging(opts: &cli::LaunchOptions) -> anyhow::Result<()> {
    let paths = interview::TodPaths::discover()?;
    paths.ensure_log_dir()?;
    let settings = interview::TodSettings::load(&paths)?;
    let level = tod_core::logging::resolve_level(opts.log_level, settings.log_level);
    tod_core::logging::init(tod_core::logging::InitConfig {
        log_dir: paths.log_dir(),
        level,
        max_size_kb: settings.log_max_size_kb,
        cli_override: opts.log_level.is_some(),
    })?;
    Ok(())
}
