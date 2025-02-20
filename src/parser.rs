use anyhow::{anyhow, bail, Context, Result};
use lazy_static::lazy_static;
use regex::Regex;

// Use lazy static to ensure regex is only compiled once
lazy_static! {
    // Regex for the start of a hunk. The start of a hunk should look like:
    //
    //      `@@ -731,7 +731,7 @@[...]`
    //
    static ref HUNK_START: Regex = Regex::new(r"^@@ -(?P<lstart>\d+),\d+ \+(?P<rstart>\d+),\d+ @@").unwrap();
    // Regex for start of a file diff. The start of a file diff should look like:
    //
    //      `diff --git a/ch1.txt b/ch1.txt`
    //
    static ref DIFF_START: Regex = Regex::new(r"^diff --git a/(?P<old>.+) b/(?P<new>.+)$").unwrap();
}

/// The location of a line
///
/// The distinction between Left and Right is important when commenting on
/// deleted or added lines. A useful way to think about the line location is
/// the line number a comment should be attached to in the file pre-change (left)
/// or the file post-change (right).
///
/// The two numbers are the line location before and after the applied diff.
#[derive(Debug, PartialEq, Clone)]
pub enum LineLocation {
    /// The "red"/deleted side of the diff
    Left(u64, u64),
    /// The "green"/added side of the diff
    Right(u64, u64),
    /// The "white"/existing side of the diff
    Both(u64, u64),
}

/// Represents a single inline comment on a review
#[derive(Debug, PartialEq)]
pub struct InlineComment {
    /// File the comment is in, before an eventual rename
    pub old_file: String,
    /// File the comment is in, after an eventual rename
    pub new_file: String,
    pub line: LineLocation,
    /// For a spanned comment, the first line of the span. See `line` for docs on semantics
    pub start_line: Option<LineLocation>,
    /// The user-supplied review comment
    pub comment: String,
}

#[derive(Debug, PartialEq)]
pub enum ReviewAction {
    Approve,
    RequestChanges,
    Comment,
}

/// Represents a comment of some sort on a review
#[derive(Debug, PartialEq)]
pub enum Comment {
    /// Overall review comment (the summary comment)
    Review(String),
    /// An inline comment (attached to a line)
    Inline(InlineComment),
    /// Overall approve, reject, or comment on review
    ReviewAction(ReviewAction),
}

#[derive(Default)]
struct StartState {
    /// Each line of review-level comment is stored as an entry
    comment: Vec<String>,
}

struct FilePreambleState {
    /// Relative path of the file under diff, before rename
    old_file: String,
    /// Relative path of the file under diff, after rename
    new_file: String,
}

#[derive(Clone)]
struct FileDiffState {
    /// Relative path of the file under diff, before rename
    old_file: String,
    /// Relative path of the file under diff, after rename
    new_file: String,
    /// Current left line position. See `LineLocation` for docs on semantics of `line`
    left_line: u64,
    /// Current right line position. See `LineLocation` for docs on semantics of `line`
    right_line: u64,
    /// Current line position
    line: LineLocation,
    /// First line of the span. See `LineLocation` for docs on
    /// semantics of `line`
    span_start_line: Option<LineLocation>,
}

struct SpanStartOrCommentState {
    /// State of the file diff before we entered this state
    file_diff_state: FileDiffState,
}

struct CommentState {
    /// State of the file diff before we entered comment processing
    file_diff_state: FileDiffState,
    /// Each line of comment is stored as an entry
    comment: Vec<String>,
}

/// State machine states
///
/// Only the following state transitions are valid:
///
///                                  +---------------+
///                                  |               |
///                                  v               |
///     Start -> FilePreamble -> FileDiff -> StartSpanOrComment -> Comment
///                 ^    ^        |  | ^                            ^   |
///                 |    |        |  | |                            |   |
///                 |    +--------+--+-+----------------------------+---+
///                 |             |  |                              |
///                 +-------------+  +------------------------------+
///
enum State {
    /// Starting state
    Start(StartState),
    /// The `diff --git a/...` preamble as well as the lines before the first hunk
    FilePreamble(FilePreambleState),
    /// We are inside the diff of a file
    FileDiff(FileDiffState),
    /// We are either the start of a span or the beginning of a comment
    ///
    /// The uncertainty comes from the fact that comments typically begin with one
    /// or more newlines
    SpanStartOrComment(SpanStartOrCommentState),
    /// We are inside a user-supplied comment
    Comment(CommentState),
}

