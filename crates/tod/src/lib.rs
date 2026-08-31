mod agent_traffic;
mod app;
mod cli;
mod fleet;
mod install;
mod interview;
mod logging;
mod outline;
mod process;
mod process_bundle;
mod ui;
mod views;

#[cfg(feature = "agent-socket")]
mod agent_socket;

fn verify_process_bundle() -> anyhow::Result<()> {
    use process_bundle::{ProcessManifest, TodInstallPaths};

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
    let level = logging::resolve_level(opts.log_level, settings.log_level);
    logging::init(logging::InitConfig {
        log_dir: paths.log_dir(),
        level,
        max_size_kb: settings.log_max_size_kb,
        cli_override: opts.log_level.is_some(),
    })?;
    Ok(())
}
