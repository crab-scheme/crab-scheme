//! `crabscheme-lsp` — the CrabScheme Language Server *and* a headless
//! code-intelligence CLI.
//!
//! With no subcommand (how editors spawn it) it runs the LSP over stdio:
//! `tower-lsp` handles framing + async dispatch, all logic lives in
//! [`cs_lsp::Backend`].
//!
//! With a subcommand it runs one-shot, emitting JSON for a coding harness
//! (an agent shells out, parses stdout). Every subcommand reuses
//! [`cs_lsp::harness`] — the *same* analysis the LSP serves editors — so
//! results never drift. Positions are 1-based (see `harness`).
//!
//! ```text
//! crabscheme-lsp                      # LSP server (stdio)
//! crabscheme-lsp check  foo.scm       # diagnostics JSON; exit 1 if any
//! crabscheme-lsp symbols foo.scm      # outline JSON
//! crabscheme-lsp def   foo.scm --line 2 --col 2
//! crabscheme-lsp refs  foo.scm --line 2 --col 2
//! crabscheme-lsp hover foo.scm --line 1 --col 2
//! crabscheme-lsp fmt   foo.scm [--write]
//! crabscheme-lsp workspace-symbols ./src --query alpha
//! ```

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use cs_lsp::harness::{self, Pos};
use cs_lsp::Backend;
use tower_lsp::{LspService, Server};

#[derive(Parser)]
#[command(
    name = "crabscheme-lsp",
    version,
    about = "CrabScheme Language Server + headless code-intelligence CLI"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the LSP server over stdio (the default with no subcommand).
    Lsp,
    /// Print parse/expand diagnostics as JSON. Exit 1 if any are found.
    Check { file: PathBuf },
    /// Print the document outline (every define) as JSON.
    Symbols { file: PathBuf },
    /// Print the definition site of the identifier at --line/--col.
    Def {
        file: PathBuf,
        #[arg(long)]
        line: u32,
        #[arg(long)]
        col: u32,
    },
    /// Print every reference (definition included) to the identifier.
    Refs {
        file: PathBuf,
        #[arg(long)]
        line: u32,
        #[arg(long)]
        col: u32,
    },
    /// Print hover documentation for the identifier at --line/--col.
    Hover {
        file: PathBuf,
        #[arg(long)]
        line: u32,
        #[arg(long)]
        col: u32,
    },
    /// Reformat the file. Prints to stdout, or rewrites it with --write.
    Fmt {
        file: PathBuf,
        #[arg(long)]
        write: bool,
    },
    /// Find defines across a workspace whose name matches --query.
    WorkspaceSymbols {
        root: PathBuf,
        #[arg(long, default_value = "")]
        query: String,
    },
}

#[tokio::main]
async fn main() {
    match Cli::parse().command {
        None | Some(Command::Lsp) => serve().await,
        Some(cmd) => std::process::exit(run_cli(cmd)),
    }
}

/// The stdio LSP transport.
async fn serve() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let (service, socket) = LspService::new(Backend::new);
    Server::new(stdin, stdout, socket).serve(service).await;
}

/// Run `f` (front-end analysis) under `catch_unwind` so a parser panic
/// exits cleanly (code 3) instead of dumping a backtrace at an agent.
fn guard(mut f: impl FnMut() -> i32) -> i32 {
    catch_unwind(AssertUnwindSafe(&mut f)).unwrap_or_else(|_| {
        eprintln!("crabscheme-lsp: internal error analyzing input");
        3
    })
}

/// Run one CLI subcommand; return the process exit code.
fn run_cli(cmd: Command) -> i32 {
    match cmd {
        Command::Lsp => unreachable!("handled in main"),
        Command::Check { file } => {
            let Some((name, text)) = read(&file) else {
                return 2;
            };
            guard(|| {
                let diags = harness::check(&name, &text);
                print_json(&diags);
                i32::from(!diags.is_empty()) // 1 if diagnostics, else 0
            })
        }
        Command::Symbols { file } => {
            let Some((name, text)) = read(&file) else {
                return 2;
            };
            guard(|| {
                print_json(&harness::symbols(&name, &text));
                0
            })
        }
        Command::Def { file, line, col } => {
            let Some((name, text)) = read(&file) else {
                return 2;
            };
            guard(|| {
                print_json(&harness::definition(&name, &text, Pos::new(line, col)));
                0
            })
        }
        Command::Refs { file, line, col } => {
            let Some((name, text)) = read(&file) else {
                return 2;
            };
            guard(|| {
                print_json(&harness::references(&name, &text, Pos::new(line, col)));
                0
            })
        }
        Command::Hover { file, line, col } => {
            let Some((name, text)) = read(&file) else {
                return 2;
            };
            guard(|| {
                print_json(&harness::hover(&name, &text, Pos::new(line, col)));
                0
            })
        }
        Command::Fmt { file, write } => {
            let Some((_, text)) = read(&file) else {
                return 2;
            };
            let formatted = match catch_unwind(AssertUnwindSafe(|| harness::format(&text))) {
                Ok(s) => s,
                Err(_) => {
                    eprintln!("crabscheme-lsp: internal error formatting input");
                    return 3;
                }
            };
            if write {
                if let Err(e) = std::fs::write(&file, formatted) {
                    eprintln!("crabscheme-lsp: cannot write {}: {e}", file.display());
                    return 2;
                }
                0
            } else {
                print!("{formatted}");
                0
            }
        }
        Command::WorkspaceSymbols { root, query } => guard(|| {
            print_json(&harness::workspace_symbols(&root, &query));
            0
        }),
    }
}

/// Read a source file, returning `(name, text)` where `name` is the path
/// string used to label the source map. Prints to stderr on failure.
fn read(file: &PathBuf) -> Option<(String, String)> {
    match std::fs::read_to_string(file) {
        Ok(text) => Some((file.display().to_string(), text)),
        Err(e) => {
            eprintln!("crabscheme-lsp: cannot read {}: {e}", file.display());
            None
        }
    }
}

/// Print a value as pretty JSON to stdout.
fn print_json<T: serde::Serialize>(value: &T) {
    match serde_json::to_string_pretty(value) {
        Ok(s) => println!("{s}"),
        Err(e) => eprintln!("crabscheme-lsp: serialization error: {e}"),
    }
}