/// Simple state machine to parse a review file
pub struct ReviewParser {
    state: State,
}

fn is_diff_header(s: &str) -> bool {
    s.starts_with("diff --git ")
}

/// Parses lines in the form of `@prr DIRECTIVE`
///
/// Returns Some(directive) if found, else None
fn is_prr_directive(s: &str) -> Option<&str> {
    let t = s.trim();
    if let Some(d) = t.strip_prefix("@prr ") {
        Some(d)
    } else {
        None
    }
}

/// Parses the new filename out of a diff header
fn parse_diff_header(line: &str) -> Result<(String, String)> {
    if let Some(captures) = DIFF_START.captures(line) {
        let old = captures.name("old").unwrap().as_str().trim().to_string();
        let new = captures.name("new").unwrap().as_str().trim().to_string();

        Ok((old, new))
    } else {
        Err(anyhow!("Invalid diff header: could not parse"))
    }
}

/// Parses the starting left & right lines out of the hunk start
fn parse_hunk_start(line: &str) -> Result<Option<(u64, u64)>> {
    if let Some(captures) = HUNK_START.captures(line) {
        let hunk_start_line_left: u64 = captures
            .name("lstart")
            .unwrap()
            .as_str()
            .parse()
            .context("Failed to parse hunk start left line")?;

        let hunk_start_line_right: u64 = captures
            .name("rstart")
            .unwrap()
            .as_str()
            .parse()
            .context("Failed to parse hunk start right line")?;

        // Hunks starting at line 0 implies the file was new (left side) or deleted
        // (right side). Ensure that at least one of the start lines is non-zero.
        //
        // For the side that is zero, we allow the "UB" of underflowing when the
        // caller subtracts 1 from the result. That is OK b/c we will never use that
        // value, as you cannot comment on text that does not exist.
        if hunk_start_line_left + hunk_start_line_right == 0 {
            bail!("Both hunks lines start at 0");
        }

        return Ok(Some((hunk_start_line_left, hunk_start_line_right)));
    }

    Ok(None)
}

fn is_left_line(line: &str) -> bool {
    line.starts_with('-')
}

fn is_right_line(line: &str) -> bool {
    line.starts_with('+')
}

/// Given the current line and line positions, returns what the next line positions should be
fn get_next_lines(line: &str, left: u64, right: u64) -> (u64, u64) {
    if is_left_line(line) {
        (left + 1, right)
    } else if is_right_line(line) {
        (left, right + 1)
    } else {
        (left + 1, right + 1)
    }
}

impl ReviewParser {
    pub fn new() -> ReviewParser {
        ReviewParser {
            state: State::Start(StartState::default()),
        }
    }

