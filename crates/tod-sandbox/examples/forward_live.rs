//! Carries a relay's port forwards to this machine: `forward_live <relay
//! base URL, e.g. http://127.0.0.1:2222> <[PORT=]HOST:PORT>...`. A manual
//! check of `doc/cloud-sandboxes/port-forwarding.md` against a relay
//! running locally (in Docker, since the relay is Linux-only).

use tod_sandbox::config::Forward;

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let url = args.next().expect("relay base URL");
    let forwards = args.map(|a| Forward::parse(&a)).collect::<anyhow::Result<Vec<_>>>()?;
    tod_sandbox::tunnel::run(&url, "unused", 1, &forwards, None).await
}
