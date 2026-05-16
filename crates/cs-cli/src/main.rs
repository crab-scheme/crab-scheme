//! `crabscheme` binary — minimal CLI entry.

use std::fs;
use std::io::{self, BufRead, Write};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use cs_core::{Value, WriteMode};
use cs_diag::{render_with, Diagnostic, SourceMap};
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

    /// When to color diagnostics: auto (TTY-dependent), always, or never.
    #[arg(long = "color", value_name = "WHEN", default_value = "auto")]
    color: String,

    #[command(subcommand)]
    cmd: Option<Cmd>,
}

/// Resolve the `--color` flag: 'auto' inspects whether stderr is a TTY.
fn color_enabled(flag: &str) -> bool {
    match flag {
        "always" => true,
        "never" => false,
        _ => is_stderr_tty(),
    }
}

fn is_stderr_tty() -> bool {
    use std::io::IsTerminal;
    std::io::stderr().is_terminal()
}

fn render_diag(diag: &Diagnostic, sm: &SourceMap, color: bool) -> String {
    render_with(diag, sm, color)
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Run a Scheme source file.
    Run {
        /// Path to the .scm file.
        file: String,
        /// Args passed to the script — surfaced via R6RS `(command-line)`.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Start an interactive REPL.
    Repl,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let via_vm = cli.tier == "vm" || cli.tier == "vm-jit";
    let with_jit = cli.tier == "vm-jit";
    let color = color_enabled(&cli.color);

    if let Some(expr) = cli.expr {
        return run_eval(&expr, via_vm, with_jit, color);
    }

    match cli.cmd {
        Some(Cmd::Run { file, args }) => run_file(&file, &args, via_vm, with_jit, color),
        Some(Cmd::Repl) | None => run_repl(via_vm, color),
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

fn run_eval(src: &str, via_vm: bool, with_jit: bool, color: bool) -> ExitCode {
    let mut rt = Runtime::new();
    if with_jit {
        #[cfg(feature = "jit")]
        if let Err(e) = rt.install_jit() {
            eprintln!("crabscheme: failed to install JIT: {e}");
            return ExitCode::from(1);
        }
        #[cfg(not(feature = "jit"))]
        {
            eprintln!("crabscheme: --tier vm-jit requested but binary built without `jit` feature");
            return ExitCode::from(1);
        }
    }
    match eval_with_tier(&mut rt, "<command-line>", src, via_vm) {
        Ok(v) => {
            if !matches!(v, Value::Unspecified) {
                println!("{}", rt.format_value(&v, WriteMode::Write));
            }
            ExitCode::SUCCESS
        }
        Err(diag) => {
            let s = render_diag(&diag, rt.source_map(), color);
            eprintln!("{}", s);
            ExitCode::from(2)
        }
    }
}

fn run_file(
    path: &str,
    script_args: &[String],
    via_vm: bool,
    with_jit: bool,
    color: bool,
) -> ExitCode {
    let src = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("crabscheme: cannot read {}: {}", path, e);
            return ExitCode::from(1);
        }
    };
    let mut rt = Runtime::new();
    if with_jit {
        #[cfg(feature = "jit")]
        if let Err(e) = rt.install_jit() {
            eprintln!("crabscheme: failed to install JIT: {e}");
            return ExitCode::from(1);
        }
        #[cfg(not(feature = "jit"))]
        {
            eprintln!("crabscheme: --tier vm-jit requested but binary built without `jit` feature");
            return ExitCode::from(1);
        }
    }
    // R6RS `(command-line)` — script path + args after it. Strip the
    // crabscheme dispatcher's own argv so user code sees the same
    // shape as `gsi script.scm a b` would.
    let mut argv: Vec<String> = Vec::with_capacity(script_args.len() + 1);
    argv.push(path.to_string());
    argv.extend(script_args.iter().cloned());
    rt.set_command_line(argv);
    match eval_with_tier(&mut rt, path, &src, via_vm) {
        Ok(v) => {
            if !matches!(v, Value::Unspecified) {
                println!("{}", rt.format_value(&v, WriteMode::Write));
            }
            ExitCode::SUCCESS
        }
        Err(diag) => {
            let s = render_diag(&diag, rt.source_map(), color);
            eprintln!("{}", s);
            ExitCode::from(2)
        }
    }
}

fn run_repl(start_via_vm: bool, color: bool) -> ExitCode {
    let mut rt = Runtime::new();
    let mut counter: u32 = 0;
    let mut via_vm = start_via_vm;
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut buffer = String::new();
    println!(
        "crabscheme {} ({}) — :help for commands, ^D to exit",
        env!("CARGO_PKG_VERSION"),
        if via_vm { "vm" } else { "walker" },
    );
    loop {
        if buffer.is_empty() {
            print!("{}> ", if via_vm { "vm" } else { "" });
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
        // REPL command: line starts with `:` and we're not mid-expression.
        let trimmed = line.trim();
        if buffer.is_empty() && trimmed.starts_with(':') {
            match handle_repl_cmd(trimmed, &mut via_vm, &mut rt, color) {
                ReplCmdResult::Continue => {}
                ReplCmdResult::Quit => return ExitCode::SUCCESS,
            }
            continue;
        }
        buffer.push_str(&line);
        if !is_balanced(&buffer) {
            continue;
        }
        counter += 1;
        let name = format!("<repl:{}>", counter);
        let to_eval = std::mem::take(&mut buffer);
        let result = if via_vm {
            rt.eval_str_via_vm(&name, &to_eval)
        } else {
            rt.eval_str(&name, &to_eval)
        };
        match result {
            Ok(v) => {
                if !matches!(v, Value::Unspecified) {
                    println!("{}", rt.format_value(&v, WriteMode::Write));
                }
            }
            Err(diag) => {
                let s = render_diag(&diag, rt.source_map(), color);
                eprint!("{}", s);
            }
        }
    }
}

enum ReplCmdResult {
    Continue,
    Quit,
}

fn handle_repl_cmd(line: &str, via_vm: &mut bool, rt: &mut Runtime, color: bool) -> ReplCmdResult {
    let mut parts = line.splitn(2, char::is_whitespace);
    let cmd = parts.next().unwrap_or("");
    let arg = parts.next().unwrap_or("").trim();
    match cmd {
        ":quit" | ":q" | ":exit" => ReplCmdResult::Quit,
        ":help" | ":h" | ":?" => {
            println!(
                ":help                  this list\n\
                 :quit                  exit (also ^D)\n\
                 :tier walker|vm        switch execution tier (current: {})\n\
                 :time <expr>           evaluate <expr> and report wall time\n\
                 :load <path>           load and run a Scheme file in this session\n\
                 :reset                 reinitialize runtime, dropping definitions",
                if *via_vm { "vm" } else { "walker" }
            );
            ReplCmdResult::Continue
        }
        ":tier" => {
            match arg {
                "walker" => {
                    *via_vm = false;
                    println!("tier: walker");
                }
                "vm" => {
                    *via_vm = true;
                    println!("tier: vm");
                }
                "" => {
                    println!("tier: {}", if *via_vm { "vm" } else { "walker" });
                }
                other => println!("unknown tier {:?} — use walker or vm", other),
            }
            ReplCmdResult::Continue
        }
        ":time" => {
            if arg.is_empty() {
                println!(":time needs an expression");
                return ReplCmdResult::Continue;
            }
            let t = std::time::Instant::now();
            let r = if *via_vm {
                rt.eval_str_via_vm("<:time>", arg)
            } else {
                rt.eval_str("<:time>", arg)
            };
            let dt = t.elapsed();
            match r {
                Ok(v) => {
                    if !matches!(v, Value::Unspecified) {
                        println!("{}", rt.format_value(&v, WriteMode::Write));
                    }
                    println!("; {:.3?}", dt);
                }
                Err(diag) => {
                    let s = render_diag(&diag, rt.source_map(), color);
                    eprint!("{}", s);
                }
            }
            ReplCmdResult::Continue
        }
        ":load" => {
            if arg.is_empty() {
                println!(":load needs a file path");
                return ReplCmdResult::Continue;
            }
            match fs::read_to_string(arg) {
                Ok(src) => {
                    let r = if *via_vm {
                        rt.eval_str_via_vm(arg, &src)
                    } else {
                        rt.eval_str(arg, &src)
                    };
                    match r {
                        Ok(v) => {
                            if !matches!(v, Value::Unspecified) {
                                println!("{}", rt.format_value(&v, WriteMode::Write));
                            }
                            println!("; loaded {}", arg);
                        }
                        Err(diag) => {
                            let s = render_diag(&diag, rt.source_map(), color);
                            eprint!("{}", s);
                        }
                    }
                }
                Err(e) => println!(":load {}: {}", arg, e),
            }
            ReplCmdResult::Continue
        }
        ":reset" => {
            *rt = Runtime::new();
            println!("runtime reset");
            ReplCmdResult::Continue
        }
        other => {
            println!("unknown REPL command {:?} — try :help", other);
            ReplCmdResult::Continue
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