    pub fn parse_line(&mut self, mut line: &str) -> Result<Option<Comment>> {
        let is_quoted = line.starts_with("> ");
        if is_quoted {
            line = &line[2..];
        }

        match &mut self.state {
            State::Start(state) => {
                if is_quoted {
                    if !is_diff_header(line) {
                        bail!("Expected diff header from start state, found '{}'", line);
                    }

                    let mut review_comment = None;
                    if !state.comment.is_empty() {
                        review_comment =
                            Some(Comment::Review(state.comment.join("\n").trim().to_string()));
                    }

                    let (old_file, new_file) = parse_diff_header(line)?;
                    self.state = State::FilePreamble(FilePreambleState {
                        old_file,
                        new_file,
                    });

                    return Ok(review_comment);
                } else if let Some(d) = is_prr_directive(line) {
                    return match d {
                        "approve" => Ok(Some(Comment::ReviewAction(ReviewAction::Approve))),
                        "reject" => Ok(Some(Comment::ReviewAction(ReviewAction::RequestChanges))),
                        "comment" => Ok(Some(Comment::ReviewAction(ReviewAction::Comment))),
                        _ => bail!("Unknown @prr directive: {}", d),
                    };
                } else if !state.comment.is_empty() || !line.trim().is_empty() {
                    // Only blindly add lines if lines have already been added
                    state.comment.push(line.to_owned());
                }

                Ok(None)
            }
            State::FilePreamble(state) => {
                if !is_quoted {
                    bail!(
                        "Unexpected comment in file preamble state, file: a/{} b/{}",
                        state.old_file,
                        state.new_file,
                    );
                }

                if let Some((mut left_start, mut right_start)) = parse_hunk_start(line)? {
                    // Subtract 1 b/c this line is before the actual diff hunk
                    left_start = left_start.saturating_sub(1);
                    right_start = right_start.saturating_sub(1);

                    self.state = State::FileDiff(FileDiffState {
                        old_file: state.old_file.to_owned(),
                        new_file: state.new_file.to_owned(),
                        left_line: left_start,
                        right_line: right_start,
                        line: if is_left_line(line) {
                            LineLocation::Left(left_start, right_start)
                        } else if is_right_line(line) {
                            LineLocation::Right(left_start, right_start)
                        } else {
                            LineLocation::Both(left_start, right_start)
                        },
                        span_start_line: None,
                    });
                }

                Ok(None)
            }
            State::FileDiff(state) => {
                if is_quoted {
                    if is_diff_header(line) {
                        if state.span_start_line.is_some() {
                            bail!(
                                "Detected span that was not terminated with a comment, file: a/{} b/{}",
                                state.old_file,
                                state.new_file,
                            );
                        }

                        let (old_file, new_file) = parse_diff_header(line)?;
                        self.state = State::FilePreamble(FilePreambleState {
                            old_file,
                            new_file,
                        });
                    } else if let Some((mut left_start, mut right_start)) = parse_hunk_start(line)?
                    {
                        if state.span_start_line.is_some() {
                            bail!(
                                "Detected cross chunk span, file: a/{} b/{}",
                                state.old_file,
                                state.new_file,
                            );
                        }

                        // Subtract 1 b/c this line is before the actual diff hunk
                        left_start = left_start.saturating_sub(1);
                        right_start = right_start.saturating_sub(1);

                        state.left_line = left_start;
                        state.right_line = right_start;
                        if is_left_line(line) {
                            state.line = LineLocation::Left(left_start, right_start);
                        } else if is_right_line(line) {
                            state.line = LineLocation::Right(left_start, right_start);
                        } else {
                            state.line = LineLocation::Both(left_start, right_start);
                        }
                    } else {
                        let (next_left, next_right) =
                            get_next_lines(line, state.left_line, state.right_line);
                        state.left_line = next_left;
                        state.right_line = next_right;
                        if is_left_line(line) {
                            state.line = LineLocation::Left(next_left, next_right);
                        } else if is_right_line(line) {
                            state.line = LineLocation::Right(next_left, next_right);
                        } else {
                            state.line = LineLocation::Both(next_left, next_right);
                        }
                    }

                    return Ok(None);
                }

                // Now that we know this line is not quoted, there's only two options:
                // 1) beginning of a spanned comment
                // 2) beginning of a comment
                if line.trim().is_empty() {
                    self.state = State::SpanStartOrComment(SpanStartOrCommentState {
                        file_diff_state: state.clone(),
                    })
                } else {
                    self.state = State::Comment(CommentState {
                        file_diff_state: state.clone(),
                        comment: vec![line.to_owned()],
                    })
                }

                Ok(None)
            }
            State::SpanStartOrComment(state) => {
                if is_quoted {
                    if state.file_diff_state.span_start_line.is_some() {
                        bail!(
                            "Detected span that was not terminated with a comment, file: a/{} b/{}",
                            state.file_diff_state.old_file,
                            state.file_diff_state.new_file,
                        );
                    }

                    // Back to the original file diff
                    let (next_left, next_right) = get_next_lines(
                        line,
                        state.file_diff_state.left_line,
                        state.file_diff_state.right_line,
                    );
                    let line = if is_left_line(line) {
                        LineLocation::Left(next_left, next_right)
                    } else if is_right_line(line) {
                        LineLocation::Right(next_left, next_right)
                    } else {
                        LineLocation::Both(next_left, next_right)
                    };
                    self.state = State::FileDiff(FileDiffState {
                        old_file: state.file_diff_state.old_file.to_owned(),
                        new_file: state.file_diff_state.new_file.to_owned(),
                        left_line: next_left,
                        right_line: next_right,
                        line: line.clone(),
                        span_start_line: Some(line),
                    });

                    Ok(None)
                } else if line.trim().is_empty() {
                    // In a multi-line span spart
                    Ok(None)
                } else {
                    // In a comment now
                    self.state = State::Comment(CommentState {
                        file_diff_state: state.file_diff_state.clone(),
                        comment: vec![line.to_owned()],
                    });

                    Ok(None)
                }
            }
            State::Comment(state) => {
                if is_quoted {
                    let comment = Comment::Inline(InlineComment {
                        old_file: state.file_diff_state.old_file.clone(),
                        new_file: state.file_diff_state.new_file.clone(),
                        line: state.file_diff_state.line.clone(),
                        start_line: state.file_diff_state.span_start_line.clone(),
                        comment: state.comment.join("\n").trim_end().to_string(),
                    });

                    if is_diff_header(line) {
                        let (old_file, new_file) = parse_diff_header(line)?;
                        self.state = State::FilePreamble(FilePreambleState {
                            old_file,
                            new_file,
                        });
                    } else {
                        let (next_left, next_right) = get_next_lines(
                            line,
                            state.file_diff_state.left_line,
                            state.file_diff_state.right_line,
                        );
                        self.state = State::FileDiff(FileDiffState {
                            old_file: state.file_diff_state.old_file.to_owned(),
                            new_file: state.file_diff_state.new_file.to_owned(),
                            left_line: next_left,
                            right_line: next_right,
                            line: if is_left_line(line) {
                                LineLocation::Left(next_left, next_right)
                            } else if is_right_line(line) {
                                LineLocation::Right(next_left, next_right)
                            } else {
                                LineLocation::Both(next_left, next_right)
                            },
                            span_start_line: None,
                        });
                    }

                    return Ok(Some(comment));
                }

                state.comment.push(line.to_owned());
                Ok(None)
            }
        }
    }

