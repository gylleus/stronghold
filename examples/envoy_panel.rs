//! Synthetic Envoy panel: `cargo run --example envoy_panel -- [--outbox] [--shot path.png]`.
//! Decisions only change these in-memory fixtures; no app services or delivery are started.

use std::time::{Duration, Instant};

use eframe::egui;
use myrstack::envoy::channel::{EventKind, InboundEvent};
use myrstack::envoy::drafts::{Draft, DraftStatus, ReplyTo};
use myrstack::envoy::inbox::Inbox;
use myrstack::ui::envoy::{DraftAction, DraftsPanel};

struct Demo {
    panel: DraftsPanel,
    drafts: Vec<Draft>,
    inbox: Inbox,
    shot: Option<String>,
    since: Instant,
    shooting: bool,
}

impl eframe::App for Demo {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        egui::CentralPanel::default().show(ui, |ui| {
            if let Some(action) = self.panel.show(ui, &self.drafts, &self.inbox, true, None) {
                let (id, status, text) = match action {
                    DraftAction::Approve { id, text } => (id, DraftStatus::DryRunSent, Some(text)),
                    DraftAction::Reject(id) => (id, DraftStatus::Rejected, None),
                    _ => return,
                };
                let draft = self.drafts.iter_mut().find(|d| d.id == id).unwrap();
                draft.status = status;
                if let Some(text) = text {
                    draft.text = text;
                }
                self.panel.result(&id, Ok(()));
            }
        });
        let Some(path) = &self.shot else { return };
        ctx.request_repaint_after(Duration::from_millis(100));
        if !self.shooting && self.since.elapsed() > Duration::from_millis(1200) {
            self.shooting = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        }
        let image = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = image {
            let [w, h] = image.size;
            let bytes: Vec<u8> = image.pixels.iter().flat_map(|p| p.to_array()).collect();
            image::save_buffer(path, &bytes, w as u32, h as u32, image::ColorType::Rgba8)
                .expect("save screenshot");
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

fn fixtures(outbox: bool) -> (Vec<Draft>, Inbox) {
    let link = "https://example.slack.com/archives/C123/p1790000000000001";
    let mut inbox = Inbox::default();
    inbox.insert(InboundEvent {
        channel_kind: "slack".into(),
        conversation: "C123".into(),
        ts: "1790000000.000001".into(),
        thread_ts: None,
        user: "U123".into(),
        user_name: "Avery".into(),
        text: "The fixture build is ready. Can someone review it?".into(),
        kind: EventKind::AgentComms,
        permalink: Some(link.into()),
    });
    let reply = Draft::reply(
        ReplyTo {
            conversation: "C123".into(),
            ts: "1790000000.000001".into(),
        },
        Some("1790000000.000001".into()),
        "The build is ready for review. I will report back once it has been checked.".into(),
        "Avery asked for a build update.".into(),
        0,
    );
    let task = Draft::proposal(
        "Review the fixture build and report any regressions.".into(),
        Some("myrstack".into()),
        "Avery requested a review; the King can start a dwarf to check it.".into(),
        link.into(),
        0,
    );
    let mut drafts = if outbox {
        let mut recorded = task.clone();
        recorded.status = DraftStatus::DryRunSent;
        let mut failed = reply.clone();
        failed.status = DraftStatus::Failed;
        failed.error = Some("Fixture: connection lost before delivery could be confirmed.".into());
        let mut sent = reply.clone();
        sent.status = DraftStatus::Sent;
        let mut rejected = task;
        rejected.status = DraftStatus::Rejected;
        vec![rejected, sent, recorded, failed]
    } else {
        vec![reply, task]
    };
    for (i, draft) in drafts.iter_mut().enumerate() {
        draft.id = format!("envoy-draft-{}", i + 1);
        draft.updated_ms = i as i64;
    }
    (drafts, inbox)
}

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let shot = args
        .iter()
        .position(|a| a == "--shot")
        .and_then(|i| args.get(i + 1))
        .cloned();
    let outbox = args.iter().any(|a| a == "--outbox");
    let (mut drafts, inbox) = fixtures(outbox);
    let gateway = args.iter().any(|a| a == "--gateway");
    if gateway {
        let link=myrstack::envoy::drafts::RequestLink{id:"envoy-request-1".into(),turn_id:1,original_ask:"Please review abc012def at 2026-10-06T12:34:56Z.\nCount: 42\n```rust\nlet x = 17;\n```\n".into(),requester:"Build dwarf".into(),why:"Unblock the fixture release review".into(),agent:true};
        let mut draft = Draft::outgoing(
            "C123".into(),
            None,
            link.verbatim(),
            "Avery owns this review".into(),
            link,
            myrstack::control::server::now_ms(),
        );
        draft.id = "envoy-draft-1".into();
        draft.automatic = true;
        draft.auto_at_ms = Some(myrstack::control::server::now_ms() + 120_000);
        drafts = vec![draft];
    }
    eframe::run_native(
        "Envoy fixture",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default().with_inner_size([780.0, 900.0]),
            ..Default::default()
        },
        Box::new(move |cc| {
            myrstack::ui::theme::install(&cc.egui_ctx);
            cc.egui_ctx.set_theme(egui::Theme::Dark);
            let mut panel = DraftsPanel::default();
            if outbox {
                panel.open(&[]);
            }
            if gateway {
                let mut orphan = myrstack::envoy::requests::Request {
                    id: "envoy-request-2".into(),
                    to: "Adopted fixture thread".into(),
                    requester: None,
                    binding_approved: false,
                    binding_sequence: 0,
                    binding_proposal: None,
                    turns: Vec::new(),
                    thread: Some(myrstack::envoy::requests::Thread {
                        conversation: "D123".into(),
                        ts: "1790000000.000001".into(),
                    }),
                    status: myrstack::envoy::requests::Status::Sent,
                    settled: false,
                    adopted_ms: Some(0),
                    reply_after_us: None,
                    orphan_kept: false,
                    close_requested: false,
                    deliveries: Vec::new(),
                };
                if args.iter().any(|a| a == "--binding") {
                    orphan.binding_sequence = 1;
                    orphan.binding_proposal = Some(myrstack::envoy::requests::BindingProposal {
                        id: 1,
                        requester: myrstack::envoy::requests::Requester::from_origin(
                            myrstack::review::mark::Origin {
                                terminal_handle: Some("Build dwarf".into()),
                                ..Default::default()
                            },
                        )
                        .unwrap(),
                        reason: "The build dwarf owns this review and needs the replies.".into(),
                        preview: myrstack::envoy::requests::BindingPreview {
                            thread: orphan.thread.clone().unwrap(),
                            first_line: "Please review the fixture release.".into(),
                            slack_link: "https://example.slack.com/archives/D123/p1790000000000001"
                                .into(),
                        },
                    });
                }
                panel.set_requests(&[orphan]);
            }
            Ok(Box::new(Demo {
                panel,
                drafts,
                inbox,
                shot,
                since: Instant::now(),
                shooting: false,
            }))
        }),
    )
}
