mod agent_socket;
mod app;
mod interview;
mod ui;
mod views;

fn main() {
    let opts = match agent_socket::LaunchOptions::from_args(std::env::args()) {
        Ok(opts) => opts,
        Err(err) => {
            eprintln!("tod: {err}");
            eprintln!(
                "usage: tod [--width PX] [--height PX] [--agent-socket HOST:PORT] \
                 [--data-root PATH] [--agent mock|cursor] [--no-focus]"
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
    app::App::run(opts);
}
