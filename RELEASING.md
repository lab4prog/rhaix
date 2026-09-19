# Як випустити версію

Автоматизовано все, що можна відкотити. Кроки, які публікують щось назавжди
(crates.io, Marketplace, публічний реліз), людина робить сама — вони позначені ⚠.

## 0. Один раз: репозиторій

```bash
git remote add origin git@github.com:lab4prog/rhaix.git
git push -u origin main
```

Після цього на кожен push у `main` і на кожен pull request іде CI
(`.github/workflows/ci.yml`): тести на Linux/macOS/Windows, `clippy`, `fmt`,
перевірка MSRV (1.88), живі тести драйвера PostgreSQL і збірка розширення VS Code.

## 1. Підготувати версію

1. Підняти `version` у `[workspace.package]` **і** в усіх `rhaix-*` рядках
   `[workspace.dependencies]` кореневого `Cargo.toml` — вони мають збігатися.
2. Підняти `version` у `editors/vscode/package.json`.
3. Додати розділ `## X.Y.Z — дата` на початок `CHANGELOG.md`. Реліз-workflow
   бере текст релізу саме звідти й падає, якщо розділу немає.
4. Перевірити локально:

   ```bash
   cargo test --workspace
   cargo publish --workspace --dry-run
   ```

5. Закомітити в `main`.

## 2. Тег → чернетка релізу

```bash
git tag -a vX.Y.Z -m "X.Y.Z"
git push origin vX.Y.Z
```

`.github/workflows/release.yml`:

- звіряє тег із версією в `Cargo.toml` (розбіжність — помилка: `rhaix build`
  пінить версію ядра за власною);
- збирає `rhaix` і `rhaix-lsp` під `x86_64-linux-gnu`, `aarch64-apple-darwin`,
  `x86_64-apple-darwin`, `x86_64-windows-msvc` і проганяє на кожному
  `rhaix new` + `rhaix check`;
- пакує `.vsix`;
- створює **чернетку** релізу з архівами, `.vsix`, `SHA256SUMS` і нотатками з
  `CHANGELOG.md`.

## 3. ⚠ Опублікувати

Усе нижче незворотне. Порядок має значення: реліз на GitHub і `rhaix build`
зі свіжого бінарника працюють лише тоді, коли крейти вже є в crates.io.

1. **crates.io.** Потрібен токен (`cargo login`). Назви крейтів `rhaix-*` мають
   бути вільні або вашими — перевірте на crates.io до першого випуску.

   ```bash
   cargo publish --workspace
   ```

   Cargo публікує крейти в порядку залежностей і чекає, поки кожен з'явиться в
   індексі. Видалити опубліковану версію не можна, лише `cargo yank`.

2. **GitHub.** Відкрити чернетку релізу, переглянути нотатки й файли,
   натиснути «Publish release».

3. **VS Code Marketplace** (за бажанням). Потрібен власний publisher: у
   `editors/vscode/package.json` зараз стоїть `"publisher": "rhaix"`, його
   треба замінити на свій ідентифікатор.

   ```bash
   cd editors/vscode && npx @vscode/vsce publish
   ```

## Що перевіряти після

```bash
cargo install rhaix-cli --version X.Y.Z
rhaix new /tmp/check && rhaix build /tmp/check
```

Саме `rhaix build` на машині без клону репозиторію — те, що до 1.2.1 було
зламане для всіх, крім автора: згенерований крейт посилався на шлях до
репозиторію на машині збірки.
