use anyhow::Result;
use async_trait::async_trait;

use crate::Config;
use crate::review::Review;

pub mod github;
pub mod gitlab;

#[async_trait]
pub trait Api {
    async fn get_pr(&self, owner: &str, repo: &str, pr_num: u64, force: bool) -> Result<Review>;
    async fn submit_pr(&self, owner: &str, repo: &str, pr_num: u64, force: bool) -> Result<()>;
}

pub enum Host {
    Github,
    Gitlab,
}

impl Host {
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "github" => Some(Host::Github),
            "gitlab" => Some(Host::Gitlab),
            _ => None,
        }
    }

    pub fn init(self, config: Config) -> Result<Box<dyn Api>> {
        match self {
            Host::Github => github::Github::new(config).map(|gh| Box::new(gh) as Box<dyn Api>),
            Host::Gitlab => todo!()
        }
    }
}
