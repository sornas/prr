use lazy_static::lazy_static;
use regex::Regex;

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use octocrab::Octocrab;
use reqwest::StatusCode;
use serde_json::{json, Value};

use crate::Config;
use crate::api::Api;
use crate::parser::{LineLocation, ReviewAction};
use crate::review::{Extra, Review};

// Use lazy static to ensure regex is only compiled once
lazy_static! {
    // Regex for url input. Url looks something like:
    //
    //      https://github.com/danobi/prr-test-repo/pull/6
    //
    pub static ref URL: Regex = Regex::new(r".*github\.com/(?P<org>.+)/(?P<repo>.+)/pull/(?P<pr_num>\d+)").unwrap();
}

const GITHUB_BASE_URL: &str = "https://api.github.com";

/// Main struct that coordinates all business logic and talks to GH
pub struct Github {
    /// User config
    config: Config,
    /// Instantiated github client
    crab: Octocrab,
}

impl Github {
    pub fn new(config: Config) -> Result<Self> {
        let octocrab = Octocrab::builder()
            .personal_token(config.prr.token.clone())
            .base_url(config.prr.url.as_deref().unwrap_or(GITHUB_BASE_URL))
            .context("Failed to parse github base URL")?
            .build()
            .context("Failed to create GH client")?;

        Ok(Self {
            config,
            crab: octocrab,
        })
    }
}

impl Api for Github {
    fn get_pr(
        &self,
        owner: &str,
        repo: &str,
        pr_num: u64,
        force: bool,
    ) -> Result<Review> {
        tokio::runtime::Runtime::new()?.block_on(async {
            let diff = self
                .crab
                .pulls(owner, repo)
                .get_diff(pr_num)
                .await
                .context("Failed to fetch diff")?;

            Review::new(&self.config.workdir()?, diff, owner, repo, pr_num, Extra::default(), force)
        })
    }

    fn submit_pr(&self, owner: &str, repo: &str, pr_num: u64, debug: bool) -> Result<()> {
        tokio::runtime::Runtime::new()?.block_on(async {
            let review = Review::new_existing(&self.config.workdir()?, owner, repo, pr_num);
            let (review_action, review_comment, inline_comments) = review.comments()?;

            if review_comment.is_empty() && inline_comments.is_empty() {
                bail!("No review comments");
            }

            let body = json!({
                "body": review_comment,
                "event": match review_action {
                    ReviewAction::Approve => "APPROVE",
                    ReviewAction::RequestChanges => "REQUEST_CHANGES",
                    ReviewAction::Comment => "COMMENT"
                },
                "comments": inline_comments
                    .iter()
                    .map(|c| {
                        let (line, side) = match c.line {
                            LineLocation::Left(line) => (line, "LEFT"),
                            LineLocation::Right(line) | LineLocation::Both(_, line) => (line, "RIGHT"),
                        };

                        let mut json_comment = json!({
                            "path": c.new_file,
                            "line": line,
                            "body": c.comment,
                            "side": side,
                        });
                        if let Some(start_line) = &c.start_line {
                            let (line, side) = match start_line {
                                LineLocation::Left(line) => (line, "LEFT"),
                                LineLocation::Right(line) | LineLocation::Both(_, line) => (line, "RIGHT"),
                            };

                            json_comment["start_line"] = (*line).into();
                            json_comment["start_side"] = side.into();
                        }

                        json_comment
                    })
                    .collect::<Vec<Value>>(),
            });

            if debug {
                println!("{}", serde_json::to_string_pretty(&body)?);
            }

            let path = format!("/repos/{}/{}/pulls/{}/reviews", owner, repo, pr_num);
            match self
                .crab
                ._post(self.crab.absolute_url(path)?, Some(&body))
                .await
            {
                Ok(resp) => {
                    let status = resp.status();
                    if status != StatusCode::OK {
                        let text = resp
                            .text()
                            .await
                            .context("Failed to decode failed response")?;
                        bail!("Error during POST: Status code: {}, Body: {}", status, text);
                    }

                    review
                        .mark_submitted()
                        .context("Failed to update review metadata")?;

                    Ok(())
                }
                // GH is known to send unescaped control characters in JSON responses which
                // serde will fail to parse (not that it should succeed)
                Err(octocrab::Error::Json {
                    source: _,
                    backtrace: _,
                }) => {
                    eprintln!("Warning: GH response had invalid JSON");
                    Ok(())
                }
                Err(e) => bail!("Error during POST: {}", e),
            }
        })
    }
}
