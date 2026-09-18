//! `rhaix-lsp` — мовний сервер для файлів `.rhx`.
//!
//! Компілятор у rhaix уже дає помилку з координатами `файл:рядок:колонка`.
//! Мовний сервер нічого нового не перевіряє — він доносить ту саму помилку в
//! редактор, поки її ще не видно в браузері.
//!
//! Спілкується по stdio за протоколом LSP. Запускає його розширення
//! (`editors/vscode`), вручну запускати не треба.

mod project;
mod rpc;
mod server;

fn main() {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut input = stdin.lock();
    let mut output = stdout.lock();

    server::Server::new().run(&mut input, &mut output);
}
