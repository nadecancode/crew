//! Command-line interface.

use std::path::PathBuf;

use anyhow::{bail, Result};
use clap::{Args, CommandFactory, Parser, Subcommand};

mod agent_side;
mod attach;
pub mod daemonize;
mod fake_cli;
mod ops;
mod scripted;

#[derive(Parser)]
#[command(
    name = "crew",
    version,
    about = "Run a team of Claude Code and Codex agents that talk to each other directly",
    long_about = "crew launches a team of coding agents (Claude Code, Codex), gives each one a \
role and a task, lets them message each other directly over a local bus, and shows the whole \
thing in a terminal dashboard. A background daemon owns the agents, so closing the dashboard \
never stops them.

Run plain `crew` in a project directory (an empty one is fine) to start: pick a work \
style (research, experiment, …), say what to do, and an orchestrator staffs and plans the team. Run it again \
later to get back to the dashboard.",
    after_help = "Examples:
  crew                                 start here: pick a work style, say what to do; or reopen the dashboard
  crew \"build a todo CLI with tests\"   start with the objective given up front
  crew --style research \"how does X work?\"   launch a work style directly
  crew run examples/demo.yaml          launch a team from a team file
  crew status | logs -f | stop         inspect or stop the running team"
)]
pub struct Cli {
    /// Workspace directory of the session to talk to (default: discovered)
    #[arg(short = 'C', long = "dir", global = true, value_name = "DIR")]
    pub dir: Option<PathBuf>,
    /// Project name of the session to talk to, when several are running
    #[arg(long, global = true, value_name = "NAME")]
    pub project: Option<String>,
    /// What the team should build, as one quoted sentence (no subcommand)
    #[arg(value_name = "OBJECTIVE")]
    pub objective: Option<String>,
    #[command(flatten)]
    pub quick: QuickArgs,
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Options of plain `crew` (no subcommand).
#[derive(Args, Clone, Default)]
pub struct QuickArgs {
    /// Runtime of the team: claude or codex
    #[arg(long, value_name = "RUNTIME")]
    pub runtime: Option<String>,
    /// Model of the orchestrator and of members that do not pick their own
    #[arg(long, value_name = "MODEL")]
    pub model: Option<String>,
    /// Start the team but do not open the dashboard
    #[arg(long)]
    pub no_attach: bool,
    /// Process backend: auto, pty or herdr
    #[arg(long, value_name = "BACKEND")]
    pub backend: Option<String>,
    /// Ignore the saved roster and let the orchestrator design the team again
    #[arg(long)]
    pub replan: bool,
    /// Work style to launch (skips the chooser); see `crew style list`
    #[arg(long, value_name = "STYLE")]
    pub style: Option<String>,
}

impl QuickArgs {
    fn any(&self) -> bool {
        self.style.is_some()
            || self.runtime.is_some()
            || self.model.is_some()
            || self.no_attach
            || self.backend.is_some()
            || self.replan
    }
}

#[derive(Subcommand)]
pub enum Command {
    /// Launch a team from a YAML or TOML file and open the dashboard
    Run {
        /// Team definition (see examples/demo.yaml)
        team: PathBuf,
        /// Start the team but do not open the dashboard
        #[arg(long)]
        no_attach: bool,
        /// Process backend: auto, pty or herdr (overrides the team file)
        #[arg(long, value_name = "BACKEND")]
        backend: Option<String>,
        /// Ignore the saved roster and let the orchestrator design the team again
        #[arg(long)]
        replan: bool,
    },
    /// Start the daemon for a workspace without launching a team
    Start,
    /// Open the dashboard for a running session
    Attach,
    /// Show agents and tasks
    Status {
        #[arg(long)]
        json: bool,
        /// Print one dashboard frame as plain text, e.g. --frame 120x40
        #[arg(long, value_name = "WxH", num_args = 0..=1, default_missing_value = "120x40")]
        frame: Option<String>,
    },
    /// Stop the agents and the daemon
    Stop {
        /// Leave agent processes running (herdr backend only keeps them alive)
        #[arg(long)]
        keep_agents: bool,
    },
    /// Continue agents that stopped at a usage limit
    #[command(long_about = "Continue agents that stopped at a usage limit.\n\n\
An agent whose account runs out is paused with its task kept, and crew resumes it by itself \
when the limit resets. Use this to continue sooner: plain `crew resume` once the limit has \
lifted, or `crew resume --restart` after you switched to another account (claude /login, \
codex login) or changed the key in the crew config, so the sessions start afresh on the new \
credentials with their conversations resumed.")]
    Resume {
        /// Agents to continue (default: every agent paused at a limit)
        agents: Vec<String>,
        /// Restart the sessions, resuming their conversations, so a new login or key takes effect
        #[arg(long)]
        restart: bool,
    },
    /// Show the structured event log
    Logs {
        /// Only events of this agent
        agent: Option<String>,
        #[arg(short = 'n', long, default_value_t = 100)]
        lines: usize,
        /// Keep printing new events
        #[arg(short, long)]
        follow: bool,
        #[arg(long)]
        json: bool,
    },
    /// List running crew sessions
    Sessions,
    /// Manage agents
    #[command(subcommand)]
    Agent(AgentCommand),
    /// Manage the task graph
    #[command(subcommand)]
    Task(TaskCommand),
    /// Send a message to an agent as the operator (`*` broadcasts)
    Send {
        to: String,
        body: String,
        #[arg(long = "type", default_value = "message")]
        kind: String,
    },
    /// Show the questions the team has for you
    Questions {
        #[arg(long)]
        json: bool,
    },
    /// Answer a question the team has for you
    #[command(long_about = "Answer a question the team has for you.\n\n\
With one question open, `crew answer \"text\"` answers it; with several, name it by the id \
`crew questions` shows: `crew answer m-1f2e \"text\"`. A number picks one of the suggested \
answers: `crew answer 2`.")]
    Answer {
        /// The answer (or the number of a suggested one), after the
        /// question's id when more than one is open
        #[arg(value_name = "[ID] ANSWER", num_args = 1..=2, required = true)]
        words: Vec<String>,
    },
    /// Show recent bus messages
    Messages {
        /// Only messages involving this agent
        agent: Option<String>,
        #[arg(short = 'n', long, default_value_t = 50)]
        lines: usize,
        #[arg(long)]
        json: bool,
    },
    /// Merge every agent branch into the integration branch
    Integrate,
    /// Serve the dashboard in the browser and open it (a one-time pairing
    /// address; Chromium-based browsers and Firefox)
    Web {
        /// Serve on exactly this port (1-65535)
        #[arg(long, value_parser = clap::value_parser!(u16).range(1..))]
        port: Option<u16>,
        /// Print the address without opening the browser
        #[arg(long)]
        no_open: bool,
        /// Print the address as JSON (and open nothing)
        #[arg(long)]
        json: bool,
        /// Stop serving: every browser session ends
        #[arg(long, conflicts_with_all = ["port", "no_open", "json"])]
        stop: bool,
    },
    /// Agent-side commands (identity comes from CREW_IDENTITY)
    #[command(subcommand)]
    Ctl(CtlCommand),
    /// Run the MCP server that gives an agent its crew tools (stdio)
    Mcp(IdentityArg),
    /// Report a runtime lifecycle event to the daemon (used by agent hooks)
    #[command(hide = true)]
    Hook {
        event: String,
        #[command(flatten)]
        identity: IdentityArg,
        /// Event payload as JSON (default: read from stdin)
        payload: Option<String>,
    },
    /// Hold an agent's delivery channel open and print deliveries as JSON lines
    #[command(hide = true)]
    Bridge(IdentityArg),
    /// Confirm that messages were injected into the session
    #[command(hide = true)]
    Delivered {
        #[command(flatten)]
        identity: IdentityArg,
        ids: Vec<String>,
    },
    /// Print the API key the crew config assigns to an agent (Claude Code's apiKeyHelper)
    #[command(hide = true)]
    Credential(IdentityArg),
    /// Show the endpoints and keys team members are launched with
    Config {
        #[command(subcommand)]
        command: Option<ConfigCommand>,
    },
    /// The model catalog: what each model is good for, across Claude Code and Codex
    Models {
        #[command(subcommand)]
        command: Option<ModelsCommand>,
    },
    /// Work styles: presets for how a team is staffed and how it works
    Style {
        #[command(subcommand)]
        command: Option<StyleCommand>,
    },
    /// A scripted stand-in for a real agent (for dry runs and tests)
    ScriptedAgent(scripted::ScriptArgs),
    /// A stand-in for an agent CLI (claude, codex, pi), for tests
    #[command(hide = true)]
    FakeCli(fake_cli::FakeArgs),
    /// Start a program in an agent sandbox (the daemon puts this in front of it)
    #[command(hide = true)]
    SandboxRun(crate::sandbox::run::RunArgs),
    /// Inside an agent sandbox: confine this process, then run the program
    #[command(hide = true)]
    SandboxExec(crate::sandbox::shim::ExecArgs),
    /// Run the daemon in the foreground
    #[command(hide = true)]
    Daemon {
        #[arg(long)]
        workspace: PathBuf,
    },
    /// Run the dashboard server in the foreground (`crew web` starts it)
    #[command(hide = true)]
    WebServer {
        /// Serve on exactly this port (1-65535)
        #[arg(long, value_parser = clap::value_parser!(u16).range(1..))]
        port: Option<u16>,
    },
}

#[derive(Args, Clone, Default)]
pub struct IdentityArg {
    /// Agent identity file (default: $CREW_IDENTITY)
    #[arg(long, value_name = "FILE")]
    pub identity: Option<PathBuf>,
}

#[derive(Subcommand)]
pub enum ModelsCommand {
    /// List the models and which to pick for what (the default)
    List,
    /// Show one model's profile in full, with its sources
    Show { name: String },
    /// Write a template for your own additions and corrections
    Init {
        /// user or global
        #[arg(long, default_value = "user")]
        scope: String,
    },
}

#[derive(Subcommand)]
pub enum StyleCommand {
    /// List the styles available here (the default)
    List,
    /// Show one style in full
    Show { name: String },
    /// Create a style file to edit
    New {
        name: String,
        /// Where to put it: global, user or project
        #[arg(long, default_value = "user")]
        scope: String,
        /// Start from a copy of this style instead of the template
        #[arg(long, value_name = "STYLE")]
        from: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum ConfigCommand {
    /// Show the config files and what is in effect (the default)
    Show,
    /// Write a commented template for a scope
    Init {
        /// global, user or project
        #[arg(long, default_value = "user")]
        scope: String,
    },
}

#[derive(Subcommand)]
pub enum AgentCommand {
    /// List agents
    List {
        #[arg(long)]
        json: bool,
    },
    /// Add an agent to the running team
    Add {
        #[arg(long = "name", alias = "id")]
        id: String,
        #[arg(long, default_value = "")]
        role: String,
        /// claude, codex, pi or shell
        #[arg(long)]
        runtime: Option<String>,
        #[arg(long)]
        model: Option<String>,
        /// Give the agent a task right away
        #[arg(long)]
        task: Option<String>,
    },
    /// Stop an agent's process
    Kill { id: String },
    /// Take an agent off the team (its open tasks return to the pool)
    Remove { id: String },
    /// Restart an agent, resuming its session (with --fresh: in a new session)
    Restart {
        id: String,
        /// Start a new session instead of resuming the old one (for a session
        /// the provider keeps refusing)
        #[arg(long)]
        fresh: bool,
    },
    /// Put an agent on another model or CLI: on its own CLI the session is
    /// restarted on the model; on another, a fresh session there is handed
    /// the work
    Model {
        id: String,
        /// e.g. opus, sonnet, gpt-6.1-sol, or a pi model (anthropic/claude-opus-4-6);
        /// omitted with --runtime, that CLI's default
        model: Option<String>,
        /// claude, codex or pi: the CLI to run it on (a catalog model says
        /// its own)
        #[arg(long)]
        runtime: Option<String>,
        /// What the agent should know on its new model, read first
        #[arg(long)]
        note: Option<String>,
    },
    /// Queue a compaction of an agent's context (runs when its turn ends)
    Compact {
        id: String,
        /// What the summary must keep
        #[arg(long)]
        keep: Option<String>,
    },
    /// Suspend an agent's process
    Pause { id: String },
    /// Continue a paused agent
    Resume { id: String },
    /// Print an agent's terminal contents
    Output {
        id: String,
        #[arg(short = 'n', long, default_value_t = 60)]
        lines: usize,
        /// Only what the terminal shows now (herdr is slow to read scrollback)
        #[arg(long)]
        screen: bool,
        /// With the terminal's colours (implies --screen)
        #[arg(long)]
        ansi: bool,
    },
    /// Print what an agent was told, wrote and did, from its session file
    Transcript {
        id: String,
        /// How many of the newest entries to print
        #[arg(short = 'n', long, default_value_t = 60)]
        entries: usize,
    },
    /// Attach your terminal to an agent's session (detach with Ctrl-])
    Attach { id: String },
    /// Type a prompt into an agent's session
    Prompt { id: String, text: String },
}

#[derive(Subcommand)]
pub enum TaskCommand {
    /// Show the task graph
    List {
        #[arg(long)]
        json: bool,
    },
    /// Add a task
    Add {
        #[arg(long)]
        id: String,
        #[arg(long)]
        title: Option<String>,
        #[arg(long, default_value = "")]
        description: String,
        #[arg(long)]
        agent: Option<String>,
        #[arg(long = "depends-on", value_delimiter = ',')]
        depends_on: Vec<String>,
        #[arg(long)]
        reviewer: Option<String>,
    },
    /// Show a task's card: its description, what it builds on, where it stands
    Show {
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// Revise a task's card; whoever is at work on it is told
    Edit {
        id: String,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        description: Option<String>,
    },
    /// Assign a task to an agent
    Assign { id: String, agent: String },
    /// Queue a failed or blocked task again
    Retry { id: String },
    /// Mark a task done by hand
    Done {
        id: String,
        #[arg(default_value = "completed by the operator")]
        result: String,
    },
}

#[derive(Subcommand)]
pub enum CtlCommand {
    /// List the team
    List,
    /// Send a message to a teammate
    Send {
        to: String,
        body: String,
        #[arg(long = "type", default_value = "message")]
        kind: String,
    },
    /// Send a message to everyone
    Broadcast { body: String },
    /// Read unread messages (and mark them read)
    Inbox {
        /// Include messages already read
        #[arg(long)]
        all: bool,
    },
    /// Set your status: working, waiting, blocked or idle
    Status {
        status: String,
        note: Option<String>,
    },
    /// Report progress on your task
    Progress {
        percent: u8,
        message: Option<String>,
    },
    /// Show the board
    Tasks,
    /// Show a task's card (default: your task in progress)
    Task { id: Option<String> },
    /// Complete your current task
    Done {
        #[arg(default_value = "done")]
        result: String,
    },
    /// Give up on your current task
    Fail { reason: String },
    /// Commit everything in your worktree
    Commit { message: String },
    /// Show your changes
    Diff { against: Option<String> },
    /// Show branch and working-tree status
    GitStatus,
    /// Apply a teammate's commit
    CherryPick { commit: String },
    /// Merge a teammate's branch
    Merge { agent: String },
    /// Launch a subagent with a task
    Spawn {
        task: String,
        #[arg(long)]
        role: Option<String>,
        #[arg(long)]
        id: Option<String>,
    },
    /// Ask a teammate for a review
    ReviewRequest { agent: String, artifact: String },
    /// Submit a review verdict
    Review {
        task: String,
        #[arg(long, conflicts_with = "reject")]
        approve: bool,
        #[arg(long)]
        reject: bool,
        #[arg(default_value = "")]
        comments: String,
    },
    /// Call any tool by name with JSON arguments
    Tool {
        name: String,
        #[arg(default_value = "{}")]
        args: String,
    },
    /// List the tools this agent has, as JSON (name, description, inputSchema)
    Tools,
}

/// What `main` should do after parsing.
pub fn run(cli: Cli) -> Result<()> {
    let target = ops::Target {
        dir: cli.dir.clone(),
        project: cli.project.clone(),
    };
    let Some(command) = cli.command else {
        if let Some(objective) = &cli.objective {
            ops::refuse_command(objective)?;
        }
        if let Some(word) = cli
            .objective
            .as_deref()
            .filter(|o| o.split_whitespace().count() < 2)
        {
            // A lone word is far more likely a mistyped command than an
            // objective, and starting a team is not a cheap mistake.
            bail!("{}", unknown_command(word));
        }
        return ops::quick(&target, cli.objective, cli.quick);
    };
    if cli.objective.is_some() || cli.quick.any() {
        bail!(
            "an objective and the options of plain `crew` (--runtime, --model, --no-attach, \
--backend, --replan, --style) do not go with a subcommand; see `crew --help`"
        );
    }
    match command {
        Command::Run {
            team,
            no_attach,
            backend,
            replan,
        } => ops::run_team(&target, &team, no_attach, backend.as_deref(), replan),
        Command::Start => ops::start(&target),
        Command::Attach => ops::attach(&target),
        Command::Status { json, frame } => match frame {
            Some(size) => ops::frame(&target, &size),
            None => ops::status(&target, json),
        },
        Command::Stop { keep_agents } => ops::stop(&target, keep_agents),
        Command::Resume { agents, restart } => ops::resume(&target, agents, restart),
        Command::Logs {
            agent,
            lines,
            follow,
            json,
        } => ops::logs(&target, agent, lines, follow, json),
        Command::Sessions => ops::sessions(),
        Command::Agent(cmd) => ops::agent(&target, cmd),
        Command::Task(cmd) => ops::task(&target, cmd),
        Command::Send { to, body, kind } => ops::send(&target, &to, &body, &kind),
        Command::Questions { json } => ops::questions(&target, json),
        Command::Answer { words } => match words.as_slice() {
            [answer] => ops::answer(&target, None, answer),
            [id, answer] => ops::answer(&target, Some(id), answer),
            _ => bail!("usage: crew answer [ID] ANSWER"),
        },
        Command::Messages { agent, lines, json } => ops::messages(&target, agent, lines, json),
        Command::Integrate => ops::integrate(&target),
        Command::Web {
            port,
            no_open,
            json,
            stop,
        } => {
            if stop {
                ops::web_stop(&target)
            } else {
                ops::web(&target, port, no_open, json)
            }
        }
        Command::Ctl(cmd) => agent_side::ctl(cmd),
        Command::Mcp(identity) => crate::mcp::serve(identity.identity.as_deref()),
        Command::Hook {
            event,
            identity,
            payload,
        } => {
            // A hook must never disturb the session it runs in.
            agent_side::hook(&event, identity.identity.as_deref(), payload);
            Ok(())
        }
        Command::Bridge(identity) => agent_side::bridge(identity.identity.as_deref()),
        Command::Delivered { identity, ids } => {
            agent_side::delivered(identity.identity.as_deref(), ids)
        }
        Command::Credential(identity) => agent_side::credential(identity.identity.as_deref()),
        Command::Config { command } => ops::config(&target, command.unwrap_or(ConfigCommand::Show)),
        Command::Style { command } => ops::style(&target, command.unwrap_or(StyleCommand::List)),
        Command::Models { command } => ops::models(command.unwrap_or(ModelsCommand::List)),
        Command::ScriptedAgent(args) => scripted::run(args),
        Command::FakeCli(args) => fake_cli::run(args),
        Command::SandboxRun(args) => crate::sandbox::run::run(args),
        Command::SandboxExec(args) => crate::sandbox::shim::run(args),
        Command::Daemon { workspace } => daemonize::run_foreground(&workspace),
        Command::WebServer { port } => crate::web::process::run(port),
    }
}

/// The error for `crew <word>` where the word is no command.
fn unknown_command(word: &str) -> String {
    let command = Cli::command();
    let closest = command
        .get_subcommands()
        .filter(|c| !c.is_hide_set())
        .map(|c| (edit_distance(word, c.get_name()), c.get_name()))
        .min()
        .filter(|(distance, _)| *distance <= 2);
    let mut text = format!("unknown command '{word}'");
    if let Some((_, name)) = closest {
        text.push_str(&format!(" (did you mean `crew {name}`?)"));
    }
    text.push_str(
        "\nTo start a team, give the objective as one quoted sentence: \
crew \"build a todo CLI with tests\"",
    );
    text
}

fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let substitution = diagonal + usize::from(ca != *cb);
            diagonal = row[j + 1];
            row[j + 1] = substitution.min(row[j] + 1).min(diagonal + 1);
        }
    }
    row[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restart_fresh_asks_for_a_new_session_and_plain_restart_still_resumes() {
        use crew_core::rpc::Call;
        let parse = |args: &[&str]| {
            let mut argv = vec!["crew", "agent", "restart"];
            argv.extend_from_slice(args);
            match Cli::parse_from(argv).command {
                Some(Command::Agent(AgentCommand::Restart { id, fresh })) => (id, fresh),
                _ => panic!("not an agent restart"),
            }
        };
        let (id, fresh) = parse(&["worker-1", "--fresh"]);
        assert!(matches!(
            ops::restart_call(&id, fresh),
            Call::AgentRestartFresh { id } if id == "worker-1"
        ));
        let (id, fresh) = parse(&["worker-1"]);
        assert!(matches!(
            ops::restart_call(&id, fresh),
            Call::AgentRestart { id } if id == "worker-1"
        ));
    }

    #[test]
    fn a_mistyped_command_is_not_taken_for_an_objective() {
        assert_eq!(edit_distance("atach", "attach"), 1);
        assert_eq!(edit_distance("status", "status"), 0);
        assert_eq!(edit_distance("", "run"), 3);
        let text = unknown_command("atach");
        assert!(text.contains("did you mean `crew attach`"), "{text}");
        assert!(!unknown_command("zzzzzzzz").contains("did you mean"));
    }

    #[test]
    fn a_slash_command_is_not_taken_for_an_objective() {
        for objective in ["/exit", "/stop now", "/compact the context please"] {
            let error = run(Cli::parse_from(["crew", objective]))
                .unwrap_err()
                .to_string();
            assert!(
                error.contains("looks like a command, not an objective"),
                "{objective}: {error}"
            );
            assert!(error.contains("crew stop"), "{error}");
        }
        assert!(ops::refuse_command("/src/main.rs is slow, find out why").is_ok());
        assert!(ops::refuse_command(" /exit handling needs a test").is_ok());
        assert!(ops::refuse_command("build a todo CLI").is_ok());
    }

    #[test]
    fn the_command_line_is_well_formed() {
        Cli::command().debug_assert();
        let cli = Cli::parse_from(["crew", "build a thing", "--model", "haiku"]);
        assert!(cli.command.is_none());
        assert_eq!(cli.objective.as_deref(), Some("build a thing"));
        assert_eq!(cli.quick.model.as_deref(), Some("haiku"));
        let cli = Cli::parse_from(["crew", "status", "-C", "/tmp"]);
        assert!(matches!(cli.command, Some(Command::Status { .. })));
        assert!(cli.objective.is_none());
        let cli = Cli::parse_from(["crew", "run", "team.yaml", "--no-attach"]);
        assert!(matches!(
            cli.command,
            Some(Command::Run {
                no_attach: true,
                ..
            })
        ));
        assert!(!cli.quick.no_attach);
        assert!(Cli::parse_from(["crew"]).command.is_none());
    }
}
