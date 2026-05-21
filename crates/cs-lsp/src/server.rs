//! The LSP backend — `tower_lsp::LanguageServer` implementation.
//!
//! Phase 1: skeleton (1.1) + the document cache and live
//! parse/expand diagnostics pipeline (1.2–1.4, 1.6). On every
//! didOpen/didChange the server re-runs the front-end (cheap for
//! typical files) and publishes diagnostics; didClose clears them.

use dashmap::DashMap;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::{
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    InitializeParams, InitializeResult, InitializedParams, MessageType, ServerCapabilities,
    ServerInfo, TextDocumentSyncCapability, TextDocumentSyncKind, Url,
};
use tower_lsp::{Client, LanguageServer};

/// A cached open document. Phase 1 only needs the latest text +
/// version; later phases add the parsed/expanded forms for hover,
/// go-to-def, and completion.
struct Document {
    text: String,
    version: i32,
}

/// The language server. Holds the `tower-lsp` client handle (to push
/// diagnostics/logs back to the editor) and a per-file document cache.
pub struct Backend {
    client: Client,
    documents: DashMap<Url, Document>,
}

impl Backend {
    /// Construct a backend bound to `client`. Used by the binary's
    /// `LspService::new(Backend::new)` and by tests.
    pub fn new(client: Client) -> Self {
        Self {
            client,
            documents: DashMap::new(),
        }
    }

    /// Cache `text` for `uri` and publish fresh diagnostics. Runs the
    /// front-end under `catch_unwind` so a front-end panic on
    /// pathological input degrades to "no diagnostics" instead of
    /// taking down the server.
    async fn refresh(&self, uri: Url, text: String, version: i32) {
        // iter 1.7 — stale/no-op guard: drop an out-of-order change (an
        // older version arriving after a newer one) and skip redundant
        // re-analysis when the text is byte-identical to what's cached.
        // Avoids editor flicker on races and no-op saves. The `get`
        // guard is scoped to this block so it's released before the
        // `insert` below (no re-entrant DashMap lock).
        if let Some(existing) = self.documents.get(&uri) {
            if version < existing.version || existing.text == text {
                return;
            }
        }
        let diagnostics = {
            let name = uri.to_string();
            let text_ref = &text;
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                crate::diagnostics::analyze(&name, text_ref)
            }))
            .unwrap_or_else(|_| Vec::new())
        };
        self.documents
            .insert(uri.clone(), Document { text, version });
        self.client
            .publish_diagnostics(uri, diagnostics, Some(version))
            .await;
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

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let doc = params.text_document;
        self.refresh(doc.uri, doc.text, doc.version).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        // Full-document sync: the last content change carries the whole
        // buffer. Ignore if (somehow) empty.
        if let Some(change) = params.content_changes.into_iter().last() {
            self.refresh(
                params.text_document.uri,
                change.text,
                params.text_document.version,
            )
            .await;
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        self.documents.remove(&uri);
        // Clear the editor's squigglies for the now-closed file.
        self.client.publish_diagnostics(uri, Vec::new(), None).await;
    }
}
