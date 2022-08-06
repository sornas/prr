use gitlab::api::Query;
use lazy_static::lazy_static;
use regex::Regex;

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use reqwest::StatusCode;
use serde_json::{json, Value};

use crate::Config;
use crate::api::Api;
use crate::parser::{LineLocation, ReviewAction};
use crate::review::Review;

// Use lazy static to ensure regex is only compiled once
lazy_static! {
    // Regex for url input. Url looks something like:
    //
    //      https://github.com/danobi/prr-test-repo/pull/6
    //
    pub static ref URL: Regex = Regex::new(r".*gitlab\.com/(?P<org>.+)/(?P<repo>.+)/-/merge_requests/(?P<pr_num>\d+)").unwrap();
}

const GITLAB_BASE_URL: &str = "gitlab.com";

pub struct Gitlab {
    config: Config,
    client: gitlab::Gitlab,
}

impl Gitlab {
    pub fn new(config: Config) -> Result<Self> {
        let client = gitlab::Gitlab::new(
            config.prr.url.as_deref().unwrap_or(GITLAB_BASE_URL),
            &config.prr.token
        )?;
        Ok(Self {
            config,
            client,
        })
    }
}

impl Api for Gitlab {
    fn get_pr(
        &self,
        owner: &str,
        repo: &str,
        pr_num: u64,
        force: bool,
    ) -> Result<Review> {
        let endpoint = gitlab::api::projects::merge_requests::MergeRequestChanges::builder()
            .project(format!("{}/{}", owner, repo))
            .merge_request(pr_num)
            .build()?;
        let mr: gitlab::MergeRequestChanges = endpoint.query(&self.client)?;
        dbg!(&mr.changes);
        let diff = mr.changes.iter().map(|change| format!(
            "dif --git a/{} b/{}\nindex {}..{} {}\n{}",
            change.old_path,
            change.new_path,
            "aaaaaaa",
            "bbbbbbb",
            change.b_mode, // TODO a_mode?
            change.diff,
        )).collect::<Vec<_>>().join("\n");
        Review::new(&self.config.workdir()?, diff, owner, repo, pr_num, force)
    }

    fn submit_pr(&self, owner: &str, repo: &str, pr_num: u64, debug: bool) -> Result<()> {
        todo!()
    }
}
