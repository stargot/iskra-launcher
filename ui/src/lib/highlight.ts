// Самописная подсветка синтаксиса для превью сниппетов (прогон 2, D8).
// Без новых зависимостей и без dangerouslySetInnerHTML: однопроходный
// посимвольный сканер (регулярных выражений с квантификаторами нет вовсе —
// риск 7 «катастрофический бэктрекинг» невозможен), рендер — React-узлы
// <span class="tok-*">. Режимы:
//   generic — строки ('…', "…", `…`), комментарии // /* */ #, числа,
//             ~105 ключевых слов общих C-подобных языков (C/C++/C#/Java/
//             JS/TS/Rust/Go/Python);
//   JSON    — ключи/строки/числа/true-false-null; авто-детект: trimmed body
//             начинается с '{' или '['.
// Лимит: тело длиннее HIGHLIGHT_LIMIT символов → один plain-токен (CPU/RAM).
import { createElement, Fragment, useMemo, type ReactElement } from "react";

/** Токен: кусок текста + CSS-класс (.tok-* из theme.css); null — plain. */
export interface Token {
  text: string;
  cls: string | null;
}

/** D8: тела длиннее этого лимита не токенизируем — превью остаётся plain. */
export const HIGHLIGHT_LIMIT = 100_000;

// Общий набор ключевых слов C-подобных языков (~72 слова).
const KEYWORDS: ReadonlySet<string> = new Set([
  "abstract", "as", "async", "await", "base", "bool", "break", "byte", "case", "catch",
  "char", "class", "const", "continue", "default", "delegate", "delete", "do", "double",
  "elif", "else", "enum", "event", "export", "extends", "extern", "false", "final",
  "finally", "float", "fn", "for", "foreach", "func", "function", "get", "goto", "if",
  "implements", "import", "in", "inline", "int", "interface", "internal", "is", "lambda",
  "let", "lock", "long", "match", "mut", "namespace", "new", "nil", "none", "not", "null",
  "object", "operator", "or", "out", "override", "package", "params", "pass", "private",
  "protected", "public", "readonly", "ref", "return", "sealed", "self", "set", "short",
  "sizeof", "static", "string", "struct", "super", "switch", "this", "throw", "throws",
  "trait", "true", "try", "type", "typedef", "typeof", "union", "unsafe", "use", "using",
  "val", "var", "virtual", "void", "volatile", "when", "where", "while", "with", "yield",
]);

const isDigit = (c: string) => c >= "0" && c <= "9";
const isHexDigit = (c: string) => isDigit(c) || (c >= "a" && c <= "f") || (c >= "A" && c <= "F");
const isIdentStart = (c: string) =>
  (c >= "a" && c <= "z") || (c >= "A" && c <= "Z") || c === "_" || c === "$";
const isIdentPart = (c: string) => isIdentStart(c) || isDigit(c);
const isSpace = (c: string) => c === " " || c === "\t" || c === "\n" || c === "\r";

function push(tokens: Token[], text: string, cls: string | null): void {
  if (text) tokens.push({ text, cls });
}

/** Строка с \-эскейпами: start — индекс открывающей кавычки. */
function scanString(src: string, start: number): number {
  const quote = src[start];
  let i = start + 1;
  while (i < src.length) {
    if (src[i] === "\\") {
      i += 2;
      continue;
    }
    if (src[i] === quote) return i + 1;
    i++;
  }
  return i; // незакрытая строка — до конца тела
}

/** //… или #… — до конца строки. */
function scanLineComment(src: string, start: number): number {
  let i = start;
  while (i < src.length && src[i] !== "\n") i++;
  return i;
}

/** Блочный комментарий (слэш-звёздочка) — до закрытия или до конца тела. */
function scanBlockComment(src: string, start: number): number {
  const end = src.indexOf("*/", start + 2);
  return end === -1 ? src.length : end + 2;
}

/** Число generic-режима: десятичное/0xHEX/плавающее/экспонента. */
function scanNumber(src: string, start: number): number {
  let i = start;
  if (src[i] === "0" && (src[i + 1] === "x" || src[i + 1] === "X")) {
    i += 2;
    while (i < src.length && isHexDigit(src[i])) i++;
    return i;
  }
  while (i < src.length && isDigit(src[i])) i++;
  if (src[i] === "." && isDigit(src[i + 1])) {
    i++;
    while (i < src.length && isDigit(src[i])) i++;
  }
  if (src[i] === "e" || src[i] === "E") {
    let j = i + 1;
    if (src[j] === "+" || src[j] === "-") j++;
    if (isDigit(src[j])) {
      i = j;
      while (i < src.length && isDigit(src[i])) i++;
    }
  }
  return i;
}

