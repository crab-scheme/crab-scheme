//! `crabscheme-lsp` — the CrabScheme Language Server, stdio transport.
//!
//! Editors spawn this binary and speak LSP JSON-RPC 2.0 over its
//! stdin/stdout. `tower-lsp` handles the framing and async dispatch;
//! all logic lives in [`cs_lsp::Backend`].

use cs_lsp::Backend;
use tower_lsp::{LspService, Server};

#[tokio::main]
async fn main() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let (service, socket) = LspService::new(Backend::new);
    Server::new(stdin, stdout, socket).serve(service).await;
}
