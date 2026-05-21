//! Phase 1 iter 1.1 smoke test: drive the `LspService` in-process
//! through its `tower::Service` interface (the standard tower-lsp test
//! pattern — deterministic, no stdio transport race). Sends an
//! `initialize` request and asserts the response advertises the server
//! and its capabilities.

use cs_lsp::Backend;
use tower::ServiceExt;
use tower_lsp::jsonrpc::Request;
use tower_lsp::LspService;

#[tokio::test]
async fn initialize_advertises_server_and_capabilities() {
    let (service, _socket) = LspService::new(Backend::new);

    let request = Request::build("initialize")
        .id(1)
        .params(serde_json::json!({ "capabilities": {} }))
        .finish();

    let response = service
        .oneshot(request)
        .await
        .expect("initialize call")
        .expect("initialize returns a response");

    let json = serde_json::to_value(&response).expect("serialize response");
    let result = &json["result"];

    assert_eq!(
        result["serverInfo"]["name"], "crabscheme-lsp",
        "initialize should name the server; got: {json}"
    );
    assert!(
        result["capabilities"].is_object(),
        "initialize should advertise capabilities; got: {json}"
    );
    // Full-document text sync is the capability iter 1.1 wires.
    assert_eq!(
        result["capabilities"]["textDocumentSync"], 1,
        "expected TextDocumentSyncKind::FULL (1); got: {json}"
    );
}
