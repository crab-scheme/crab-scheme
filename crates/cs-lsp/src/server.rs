//! The LSP backend — `tower_lsp::LanguageServer` implementation.
//!
//! Phase 1: skeleton (1.1) + the document cache and live
//! parse/expand diagnostics pipeline (1.2–1.4, 1.6). On every
//! didOpen/didChange the server re-runs the front-end (cheap for
//! typical files) and publishes diagnostics; didClose clears them.

use dashmap::DashMap;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::{
    CompletionOptions, CompletionParams, CompletionResponse, DidChangeTextDocumentParams,
    DidCloseTextDocumentParams, DidOpenTextDocumentParams, DocumentFormattingParams,
    DocumentHighlight, DocumentHighlightKind, DocumentHighlightParams, DocumentSymbolParams,
    DocumentSymbolResponse, GotoDefinitionParams, GotoDefinitionResponse, Hover, HoverParams,
    HoverProviderCapability, InitializeParams, InitializeResult, InitializedParams, Location,
    MessageType, OneOf, Position, Range, ReferenceParams, RenameParams, SemanticTokensFullOptions,
    SemanticTokensLegend, SemanticTokensOptions, SemanticTokensParams, SemanticTokensResult,
    SemanticTokensServerCapabilities, ServerCapabilities, ServerInfo, SignatureHelp,
    SignatureHelpOptions, SignatureHelpParams, SymbolInformation, TextDocumentSyncCapability,
    TextDocumentSyncKind, TextEdit, Url, WorkspaceEdit, WorkspaceSymbolParams,
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
    /// Workspace root, captured at `initialize`. Used by
    /// `workspace/symbol` to scan the project's `.scm` files.
    root: std::sync::Mutex<Option<Url>>,
}

