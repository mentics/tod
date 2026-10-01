//! Live check of Agent Drive transcripts against a real workspace (needs
//! Agent Drive there). Mounts a node's folder in an existing sandbox,
//! appends through the mount, and reads it back over S3 with a range.
//!
//! `BL_WORKSPACE=.. BL_TOKEN=.. cargo run -p tod-sandbox --example e2e_drive -- <sandbox>`

use anyhow::{Context, Result, ensure};
use tod_sandbox::blaxel::Blaxel;
use tod_sandbox::drive::{self, DriveReader};

fn main() -> Result<()> {
    let sandbox = std::env::args().nth(1).context("usage: e2e_drive <sandbox>")?;
    let bx = Blaxel::new(std::env::var("BL_WORKSPACE")?, std::env::var("BL_TOKEN")?);
    let url = bx.get(&sandbox)?.and_then(|i| i.url).context("no such sandbox")?;
    let node = uuid_like();
    drive::mount_for_node(&bx, &url, "us-was-1", "e2e-user", &node)?;
    let res = bx.run(
        &url,
        &format!("printf '{{\"a\":1}}\n' >> {m}/p__s.jsonl && printf '{{\"b\":2}}\n' >> {m}/p__s.jsonl", m = drive::MOUNT_PATH),
        30,
    )?;
    ensure!(res.exit_code == 0, "append failed: {}", res.output());
    let reader = DriveReader::open(&bx)?.context("no drive")?;
    let list = reader.list("e2e-user", &node)?;
    println!("listed: {list:?}");
    ensure!(list.len() == 1 && list[0].name == "p__s" && list[0].size == 16, "unexpected listing");
    let all = reader.read("e2e-user", &node, "p__s", 0)?.context("missing")?;
    let tail = reader.read("e2e-user", &node, "p__s", 8)?.context("missing")?;
    ensure!(all == b"{\"a\":1}\n{\"b\":2}\n", "whole file: {all:?}");
    ensure!(tail == b"{\"b\":2}\n", "tail: {tail:?}");
    ensure!(reader.read("e2e-user", &node, "nope", 0)?.is_none(), "a missing file is None");
    println!("ok: appended through the mount, read over S3 (whole and from byte 8)");
    Ok(())
}

fn uuid_like() -> String {
    format!("e2e-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs())
}
