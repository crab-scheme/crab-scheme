//! The LSP backend — `tower_lsp::LanguageServer` implementation.
//!
//! Phase 1 iter 1.1: the skeleton. `initialize` advertises the
//! capabilities the later iters fill in (full-text sync now;
//! hover/definition/completion get switched on as their handlers
//! land). Document lifecycle methods are present as stubs so the
//! protocol handshake is complete; iter 1.2 wires the document cache
//! and iter 1.3 the parse-diagnostics pipeline.

use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::{
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    InitializeParams, InitializeResult, InitializedParams, MessageType, ServerCapabilities,
    ServerInfo, TextDocumentSyncCapability, TextDocumentSyncKind,
};
use tower_lsp::{Client, LanguageServer};

/// The language server. Holds the `tower-lsp` client handle (used to
/// push diagnostics / log messages back to the editor). The per-file
/// document cache lands in iter 1.2.
pub struct Backend {
    client: Client,
}

impl Backend {
    /// Construct a backend bound to `client`. Used by the binary's
    /// `LspService::new(Backend::new)` and by tests.
    pub fn new(client: Client) -> Self {
        Self { client }
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, _params: InitializeParams) -> Result<InitializeResult> {
        Ok(InitializeResult {
            server_info: Some(ServerInfo {
                name: "crabscheme-lsp".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
            capabilities: ServerCapabilities {
                // Full-document sync: the editor sends the whole buffer
                // on every change. Re-parsing a typical Scheme file is
                // microseconds (incremental sync is a later optimization).
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                ..Default::default()
            },
        })
    }

    async fn initialized(&self, _params: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "crabscheme-lsp initialized")
            .await;
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    // ---- Document lifecycle (stubs until iter 1.2/1.3) ----

    async fn did_open(&self, _params: DidOpenTextDocumentParams) {}

    async fn did_change(&self, _params: DidChangeTextDocumentParams) {}

    async fn did_close(&self, _params: DidCloseTextDocumentParams) {}
}
