//! A live agent's conversation pane (or the King's chat) as the app draws it, loaded from the
//! real session, for checking history loading and scrolling.
//!
//! `cargo run --example chat_history -- (--terminal <handle> | --king) [--screen] [--light]
//! [--scroll-up <points>] [--shot out.png]`: `--terminal` shows that Orca terminal's agent with
//! its session transcript (`--screen`: its live screen), `--king` the King's chat with his
//! resumed session. `--scroll-up` scrolls the list up once drawn. With `--shot`, it saves a
//! screenshot once settled and exits.

use std::time::{Duration, Instant};

use eframe::egui;
use myrstack::chat::history::HistoryReader;
use myrstack::chat::{SessionState, Transcript};
use myrstack::claude::SessionLocator;
use myrstack::model::{Node, NodeId, NodeKind, NodeState};
use myrstack::orca::{AgentIdentity, OrcaClient, Terminal};
use myrstack::poller::ScreenUpdate;
use myrstack::ui;
use myrstack::ui::chat::TranscriptView;

const SETTLE_TIME: Duration = Duration::from_secs(3);
const SCROLL_AT: Duration = Duration::from_secs(1);
const WINDOW: [f32; 2] = [760.0, 900.0];

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1).cloned())
    };
    let flag = |name: &str| args.iter().any(|a| a == name);
    let shot = arg("--shot");
    let light = flag("--light");
    let scroll_up = arg("--scroll-up").and_then(|s| s.parse::<f32>().ok());
    let content = match arg("--terminal") {
        Some(handle) => agent(&handle, flag("--screen")),
        None if flag("--king") => king(),
        None => panic!("--terminal <handle> or --king"),
    };
    eframe::run_native(
        "chat history",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default().with_inner_size(WINDOW),
            ..Default::default()
        },
        Box::new(move |cc| {
            ui::theme::install(&cc.egui_ctx);
            cc.egui_ctx.set_theme(if light {
                egui::Theme::Light
            } else {
                egui::Theme::Dark
            });
            Ok(Box::new(Pane {
                content,
                input: Default::default(),
                chat: Default::default(),
                shot,
                scroll_up,
                since: Instant::now(),
                shooting: false,
            }))
        }),
    )
}

enum Content {
    Agent {
        node: Box<Node>,
        screen: ScreenUpdate,
        view: Box<Option<TranscriptView>>,
        show_screen: bool,
    },
    King(Box<Transcript>),
}

/// The agent in Orca terminal `handle`, its screen and its session's conversation.
fn agent(handle: &str, show_screen: bool) -> Content {
    let orca = OrcaClient::new();
    let screen = orca.terminal_read_screen(handle).expect("read screen");
    let mut terminal: Terminal = orca
        .terminal_list()
        .expect("terminal list")
        .terminals
        .into_iter()
        .find(|t| t.handle == handle)
        .expect("no such terminal");
    terminal.agent_identity = Some(AgentIdentity::Claude);
    let hooks = myrstack::claude::hook::load_hook_mappings(
        &myrstack::claude::hook::default_session_map_path(),
    );
    let sessions = SessionLocator::default().map_terminals(&[terminal.clone()], Some(&hooks));
    let view = sessions.get(handle).and_then(|m| m.session()).map(|info| {
        let started = Instant::now();
        let mut view = TranscriptView::new(info.transcript_path.clone());
        let chunk = HistoryReader::new(&info.transcript_path)
            .read_new()
            .expect("read transcript")
            .expect("transcript");
        view.apply(&chunk);
        eprintln!(
            "{}: {} records → {} items in {:?}",
            info.transcript_path.display(),
            chunk.records.len(),
            view.transcript.items.len(),
            started.elapsed()
        );
        view
    });
    let node = Node {
        id: NodeId::terminal(handle),
        kind: NodeKind::Coordinator,
        label: terminal.title.clone().unwrap_or_else(|| handle.to_owned()),
        terminal_handle: Some(handle.into()),
        live: true,
        run_id: None,
        task_id: None,
        dispatch_id: None,
        agent: Some(AgentIdentity::Claude),
        worktree_path: terminal.worktree_path.clone(),
        branch: None,
        last_output_ms: None,
        preview: None,
        state: NodeState::Idle,
        worker_state: None,
        dispatch_status: None,
        worker_terminal_state: None,
        released_at_ms: None,
        parked_at_ms: None,
    };
    Content::Agent {
        node: Box::new(node),
        screen: ScreenUpdate {
            handle: handle.to_owned(),
            lines: screen.tail,
            status: screen.status,
            read_at_ms: 0,
            error: None,
        },
        view: Box::new(view),
        show_screen,
    }
}

/// The King's resumed conversation, as the chat loads it at start.
fn king() -> Content {
    let dir = myrstack::claude::hook::data_dir().join("chat");
    let record = myrstack::chat::session::SessionStore::saved(&dir).expect("no King session");
    let info = SessionLocator::default()
        .find_session(&record.session_id, Some(&dir))
        .expect("no King transcript");
    let transcript = myrstack::chat::history::load(&info.transcript_path).expect("read");
    eprintln!(
        "{}: {} items",
        info.transcript_path.display(),
        transcript.items.len()
    );
    Content::King(Box::new(transcript))
}

struct Pane {
    content: Content,
    input: ui::terminal::InputPanelState,
    chat: ui::chat::ChatPanelState,
    shot: Option<String>,
    scroll_up: Option<f32>,
    since: Instant,
    shooting: bool,
}

impl eframe::App for Pane {
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw: &mut egui::RawInput) {
        if self.since.elapsed() >= SCROLL_AT
            && let Some(points) = self.scroll_up.take()
        {
            let at = egui::pos2(WINDOW[0] / 2.0, WINDOW[1] / 2.0);
            raw.events.push(egui::Event::PointerMoved(at));
            raw.events.push(egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, points),
                modifiers: egui::Modifiers::NONE,
                phase: egui::TouchPhase::Move,
            });
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ui, |ui| match &mut self.content {
            Content::Agent {
                node,
                screen,
                view,
                show_screen,
            } => {
                self.input.show_screen = *show_screen;
                let input = ui::terminal::PanelInput {
                    review_rows: &[],
                    state: &mut self.input,
                    terminal: None,
                    conversation: view.as_mut().as_mut(),
                    is_node: &|_| false,
                };
                ui::terminal::terminal_panel(ui, node, Some(screen), input, 0);
            }
            Content::King(transcript) => {
                let view = ui::chat::ChatView {
                    transcript,
                    state: &SessionState::Ready,
                    is_node: &|_| false,
                    auto_mode: false,
                };
                ui::chat::chat_ui(ui, &mut self.chat, view);
            }
        });
        ui.ctx().request_repaint_after(Duration::from_millis(50));
        let Some(path) = &self.shot else { return };
        if !self.shooting && self.since.elapsed() >= SETTLE_TIME {
            self.shooting = true;
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
            return;
        }
        let image = ui.ctx().input(|i| {
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
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}
