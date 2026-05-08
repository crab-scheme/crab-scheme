//! `crabscheme` binary — minimal CLI entry.

use std::fs;
use std::io::{self, BufRead, Write};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use cs_core::{Value, WriteMode};
use cs_diag::render;
use cs_runtime::Runtime;

#[derive(Parser, Debug)]
#[command(
    name = "crabscheme",
    version,
    about = "CrabScheme — R6RS Scheme implementation in Rust"
)]
struct Cli {
    /// Evaluate an expression and print its value.
    #[arg(short = 'e', long = "eval", value_name = "EXPR")]
    expr: Option<String>,

    /// Execution tier: tree-walker (default) or vm (bytecode).
    #[arg(long = "tier", value_name = "TIER", default_value = "walker")]
    tier: String,

    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Run a Scheme source file.
    Run {
        /// Path to the .scm file.
        file: String,
    },
    /// Start an interactive REPL.
    Repl,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let via_vm = cli.tier == "vm";

    if let Some(expr) = cli.expr {
        return run_eval(&expr, via_vm);
    }

    match cli.cmd {
        Some(Cmd::Run { file }) => run_file(&file, via_vm),
        Some(Cmd::Repl) | None => run_repl(),
    }
}

fn eval_with_tier(
    rt: &mut Runtime,
    name: &str,
    src: &str,
    via_vm: bool,
) -> Result<Value, cs_diag::Diagnostic> {
    if via_vm {
        rt.eval_str_via_vm(name, src)
    } else {
        rt.eval_str(name, src)
    }
}

fn run_eval(src: &str, via_vm: bool) -> ExitCode {
    let mut rt = Runtime::new();
    match eval_with_tier(&mut rt, "<command-line>", src, via_vm) {
        Ok(v) => {
            if !matches!(v, Value::Unspecified) {
                println!("{}", rt.format_value(&v, WriteMode::Write));
            }
            ExitCode::SUCCESS
        }
        Err(diag) => {
            let s = render(&diag, rt.source_map());
            eprintln!("{}", s);
            ExitCode::from(2)
        }
    }
}

fn run_file(path: &str, via_vm: bool) -> ExitCode {
    let src = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("crabscheme: cannot read {}: {}", path, e);
            return ExitCode::from(1);
        }
    };
    let mut rt = Runtime::new();
    match eval_with_tier(&mut rt, path, &src, via_vm) {
        Ok(v) => {
            if !matches!(v, Value::Unspecified) {
                println!("{}", rt.format_value(&v, WriteMode::Write));
            }
            ExitCode::SUCCESS
        }
        Err(diag) => {
            let s = render(&diag, rt.source_map());
            eprintln!("{}", s);
            ExitCode::from(2)
        }
    }
}

fn run_repl() -> ExitCode {
    let mut rt = Runtime::new();
    let mut counter: u32 = 0;
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut buffer = String::new();
    println!(
        "crabscheme {} — type Scheme expressions, ^D to exit",
        env!("CARGO_PKG_VERSION")
    );
    loop {
        if buffer.is_empty() {
            print!("> ");
        } else {
            print!("… ");
        }
        let _ = stdout.flush();
        let mut line = String::new();
        match stdin.lock().read_line(&mut line) {
            Ok(0) => {
                println!();
                return ExitCode::SUCCESS;
            }
            Ok(_) => {}
            Err(_) => return ExitCode::from(1),
        }
        buffer.push_str(&line);
        if !is_balanced(&buffer) {
            continue;
        }
        counter += 1;
        let name = format!("<repl:{}>", counter);
        let to_eval = std::mem::take(&mut buffer);
        match rt.eval_str(&name, &to_eval) {
            Ok(v) => {
                if !matches!(v, Value::Unspecified) {
                    println!("{}", v);
                }
            }
            Err(diag) => {
                let s = render(&diag, rt.source_map());
                eprint!("{}", s);
            }
        }
    }
}

fn is_balanced(src: &str) -> bool {
    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut chars = src.chars().peekable();
    while let Some(c) = chars.next() {
        if in_string {
            match c {
                '\\' => {
                    chars.next();
                }
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            ';' => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
            }
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            _ => {}
        }
    }
    depth <= 0 && !in_string
}