/** Число JSON-режима (-1.5e+10); неполные формы останутся plain. */
function scanJsonNumber(src: string, start: number): number {
  let i = start;
  if (src[i] === "-") i++;
  while (i < src.length && isDigit(src[i])) i++;
  if (src[i] === ".") {
    i++;
    while (i < src.length && isDigit(src[i])) i++;
  }
  if (src[i] === "e" || src[i] === "E") {
    i++;
    if (src[i] === "+" || src[i] === "-") i++;
    while (i < src.length && isDigit(src[i])) i++;
  }
  return i;
}

/** Слово (идентификатор) начиная с start; возврат — индекс после слова. */
function scanIdent(src: string, start: number): number {
  let i = start + 1;
  while (i < src.length && isIdentPart(src[i])) i++;
  return i;
}

/** Generic-режим: строки, комментарии (//, блочные, #), числа, ключевые слова. */
function tokenizeGeneric(src: string): Token[] {
  const tokens: Token[] = [];
  let plain = "";
  const flush = () => {
    push(tokens, plain, null);
    plain = "";
  };
  let i = 0;
  while (i < src.length) {
    const c = src[i];
    const start = i;
    if (c === "'" || c === '"' || c === "`") {
      i = scanString(src, i);
      flush();
      push(tokens, src.slice(start, i), "tok-str");
    } else if ((c === "/" && src[i + 1] === "/") || c === "#") {
      i = scanLineComment(src, i);
      flush();
      push(tokens, src.slice(start, i), "tok-comment");
    } else if (c === "/" && src[i + 1] === "*") {
      i = scanBlockComment(src, i);
      flush();
      push(tokens, src.slice(start, i), "tok-comment");
    } else if (isDigit(c) || (c === "." && isDigit(src[i + 1]))) {
      i = scanNumber(src, i);
      flush();
      push(tokens, src.slice(start, i), "tok-num");
    } else if (isIdentStart(c)) {
      i = scanIdent(src, start);
      flush();
      const word = src.slice(start, i);
      push(tokens, word, KEYWORDS.has(word) ? "tok-kw" : null);
    } else {
      plain += c;
      i++;
    }
  }
  flush();
  return tokens;
}

/** JSON-режим: ключи (строка перед ':'), значения, числа, true/false/null. */
function tokenizeJson(src: string): Token[] {
  const tokens: Token[] = [];
  let plain = "";
  const flush = () => {
    push(tokens, plain, null);
    plain = "";
  };
  let i = 0;
  while (i < src.length) {
    const c = src[i];
    const start = i;
    if (c === '"') {
      i = scanString(src, i);
      let j = i;
      while (j < src.length && isSpace(src[j])) j++;
      flush();
      push(tokens, src.slice(start, i), j < src.length && src[j] === ":" ? "tok-key" : "tok-str");
    } else if (c === "-" || isDigit(c)) {
      i = scanJsonNumber(src, i);
      flush();
      push(tokens, src.slice(start, i), "tok-num");
    } else if (isIdentStart(c)) {
      i = scanIdent(src, start);
      flush();
      const word = src.slice(start, i);
      push(
        tokens,
        word,
        word === "true" || word === "false" || word === "null" ? "tok-kw" : null,
      );
    } else {
      plain += c;
      i++;
    }
  }
  flush();
  return tokens;
}

/** D8, авто-детект JSON: trimmed body начинается с '{' или '['. */
function looksLikeJson(body: string): boolean {
  let i = 0;
  while (i < body.length && isSpace(body[i])) i++;
  return body[i] === "{" || body[i] === "[";
}

/** Токены тела сниппета для рендера (generic или JSON по авто-детекту). */
export function highlight(body: string): Token[] {
  if (!body) return [];
  if (body.length > HIGHLIGHT_LIMIT) return [{ text: body, cls: null }];
  return looksLikeJson(body) ? tokenizeJson(body) : tokenizeGeneric(body);
}

/**
 * Готовый React-узел превью: <Highlighted body={...} />. Рендерит span.tok-*
 * через createElement — никакого dangerouslySetInnerHTML (D8).
 */
export function Highlighted({ body }: { body: string }): ReactElement {
  const tokens = useMemo(() => highlight(body), [body]);
  const children: Array<string | ReactElement> = tokens.map((t, i) =>
    t.cls ? createElement("span", { key: i, className: t.cls }, t.text) : t.text,
  );
  return createElement(Fragment, null, children);
}
