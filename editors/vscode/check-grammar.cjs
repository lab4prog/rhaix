// Перевірка граматики .rhx справжнім токенайзером VS Code.
const fs = require('node:fs');
const path = require('node:path');

let oniguruma, textmate;
try {
  oniguruma = require('vscode-oniguruma');
  textmate = require('vscode-textmate');
} catch {
  console.error('Спершу: npm install vscode-textmate vscode-oniguruma');
  process.exit(2);
}

const GRAMMAR = path.join(__dirname, 'syntaxes/rhx.tmLanguage.json');

const wasm = fs.readFileSync(
  path.join(__dirname, 'node_modules/vscode-oniguruma/release/onig.wasm')
);

async function main() {
await oniguruma.loadWASM(wasm.buffer);
const registry = new textmate.Registry({
  onigLib: Promise.resolve({
    createOnigScanner: (s) => new oniguruma.OnigScanner(s),
    createOnigString: (s) => new oniguruma.OnigString(s),
  }),
  loadGrammar: async (scope) => {
    if (scope === 'text.html.rhx') {
      return textmate.parseRawGrammar(fs.readFileSync(GRAMMAR, 'utf8'), GRAMMAR);
    }
    return null; // text.html.basic відсутній — для перевірки наших правил не потрібен
  },
});

const grammar = await registry.loadGrammar('text.html.rhx');

const sample = `---
let todos = db.find("todos", #{ done: false });
page.title = t("greeting");
// коментар
---
<h1>{{ page.title }}</h1>
{{! невидимий коментар }}
<ul>
  <TodoItem @for={t in todos} @key={t.id} todo={t} />
  <li @if={todos.is_empty()} class="empty">Порожньо</li>
  <a href="/todo/{{ id }}" @class={#{"active": on}}>клац</a>
  <input {...attrs}>
</ul>
<rhaix:head />
`;

const lines = sample.split('\n');
let state = textmate.INITIAL;
const found = [];
for (const line of lines) {
  const res = grammar.tokenizeLine(line, state);
  for (const tok of res.tokens) {
    found.push({ text: line.slice(tok.startIndex, tok.endIndex), scopes: tok.scopes });
  }
  state = res.ruleStack;
}

// Перевірки: текст → очікуваний scope
const expect = [
  ['db.find у frontmatter', (t) => t.text === 'db', 'variable.language.rhaix.rhx'],
  ['let у frontmatter', (t) => t.text === 'let', 'keyword.control.rhx'],
  ['t() у frontmatter', (t) => t.text === 't', 'support.function.rhaix.rhx'],
  ['коментар //', (t) => t.text.startsWith('// коментар'), 'comment.line.double-slash.rhx'],
  ['{{ ... }}', (t) => t.text === '{{', 'punctuation.section.embedded.begin.rhx'],
  ['{{! ... }}', (t) => t.text === '{{!', 'comment.block.rhx'],
  ['компонент TodoItem', (t) => t.text === 'TodoItem', 'entity.name.tag.component.rhx'],
  ['директива @for', (t) => t.text === '@for', 'keyword.control.directive.rhx'],
  ['директива @if', (t) => t.text === '@if', 'keyword.control.directive.rhx'],
  ['директива @class', (t) => t.text === '@class', 'keyword.control.directive.rhx'],
  ['спец-тег rhaix:head', (t) => t.text === 'rhaix:head', 'entity.name.tag.rhaix.rhx'],
  ['спред {...', (t) => t.text.startsWith('{...') || t.text === '{...', 'keyword.operator.spread.rhx'],
  ['звичайний тег li', (t) => t.text === 'li', 'entity.name.tag.rhx'],
];

let bad = 0;
for (const [label, pick, scope] of expect) {
  const tok = found.find(pick);
  const ok = tok && tok.scopes.some((s) => s === scope);
  if (!ok) {
    bad++;
    console.log(`FAIL  ${label}: очікував ${scope}, отримав ${tok ? tok.scopes.join(' ') : '<токена не знайдено>'}`);
  } else {
    console.log(`ok    ${label}`);
  }
}
console.log(bad === 0 ? '\nУСЕ ГАРАЗД' : `\nПРОВАЛЕНО: ${bad}`);
process.exit(bad === 0 ? 0 : 1);
}
main();
