//! `tod-agentd --data-root <dir>`: started by `tod` (or `tod-cli`), never by
//! the operating system. See `doc/agentd.md`.

fn main() {
    let mut args = std::env::args().skip(1);
    let mut root = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--data-root" => root = args.next().map(std::path::PathBuf::from),
            "--build-stamp" => {
                println!("{}", tod_agentd::BUILD_STAMP);
                return;
            }
            other => {
                eprintln!("tod-agentd: unknown argument {other}");
                std::process::exit(2);
            }
        }
    }
    let Some(root) = root.or_else(|| std::env::var_os("TOD_DATA_ROOT").map(Into::into)) else {
        eprintln!("tod-agentd: --data-root is required");
        std::process::exit(2);
    };
    match tod_agentd::server::run(&root) {
        Ok(true) => {}
        Ok(false) => eprintln!("tod-agentd: another daemon holds {}", root.display()),
        Err(err) => {
            eprintln!("tod-agentd: {err:#}");
            std::process::exit(1);
        }
    }
}
