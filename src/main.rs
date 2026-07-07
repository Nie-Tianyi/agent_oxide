//! # Agent Oxide — Interactive CLI & TUI
//!
//! By default, launches a ratatui-based chat interface similar to
//! Claude Code's terminal UX. Pass `--no-tui` for the legacy
//! line-based REPL.
//!
//! ## Usage
//!
//! ```text
//! $ cargo run              # TUI mode (default)
//! $ cargo run -- --no-tui  # Legacy line-based CLI
//! ```
//!
//! Set `DEEPSEEK_API` in `.env` before running.

use std::io::{self, BufRead, Write};
use std::sync::Arc;

use agent_oxide::core::agent::{Agent, AgentEvent};
use agent_oxide::core::client::{DeepSeekClient, Message, Role};
use agent_oxide::memory::Memory;
use agent_oxide::tools::{
    CalculatorTool, GlobTool, GrepTool, LsTool, ReadTool, ToolRegistry, WorkspaceFs, WriteTool,
};

// ── Constants ──────────────────────────────────────────────────────────────────

const DEFAULT_MODEL: &str = "deepseek-chat";
const MAX_STEPS: usize = 15;

const SYSTEM_PROMPT: &str = "\
You are a helpful, accurate coding assistant. You have tools for file operations \
(read, write, edit, glob, grep, ls) and calculations.

## Core rules — follow strictly

1. **Ground everything in tools.** Before making ANY claim about file paths, \
code contents, directory structure, or the codebase: verify with the \
appropriate tool (glob to find files, grep to search content, read to read, \
ls to list). Never guess. If a tool returns nothing or errors, report that \
honestly — do not fabricate a result.

2. **Express uncertainty.** If you don't know something or can't verify it, \
say so. It is better to admit uncertainty than to give a confident wrong \
answer. If the user \
asks something ambiguous, ask for clarification.

3. **Quote, don't summarise from memory.** When referencing code, always read \
the file first and quote the actual content. Never invent function signatures, \
variable names, or line numbers.

4. **Verify before editing.** Before writing or editing a file, read it first. \
Before running a glob, check the directory exists. Before claiming a fix works, \
explain what you verified.

5. **No phantom files or features.** If the user mentions a file that doesn't \
exist, say so. If they ask you to implement something, only write code that \
actually compiles and uses real APIs.

6. **Use the right tool for the job.** grep to search content, glob to find \
files by name, ls to list directories, read to view contents, write to create, \
edit to modify. Don't try to use read where grep is appropriate.

7. **Be concise and accurate.** Short, factual responses are better than long, \
speculative ones. Respond in the same language the user uses.\
";

// ── main ───────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    // ── Mode selection ──────────────────────────────────────────────
    let use_tui = !std::env::args().any(|a| a == "--no-tui");

    // ── Load environment ────────────────────────────────────────────
    dotenvy::dotenv().ok();
    let api_key = std::env::var("DEEPSEEK_API").unwrap_or_else(|_| {
        eprintln!("ERROR: DEEPSEEK_API not set.");
        eprintln!("Create a .env file with: DEEPSEEK_API=sk-...");
        std::process::exit(1);
    });

    // ── Workspace filesystem ────────────────────────────────────────
    let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
    let workspace = match WorkspaceFs::new(&cwd) {
        Ok(ws) => Arc::new(ws),
        Err(e) => {
            eprintln!("ERROR: Cannot create workspace at {}: {e}", cwd.display());
            std::process::exit(1);
        }
    };

    // ── Tool registry ───────────────────────────────────────────────
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(CalculatorTool));
    registry.register(Arc::new(ReadTool::new(workspace.clone())));
    registry.register(Arc::new(WriteTool::new(workspace.clone())));
    registry.register(Arc::new(GlobTool::new(workspace.clone())));
    registry.register(Arc::new(GrepTool::new(workspace.clone())));
    registry.register(Arc::new(LsTool::new(workspace.clone())));

    // ── Collect tool names for the TUI /stats command ───────────────
    let tool_names: Vec<String> = registry.iter().map(|(name, _)| name.to_string()).collect();

    let registry = Arc::new(registry);

    // ── Agent ───────────────────────────────────────────────────────
    let model = std::env::var("DEFAULT_PRO_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string());
    let client = DeepSeekClient::new(&api_key);
    let memory = Arc::new(std::sync::RwLock::new(Memory::new()));
    let agent = Agent::new(client, memory.clone(), registry)
        .with_model(&model)
        .with_max_steps(MAX_STEPS);

    // ── Seed system prompt ──────────────────────────────────────────
    {
        let mut mem = memory.write().unwrap();
        mem.push(Message::new(Role::System, SYSTEM_PROMPT));
    }

    // ── Dispatch ────────────────────────────────────────────────────
    if use_tui {
        match agent_oxide::tui::run(agent, memory, tool_names, &model) {
            Ok(()) => {}
            Err(e) => eprintln!("TUI error: {e}"),
        }
    } else {
        run_cli(agent, memory, &model, &cwd).await;
    }
}

// ── Legacy CLI ─────────────────────────────────────────────────────────────────

