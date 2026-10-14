//! GitHub pull request references: parsing `github.com/<owner>/<repo>/pull/<n>` out of URLs and
//! free text (bus messages).

use serde::{Deserialize, Serialize};

/// One pull request on github.com.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PrRef {
    pub owner: String,
    pub repo: String,
    pub number: u64,
}

impl PrRef {
    /// Parses a PR URL (`https://github.com/o/r/pull/12`, with or without scheme, `www.`, a
    /// trailing path such as `/files`, a query or a fragment). `None` for anything else.
    pub fn parse(url: &str) -> Option<Self> {
        let url = url.trim();
        let rest = url
            .strip_prefix("https://")
            .or_else(|| url.strip_prefix("http://"))
            .unwrap_or(url);
        let rest = rest.strip_prefix("www.").unwrap_or(rest);
        let rest = rest.strip_prefix("github.com/")?;
        let (pr, _) = parse_after_host(rest)?;
        Some(pr)
    }

    /// `https://github.com/<owner>/<repo>/pull/<n>`: the key the review queue uses.
    pub fn url(&self) -> String {
        format!(
            "https://github.com/{}/{}/pull/{}",
            self.owner, self.repo, self.number
        )
    }

    /// `api#255`.
    pub fn short(&self) -> String {
        format!("{}#{}", self.repo, self.number)
    }
}

impl std::fmt::Display for PrRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}#{}", self.owner, self.repo, self.number)
    }
}

/// Every PR URL in `text`, in order of first appearance, without duplicates.
pub fn find_prs(text: &str) -> Vec<PrRef> {
    const HOST: &str = "github.com/";
    let mut found: Vec<PrRef> = Vec::new();
    let mut from = 0;
    while let Some(at) = text[from..].find(HOST) {
        let start = from + at + HOST.len();
        let preceded_ok = text[..from + at]
            .chars()
            .next_back()
            .is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        match parse_after_host(&text[start..]) {
            Some((pr, used)) if preceded_ok => {
                if !found.contains(&pr) {
                    found.push(pr);
                }
                from = start + used;
            }
            _ => from = start,
        }
    }
    found
}

/// `<owner>/<repo>/pull/<digits>` at the start of `rest`, and how many bytes it spans.
fn parse_after_host(rest: &str) -> Option<(PrRef, usize)> {
    let name_len = |s: &str| {
        s.find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
            .unwrap_or(s.len())
    };
    let owner_len = name_len(rest);
    let owner = &rest[..owner_len];
    let after_owner = rest[owner_len..].strip_prefix('/')?;
    let repo_len = name_len(after_owner);
    let repo = &after_owner[..repo_len];
    let after_repo = after_owner[repo_len..].strip_prefix("/pull/")?;
    let digits = after_repo
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(after_repo.len());
    if owner.is_empty() || repo.is_empty() || digits == 0 {
        return None;
    }
    // A trailing letter would make it a different word (`/pull/12abc`); punctuation is fine.
    if after_repo[digits..]
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return None;
    }
    let number = after_repo[..digits].parse().ok().filter(|n| *n > 0)?;
    let used = owner_len + 1 + repo_len + "/pull/".len() + digits;
    Some((
        PrRef {
            owner: owner.to_owned(),
            repo: repo.trim_end_matches('.').to_owned(),
            number,
        },
        used,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pr(owner: &str, repo: &str, number: u64) -> PrRef {
        PrRef {
            owner: owner.into(),
            repo: repo.into(),
            number,
        }
    }

    #[test]
    fn parses_urls_in_their_usual_forms() {
        let want = pr("acme", "api", 255);
        for url in [
            "https://github.com/acme/api/pull/255",
            "http://github.com/acme/api/pull/255",
            "github.com/acme/api/pull/255",
            "https://www.github.com/acme/api/pull/255/files",
            "https://github.com/acme/api/pull/255#issuecomment-1",
            "https://github.com/acme/api/pull/255?x=1",
            "  https://github.com/acme/api/pull/255\n",
        ] {
            assert_eq!(PrRef::parse(url), Some(want.clone()), "{url}");
        }
        assert_eq!(want.url(), "https://github.com/acme/api/pull/255");
        assert_eq!(want.short(), "api#255");
        assert_eq!(want.to_string(), "acme/api#255");
        assert_eq!(
            PrRef::parse("https://github.com/o/foundation-ingestion.v2/pull/1"),
            Some(pr("o", "foundation-ingestion.v2", 1))
        );
    }

    #[test]
    fn rejects_non_pr_urls() {
        for url in [
            "",
            "https://github.com/acme/api",
            "https://github.com/acme/api/issues/255",
            "https://github.com/acme/api/pull/",
            "https://github.com/acme/api/pull/0",
            "https://github.com/acme/api/pull/12abc",
            "https://gitlab.com/acme/api/pull/255",
            "https://github.com//api/pull/255",
            "#255",
        ] {
            assert_eq!(PrRef::parse(url), None, "{url}");
        }
    }

    #[test]
    fn finds_every_pr_in_text_once() {
        let text = "https://github.com/acme/storefront/pull/249 at 72c51c3e: green. \
                    Stacked on (https://github.com/acme/storefront/pull/248), see \
                    github.com/acme/storefront/pull/249 again; not \
                    https://github.com/acme/storefront/issues/3 or \
                    notgithub.com/a/b/pull/1.";
        assert_eq!(
            find_prs(text),
            [pr("acme", "storefront", 249), pr("acme", "storefront", 248)]
        );
        assert!(find_prs("PR #230 rebased, green").is_empty());
        assert_eq!(
            find_prs("{\"prUrl\":\"https://github.com/o/r/pull/7\"}"),
            [pr("o", "r", 7)]
        );
    }
}
