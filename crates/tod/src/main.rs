mod agent_socket;
mod app;
mod interview;
mod logging;
mod ui;
mod views;

fn main() {
    let opts = match agent_socket::LaunchOptions::from_args(std::env::args()) {
        Ok(opts) => opts,
        Err(err) => {
            eprintln!("tod: {err}");
            eprintln!(
                "usage: tod [--width PX] [--height PX] [--agent-socket HOST:PORT] \
                 [--data-root PATH] [--agent mock|cursor] [--log-level error|info|debug|trace] \
                 [--no-focus]"
            );
            std::process::exit(2);
        }
    };
    if let Some(root) = opts.data_root.clone() {
        if let Err(err) = std::fs::create_dir_all(&root) {
            eprintln!(
                "tod: failed to create --data-root {}: {err}",
                root.display()
            );
            std::process::exit(2);
        }
        // Ensure discover() treats this as a repo root even before `.local` exists.
        let marker = root.join(".local");
        let _ = std::fs::create_dir_all(&marker);
        interview::set_data_root(root);
    }

    if let Err(err) = init_logging(&opts) {
        eprintln!("tod: {err:#}");
        std::process::exit(1);
    }

    app::App::run(opts);
}

fn init_logging(opts: &agent_socket::LaunchOptions) -> anyhow::Result<()> {
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