/// Legacy line-based REPL. Pass `--no-tui` to reach this path.
async fn run_cli(
    agent: Agent,
    memory: Arc<std::sync::RwLock<Memory>>,
    model: &str,
    cwd: &std::path::Path,
) {
    print_welcome(model, cwd, &agent);

    let stdin = io::stdin();
    let mut stdout = io::stdout();

    loop {
        print!("\n> ");
        stdout.flush().unwrap();

        let mut input = String::new();
        match stdin.lock().read_line(&mut input) {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) => {
                eprintln!("Read error: {e}");
                break;
            }
        }

        let input = input.trim().to_owned();
        if input.is_empty() {
            continue;
        }

        if handle_cli_command(&input, &memory).await {
            continue;
        }

        {
            let mut mem = memory.write().unwrap();
            mem.push(Message::new(Role::User, &input));
        }

        print!("🤖 ");
        stdout.flush().unwrap();

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        let printer = tokio::spawn(async move {
            render_events(&mut rx).await;
        });

        match agent.run_with_events(tx).await {
            Ok(_response) => {}
            Err(e) => {
                println!("\n  ✗ {e}");
            }
        }

        printer.await.unwrap();
    }

    println!("\nGoodbye!");
}

// ── Legacy Helpers ─────────────────────────────────────────────────────────────

/// Prints the welcome banner for CLI mode.
fn print_welcome(model: &str, cwd: &std::path::Path, agent: &Agent) {
    let streaming_label = if agent.streaming() {
        "streaming"
    } else {
        "batch"
    };
    println!();
    println!("╔══════════════════════════════════════════════╗");
    println!("║       Agent Oxide — Interactive CLI          ║");
    println!("╠══════════════════════════════════════════════╣");
    println!("║  Model  : {model:<33}║");
    println!("║  Mode   : {streaming_label:<33}║");
    println!("║  Root   : {:<33}║", truncate_path(cwd, 33));
    println!("╠══════════════════════════════════════════════╣");
    println!("║  /exit   — quit                             ║");
    println!("║  /clear  — reset conversation               ║");
    println!("║  /stats  — memory statistics                ║");
    println!("║  /tools  — list registered tools            ║");
    println!("╚══════════════════════════════════════════════╝");
}

/// Handles slash commands in CLI mode. Returns `true` if the input was a command.
async fn handle_cli_command(
    input: &str,
    memory: &std::sync::Arc<std::sync::RwLock<Memory>>,
) -> bool {
    match input {
        "/exit" => {
            println!("Goodbye!");
            std::process::exit(0);
        }
        "/clear" => {
            let mut mem = memory.write().unwrap();
            let system_msgs: Vec<Message> = mem
                .to_context_vec()
                .into_iter()
                .filter(|m| m.role == Role::System)
                .collect();
            let count = mem.message_count();
            *mem = Memory::new();
            for msg in system_msgs {
                mem.push(msg);
            }
            println!("  ✓ Cleared {count} messages (system prompt preserved)");
            return true;
        }
        "/stats" => {
            let mem = memory.read().unwrap();
            println!("  Messages : {}", mem.message_count());
            println!("  Chars    : {}", mem.total_chars());
            println!("  Threshold: {} chars", mem.compact_threshold());
            println!("  Keep last: {}", mem.keep_last_n());
            return true;
        }
        "/tools" => {
            println!("  calculator  — evaluate math expressions");
            println!("  read        — read file contents");
            println!("  write       — create or overwrite a file");
            println!("  glob        — find files by pattern");
            println!("  grep        — search file contents");
            println!("  ls          — list directory contents");
            return true;
        }
        _ => {}
    }
    false
}

/// Consumes [`AgentEvent`]s from the channel and renders them to stdout.
async fn render_events(rx: &mut tokio::sync::mpsc::UnboundedReceiver<AgentEvent>) {
    let mut stdout = io::stdout();
    let mut tool_mode = false;

    while let Some(event) = rx.recv().await {
        match event {
            AgentEvent::Token(text) => {
                if tool_mode {
                    println!();
                    tool_mode = false;
                }
                print!("{text}");
            }

            AgentEvent::ReasoningToken(_text) => {
                // Chain-of-thought is verbose — suppressed by default.
                // Uncomment to see the model's internal reasoning:
                // print!("\x1b[2m{_text}\x1b[0m");
            }

            AgentEvent::ToolCallStart { name, .. } => {
                print!("\n  🔧 {name} ");
                tool_mode = true;
            }

            AgentEvent::ToolCallArgsDelta { .. } => {}

            AgentEvent::ToolResult { name, output, .. } => {
                let preview = truncate_for_display(&output, 150);
                println!("\n  ✓ {name} → {preview}");
            }

            AgentEvent::Done => {
                println!();
            }
        }

        stdout.flush().unwrap();
    }
}

fn truncate_for_display(text: &str, max_len: usize) -> String {
    let text = text.replace('\n', " ");
    if text.len() <= max_len {
        return text;
    }
    // Find a valid UTF-8 char boundary at or before max_len
    let boundary = text.floor_char_boundary(max_len);
    format!("{}...", &text[..boundary])
}

fn truncate_path(path: &std::path::Path, max_len: usize) -> String {
    let s = path.display().to_string();
    if s.len() <= max_len {
        return s;
    }
    format!("...{}", &s[s.len().saturating_sub(max_len - 3)..])
}
