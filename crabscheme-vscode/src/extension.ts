// CrabScheme VS Code extension: a thin client that launches the
// `crabscheme-lsp` language server over stdio and lets VS Code's built-in
// LSP machinery do the rest (diagnostics, hover, completion, formatting,
// rename, semantic tokens, …). All the intelligence lives in the server.

import { workspace, ExtensionContext, window } from "vscode";
import {
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
  TransportKind,
} from "vscode-languageclient/node";

let client: LanguageClient | undefined;

export function activate(_context: ExtensionContext): void {
  const config = workspace.getConfiguration("crabscheme");
  const command = config.get<string>("serverPath") || "crabscheme-lsp";

  // Executable form: VS Code spawns `command` and speaks LSP over its
  // stdin/stdout (the server's default no-argument mode).
  const serverOptions: ServerOptions = {
    command,
    args: [],
    transport: TransportKind.stdio,
  };

  const clientOptions: LanguageClientOptions = {
    documentSelector: [{ scheme: "file", language: "scheme" }],
    synchronize: {
      fileEvents: workspace.createFileSystemWatcher("**/*.scm"),
    },
  };

  client = new LanguageClient(
    "crabscheme-lsp",
    "CrabScheme LSP",
    serverOptions,
    clientOptions,
  );

  client.start().catch((err) => {
    window.showErrorMessage(
      `CrabScheme: failed to start "${command}". Is it on your PATH? (${err})`,
    );
  });
}

export function deactivate(): Thenable<void> | undefined {
  return client?.stop();
}
