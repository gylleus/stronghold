use super::*;
use crate::envoy::channel::EventKind;

fn event(ts: &str, thread: Option<&str>, text: &str) -> InboundEvent {
    InboundEvent {
        channel_kind: "slack".into(),
        conversation: "C1".into(),
        ts: ts.into(),
        thread_ts: thread.map(str::to_owned),
        user: "U2".into(),
        user_name: "Alex".into(),
        text: text.into(),
        kind: EventKind::ThreadReply,
        permalink: None,
    }
}

fn dwarf(id: &str, label: &str, repo: &str, branch: &str) -> Dwarf {
    Dwarf {
        id: id.into(),
        label: label.into(),
        repo: Some(repo.into()),
        branch: Some(branch.into()),
        ..Default::default()
    }
}

fn clan() -> Vec<Dwarf> {
    let mut redis = dwarf(
        "term:redis",
        "Fable Redis review",
        "api",
        "karl/redis-read-replicas",
    );
    redis.prs = vec!["api#649".into()];
    redis.context = vec!["ACME-1716: api reads Redis from the replica".into()];
    let mut wheat = dwarf(
        "term:wheat",
        "Finish search kickoff and PR",
        "shop",
        "karl/wheat-search-preload",
    );
    wheat.threads = vec!["C1:100.000001".into()];
    vec![
        redis,
        wheat,
        dwarf(
            "term:recs",
            "Recs v2 config scoping",
            "api",
            "karl/recs-v2-config-scoping",
        ),
    ]
}

fn ids(c: &[Candidate]) -> Vec<&str> {
    c.iter().map(|c| c.id.as_str()).collect()
}

#[test]
fn refs_cover_tickets_repo_numbers_and_pr_links() {
    let r =
        refs("ACME-1716 lands with api#694, see https://github.com/acme/shop/pull/8748 and #12");
    let want: BTreeSet<String> = ["acme-1716", "api#694", "shop#8748", "#12"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    assert_eq!(r, want);
}

#[test]
fn thread_owner_wins_over_everything() {
    let e = event("100.000500", Some("100.000001"), "ACME-1716 too");
    let c = candidates(&e, &[], &clan(), &BTreeMap::new());
    assert_eq!(ids(&c), ["term:wheat", "term:redis"]);
    assert_eq!(c[0].why, "its thread");
}

#[test]
fn pr_and_ticket_refs_match_including_from_the_thread() {
    let e = event("200.1", None, "api#649 is merged");
    assert_eq!(
        ids(&candidates(&e, &[], &clan(), &BTreeMap::new())),
        ["term:redis"]
    );

    let root = event("300.1", None, "Heads up on acme-1716");
    let reply = event("300.2", Some("300.1"), "done, deploying now");
    let c = candidates(&reply, &[&root], &clan(), &BTreeMap::new());
    assert_eq!(ids(&c), ["term:redis"]);
    assert_eq!(c[0].why, "acme-1716");
}

#[test]
fn keywords_need_two_distinctive_hits() {
    let e = event("400.1", None, "the wheat search preload is slow");
    assert_eq!(
        ids(&candidates(&e, &[], &clan(), &BTreeMap::new())),
        ["term:wheat"]
    );
    let e = event("400.2", None, "can someone review this config?");
    assert!(candidates(&e, &[], &clan(), &BTreeMap::new()).is_empty());
    let e = event("400.3", None, "api redis is flapping");
    assert!(
        candidates(&e, &[], &clan(), &BTreeMap::new()).is_empty(),
        "the repo name adds nothing"
    );
    let e = event("400.4", None, "redis replicas are lagging");
    assert_eq!(
        ids(&candidates(&e, &[], &clan(), &BTreeMap::new())),
        ["term:redis"]
    );
    let e = event("400.5", None, "search is slow on the first page");
    assert!(
        candidates(&e, &[], &clan(), &BTreeMap::new()).is_empty(),
        "generic words don't count"
    );
}

#[test]
fn a_pr_ref_matches_the_dwarf_on_its_head_branch() {
    let heads = BTreeMap::from([(
        "shop#8765".to_owned(),
        "karl/wheat-search-preload".to_owned(),
    )]);
    let e = event("500.1", None, "shop#8765 is NOT APPROVED yet, one blocker");
    let c = candidates(&e, &[], &clan(), &heads);
    assert_eq!(ids(&c), ["term:wheat"]);
    assert_eq!(c[0].why, "shop#8765 is its branch");
}

#[test]
fn annotation_lists_candidates_or_says_none() {
    assert_eq!(annotation(&[]), " — no dwarf match");
    let e = event("200.1", None, "api#649");
    assert_eq!(
        annotation(&candidates(&e, &[], &clan(), &BTreeMap::new())),
        " — dwarves: Fable Redis review (term:redis, api#649)"
    );
}
