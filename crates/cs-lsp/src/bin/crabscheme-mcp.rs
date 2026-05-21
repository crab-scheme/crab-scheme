//! `crabscheme-mcp` — a Model Context Protocol server for CrabScheme.
//!
//! Coding harnesses (Claude Code/Desktop and any MCP client) spawn this
//! binary and speak MCP — JSON-RPC 2.0 over **newline-delimited** stdio —
//! to call CrabScheme code intelligence as tools (cs_diagnostics,
//! cs_symbols, cs_definition, cs_references, cs_hover, cs_format,
//! cs_workspace_symbols). All protocol logic lives in [`cs_lsp::mcp`];
//! this binary is just the transport: read a line, hand it to
//! [`cs_lsp::mcp::handle`], write the response line.
//!
//! stdout carries *only* JSON-RPC messages (one per line) — anything
//! else would corrupt the stream — so diagnostics go to stderr.

use std::io::{BufRead, Write};

fn main() {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break, // stdin closed or unreadable
        };
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<serde_json::Value>(&line) {
            Ok(request) => cs_lsp::mcp::handle(&request),
            Err(_) => Some(cs_lsp::mcp::parse_error()),
        };
        // Notifications produce no response.
        if let Some(response) = response {
            if writeln!(out, "{response}").is_err() || out.flush().is_err() {
                break; // peer hung up
            }
        }
    }
}
