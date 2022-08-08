use gitlab::api::Query;
use lazy_static::lazy_static;
use regex::Regex;
use sha1::{Digest, Sha1};

use anyhow::{anyhow, bail, Result};
use gitlab::api::projects::merge_requests::discussions::{
    CreateMergeRequestDiscussion, Position, TextPosition,
};

use crate::api::Api;
use crate::parser::LineLocation;
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

// NOTE: Used for multi-line comments (not currently implemented).
// https://docs.gitlab.com/15.2/ee/api/discussions.html#line-code
#[allow(unused)]
fn line_code(filename: &str, old_line: u64, new_line: u64) -> String {
    let mut hasher = Sha1::new();
    hasher.update(filename.as_bytes());
    let hash_str = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{:x}", byte))
        .collect::<Vec<_>>()
        .join("");
    format!("{}_{}_{}", hash_str, old_line, new_line)
}

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
        let diff_refs = mr.diff_refs.ok_or_else(|| {
            anyhow!("Missing diff_refs in merge request. Won't be able to submit review.")
        })?;
        let base_sha = diff_refs
            .base_sha
            .ok_or_else(|| anyhow!("Missing base_sha"))?
            .value()
            .to_string();
        let head_sha = diff_refs
            .head_sha
            .ok_or_else(|| anyhow!("Missing head_sha"))?
            .value()
            .to_string();
        let start_sha = diff_refs
            .start_sha
            .ok_or_else(|| anyhow!("Missing start_sha"))?
            .value()
            .to_string();
        let mut extra = Extra::default();
        extra
            .base_sha(base_sha)
            .head_sha(head_sha)
            .start_sha(start_sha);
        Review::new(
            &self.config.workdir()?,
            diff,
            owner,
            repo,
            pr_num,
            extra,
            force,
        )
    }

    fn submit_pr(&self, owner: &str, repo: &str, pr_num: u64, debug: bool) -> Result<()> {
        let review = Review::new_existing(&self.config.workdir()?, owner, repo, pr_num);
        let (review_action, review_comment, inline_comments) = review.comments()?; // TODO approve
        let metadata = review.read_metadata()?;
        let base_sha = metadata
            .base_sha
            .as_ref()
            .ok_or_else(|| anyhow!("Missing base_sha in metadata"))?;
        let head_sha = metadata
            .head_sha
            .as_ref()
            .ok_or_else(|| anyhow!("Missing head_sha in metadata"))?;
        let start_sha = metadata
            .start_sha
            .as_ref()
            .ok_or_else(|| anyhow!("Missing start_sha in metadata"))?;

        if review_comment.is_empty() && inline_comments.is_empty() {
            bail!("No review comments");
        }

        // Make each comment a CreateMergeRequestDiscussion
        let discussions = inline_comments
            .iter()
            .map(|c| {
                let mut position = Position::builder();
                // These are all required by the API.
                position
                    .base_sha(base_sha)
                    .head_sha(head_sha)
                    .start_sha(start_sha);

                let mut text_position = TextPosition::builder();
                // Both of these are required by the API, even if they're the same.
                text_position.old_path(&c.old_file).new_path(&c.new_file);

                /*
                 * FIXME: This was my try at multi line comments. It didn't work that well. They
                 * came out as normal comments (which might be because I included
                 * text_position.old_line and text_position.new_line, although they _are_ noted as
                 * "required" in the API documentation.). Looking at the request that is sent when
                 * using the web UI, Gitlab sends

                "line_range":{
                  "start":{
                    "line_code":"1b290eb385892bfd4870c08a785598e98c8691b7_12_10",
                    "type":null,
                    "old_line":12,
                    "new_line":10
                  },
                  "end":{
                    "line_code":"1b290eb385892bfd4870c08a785598e98c8691b7_15_14",
                    "type":null,
                    "old_line":15,
                    "new_line":14
                  }
                }

                 * Which doesn't match the documentation:
                 * 1) "type" shouldn't be allowed to be null ("Use new for lines added by this
                 *    commit, otherwise old.")
                 * 2) "start" should only have "line_code" and "type" (both required), not
                 *    "old_line" and "new_line".
                 *
                 * Anyway. They aren't rendered that differently.

                if let Some(start_line) = &c.start_line {
                    let mut line_range = LineRange::builder();
                    match start_line {
                        LineLocation::Left(old, new) => line_range.start(
                            LineCode::builder()
                                .line_code(line_code(&c.new_file, *old, *new))
                                .type_(LineType::Old)
                                .build()?,
                        ),
                        LineLocation::Right(old, new) => line_range.start(
                            LineCode::builder()
                                .line_code(line_code(&c.new_file, *old, *new))
                                .type_(LineType::New)
                                .build()?,
                        ),
                        LineLocation::Both(old, new) => line_range.start(
                            LineCode::builder()
                                .line_code(line_code(&c.new_file, *old, *new))
                                .type_(LineType::Old)
                                .build()?,
                        ),
                    };
                    match c.line {
                        LineLocation::Left(old, new) => line_range.end(
                            LineCode::builder()
                                .line_code(line_code(&c.new_file, old, new))
                                .type_(LineType::Old)
                                .build()?,
                        ),
                        LineLocation::Right(old, new) => line_range.end(
                            LineCode::builder()
                                .line_code(line_code(&c.new_file, old, new))
                                .type_(LineType::New)
                                .build()?,
                        ),
                        LineLocation::Both(old, new) => line_range.end(
                            LineCode::builder()
                                .line_code(line_code(&c.new_file, old, new))
                                .type_(LineType::Old)
                                .build()?,
                        ),
                    };
                    text_position.line_range(line_range.build()?);
                }
                */

                // GitLab requires old_line for comments on removals, and new_line for comments on
                // additions.
                // https://docs.gitlab.com/ee/api/discussions.html#create-a-new-thread-in-the-merge-request-diff
                match c.line {
                    LineLocation::Left(old, _) => text_position.old_line(old),
                    LineLocation::Right(_, new) => text_position.new_line(new),
                    // NOTE: At least as of API version 15.2, Gitlab requires both left and right
                    // line number if commenting on an unchanged line. This is seen as a bug and
                    // might be changed in the future.
                    // https://gitlab.com/gitlab-org/gitlab/-/issues/325161
                    LineLocation::Both(old, new) => text_position.old_line(old).new_line(new),
                };

                position.text_position(text_position.build()?);

                CreateMergeRequestDiscussion::builder()
                    .project(format!("{}/{}", owner, repo))
                    .merge_request(pr_num)
                    .body(&c.comment)
                    .position(position.build()?)
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