    pub fn finish(self) -> Option<Comment> {
        match self.state {
            State::Comment(state) => Some(Comment::Inline(InlineComment {
                old_file: state.file_diff_state.old_file,
                new_file: state.file_diff_state.new_file,
                line: state.file_diff_state.line,
                start_line: state.file_diff_state.span_start_line,
                comment: state.comment.join("\n").trim_end().to_string(),
            })),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_fail(input: &str) {
        let mut parser = ReviewParser::new();

        for line in input.lines() {
            if let Err(_) = parser.parse_line(line) {
                return;
            }
        }

        panic!("Parser succeeded when it should have failed");
    }

    fn test(input: &str, expected: &[Comment]) {
        let mut parser = ReviewParser::new();
        let mut comments = Vec::new();

        for line in input.lines() {
            if let Some(c) = parser.parse_line(line).unwrap() {
                comments.push(c);
            }
        }

        if let Some(c) = parser.finish() {
            comments.push(c);
        }

        assert!(
            comments == expected,
            "Parsed different comments than expected.\n Got: {:#?}\nExpected: {:#?}",
            comments,
            expected
        );
    }

    #[test]
    fn single_comment() {
        let input = include_str!("../testdata/single_comment");
        let expected = vec![Comment::Inline(InlineComment {
            file: "libbpf-cargo/src/btf/btf.rs".to_string(),
            line: LineLocation::Right(734),
            start_line: Some(LineLocation::Right(731)),
            comment: "Comment 1".to_string(),
        })];

        test(input, &expected);
    }

    #[test]
    fn approve_review() {
        let input = include_str!("../testdata/approve_review");
        let expected = vec![
            Comment::ReviewAction(ReviewAction::Approve),
            Comment::Inline(InlineComment {
                file: "libbpf-cargo/src/btf/btf.rs".to_string(),
                line: LineLocation::Right(734),
                start_line: Some(LineLocation::Right(731)),
                comment: "Comment 1".to_string(),
            }),
        ];

        test(input, &expected);
    }

    #[test]
    fn reject_review() {
        let input = include_str!("../testdata/reject_review");
        let expected = vec![
            Comment::ReviewAction(ReviewAction::RequestChanges),
            Comment::Inline(InlineComment {
                file: "libbpf-cargo/src/btf/btf.rs".to_string(),
                line: LineLocation::Right(734),
                start_line: Some(LineLocation::Right(731)),
                comment: "Comment 1".to_string(),
            }),
        ];

        test(input, &expected);
    }

    #[test]
    fn review_comment() {
        let input = include_str!("../testdata/review_comment");
        let expected = vec![
            Comment::Review("Review comment".to_string()),
            Comment::Inline(InlineComment {
                file: "libbpf-cargo/src/btf/btf.rs".to_string(),
                line: LineLocation::Right(734),
                start_line: Some(LineLocation::Right(731)),
                comment: "Comment 1".to_string(),
            }),
        ];

        test(input, &expected);
    }

    #[test]
    fn review_comment_whitespace() {
        let input = include_str!("../testdata/review_comment_whitespace");
        let expected = vec![
            Comment::ReviewAction(ReviewAction::Approve),
            Comment::Review("Review comment".to_string()),
        ];

        test(input, &expected);
    }

    #[test]
    fn multiline_comment() {
        let input = include_str!("../testdata/multiline_comment");
        let expected = vec![Comment::Inline(InlineComment {
            file: "libbpf-cargo/src/btf/btf.rs".to_string(),
            line: LineLocation::Right(736),
            start_line: None,
            comment: "Comment line 1\nComment line 2\n\nComment line 4".to_string(),
        })];

        test(input, &expected);
    }

    #[test]
    fn back_to_back_span() {
        let input = include_str!("../testdata/back_to_back_span");
        let expected = vec![
            Comment::Inline(InlineComment {
                file: "libbpf-cargo/src/btf/btf.rs".to_string(),
                line: LineLocation::Right(734),
                start_line: Some(LineLocation::Right(731)),
                comment: "Comment 1".to_string(),
            }),
            Comment::Inline(InlineComment {
                file: "libbpf-cargo/src/btf/btf.rs".to_string(),
                line: LineLocation::Right(737),
                start_line: None,
                comment: "Comment 2".to_string(),
            }),
        ];

        test(input, &expected);
    }

    #[test]
    fn multiple_files() {
        let input = include_str!("../testdata/multiple_files");
        let expected = vec![
            Comment::Inline(InlineComment {
                file: "libbpf-cargo/src/btf/btf.rs".to_string(),
                line: LineLocation::Right(734),
                start_line: None,
                comment: "Comment 1".to_string(),
            }),
            Comment::Inline(InlineComment {
                file: "libbpf-cargo/src/test.rs".to_string(),
                line: LineLocation::Right(2159),
                start_line: None,
                comment: "Comment 2".to_string(),
            }),
        ];

        test(input, &expected);
    }

    #[test]
    fn hunk_start_no_trailing_whitespace() {
        let input = include_str!("../testdata/hunk_start_no_trailing_whitespace");
        let expected = vec![Comment::Inline(InlineComment {
            file: "ch5.txt".to_string(),
            line: LineLocation::Right(7),
            start_line: None,
            comment: "Great passage".to_string(),
        })];

        test(input, &expected);
    }

    #[test]
    fn deleted_file() {
        let input = include_str!("../testdata/deleted_file");
        let expected = vec![Comment::Inline(InlineComment {
            file: "ch1.txt".to_string(),
            line: LineLocation::Left(58),
            start_line: Some(LineLocation::Left(1)),
            comment: "Comment 1".to_string(),
        })];

        test(input, &expected);
    }

    #[test]
    fn trailing_comment() {
        let input = include_str!("../testdata/trailing_comment");
        let expected = vec![Comment::Inline(InlineComment {
            file: "ch1.txt".to_string(),
            line: LineLocation::Left(59),
            start_line: Some(LineLocation::Left(1)),
            comment: "Comment 1".to_string(),
        })];

        test(input, &expected);
    }

    #[test]
    /// https://github.com/danobi/prr/issues/3
    fn spaces_in_filename() {
        let input = include_str!("../testdata/spaces_in_filename");
        let expected = vec![Comment::Inline(InlineComment {
            file: "build/scripts/grafana/provisioning/dashboards/Docker Prometheus Monitoring-1571332751387.json".to_string(),
            line: LineLocation::Right(2),
            start_line: None,
            comment: "foo".to_string(),
        })];

        test(input, &expected);
    }

    #[test]
    fn unterminated_span() {
        let input = include_str!("../testdata/unterminated_span");
        test_fail(input);
    }

    #[test]
    fn cross_file_span_ignored() {
        let input = include_str!("../testdata/cross_file_span_ignored");
        test_fail(input);
    }

    #[test]
    fn unterminated_back_to_back_span() {
        let input = include_str!("../testdata/unterminated_back_to_back_span");
        test_fail(input);
    }

    #[test]
    fn cross_hunk_span() {
        let input = include_str!("../testdata/cross_hunk_span");
        test_fail(input);
    }

    #[test]
    fn unknown_directive() {
        let input = include_str!("../testdata/unknown_directive");
        test_fail(input);
    }
}
