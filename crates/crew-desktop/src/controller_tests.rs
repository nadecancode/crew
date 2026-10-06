//! Actual GTK controls with a typed recording double, followed by a real
//! private daemon using the production ServiceHandle/NativeSession adapter.
use super::*;
use crew_client::ClientCore;
use gtk::gio;
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use vte4::prelude::*;

#[derive(Default)]
struct Recording(RefCell<Vec<Request>>);
impl ClientService for Recording {
    fn submit(&self, request: Request) -> Result<(), ClientReason> {
        self.0.borrow_mut().push(request);
        Ok(())
    }
}
impl Recording {
    fn queries(&self, c: &Controller, core: &ClientCore) {
        let requests = self.0.borrow_mut().drain(..).collect::<Vec<_>>();
        for request in requests {
            if let Request::Query {
                token,
                expected_scope,
                query,
            } = request
            {
                c.respond(Response::View {
                    instance: 1,
                    token,
                    expected_scope,
                    result: core.view(query, clock()),
                });
            }
        }
    }
    fn plan_token(&self) -> u64 {
        self.0
            .borrow()
            .iter()
            .rev()
            .find_map(|r| {
                if let Request::Plan { token, .. } = r {
                    Some(*token)
                } else {
                    None
                }
            })
            .expect("one explicit UI plan")
    }
}
fn clock() -> ClockSample {
    ClockSample {
        wall_ms: 1_760_000_000_000,
        monotonic_ms: 100,
    }
}
fn scope() -> ClientScope {
    ClientScope {
        endpoint_id: "fixture-host".into(),
        workspace_id: "/work/demo".into(),
        generation: Some(1),
        daemon_pid: Some(4000),
        daemon_started_at_ms: Some(1_759_999_240_000),
        attempt: 1,
    }
}
fn notice(scope: Option<ClientScope>) -> ClientNotice {
    ClientNotice {
        scope,
        revision: 1,
        endpoint: EndpointStatus::Authenticated,
        workspace: WorkspaceStatus::Live,
        reason: None,
        changes: ChangeSet {
            reset: true,
            ..Default::default()
        },
        outcomes: Vec::new(),
    }
}
fn key(entity: Entity) -> EntityKey {
    EntityKey {
        scope: scope(),
        entity,
    }
}
fn core() -> ClientCore {
    core_with_mode(
        ConnectionMode::LocalOperator,
        vec![GrantTier::Read, GrantTier::Control, GrantTier::Terminal],
    )
}
fn core_with_mode(mode: ConnectionMode, tiers: Vec<GrantTier>) -> ClientCore {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../fixtures/native/state/parked-resumption.case.json"
    ))
    .unwrap();
    let snapshot: crew_protocol::rpc::Snapshot =
        serde_json::from_value(fixture["steps"][0]["snapshot"].clone()).unwrap();
    let mut core = ClientCore::new(ClientConfig {
        limits: policy::NATIVE_LIMITS,
        snapshot_mode: SnapshotMode::OrderedConnection,
    })
    .unwrap();
    let context = ConnectionContext {
        scope: scope(),
        mode,
        snapshot_mode: SnapshotMode::OrderedConnection,
        grant: Grant {
            grant_id: "fixture".into(),
            tiers,
            workspace_ids: vec![scope().workspace_id],
            expires_at_ms: None,
        },
    };
    core.apply(
        ClientInput::Bind {
            scope: context.scope,
            mode: context.mode,
            grant: context.grant,
            snapshot_mode: context.snapshot_mode,
        },
        clock(),
    )
    .unwrap();
    core.apply(
        ClientInput::SnapshotStarted {
            scope: scope(),
            operation_id: 1,
        },
        clock(),
    )
    .unwrap();
    core.apply(
        ClientInput::Snapshot {
            scope: scope(),
            operation_id: 1,
            snapshot: Box::new(snapshot),
        },
        clock(),
    )
    .unwrap();
    core
}

