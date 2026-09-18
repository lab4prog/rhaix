// Клієнт мовного сервера: знаходить `rhaix-lsp` і з'єднує його з VS Code.
//
// Сам сервер — окремий бінарник із того ж репозиторію (крейт `rhaix-lsp`).
// Розширення його лише запускає: уся робота (компіляція `.rhx`, пошук
// компонентів) відбувається там, тим самим кодом, що й у `rhaix check`.

const { workspace, window } = require("vscode");
const { LanguageClient, TransportKind } = require("vscode-languageclient/node");

let client;

/// Де шукати сервер: спершу налаштування, далі PATH.
function serverCommand() {
  const configured = workspace.getConfiguration("rhaix").get("server.path");
  return configured && configured.trim() ? configured.trim() : "rhaix-lsp";
}

function activate(context) {
  const command = serverCommand();
  const server = { command, transport: TransportKind.stdio };

  client = new LanguageClient(
    "rhaix",
    "rhaix",
    { run: server, debug: server },
    {
      documentSelector: [{ scheme: "file", language: "rhx" }],
      // Правка компонента змінює діагностику сторінок, що його вбудували.
      synchronize: { fileEvents: workspace.createFileSystemWatcher("**/*.rhx") },
    }
  );

  client.start().catch((error) => {
    window.showWarningMessage(
      `rhaix: не вдалося запустити мовний сервер (\`${command}\`). ` +
        "Встановіть його: cargo install --path crates/rhaix-lsp — " +
        "або вкажіть шлях у налаштуванні rhaix.server.path. " +
        `Підсвітка синтаксису працює й без нього. (${error.message})`
    );
  });

  context.subscriptions.push({ dispose: () => client && client.stop() });
}

function deactivate() {
  return client ? client.stop() : undefined;
}

module.exports = { activate, deactivate };
