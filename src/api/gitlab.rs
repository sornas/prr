use lazy_static::lazy_static;
use regex::Regex;

// Use lazy static to ensure regex is only compiled once
lazy_static! {
    // Regex for url input. Url looks something like:
    //
    //      https://github.com/danobi/prr-test-repo/pull/6
    //
    pub static ref URL: Regex = Regex::new(r".*gitlab\.com/(?P<org>.+)/(?P<repo>.+)/-/merge_requests/(?P<pr_num>\d+)").unwrap();
}