// One test keeps all GTK initialization/widgets on one libtest thread/process.
#[test]
fn gtk_scoped_views_confirmation_terminal_order_and_live_local_service() {
    assert!(
        std::env::var_os("CREW_NATIVE_UI_TEST_PRIVATE").is_some(),
        "use tests/run-window-test.sh --client; never use the operator's display or registry"
    );
    gtk::init().unwrap();
    eprintln!("GTK initialized");
    let app = crate::application::build();
    app.register(None::<&gio::Cancellable>).unwrap();
    let shell = Shell::new(&app);
    let recorder = Rc::new(Recording::default());
    let c = Controller::with_service(shell.clone(), recorder.clone());
    eprintln!("Native controller constructed");
    c.respond(Response::Session(1));
    c.respond(Response::Notice {
        instance: 1,
        notice: notice(Some(scope())),
    });
    let core = core();
    eprintln!("Fixed shared fixture bound");
    recorder.queries(&c, &core);
    eprintln!("Projection queries delivered");
    shell.window().present();
    until(|| shell.window().is_mapped());
    assert!(
        text(shell.window()).contains("Current task current")
            || text(shell.window()).contains("Current: current")
    );
    assert!(text(shell.window()).contains("parked-old"));
    assert!(text(shell.window()).contains("parked-new"));
    capture("fixture-team");
    // Many relations stay in a collapsed, height-bounded group above the list.
    let ViewPayload::Team(mut team) = core.view(ViewQuery::Team, clock()).unwrap().payload else {
        panic!("team projection");
    };
    team.relations = (0..40)
        .map(|i| AgentRelation {
            parent_id: "claude-1".into(),
            child_id: format!("helper-{i}"),
        })
        .collect();
    c.team.update(Some(&scope()), &team);
    let pairs = descendants(shell.window().upcast_ref())
        .into_iter()
        .find_map(|w| w.downcast::<gtk::Expander>().ok())
        .expect("pair navigation group");
    assert!(pairs.is_visible() && !pairs.is_expanded());
    assert_eq!(
        pairs.label().as_deref(),
        Some("Conversations between related agents (40)")
    );
    pairs.set_expanded(true);
    settle();
    let pairs_scroll = pairs.child().and_downcast::<gtk::ScrolledWindow>().unwrap();
    assert!(
        pairs_scroll.height() <= 200,
        "pair navigation stays bounded"
    );
    capture("fixture-team-many-pairs");
    pairs.set_expanded(false);
    recorder.queries(&c, &core);
    c.refresh(Destination::Team);
    recorder.queries(&c, &core);
    assert!(!pairs.is_visible(), "the fixture has no relations");
    graph_topology(&c, &shell, &core);
    recorder.queries(&c, &core);
    c.refresh(Destination::Tasks);
    c.refresh(Destination::Team);
    recorder.queries(&c, &core);
    shell.navigate(Destination::Team);
    c.select(key(Entity::Agent("claude-1".into())));
    recorder.queries(&c, &core);
    let inspector = c.agent.borrow().as_ref().unwrap().1.clone();
    assert!(text(shell.window()).contains("Current work: current"));
    c.select(key(Entity::Task("parked-old".into())));
    recorder.queries(&c, &core);
    assert!(text(shell.window()).contains("Waiting"));
    let task = c.task_inspector.borrow().as_ref().unwrap().1.clone();
    let refresh = button(&task.root, "Refresh evidence");
    refresh.grab_focus();
    c.refresh(Destination::Tasks);
    recorder.queries(&c, &core);
    assert!(refresh.has_focus() || refresh.is_focus());
    capture("fixture-waiting-detail");
    shell.navigate(Destination::Team);
    assert!(
        c.agent.borrow().is_some(),
        "other destination retains the qualified agent inspector"
    );
    capture("fixture-team-return");

    c.plan(
        ActionIntent::Agent {
            scope: scope(),
            id: "claude-1".into(),
            action: AgentAction::Kill,
        },
        None,
        None,
    );
    let token = recorder.plan_token();
    c.respond(Response::Action {
        instance: 1,
        token,
        scope: scope(),
        result: Ok(ActionPlan::Confirm {
            confirmation_id: 9,
            kind: ConfirmationKind::KillAgent,
            scope: scope(),
            target_ids: vec!["claude-1".into()],
        }),
    });
    let dialog = c.confirmation.borrow().as_ref().unwrap().dialog.clone();
    assert_eq!(
        dialog.default_widget().unwrap(),
        dialog
            .widget_for_response(gtk::ResponseType::Cancel)
            .unwrap()
    );
    capture("fixture-confirmation");
    let mut stale = notice(Some(scope()));
    stale.workspace = WorkspaceStatus::Reconnecting;
    c.respond(Response::Notice {
        instance: 1,
        notice: stale,
    });
    assert!(c.confirmation.borrow().is_none());
    assert!(
        !recorder
            .0
            .borrow()
            .iter()
            .any(|r| matches!(r, Request::Confirm(_))),
        "connection loss cannot approve a confirmation"
    );
    c.respond(Response::Notice {
        instance: 1,
        notice: notice(Some(scope())),
    });

    c.attach(key(Entity::Agent("claude-1".into())));
    let token = recorder.plan_token();
    let mut success = notice(Some(scope()));
    success.changes = Default::default();
    success.outcomes.push(OperationNotice {
        scope: scope(),
        operation_id: 17,
        attachment_id: Some(17),
        kind: OperationOutcomeKind::Success,
        reason: None,
    });
    c.respond(Response::Notice {
        instance: 1,
        notice: success,
    });
    assert_eq!(
        c.attachments.borrow().len(),
        1,
        "identified notice creates the renderer before the plan continuation"
    );
    c.respond(Response::Terminal {
        instance: 1,
        chunk: TerminalChunk {
            scope: scope(),
            attachment_id: 999,
            data: b"WRONG".to_vec(),
            ended: None,
        },
    });
    c.respond(Response::Terminal {
        instance: 1,
        chunk: TerminalChunk {
            scope: scope(),
            attachment_id: 17,
            data: b"FIRST\r\n".to_vec(),
            ended: Some(TerminalEnd::Normal),
        },
    });
    c.respond(Response::Action {
        instance: 1,
        token,
        scope: scope(),
        result: Ok(ActionPlan::Dispatch { operation_id: 17 }),
    });
    assert_eq!(inspector.terminal.binding(), Some((scope(), 17)));
    assert!(
        !inspector.terminal.is_active(),
        "late continuation cannot revive/reset the ended renderer"
    );
    let renderer = c.attachments.borrow()[0].2.clone();
    let tabs = descendants(inspector.root.upcast_ref())
        .into_iter()
        .find_map(|w| w.downcast::<gtk::Notebook>().ok())
        .unwrap();
    tabs.set_current_page(Some(4));
    until(|| renderer.terminal.is_realized());
    renderer.terminal.select_all();
    renderer.terminal.copy_clipboard_format(vte4::Format::Text);
    let terminal_text = glib::MainContext::default()
        .block_on(shell.window().clipboard().read_text_future())
        .unwrap()
        .unwrap();
    assert!(terminal_text.contains("FIRST"));
    assert!(!terminal_text.contains("WRONG"));
    capture("fixture-terminal-ended");
    eprintln!("Identified ended renderer retained");
    let count = recorder.0.borrow().len();
    renderer
        .terminal
        .emit_by_name::<()>("commit", &[&"bad", &3u32]);
    assert_eq!(
        recorder.0.borrow().len(),
        count,
        "ended/unfocused terminal sends no bytes"
    );

    // OSC output cannot open another window, redirect app identity or change
    // the clipboard. Only the native Copy control writes a selection.
    let original_title = shell.window().title();
    let standalone = TerminalView::new(Rc::new(|_| panic!("unfocused output must not send input")));
    standalone.bind(scope(), 31);
    let clipboard = shell.window().clipboard();
    clipboard.set_text("preserve");
    standalone.accept(&TerminalChunk { scope: scope(), attachment_id: 31, data: b"\x1b]52;c;b3ZlcndyaXRl\x07\x1b]8;;https://example.test/\x07link\x1b]8;;\x07\x1b]0;changed\x07".to_vec(), ended: None });
    settle();
    assert_eq!(
        glib::MainContext::default()
            .block_on(clipboard.read_text_future())
            .unwrap()
            .as_deref(),
        Some("preserve")
    );
    assert_eq!(shell.window().title(), original_title);
    eprintln!("OSC safety checked");

    c.compose("claude-1".into());
    eprintln!("Opening retained draft");
    let form = c.forms.borrow().values().next().unwrap().form.clone();
    let plans_before = recorder
        .0
        .borrow()
        .iter()
        .filter(|request| matches!(request, Request::Plan { .. }))
        .count();
    form.buffer.set_text("Retain this draft");
    assert_eq!(
        recorder
            .0
            .borrow()
            .iter()
            .filter(|request| matches!(request, Request::Plan { .. }))
            .count(),
        plans_before,
        "typing and querying availability cannot plan a mutation"
    );
    let read_only = core_with_mode(ConnectionMode::ScopedGateway, vec![GrantTier::Read]);
    recorder.queries(&c, &read_only);
    assert!(!form.submit.is_sensitive());
    assert_eq!(form.error.label().as_str(), "Control authority is required");
    let form_id = *c.forms.borrow().keys().next().unwrap();
    c.refresh_control(form_id);
    recorder.queries(&c, &core);
    assert!(form.submit.is_sensitive());
    form.dialog.response(gtk::ResponseType::Accept);
    let token = recorder.plan_token();
    c.respond(Response::Action {
        instance: 1,
        token,
        scope: scope(),
        result: Ok(ActionPlan::Unavailable {
            reason: ClientReason {
                code: "offline".into(),
                message: "Workspace unavailable".into(),
            },
        }),
    });
    assert_eq!(common::buffer_text(&form.buffer), "Retain this draft");
    recorder.queries(&c, &core);
    assert!(form.submit.is_sensitive());
    form.buffer.set_text("/unknown keep");
    form.dialog.response(gtk::ResponseType::Accept);
    assert!(form.error.label().contains("No command"));
    capture("fixture-draft-failure");
    eprintln!("Draft failure retained");
    form.dialog.response(gtk::ResponseType::Cancel);
    reconnected_receipts(&c, &recorder);
    c.respond(Response::Notice {
        instance: 1,
        notice: notice(Some(scope())),
    });
    let old_token = {
        c.query(ViewQuery::Team, QueryTarget::Main(Destination::Team));
        c.next_token.get() - 1
    };
    let mut other = scope();
    other.workspace_id = "/other".into();
    other.attempt = 2;
    c.respond(Response::Notice {
        instance: 1,
        notice: notice(Some(other)),
    });
    c.respond(Response::View {
        instance: 1,
        token: old_token,
        expected_scope: Some(scope()),
        result: core.view(ViewQuery::Team, clock()),
    });
    assert!(
        !text(shell.window()).contains("claude-1"),
        "late view cannot republish the old workspace"
    );
    c.respond(Response::Session(2));
    c.respond(Response::Notice {
        instance: 1,
        notice: notice(Some(scope())),
    });
    assert!(
        c.scope.borrow().is_none(),
        "retired native instance cannot rebind even an equal scope"
    );
    shell.window().close();
    drop(c);
    settle();
    eprintln!("Fixture window closed");

    live_local(&app);
    eprintln!("Starting production local service");
}