impl Backend {
    /// Construct a backend bound to `client`. Used by the binary's
    /// `LspService::new(Backend::new)` and by tests.
    pub fn new(client: Client) -> Self {
        Self {
            client,
            documents: DashMap::new(),
            root: std::sync::Mutex::new(None),
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
    async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
        // Capture the workspace root for workspace/symbol scanning.
        #[allow(deprecated)] // root_uri is deprecated but still widely sent
        let root = params
            .workspace_folders
            .and_then(|fs| fs.into_iter().next())
            .map(|f| f.uri)
            .or(params.root_uri);
        if let Some(r) = root {
            *self.root.lock().unwrap() = Some(r);
        }
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
                // Phase 2: outline view (nested defines) + hover.
                document_symbol_provider: Some(OneOf::Left(true)),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                // Phase 3: go-to-definition, references, highlight.
                definition_provider: Some(OneOf::Left(true)),
                references_provider: Some(OneOf::Left(true)),
                document_highlight_provider: Some(OneOf::Left(true)),
                // Phase 4: completion + signature help.
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec!["(".to_string()]),
                    ..Default::default()
                }),
                signature_help_provider: Some(SignatureHelpOptions {
                    trigger_characters: Some(vec!["(".to_string(), " ".to_string()]),
                    retrigger_characters: None,
                    work_done_progress_options: Default::default(),
                }),
                // Phase 5: formatting, workspace symbols, rename.
                document_formatting_provider: Some(OneOf::Left(true)),
                workspace_symbol_provider: Some(OneOf::Left(true)),
                rename_provider: Some(OneOf::Left(true)),
                // Phase 5 iter 5.1: semantic highlighting. `full` only —
                // re-lexing a whole file is cheap, so we skip the `range`
                // and delta variants. The legend order must match
                // `semantic_tokens::TOKEN_TYPES`.
                semantic_tokens_provider: Some(
                    SemanticTokensServerCapabilities::SemanticTokensOptions(
                        SemanticTokensOptions {
                            legend: SemanticTokensLegend {
                                token_types: crate::semantic_tokens::TOKEN_TYPES.to_vec(),
                                token_modifiers: vec![],
                            },
                            full: Some(SemanticTokensFullOptions::Bool(true)),
                            range: None,
                            work_done_progress_options: Default::default(),
                        },
                    ),
                ),
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

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
        let uri = params.text_document.uri;
        // Clone the text out so the DashMap guard is released before the
        // (potentially slower) parse + walk.
        let Some(text) = self.documents.get(&uri).map(|d| d.text.clone()) else {
            return Ok(None);
        };
        let name = uri.to_string();
        let symbols = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::symbols::document_symbols(&name, &text)
        }))
        .unwrap_or_default();
        Ok(Some(DocumentSymbolResponse::Nested(symbols)))
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let pos = params.text_document_position_params;
        let uri = pos.text_document.uri;
        let position = pos.position;
        let Some(text) = self.documents.get(&uri).map(|d| d.text.clone()) else {
            return Ok(None);
        };
        let name = uri.to_string();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::hover::hover(&name, &text, position)
        }))
        .unwrap_or(None);
        Ok(result)
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let pos = params.text_document_position_params;
        let uri = pos.text_document.uri;
        let position = pos.position;
        let Some(text) = self.documents.get(&uri).map(|d| d.text.clone()) else {
            return Ok(None);
        };
        let name = uri.to_string();
        let range = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::references::definition(&name, &text, position)
        }))
        .unwrap_or(None);
        Ok(range.map(|range| GotoDefinitionResponse::Scalar(Location { uri, range })))
    }

    async fn references(&self, params: ReferenceParams) -> Result<Option<Vec<Location>>> {
        let pos = params.text_document_position;
        let uri = pos.text_document.uri;
        let position = pos.position;
        let include = params.context.include_declaration;
        let Some(text) = self.documents.get(&uri).map(|d| d.text.clone()) else {
            return Ok(None);
        };
        let name = uri.to_string();
        let ranges = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::references::references(&name, &text, position, include)
        }))
        .unwrap_or_default();
        Ok(Some(
            ranges
                .into_iter()
                .map(|range| Location {
                    uri: uri.clone(),
                    range,
                })
                .collect(),
        ))
    }

    async fn document_highlight(
        &self,
        params: DocumentHighlightParams,
    ) -> Result<Option<Vec<DocumentHighlight>>> {
        let pos = params.text_document_position_params;
        let uri = pos.text_document.uri;
        let position = pos.position;
        let Some(text) = self.documents.get(&uri).map(|d| d.text.clone()) else {
            return Ok(None);
        };
        let name = uri.to_string();
        let ranges = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::references::document_highlights(&name, &text, position)
        }))
        .unwrap_or_default();
        Ok(Some(
            ranges
                .into_iter()
                .map(|range| DocumentHighlight {
                    range,
                    kind: Some(DocumentHighlightKind::TEXT),
                })
                .collect(),
        ))
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let uri = params.text_document_position.text_document.uri;
        let Some(text) = self.documents.get(&uri).map(|d| d.text.clone()) else {
            return Ok(None);
        };
        let name = uri.to_string();
        let items = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::completion::completion(&name, &text)
        }))
        .unwrap_or_default();
        Ok(Some(CompletionResponse::Array(items)))
    }

    async fn signature_help(&self, params: SignatureHelpParams) -> Result<Option<SignatureHelp>> {
        let pos = params.text_document_position_params;
        let uri = pos.text_document.uri;
        let position = pos.position;
        let Some(text) = self.documents.get(&uri).map(|d| d.text.clone()) else {
            return Ok(None);
        };
        let name = uri.to_string();
        let help = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::completion::signature_help(&name, &text, position)
        }))
        .unwrap_or(None);
        Ok(help)
    }

    async fn formatting(&self, params: DocumentFormattingParams) -> Result<Option<Vec<TextEdit>>> {
        let uri = params.text_document.uri;
        let Some(text) = self.documents.get(&uri).map(|d| d.text.clone()) else {
            return Ok(None);
        };
        let formatted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::format::format(&text)
        }))
        .unwrap_or_else(|_| text.clone());
        if formatted == text {
            return Ok(Some(Vec::new()));
        }
        // Replace the whole document with the formatted text.
        let end = crate::text::offset_to_position(&text, text.len());
        Ok(Some(vec![TextEdit {
            range: Range::new(Position::new(0, 0), end),
            new_text: formatted,
        }]))
    }

    async fn symbol(
        &self,
        params: WorkspaceSymbolParams,
    ) -> Result<Option<Vec<SymbolInformation>>> {
        let Some(root) = self.root.lock().unwrap().clone() else {
            return Ok(None);
        };
        let Ok(root_path) = root.to_file_path() else {
            return Ok(None);
        };
        let query = params.query;
        let symbols = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::workspace::workspace_symbols(&root_path, &query)
        }))
        .unwrap_or_default();
        Ok(Some(symbols))
    }

    async fn rename(&self, params: RenameParams) -> Result<Option<WorkspaceEdit>> {
        let pos = params.text_document_position;
        let uri = pos.text_document.uri;
        let position = pos.position;
        let new_name = params.new_name;
        let Some(text) = self.documents.get(&uri).map(|d| d.text.clone()) else {
            return Ok(None);
        };
        let name = uri.to_string();
        // Rename = replace every occurrence (definition included) with
        // the new name. Same-file only (cross-file rename is future work).
        let ranges = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::references::references(&name, &text, position, true)
        }))
        .unwrap_or_default();
        if ranges.is_empty() {
            return Ok(None);
        }
        let edits: Vec<TextEdit> = ranges
            .into_iter()
            .map(|range| TextEdit {
                range,
                new_text: new_name.clone(),
            })
            .collect();
        let mut changes = std::collections::HashMap::new();
        changes.insert(uri, edits);
        Ok(Some(WorkspaceEdit {
            changes: Some(changes),
            document_changes: None,
            change_annotations: None,
        }))
    }

    async fn semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> Result<Option<SemanticTokensResult>> {
        let uri = params.text_document.uri;
        let Some(text) = self.documents.get(&uri).map(|d| d.text.clone()) else {
            return Ok(None);
        };
        let name = uri.to_string();
        let tokens = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::semantic_tokens::semantic_tokens(&name, &text)
        }))
        .unwrap_or_else(|_| tower_lsp::lsp_types::SemanticTokens {
            result_id: None,
            data: Vec::new(),
        });
        Ok(Some(SemanticTokensResult::Tokens(tokens)))
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
