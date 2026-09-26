//! `POST /users/<u>/nodes/<n>/flags`: the watchdog flags a node whose sandbox
//! it let sleep (`tod_sandbox::watchdog::Flag` as JSON).
//!
//! A flag is a pending decision on the node (`tod-cli decisions ask`), so it
//! reaches the user through the decisions panel and the attention queue like
//! any other question, with no schema of its own. It syncs to the app as an
//! ordinary change.

use crate::cli;
use crate::http::Response;
use std::path::Path;
use tod_sandbox::watchdog::Flag;
use tod_store::fleet::cli_relay::{self, RelayRequest};

/// The options offered with a flag, in order.
pub const OPTIONS: [&str; 2] = ["Wake it again", "Leave it asleep"];

/// The `tod-cli` arguments that record `flag` on `node`.
pub fn ask_args(node: &str, flag: &Flag) -> Vec<String> {
    let mut args = vec![
        "decisions".to_string(),
        "ask".into(),
        "--node".into(),
        node.into(),
        format!("{} Wake it again?", flag.message),
    ];
    for option in OPTIONS {
        args.push("--option".into());
        args.push(option.into());
    }
    args
}

pub fn handle(tod_cli: &Path, prefix: &[String], data_root: &Path, node: &str, body: &[u8]) -> Response {
    let flag: Flag = match serde_json::from_slice(body) {
        Ok(f) => f,
        Err(err) => return Response::text(400, format!("flag: {err}")),
    };
    let request = RelayRequest { env: Vec::new(), args: ask_args(node, &flag), stdin: Vec::new() };
    let reply = cli::run(tod_cli, prefix, data_root, request);
    match cli_relay::decode_reply(&reply) {
        Ok(r) if r.code == 0 => Response::text(200, String::from_utf8_lossy(&r.stdout).into_owned()),
        Ok(r) => Response::text(500, format!("decisions ask exited {}: {}", r.code, String::from_utf8_lossy(&r.stderr))),
        Err(err) => Response::text(500, format!("{err:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flag_becomes_a_decision_on_its_node() {
        let flag = Flag {
            sandbox: "sb".into(),
            message: "The watchdog let sb sleep.".into(),
            awake_since_ms: None,
            reasons: vec![],
        };
        let args = ask_args("n1", &flag);
        assert_eq!(&args[..4], ["decisions", "ask", "--node", "n1"]);
        assert!(args[4].starts_with("The watchdog let sb sleep."));
        assert_eq!(&args[5..], ["--option", "Wake it again", "--option", "Leave it asleep"]);
    }

    #[test]
    fn a_bad_body_is_a_400() {
        let r = handle(Path::new("no-such-tod-cli"), &[], Path::new("."), "n1", b"not json");
        assert_eq!(r.status, 400);
    }
}