/// Show a graph's page in its List/Graph stack and let it allocate.
fn show_graph(graph: &views::graph::Graph) {
    graph
        .root
        .parent()
        .and_downcast::<gtk::Stack>()
        .expect("List/Graph stack")
        .set_visible_child(&graph.root);
    frames();
}
/// Let the frame clock allocate, so transforms and bounds reflect the change.
fn frames() {
    for _ in 0..5 {
        settle();
        std::thread::sleep(Duration::from_millis(20));
    }
    settle();
}
/// Whole graph in view: the zoomed extent fits the scrolled region.
fn in_view(graph: &views::graph::Graph) -> bool {
    let scroll = graph
        .root
        .last_child()
        .and_downcast::<gtk::ScrolledWindow>()
        .unwrap();
    let (width, height) = graph.extent();
    width * graph.zoom() <= f64::from(scroll.width()) + 1.0
        && height * graph.zoom() <= f64::from(scroll.height()) + 1.0
}
fn chips(widget: &gtk::Widget) -> Vec<String> {
    descendants(widget)
        .into_iter()
        .filter(|w| w.has_css_class("crew-chip"))
        .filter_map(|w| w.downcast::<gtk::Label>().ok())
        .map(|l| l.label().to_string())
        .collect()
}
/// Layered columns centred per layer, drawn relationships, status chips, Fit,
/// uniform zoom and one-tab-stop keyboard traversal, on both graphs.
fn graph_topology(c: &Controller, shell: &Shell, core: &ClientCore) {
    use views::graph::EdgeKind;
    let ViewPayload::Team(team) = core.view(ViewQuery::Team, clock()).unwrap().payload else {
        panic!("team projection");
    };
    let mut large = (*team).clone();
    for i in 2..=13 {
        let mut member = team.agents[0].clone();
        member.agent.id = format!("claude-{i}");
        member.agent.name = member.agent.id.clone();
        large.agents.push(member);
    }
    c.team.update(Some(&scope()), &large);
    let graph = c.team.graph();
    show_graph(graph);
    let nodes = graph.nodes();
    assert_eq!(nodes.len(), 14, "scheduler plus thirteen agents");
    let edges = graph.edges();
    assert_eq!(edges.len(), 13);
    assert!(edges
        .iter()
        .all(|e| e.from == 0 && e.kind == EdgeKind::Coordination));
    let origins = graph.origins();
    assert!(origins[1..].iter().all(|o| o.0 > origins[0].0));
    let member_columns = {
        let mut xs: Vec<i64> = origins[1..].iter().map(|o| o.0 as i64).collect();
        xs.dedup();
        xs.len()
    };
    assert!(
        member_columns > 1,
        "thirteen members wrap into several columns"
    );
    let geometry = graph.geometry();
    assert!(geometry.banded && geometry.bands[1].columns.len() == member_columns);
    assert_eq!(graph.band_titles(), ["Members"], "the hub has no band");
    views::graph::layout::assert_clean(&geometry);
    let top = origins[1..].iter().map(|o| o.1).fold(f64::MAX, f64::min);
    let node_height = f64::from(nodes[0].height());
    let bottom = origins[1..].iter().map(|o| o.1).fold(f64::MIN, f64::max) + node_height;
    assert!(
        ((origins[0].1 + node_height / 2.0) - (top + bottom) / 2.0).abs() < 1.0,
        "the scheduler is centred against its members"
    );
    assert_eq!(chips(&nodes[1]), ["working"]);
    // A push never refits: the graph already fitted its first two nodes.
    let pushed = graph.zoom();
    assert!(
        !in_view(graph),
        "fourteen nodes overflow at the earlier zoom"
    );
    button(&graph.root, "Fit graph").emit_clicked();
    frames();
    assert!(
        in_view(graph) && graph.zoom() < pushed,
        "Fit scales all fourteen nodes into view"
    );
    capture("fixture-team-graph-fit");
    button(&graph.root, "Zoom in").emit_clicked();
    button(&graph.root, "Zoom in").emit_clicked();
    frames();
    assert!(!in_view(graph));
    button(&graph.root, "Fit graph").emit_clicked();
    frames();
    assert!(in_view(graph), "Fit returns the whole graph to view");

    // Uniform zoom scales the drawing; allocation and text wrapping stay put.
    let title = descendants(&nodes[1])
        .into_iter()
        .find(|w| w.has_css_class("crew-title"))
        .and_downcast::<gtk::Label>()
        .unwrap();
    let (width, lines) = (nodes[1].width(), title.layout().line_count());
    let before = nodes[1].compute_bounds(&graph.root).unwrap().width();
    let zoom = graph.zoom();
    button(&graph.root, "Zoom in").emit_clicked();
    frames();
    let after = nodes[1].compute_bounds(&graph.root).unwrap().width();
    assert_eq!(nodes[1].width(), width);
    assert_eq!(title.layout().line_count(), lines, "zoom never rewraps");
    assert!(
        (f64::from(after) / f64::from(before) - graph.zoom() / zoom).abs() < 0.02,
        "node bounds scale with zoom: {before} -> {after} at {zoom} -> {}",
        graph.zoom()
    );

    // One tab stop; arrows move inside the graph; the scheduler is skipped.
    assert_eq!(nodes.iter().filter(|n| n.is_focusable()).count(), 1);
    assert!(!nodes[0].is_focusable());
    nodes[1].grab_focus();
    assert!(graph.navigate(gtk::gdk::Key::Down));
    assert!(nodes[2].is_focus());
    assert!(graph.navigate(gtk::gdk::Key::End));
    assert!(nodes[13].is_focus());
    // Left crosses wrapped member columns and stops before the scheduler.
    for _ in 0..member_columns + 1 {
        assert!(graph.navigate(gtk::gdk::Key::Left));
    }
    let focused = nodes.iter().position(|n| n.is_focus()).unwrap();
    assert!(focused > 0 && origins[focused].0 == origins[1].0);
    assert_eq!(nodes.iter().filter(|n| n.is_focusable()).count(), 1);

    // Task dependencies: two layers, one edge per resolved dependency.
    let ViewPayload::Tasks(tasks) = core.view(ViewQuery::Tasks, clock()).unwrap().payload else {
        panic!("tasks projection");
    };
    let mut tasks = (*tasks).clone();
    let first = tasks.tasks[0].task.id.clone();
    for (i, item) in tasks.tasks.iter_mut().enumerate().skip(1) {
        item.layer = 1;
        item.dependencies.push(DependencyState {
            id: first.clone(),
            status: Some(tasks_status(i)),
        });
    }
    tasks.tasks[2].dependencies.push(DependencyState {
        id: "absent".into(),
        status: None,
    });
    shell.navigate(Destination::Tasks);
    c.tasks.update(Some(&scope()), &tasks);
    let graph = c.tasks.graph();
    show_graph(graph);
    let nodes = graph.nodes();
    assert_eq!(nodes.len(), 3);
    let edges = graph.edges();
    assert_eq!(edges.len(), 2, "the missing dependency has no node or edge");
    assert!(edges
        .iter()
        .all(|e| e.from == 0 && e.kind == EdgeKind::Dependency));
    let origins = graph.origins();
    assert!(origins[1].0 > origins[0].0 && origins[1].0 == origins[2].0);
    assert!(
        (origins[0].1 - (origins[1].1 + origins[2].1) / 2.0).abs() < 1.0,
        "layer 0 is centred against layer 1"
    );
    assert!(text(&nodes[2]).contains("Depends on ‘absent’ (not in this workspace)"));
    assert!(chips(&nodes[0]).len() == 1 && in_view(graph));
    assert!(!graph.geometry().banded && graph.band_titles().is_empty());
    capture("fixture-task-graph");
    nodes[0].grab_focus();
    assert!(graph.navigate(gtk::gdk::Key::Right));
    assert!(nodes[1].is_focus());
    assert!(graph.navigate(gtk::gdk::Key::Down));
    assert!(nodes[2].is_focus());
    assert!(graph.navigate(gtk::gdk::Key::Left));
    assert!(nodes[0].is_focus());
    // A list selection marks the same node in the graph.
    graph.set_selected(Some(&key(Entity::Task(first))));
    assert!(nodes[0].has_css_class("selected"));
    assert!(nodes[0].is_focusable() && !nodes[1].is_focusable());
    wrapped_tasks(c, &tasks);
    for graph in [c.team.graph(), c.tasks.graph()] {
        graph
            .root
            .parent()
            .and_downcast::<gtk::Stack>()
            .unwrap()
            .set_visible_child_name("list");
    }
}
/// Six roots wrap into one titled band; depth stays one band per layer and no
/// edge runs between sibling rows.
fn wrapped_tasks(c: &Controller, small: &TasksData) {
    let template = small.tasks[0].clone();
    let layers = [0, 0, 0, 0, 0, 0, 1, 2];
    let dependencies: [&[usize]; 8] = [&[], &[], &[], &[], &[], &[], &[1, 4, 5], &[6, 2]];
    let tasks = TasksData {
        tasks: (0..8)
            .map(|i| {
                let mut item = template.clone();
                item.task.id = format!("t{i}");
                item.task.title = format!("Wrapped task {i}");
                item.layer = layers[i];
                item.dependencies = dependencies[i]
                    .iter()
                    .map(|&d| DependencyState {
                        id: format!("t{d}"),
                        status: Some(tasks_status(d)),
                    })
                    .collect();
                item
            })
            .collect(),
        layers: Vec::new(),
    };
    c.tasks.update(Some(&scope()), &tasks);
    let graph = c.tasks.graph();
    button(&graph.root, "Fit graph").emit_clicked();
    frames();
    let geometry = graph.geometry();
    assert!(geometry.banded, "six roots wrap at this window size");
    assert!(geometry.bands[0].columns.len() > 1);
    assert_eq!(
        graph.band_titles(),
        ["No dependencies", "Layer 1", "Layer 2"]
    );
    for pair in geometry.bands.windows(2) {
        assert!(pair[1].left - pair[0].right >= views::graph::layout::BAND_GAP - 0.5);
    }
    views::graph::layout::assert_clean(&geometry);
    assert_eq!(graph.edges().len(), 5);
    assert!(in_view(graph));
    capture("fixture-task-graph-bands");
    c.tasks.update(Some(&scope()), small);
    frames();
}
fn tasks_status(i: usize) -> crew_protocol::TaskStatus {
    if i == 1 {
        crew_protocol::TaskStatus::Done
    } else {
        crew_protocol::TaskStatus::Working
    }
}

