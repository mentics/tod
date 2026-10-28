//! The workspace's sandboxes that tod manages, with each one's billing
//! state, for the "Cloud VMs" window.
//!
//! A sandbox's deployment status is `DEPLOYED` whether it is running or in
//! standby, so the billing API says which: a sandbox in standby is not billed
//! for compute. Blaxel's live `state` (`RUNNING` / `STANDBY`) decides; the
//! billing API is hourly and lags a few minutes, so it only says when the
//! state is missing, and otherwise whether the sandbox was billed earlier
//! this hour.

use crate::blaxel::{Blaxel, rfc3339_utc};
use anyhow::{Result, anyhow, bail};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

const COMPUTE: &str = "sandbox_compute_runtime_gbs";

/// Whether compute is billed for a sandbox right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Billing {
    /// Running now (or, with no state from Blaxel, compute billed this hour).
    Active,
    /// In standby, and not billed for compute this hour either.
    Standby,
    /// In standby now, but compute was billed earlier this hour.
    StandbyBilledThisHour,
    /// Not deployed (`FAILED`, `DEPLOYING`, ...): its status instead.
    Other(String),
}

impl Billing {
    pub fn label(&self) -> String {
        match self {
            Billing::Active => "Active (billed)".to_string(),
            Billing::Standby => "Standby".to_string(),
            Billing::StandbyBilledThisHour => "Standby (billed earlier this hour)".to_string(),
            Billing::Other(s) => s.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Vm {
    pub name: String,
    pub created_at: Option<String>,
    pub labels: Vec<(String, String)>,
    pub billing: Billing,
    /// The newest golden: shown apart from the rest.
    pub golden: bool,
}

/// A sandbox made by tod: it carries a label whose key is `tod` or starts
/// with `tod-`.
pub fn is_managed(labels: &[(String, String)]) -> bool {
    labels.iter().any(|(k, _)| k == "tod" || k.starts_with("tod-"))
}

/// Billing state from what the listing and the billing window say.
pub fn billing_of(status: &str, state: Option<&str>, billed_this_hour: bool) -> Billing {
    if status != "DEPLOYED" {
        return Billing::Other(status.to_string());
    }
    match state {
        Some("RUNNING") => Billing::Active,
        Some("STANDBY") if billed_this_hour => Billing::StandbyBilledThisHour,
        Some("STANDBY") => Billing::Standby,
        Some(other) => Billing::Other(other.to_string()),
        None if billed_this_hour => Billing::Active,
        None => Billing::Standby,
    }
}

fn account_id(bx: &Blaxel) -> Result<String> {
    let path = format!("/workspaces/{}", bx.workspace());
    let ws = bx
        .get_path(&path, "get workspace")?
        .ok_or_else(|| anyhow!("workspace {} not found", bx.workspace()))?;
    ["accountId", "account_id"]
        .iter()
        .find_map(|k| ws[*k].as_str())
        .or_else(|| ws["account"]["id"].as_str())
        .map(str::to_string)
        .ok_or_else(|| {
            let keys: Vec<&str> = ws.as_object().map(|o| o.keys().map(String::as_str).collect()).unwrap_or_default();
            anyhow!("the workspace record has no account id (it has: {})", keys.join(", "))
        })
}

/// Sandbox name -> whether compute usage was billed in the current hour.
fn billed_this_hour(bx: &Blaxel, now_secs: i64) -> Result<HashMap<String, bool>> {
    let hour = now_secs - now_secs.rem_euclid(3600);
    let path = format!(
        "/accounts/{}/metrics?startTime={}&endTime={}&resolution=hourly&resourceType=sandbox\
         &billingDimension={COMPUTE}&groupBy=resource_name,billing_dimension&limit=100",
        account_id(bx)?,
        rfc3339_utc(hour),
        rfc3339_utc(hour + 3600),
    );
    let mut billed = HashMap::new();
    let mut cursor = String::new();
    loop {
        let url = if cursor.is_empty() { path.clone() } else { format!("{path}&cursor={cursor}") };
        let v = bx
            .get_path(&url, "get billing metrics")?
            .ok_or_else(|| anyhow!("billing metrics not found"))?;
        let Some(data) = v["data"].as_array() else {
            bail!("the billing reply has no `data` list");
        };
        for entry in data {
            let Some(name) = entry["resourceName"].as_str() else { continue };
            let usage = entry["summary"]["usage"].as_f64().unwrap_or(0.0);
            let cost = entry["summary"]["cost"].as_f64().unwrap_or(0.0);
            if usage > 0.0 || cost > 0.0 {
                billed.insert(name.to_string(), true);
            }
        }
        if v["meta"]["hasMore"].as_bool() != Some(true) {
            break;
        }
        cursor = v["meta"]["nextCursor"].as_str().unwrap_or_default().to_string();
        if cursor.is_empty() {
            break;
        }
    }
    Ok(billed)
}

/// Every tod-managed sandbox in the workspace (not terminated or being
/// deleted), the golden first, then by name. Asks Blaxel (which wakes
/// nothing): never on the UI thread. A failure of the billing API is an
/// error, not a guess.
pub fn list_managed(bx: &Blaxel, golden_name: Option<&str>) -> Result<Vec<Vm>> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;
    let records = bx.list_json()?;
    let billed = billed_this_hour(bx, now)?;
    let mut vms: Vec<Vm> = records
        .iter()
        .filter_map(|v| {
            let meta = &v["metadata"];
            let name = meta["name"].as_str()?.to_string();
            let status = v["status"].as_str().unwrap_or("UNKNOWN");
            if matches!(status, "TERMINATED" | "DELETING") {
                return None;
            }
            let mut labels: Vec<(String, String)> = meta["labels"]
                .as_object()
                .map(|m| m.iter().map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string())).collect())
                .unwrap_or_default();
            labels.sort();
            // The golden is ours by being the one nodes fork, label or not.
            if golden_name != Some(name.as_str()) && !is_managed(&labels) {
                return None;
            }
            let state = v["state"].as_str().filter(|s| !s.is_empty());
            Some(Vm {
                billing: billing_of(status, state, billed.contains_key(&name)),
                created_at: meta["createdAt"].as_str().map(str::to_string),
                golden: golden_name == Some(name.as_str()),
                name,
                labels,
            })
        })
        .collect();
    vms.sort_by(|a, b| b.golden.cmp(&a.golden).then_with(|| a.name.cmp(&b.name)));
    Ok(vms)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn l(k: &str) -> Vec<(String, String)> {
        vec![(k.to_string(), "x".to_string())]
    }

    #[test]
    fn managed_means_a_tod_label() {
        assert!(is_managed(&l("tod")));
        assert!(is_managed(&l("tod-kind")));
        assert!(!is_managed(&l("todo")));
        assert!(!is_managed(&[]));
    }

    #[test]
    fn billing_trusts_the_live_state_over_hourly_usage() {
        assert_eq!(billing_of("FAILED", Some("RUNNING"), true), Billing::Other("FAILED".into()));
        assert_eq!(billing_of("DEPLOYED", Some("RUNNING"), false), Billing::Active);
        assert_eq!(billing_of("DEPLOYED", Some("STANDBY"), true), Billing::StandbyBilledThisHour);
        assert_eq!(billing_of("DEPLOYED", Some("STANDBY"), false), Billing::Standby);
        assert_eq!(billing_of("DEPLOYED", None, true), Billing::Active);
        assert_eq!(billing_of("DEPLOYED", None, false), Billing::Standby);
    }
}
