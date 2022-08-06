use gitlab::api::Query;
use lazy_static::lazy_static;
use regex::Regex;

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use gitlab::api::projects::merge_requests::discussions::{
    CreateMergeRequestDiscussion, Position, TextPosition,
};
use reqwest::StatusCode;
use serde_json::{json, Value};

use crate::api::Api;
use crate::parser::{LineLocation, ReviewAction};
use crate::review::{Extra, Review};
use crate::Config;

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
            &config.prr.token,
        )?;
        Ok(Self { config, client })
    }
}

impl Api for Gitlab {
    fn get_pr(&self, owner: &str, repo: &str, pr_num: u64, force: bool) -> Result<Review> {
        let endpoint = gitlab::api::projects::merge_requests::MergeRequestChanges::builder()
            .project(format!("{}/{}", owner, repo))
            .merge_request(pr_num)
            .build()?;
        let mr: gitlab::MergeRequestChanges = endpoint.query(&self.client)?;
        let diff = mr
            .changes
            .iter()
            .map(|change| {
                format!(
                    "diff --git a/{} b/{}\nindex {}..{} {}\n{}",
                    change.old_path,
                    change.new_path,
                    "aaaaaaa",
                    "bbbbbbb",
                    change.b_mode, // TODO a_mode?
                    change.diff,
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let diff_refs = mr.diff_refs.ok_or_else(|| anyhow!("Missing diff_refs in merge request. Won't be able to submit review."))?;
        let base_sha = diff_refs.base_sha.ok_or_else(|| anyhow!("Missing base_sha"))?.value().to_string();
        let head_sha = diff_refs.head_sha.ok_or_else(|| anyhow!("Missing head_sha"))?.value().to_string();
        let start_sha = diff_refs.start_sha.ok_or_else(|| anyhow!("Missing start_sha"))?.value().to_string();
        let mut extra = Extra::default();
        extra.base_sha(base_sha).head_sha(head_sha).start_sha(start_sha);
        Review::new(&self.config.workdir()?, diff, owner, repo, pr_num, extra, force)
    }

    fn submit_pr(&self, owner: &str, repo: &str, pr_num: u64, debug: bool) -> Result<()> {
        let review = Review::new_existing(&self.config.workdir()?, owner, repo, pr_num);
        let (review_action, review_comment, inline_comments) = review.comments()?;
        let metadata = review.read_metadata()?;

        if review_comment.is_empty() && inline_comments.is_empty() {
            bail!("No review comments");
        }

        // Make each comment a CreateMergeRequestDiscussion
        let discussions = inline_comments
            .iter()
            .map(|c| {
                let mut text_position = TextPosition::builder();
                // Both of these are required.
                text_position.old_path(&c.old_file).new_path(&c.new_file);

                // GitLab requires old_line for comments on removals, and new_line for comments on
                // additions.
                // https://docs.gitlab.com/ee/api/discussions.html#create-a-new-thread-in-the-merge-request-diff
                match c.line {
                    LineLocation::Left(line) => text_position.old_line(line),
                    LineLocation::Right(line) => text_position.new_line(line),
                    // NOTE: At least as of API version 15.3, Gitlab requires both left and right
                    // line number if commenting on an unchanged line. This is seen as a bug and
                    // might be fixed in the future.
                    // https://gitlab.com/gitlab-org/gitlab/-/issues/325161
                    LineLocation::Both(left, right) => text_position.old_line(left).new_line(right),
                };

                if c.start_line.is_some() {
                    todo!("Multiline comments to GitLab aren't implemented yet")
                }

                let base_sha = metadata.base_sha.as_ref().ok_or_else(|| anyhow!("Missing base_sha in metadata"))?;
                let head_sha = metadata.head_sha.as_ref().ok_or_else(|| anyhow!("Missing head_sha in metadata"))?;
                let start_sha = metadata.start_sha.as_ref().ok_or_else(|| anyhow!("Missing start_sha in metadata"))?;

                CreateMergeRequestDiscussion::builder()
                    .project(format!("{}/{}", owner, repo))
                    .merge_request(pr_num)
                    .body(&c.comment)
                    .position(
                        Position::builder()
                            .text_position(text_position.build()?)
                            .base_sha(base_sha)
                            .head_sha(head_sha)
                            .start_sha(start_sha)
                            .build()?,
                    )
                    .build()
                    .map_err(|e| anyhow!(e))
            })
            .collect::<Result<Vec<_>>>()?;

        for discussion in discussions {
            gitlab::api::ignore(discussion).query(&self.client)?;
        }

        Ok(())
    }
}