struct PrivateDaemon {
    socket: PathBuf,
    thread: Option<std::thread::JoinHandle<()>>,
}
fn begin_receipt(c: &Controller, recorder: &Recording, body: &str, operation: u64) -> (u64, Form) {
    c.respond(Response::Notice {
        instance: 1,
        notice: notice(Some(scope())),
    });
    c.compose_message(
        "claude-1".into(),
        Some(body.into()),
        Some((
            "claude-1".into(),
            body.into(),
            format!("receipt-{operation}"),
        )),
    );
    let id = *c.forms.borrow().keys().next_back().unwrap();
    let form = c.forms.borrow().get(&id).unwrap().form.clone();
    form.dialog.response(gtk::ResponseType::Accept);
    let token = recorder.plan_token();
    c.respond(Response::Action {
        instance: 1,
        token,
        scope: scope(),
        result: Ok(ActionPlan::Dispatch {
            operation_id: operation,
        }),
    });
    (id, form)
}
fn last_form(c: &Controller) -> (u64, Form) {
    let id = *c.forms.borrow().keys().next_back().unwrap();
    let form = c.forms.borrow().get(&id).unwrap().form.clone();
    (id, form)
}
/// The latest planned message as (scope, recipient, body, message ID).
fn last_send(recorder: &Recording) -> (ClientScope, String, String, String) {
    recorder
        .0
        .borrow()
        .iter()
        .rev()
        .find_map(|request| match request {
            Request::Plan {
                intent:
                    ActionIntent::SendMessage {
                        scope,
                        to,
                        body,
                        message_id,
                    },
                ..
            } => Some((scope.clone(), to.clone(), body.clone(), message_id.clone())),
            _ => None,
        })
        .expect("one planned message")
}
/// Answers the latest availability query as Core would for an eligible send.
fn grant_send(c: &Controller, recorder: &Recording, scope: &ClientScope) {
    let token = recorder
        .0
        .borrow()
        .iter()
        .rev()
        .find_map(|request| match request {
            Request::Query {
                token,
                query: ViewQuery::Availability { .. },
                ..
            } => Some(*token),
            _ => None,
        })
        .expect("one availability query");
    c.respond(Response::View {
        instance: 1,
        token,
        expected_scope: Some(scope.clone()),
        result: Ok(ViewData {
            scope: Some(scope.clone()),
            revision: 1,
            synchronized_at_ms: None,
            freshness: Freshness::Fresh,
            payload: ViewPayload::Availability(ActionAvailability {
                action: ActionKind::SendMessage,
                available: true,
                reason: None,
            }),
        }),
    });
}
fn unsent_draft_across_reconnect(c: &Controller, recorder: &Recording) {
    c.respond(Response::Notice {
        instance: 1,
        notice: notice(Some(scope())),
    });
    c.compose("claude-1".into());
    let (id, form) = last_form(c);
    form.buffer.set_text("Unsent across reconnect");
    let mut reconnected = scope();
    reconnected.attempt = 2;
    c.respond(Response::Notice {
        instance: 1,
        notice: notice(Some(reconnected.clone())),
    });
    assert!(!c.forms.borrow().contains_key(&id));
    c.compose("claude-1".into());
    let (_, form) = last_form(c);
    assert_eq!(
        common::buffer_text(&form.buffer),
        "Unsent across reconnect",
        "unsent text is kept by endpoint/workspace/purpose, not by attempt"
    );
    form.dialog.response(gtk::ResponseType::Accept);
    let (planned, _, body, _) = last_send(recorder);
    assert_eq!(
        planned, reconnected,
        "the restored draft plans with the current verified scope"
    );
    assert_eq!(body, "Unsent across reconnect");
    c.respond(Response::Action {
        instance: 1,
        token: recorder.plan_token(),
        scope: reconnected,
        result: Ok(ActionPlan::Unavailable {
            reason: ClientReason {
                code: "offline".into(),
                message: "Workspace unavailable".into(),
            },
        }),
    });
    form.dialog.response(gtk::ResponseType::Cancel);
}
/// Accepted A whose reply was lost: the original receipt stays read-only;
/// an unchanged restore keeps A's ID, any edit takes a fresh ID.
fn restored_identity(c: &Controller, recorder: &Recording, unknown: u64, current: &ClientScope) {
    c.restore_receipt(unknown);
    grant_send(c, recorder, current);
    let (_, unchanged) = last_form(c);
    assert!(c.forms.borrow().contains_key(&unknown));
    assert_eq!(
        common::buffer_text(&unchanged.buffer),
        "Unknown submitted body"
    );
    unchanged.dialog.response(gtk::ResponseType::Accept);
    let (planned, to, body, message_id) = last_send(recorder);
    assert_eq!(&planned, current);
    assert_eq!(
        (to.as_str(), body.as_str()),
        ("claude-1", "Unknown submitted body")
    );
    assert_eq!(
        message_id, "receipt-2002",
        "an unchanged retry keeps the original ID"
    );

    c.restore_receipt(unknown);
    grant_send(c, recorder, current);
    let (_, edited) = last_form(c);
    edited.buffer.set_text("Edited B body");
    edited.dialog.response(gtk::ResponseType::Accept);
    let (_, _, body, edited_id) = last_send(recorder);
    assert_eq!(body, "Edited B body");
    assert_ne!(
        edited_id, "receipt-2002",
        "an edited body cannot reuse A's ID, which the bus would return as a duplicate"
    );

    c.restore_receipt(unknown);
    grant_send(c, recorder, current);
    let (_, readdressed) = last_form(c);
    let entry = descendants(readdressed.dialog.upcast_ref())
        .into_iter()
        .find_map(|w| w.downcast::<gtk::Entry>().ok())
        .expect("recipient entry");
    entry.set_text("claude-2");
    readdressed.dialog.response(gtk::ResponseType::Accept);
    let (_, to, _, readdressed_id) = last_send(recorder);
    assert_eq!(to, "claude-2");
    assert!(readdressed_id != "receipt-2002" && readdressed_id != edited_id);
    let unknown_form = c.forms.borrow().get(&unknown).unwrap().form.clone();
    assert!(unknown_form.receipt.get());
    assert!(unknown_form.error.label().contains("Outcome unknown"));
}
fn reconnected_receipts(c: &Controller, recorder: &Recording) {
    unsent_draft_across_reconnect(c, recorder);
    c.respond(Response::Notice {
        instance: 1,
        notice: notice(Some(scope())),
    });
    c.compose_message(
        "claude-1".into(),
        Some("Early reconnected success".into()),
        Some((
            "claude-1".into(),
            "Early reconnected success".into(),
            "receipt-early".into(),
        )),
    );
    let early_id = *c.forms.borrow().keys().next_back().unwrap();
    let early_form = c.forms.borrow().get(&early_id).unwrap().form.clone();
    early_form.dialog.response(gtk::ResponseType::Accept);
    let token = recorder.plan_token();
    let mut early_scope = scope();
    early_scope.attempt = 2;
    c.respond(Response::Notice {
        instance: 1,
        notice: notice(Some(early_scope.clone())),
    });
    let mut early_success = notice(Some(early_scope.clone()));
    early_success.outcomes.push(OperationNotice {
        scope: early_scope,
        operation_id: 2000,
        attachment_id: None,
        kind: OperationOutcomeKind::Success,
        reason: None,
    });
    c.respond(Response::Notice {
        instance: 1,
        notice: early_success,
    });
    assert!(c.forms.borrow().contains_key(&early_id));
    c.respond(Response::Action {
        instance: 1,
        token,
        scope: scope(),
        result: Ok(ActionPlan::Dispatch { operation_id: 2000 }),
    });
    assert!(
        !c.forms.borrow().contains_key(&early_id),
        "early replay success uses endpoint/workspace/op before the old-scope plan continuation"
    );
    assert_eq!(common::buffer_text(&early_form.buffer), "");
    let (id, form) = begin_receipt(c, recorder, "Original submitted body", 2001);
    form.dialog.response(gtk::ResponseType::Cancel);
    assert!(
        c.forms.borrow().contains_key(&id),
        "Close hides the submitted receipt without canceling or losing it"
    );
    let plans = recorder
        .0
        .borrow()
        .iter()
        .filter(|request| matches!(request, Request::Plan { .. }))
        .count();
    let mut disconnected = notice(None);
    disconnected.endpoint = EndpointStatus::Reconnecting;
    disconnected.workspace = WorkspaceStatus::Reconnecting;
    c.respond(Response::Notice {
        instance: 1,
        notice: disconnected,
    });
    assert!(form.pending.get());
    assert!(
        c.operations.borrow().contains_key(&2001),
        "same-workspace reattempt retains the Core-held operation entry"
    );
    assert!(!form.submit.is_sensitive());
    assert_eq!(common::buffer_text(&form.buffer), "Original submitted body");
    let mut after = scope();
    after.attempt = 2;
    c.respond(Response::Notice {
        instance: 1,
        notice: notice(Some(after.clone())),
    });
    assert_eq!(
        recorder
            .0
            .borrow()
            .iter()
            .filter(|request| matches!(request, Request::Plan { .. }))
            .count(),
        plans,
        "reauthorization cannot replan the held message"
    );
    let mut mismatch = notice(Some(after.clone()));
    mismatch.outcomes.push(OperationNotice {
        scope: ClientScope {
            workspace_id: "/wrong".into(),
            ..after.clone()
        },
        operation_id: 2001,
        attachment_id: None,
        kind: OperationOutcomeKind::Success,
        reason: None,
    });
    c.respond(Response::Notice {
        instance: 1,
        notice: mismatch,
    });
    assert!(c.forms.borrow().contains_key(&id));
    let mut success = notice(Some(after.clone()));
    success.outcomes.push(OperationNotice {
        scope: after.clone(),
        operation_id: 2001,
        attachment_id: None,
        kind: OperationOutcomeKind::Success,
        reason: None,
    });
    c.respond(Response::Notice {
        instance: 1,
        notice: success,
    });
    assert!(
        !c.forms.borrow().contains_key(&id),
        "new-attempt scope plus original operation resolves exact receipt"
    );
    assert_eq!(
        common::buffer_text(&form.buffer),
        "",
        "matching success clears only the submitted body"
    );

    let (id, form) = begin_receipt(c, recorder, "Unknown submitted body", 2002);
    c.respond(Response::Notice {
        instance: 1,
        notice: notice(Some(after.clone())),
    });
    let mut unknown = notice(Some(after.clone()));
    unknown.outcomes.push(OperationNotice {
        scope: after.clone(),
        operation_id: 2002,
        attachment_id: None,
        kind: OperationOutcomeKind::Unknown,
        reason: Some(ClientReason {
            code: "closed".into(),
            message: "Acknowledgment unavailable".into(),
        }),
    });
    c.respond(Response::Notice {
        instance: 1,
        notice: unknown,
    });
    assert!(c.forms.borrow().contains_key(&id));
    assert_eq!(common::buffer_text(&form.buffer), "Unknown submitted body");
    assert!(form.error.label().contains("Outcome unknown"));
    assert!(!form.submit.is_sensitive());
    restored_identity(c, recorder, id, &after);
    let (edited_id, edited) = begin_receipt(c, recorder, "Acknowledged original", 2003);
    edited.buffer.set_text("Later edited draft");
    c.respond(Response::Notice {
        instance: 1,
        notice: notice(Some(after.clone())),
    });
    let mut edited_success = notice(Some(after.clone()));
    edited_success.outcomes.push(OperationNotice {
        scope: after,
        operation_id: 2003,
        attachment_id: None,
        kind: OperationOutcomeKind::Success,
        reason: None,
    });
    c.respond(Response::Notice {
        instance: 1,
        notice: edited_success,
    });
    assert!(c.forms.borrow().contains_key(&edited_id));
    assert_eq!(common::buffer_text(&edited.buffer), "Later edited draft");
    let mut revoke = notice(None);
    revoke.workspace = WorkspaceStatus::AccessRevoked;
    c.respond(Response::Notice {
        instance: 1,
        notice: revoke,
    });
    assert!(c.forms.borrow().is_empty());
    assert!(c.operations.borrow().is_empty());
    assert_eq!(
        common::buffer_text(&form.buffer),
        "",
        "learned revocation clears retained receipt text"
    );
}
impl Drop for PrivateDaemon {
    fn drop(&mut self) {
        if let Ok((mut caller, _)) =
            crew_core::client::Client::operator(&self.socket, "gtk-test-cleanup")
        {
            let _ = caller
                .call::<serde_json::Value>(crew_core::rpc::Call::Shutdown { stop_agents: true });
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
fn live_local(app: &gtk::Application) {
    let workspace = tempfile::Builder::new()
        .prefix("crew-gtk-live")
        .tempdir_in("/tmp")
        .unwrap();
    let path = workspace.path().canonicalize().unwrap();
    let (send, receive) = std::sync::mpsc::channel();
    let daemon_path = path.clone();
    let daemon = std::thread::spawn(move || {
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let running = crew::daemon::start(crew::daemon::DaemonOptions {
                workspace: daemon_path,
                crew_bin: std::env::current_exe().unwrap(),
                pin: None,
            })
            .await
            .unwrap();
            send.send(running.socket().to_path_buf()).unwrap();
            running.serve().await.unwrap();
        });
    });
    let socket = receive.recv_timeout(Duration::from_secs(10)).unwrap();
    let mut private_daemon = PrivateDaemon {
        socket: socket.clone(),
        thread: Some(daemon),
    };
    let shell = Shell::new(app);
    let c = Controller::start(shell.clone());
    shell.window().present();
    until(|| c.profiles.borrow().len() == 1);
    eprintln!("Private daemon discovered");
    c.connections();
    let dialog = c.auxiliary.borrow().last().unwrap().clone();
    button(
        dialog.upcast_ref::<gtk::Widget>(),
        "Connect to local workspace",
    )
    .emit_clicked();
    live_until(&c, || c.live.get());
    eprintln!("Private daemon live");
    assert_eq!(
        c.scope.borrow().as_ref().unwrap().workspace_id,
        path.to_string_lossy()
    );
    live_until(&c, || c.connection.borrow().is_some());
    assert_eq!(
        c.connection.borrow().as_ref().unwrap().mode,
        ConnectionMode::LocalOperator
    );
    // One handler's burst larger than the worker channel waits in order on
    // the GTK side; nothing is refused as busy and every answer arrives.
    let live = c.scope.borrow().clone().unwrap();
    let burst: Vec<QueryTarget> = (0..64)
        .map(|i| {
            QueryTarget::Detail(EntityKey {
                scope: live.clone(),
                entity: Entity::Agent(format!("burst-{i}")),
            })
        })
        .collect();
    for target in &burst {
        c.query(ViewQuery::Overview, target.clone());
    }
    assert!(!c.result.label().contains("busy"));
    assert_eq!(
        c.queries
            .borrow()
            .values()
            .filter(|t| burst.contains(t))
            .count(),
        64,
        "every burst query was accepted"
    );
    live_until(&c, || {
        !c.queries.borrow().values().any(|t| burst.contains(t))
    });
    shell.navigate(Destination::Messages);
    let (mut caller, _) = crew_core::client::Client::operator(&socket, "gtk-private-test").unwrap();
    let _: crew_core::rpc::SendResult = caller
        .call(crew_core::rpc::Call::Send(crew_core::rpc::SendParams {
            to: "*".into(),
            body: "Real private daemon message".into(),
            id: Some("gtk-live-message".into()),
            ..Default::default()
        }))
        .unwrap();
    live_until(&c, || {
        text(shell.window()).contains("Real private daemon message")
    });
    capture("live-local-messages");
    let scope = c.scope.borrow().clone().unwrap();
    c.select(EntityKey {
        scope,
        entity: Entity::Message("gtk-live-message".into()),
    });
    live_until(&c, || {
        text(shell.window()).contains("Ready")
            && descendants(shell.window().upcast_ref())
                .iter()
                .filter_map(|w| w.clone().downcast::<gtk::TextView>().ok())
                .any(|v| common::buffer_text(&v.buffer()).contains("Real private daemon message"))
    });
    capture("live-local-evidence");
    c.compose("*".into());
    let form = c.forms.borrow().values().next().unwrap().form.clone();
    form.buffer
        .set_text("Submitted through the real GTK service");
    form.dialog.response(gtk::ResponseType::Accept);
    live_until(&c, || c.forms.borrow().is_empty());
    live_until(&c, || {
        text(shell.window()).contains("Submitted through the real GTK service")
    });
    capture("live-local-send");

    let config: crew_core::config::TeamConfig = serde_json::from_value(serde_json::json!({
        "project": "native-gtk-validation", "workspace": {"path": path, "isolation":"shared"}, "backend":"pty", "orchestrator":{"agent":false},
        "agents":[{"id":"pty-fixture", "role":"native terminal", "runtime":"shell", "command":["/bin/sh","-c","printf 'native PTY ready\\n'; while IFS= read -r line; do printf 'native-input:%s\\n' \"$line\"; done"]}], "tasks":[]
    })).unwrap();
    let _: serde_json::Value = caller
        .call(crew_core::rpc::Call::LoadTeam {
            config: Box::new(config),
            herdr_socket: None,
        })
        .unwrap();
    shell.navigate(Destination::Team);
    live_until(&c, || text(shell.window()).contains("pty-fixture"));
    // The Team row can precede the real spawn, and the daemon rejects Attach
    // with "has no process" until the PTY exists. Wait for that process.
    live_until(&c, || {
        let snapshot: crew_core::rpc::Snapshot = caller
            .call(crew_core::rpc::Call::Snapshot {
                messages: Some(0),
                events: Some(0),
            })
            .unwrap();
        snapshot
            .agents
            .iter()
            .any(|agent| agent.id == "pty-fixture" && agent.pid.is_some())
    });
    let live_scope = c.scope.borrow().clone().unwrap();
    let agent_key = EntityKey {
        scope: live_scope.clone(),
        entity: Entity::Agent("pty-fixture".into()),
    };
    c.select(agent_key.clone());
    live_until(&c, || c.agent.borrow().as_ref().unwrap().1.root.is_mapped());
    let agent = c.agent.borrow().as_ref().unwrap().1.clone();
    live_until(&c, || {
        button(&agent.root, "Attach PTY terminal").is_sensitive()
    });
    button(&agent.root, "Attach PTY terminal").emit_clicked();
    live_until(&c, || agent.terminal.is_active());
    let tabs = descendants(agent.root.upcast_ref())
        .into_iter()
        .find_map(|w| w.downcast::<gtk::Notebook>().ok())
        .unwrap();
    tabs.set_current_page(Some(4));
    let (scope, attachment_id) = agent.terminal.binding().unwrap();
    let renderer = c
        .attachments
        .borrow()
        .iter()
        .find(|(s, id, _)| s == &scope && *id == attachment_id)
        .unwrap()
        .2
        .clone();
    live_until(&c, || renderer.terminal.is_realized());
    c.plan(
        ActionIntent::AttachInput {
            scope,
            attachment_id,
            data: b"gtk-input\n".to_vec(),
        },
        None,
        None,
    );
    live_until(&c, || {
        let output: crew_core::rpc::OutputResult = caller
            .call(crew_core::rpc::Call::AgentOutput {
                id: "pty-fixture".into(),
                lines: Some(80),
                screen: false,
                ansi: false,
            })
            .unwrap();
        output.text.contains("native-input:gtk-input")
    });
    capture("live-local-terminal");
    button(&renderer.root, "Detach terminal").emit_clicked();
    live_until(&c, || !renderer.is_active());
    assert!(
        c.live.get(),
        "local detach keeps the workspace connection usable"
    );
    shell.window().close();
    drop(c);
    settle();
    let snapshot: crew_core::rpc::Snapshot = caller
        .call(crew_core::rpc::Call::Snapshot {
            messages: Some(10),
            events: Some(10),
        })
        .unwrap();
    assert!(
        snapshot.agents.iter().any(|agent| agent.id == "pty-fixture"
            && agent.pid.is_some()
            && !agent.status.is_terminal()),
        "closing GTK client leaves the daemon and its real PTY agent running"
    );
    let _: serde_json::Value = caller
        .call(crew_core::rpc::Call::Shutdown { stop_agents: true })
        .unwrap();
    private_daemon.thread.take().unwrap().join().unwrap();
}

fn descendants(widget: &gtk::Widget) -> Vec<gtk::Widget> {
    let mut out = Vec::new();
    let mut child = widget.first_child();
    while let Some(w) = child {
        out.extend(descendants(&w));
        child = w.next_sibling();
        out.push(w);
    }
    out
}
fn text(widget: &impl IsA<gtk::Widget>) -> String {
    descendants(widget.as_ref())
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::Label>().ok())
        .map(|l| l.label().to_string())
        .collect::<Vec<_>>()
        .join("\n")
}
fn button(widget: &impl IsA<gtk::Widget>, name: &str) -> gtk::Button {
    descendants(widget.as_ref())
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::Button>().ok())
        .find(|b| b.label().as_deref() == Some(name))
        .unwrap_or_else(|| panic!("native button {name}"))
}
fn settle() {
    let main = glib::MainContext::default();
    while main.pending() {
        main.iteration(false);
    }
}
fn until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(Instant::now() < deadline, "GTK/service condition timed out");
        settle();
        std::thread::sleep(Duration::from_millis(5));
    }
    settle();
}
/// `until` against the real service, reporting controller state on timeout.
fn live_until(c: &Controller, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "GTK/service condition timed out: status {:?}, result {:?}, scope {:?}, \
             agent panel {}, plans {}, operations {}",
            c.status.label(),
            c.result.label(),
            c.scope.borrow(),
            c.agent.borrow().is_some(),
            c.plans.borrow().len(),
            c.operations.borrow().len()
        );
        settle();
        std::thread::sleep(Duration::from_millis(5));
    }
    settle();
}
fn capture(name: &str) {
    // Let the native frame clock paint model changes before an external X11
    // capture. A synchronous screen grab otherwise records the previous frame.
    for _ in 0..3 {
        settle();
        std::thread::sleep(Duration::from_millis(20));
    }
    settle();
    if let Some(dir) = std::env::var_os("CREW_NATIVE_UI_EVIDENCE_DIR") {
        let file = PathBuf::from(dir).join(format!("{name}.png"));
        assert!(std::process::Command::new("magick")
            .args(["import", "-window", "root"])
            .arg(file)
            .status()
            .unwrap()
            .success());
    }
}
