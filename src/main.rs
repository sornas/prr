use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use lazy_static::lazy_static;
use regex::{Captures, Regex};
use serde::Deserialize;

mod api;
mod parser;
mod review;

use api::Host;

// Use lazy static to ensure regex is only compiled once
lazy_static! {
    // Regex for short input. Example:
    //
    //      [<host>:]danobi/prr-test-repo/6
    //
    static ref SHORT: Regex = Regex::new(r"^((?P<host>\w+):)?(?P<org>[\w\-_]+)/(?P<repo>[\w\-_]+)/(?P<pr_num>\d+)").unwrap();
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Get a pull request and begin a review
    Get {
        /// Ignore unsubmitted review checks
        #[clap(short, long)]
        force: bool,
        /// Pull request to review (eg. `danobi/prr/24`)
        pr: String,
    },
    /// Submit a review
    Submit {
        /// Pull request to review (eg. `danobi/prr/24`)
        pr: String,
        #[clap(short, long)]
        debug: bool,
    },
}

#[derive(Parser, Debug)]
#[clap(version)]
struct Args {
    /// Path to config file
    #[clap(long, parse(from_os_str))]
    config: Option<PathBuf>,
    #[clap(subcommand)]
    command: Command,
}

#[derive(Debug, Deserialize)]
struct PrrConfig {
    /// API token for the given service
    // TODO per service
    token: String,
    /// Directory to place review files
    workdir: Option<String>,
    /// Instance URL
    ///
    /// Useful for hosted instances with custom URLs
    // TODO per service
    url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Config {
    prr: PrrConfig,
}

impl Config {
    fn workdir(&self, host: impl AsRef<Path>) -> Result<PathBuf> {
        match &self.prr.workdir {
            Some(d) => {
                if d.starts_with('~') {
                    bail!("Workdir may not use '~' to denote home directory");
                }

                Ok(PathBuf::from(d))
            }
            None => {
                let xdg_dirs = xdg::BaseDirectories::with_prefix("prr")?;
                Ok(xdg_dirs.get_data_home())
            }
        }
        .map(|p| p.join(host))
    }

    fn host_or<'s>(&'s self, default: &'s str) -> &'s str {
        self.prr.url.as_deref().unwrap_or(default)
    }
}

/// Parses a PR string and returns a tuple (Host::Github, "danobi", "prr", 24) or an error if
/// string is malformed
///
/// Allowed formats:
/// - `danobi/prr/24` (defaults to github)
/// - `gitlab:danobi/prr/24`
fn parse_pr_str<'a>(s: &'a str) -> Result<(Host, String, String, u64)> {
    let f = |host_override: Option<Host>, captures: Captures<'a>|
        -> Result<(Host, String, String, u64)>
    {
        let host = host_override.unwrap_or_else(
            || captures
                .name("host")
                .and_then(|capture| Host::from_str(capture.as_str()))
                .unwrap_or(Host::Github)
        );
        let owner = captures.name("org").unwrap().as_str().to_owned();
        let repo = captures.name("repo").unwrap().as_str().to_owned();
        let pr_nr: u64 = captures
            .name("pr_num")
            .unwrap()
            .as_str()
            .parse()
            .context("Failed to parse pr number")?;

        Ok((host, owner, repo, pr_nr))
    };

    if let Some(captures) = SHORT.captures(s) {
        f(None, captures)
    } else if let Some(captures) = api::github::URL.captures(s) {
        f(Some(Host::Github), captures)
    } else if let Some(captures) = api::gitlab::URL.captures(s) {
        f(Some(Host::Gitlab), captures)
    } else {
        bail!("Invalid PR ref format")
    }
}

fn main() -> Result<()> {
    let args = Args::parse();

    // Figure out where config file is
    let config_path = match args.config {
        Some(c) => c,
        None => {
            let xdg_dirs = xdg::BaseDirectories::with_prefix("prr")?;
            xdg_dirs.get_config_file("config.toml")
        }
    };

    let config_contents = std::fs::read_to_string(config_path).context("Failed to read config")?;
    let config: Config = toml::from_str(&config_contents).context("Failed to parse toml")?;

    match args.command {
        Command::Get { pr, force } => {
            let (host, owner, repo, pr_num) = parse_pr_str(&pr)?;
            let api = host.init(config)?;
            let review = api.get_pr(&owner, &repo, pr_num, force)?;
            println!("{}", review.path().display());
        }
        Command::Submit { pr, debug } => {
            let (host, owner, repo, pr_num) = parse_pr_str(&pr)?;
            let api = host.init(config)?;
            api.submit_pr(&owner, &repo, pr_num, debug)?;
        }
    }

    Ok(())
}
