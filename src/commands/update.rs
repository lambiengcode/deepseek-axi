//! Self-update, hand-built — unlike the Node build (which got `update`/
//! `update --check` for free from `axi-sdk-js`), the Rust build has no
//! shared-ecosystem SDK, so this is entirely ours to write (a real,
//! disclosed cost of leaving the AXI ecosystem's runtime — RUST_MIGRATION_PLAN.md §3).
//! Uses GitHub Releases (via the `self_update` crate) instead of an npm
//! registry lookup, since a Rust binary isn't published to npm.

use crate::error::CliError;
use crate::output::Output;
use clap::Args;
use serde_json::json;

const REPO_OWNER: &str = "lambiengcode";
const REPO_NAME: &str = "deepseek-axi";
const BIN_NAME: &str = "deepseek-axi";

#[derive(Args, Debug)]
pub struct UpdateArgs {
    /// Report current vs latest without installing
    #[arg(long)]
    pub check: bool,
}

pub fn run(args: &UpdateArgs) -> Result<Output, CliError> {
    let current = env!("CARGO_PKG_VERSION");

    let releases = self_update::backends::github::ReleaseList::configure()
        .repo_owner(REPO_OWNER)
        .repo_name(REPO_NAME)
        .build()
        .and_then(|b| b.fetch());

    let latest = match releases {
        Ok(list) if !list.is_empty() => list[0].version.clone(),
        Ok(_) => {
            return Err(CliError::new(
                format!("{REPO_OWNER}/{REPO_NAME} has no published releases yet"),
                "UPDATE_ERROR",
                vec!["This build hasn't been published — build from source instead".into()],
            ));
        }
        Err(e) => {
            return Err(CliError::new(
                format!("Could not check for updates: {e}"),
                "UPDATE_ERROR",
                vec![format!(
                    "Confirm {REPO_OWNER}/{REPO_NAME} is correct, or check manually at https://github.com/{REPO_OWNER}/{REPO_NAME}/releases"
                )],
            ));
        }
    };

    let available = latest != current;

    if args.check {
        return Ok(Output::Data(json!({
            "update": {"package": BIN_NAME, "current": current, "latest": latest, "available": available},
            "help": if available {
                vec![format!("Run `{BIN_NAME} update` to upgrade")]
            } else {
                vec!["Already up to date".to_string()]
            },
        })));
    }

    if !available {
        return Ok(Output::Data(
            json!({"update": format!("{BIN_NAME} is already up to date ({current})")}),
        ));
    }

    let status = self_update::backends::github::Update::configure()
        .repo_owner(REPO_OWNER)
        .repo_name(REPO_NAME)
        .bin_name(BIN_NAME)
        .show_download_progress(false)
        .current_version(current)
        .build()
        .and_then(|u| u.update())
        .map_err(|e| CliError::new(format!("Update failed: {e}"), "UPDATE_ERROR", vec![]))?;

    Ok(Output::Data(
        json!({"update": format!("{BIN_NAME} upgraded {current} -> {}", status.version())}),
    ))
}
