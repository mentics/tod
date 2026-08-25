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
                "usage: tod [--width PX] [--height PX] [--agent-socket HOST:PORT]"
            );
            std::process::exit(2);
        }
    };
    app::App::run(opts);
}
