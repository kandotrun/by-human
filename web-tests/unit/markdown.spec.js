import { readFile } from 'node:fs/promises';
import { test, expect } from '@playwright/test';

// The module is loaded into a blank page through a Blob URL, so these
// contracts need neither the Rust fixture nor PostgreSQL.
const source = await readFile(new URL('../../server/web/markdown.js', import.meta.url), 'utf8');

test.beforeEach(async ({ page }) => {
  await page.evaluate(async code => {
    const url = URL.createObjectURL(new Blob([code], { type: 'text/javascript' }));
    window.markdown = await import(url);
  }, source);
});

const html = (page, text) => page.evaluate(value => {
  const host = document.createElement('div');
  host.append(window.markdown.renderMarkdown(value));
  return host.innerHTML;
}, text);

const COPY = '<div class="code-toolbar"><span class="code-language">LANG</span><button type="button" class="code-copy" aria-label="コードをコピー">コピー</button></div>';
const code = (language, body) => {
  const attribute = language ? ` data-language="${language}"` : '';
  return `<div class="code-block"${attribute}>${COPY.replace('LANG', language || 'コード')}<pre><code>${body}</code></pre></div>`;
};
const link = (href, text, title) =>
  `<a href="${href}"${title ? ` title="${title}"` : ''} rel="noopener noreferrer" target="_blank">${text}</a>`;

test('paragraphs keep single newlines as visible line breaks', async ({ page }) => {
  expect(await html(page, '一行目\n二行目\n\n次の段落')).toBe('<p>一行目<br>二行目</p><p>次の段落</p>');
  expect(await html(page, '\r\n前後の空行\r\n\r\n')).toBe('<p>前後の空行</p>');
});

test('ATX headings need a space and drop closing hashes', async ({ page }) => {
  expect(await html(page, '# 見出し1\n### 見出し3 ###\n#タグ')).toBe('<h1>見出し1</h1><h3>見出し3</h3><p>#タグ</p>');
});

test('emphasis works next to Japanese text and leaves identifiers alone', async ({ page }) => {
  expect(await html(page, '**太字**と*斜体*と~~取り消し~~')).toBe('<p><strong>太字</strong>と<em>斜体</em>と<del>取り消し</del></p>');
  expect(await html(page, 'これは**「重要」**です')).toBe('<p>これは<strong>「重要」</strong>です</p>');
  expect(await html(page, '__strong__ and _em_')).toBe('<p><strong>strong</strong> and <em>em</em></p>');
  expect(await html(page, 'snake_case_name と 2 * 3 * 4')).toBe('<p>snake_case_name と 2 * 3 * 4</p>');
  expect(await html(page, '***両方***')).toBe('<p><em><strong>両方</strong></em></p>');
  expect(await html(page, '**閉じていない')).toBe('<p>**閉じていない</p>');
});

test('inline code, escapes and raw HTML stay literal', async ({ page }) => {
  expect(await html(page, '`code` と ``a ` b`` と `閉じない')).toBe('<p><code>code</code> と <code>a ` b</code> と `閉じない</p>');
  expect(await html(page, '\\*強調しない\\* と `**そのまま**`')).toBe('<p>*強調しない* と <code>**そのまま**</code></p>');
  expect(await html(page, '<b>太字</b> &amp; <script>alert(1)</script>')).toBe('<p>&lt;b&gt;太字&lt;/b&gt; &amp;amp; &lt;script&gt;alert(1)&lt;/script&gt;</p>');
});

test('fenced code keeps its text exactly, with a language label and copy button', async ({ page }) => {
  expect(await html(page, '```js\nconst a = "<b>";\n  indented\n```')).toBe(code('js', 'const a = "&lt;b&gt;";\n  indented'));
  expect(await html(page, '~~~\nplain\n~~~\n後ろの段落')).toBe(code('', 'plain') + '<p>後ろの段落</p>');
  expect(await html(page, '````md\n```\ninner\n```\n````')).toBe(code('md', '```\ninner\n```'));
  // While a reply streams, an unterminated fence renders to the end.
  expect(await html(page, '説明\n```python\nprint(1)\n')).toBe('<p>説明</p>' + code('python', 'print(1)\n'));
});

test('block quotes and thematic breaks', async ({ page }) => {
  expect(await html(page, '> 引用\n> 続き\n>> 入れ子')).toBe('<blockquote><p>引用<br>続き</p><blockquote><p>入れ子</p></blockquote></blockquote>');
  expect(await html(page, '前\n\n---\n\n後\n***')).toBe('<p>前</p><hr><p>後</p><hr>');
});

test('lists nest, start at the given number and show task state', async ({ page }) => {
  expect(await html(page, '- a\n- b')).toBe('<ul><li>a</li><li>b</li></ul>');
  expect(await html(page, '説明:\n* a\n+ b')).toBe('<p>説明:</p><ul><li>a</li></ul><ul><li>b</li></ul>');
  expect(await html(page, '3. 三\n4) 四')).toBe('<ol start="3"><li>三</li></ol><ol start="4"><li>四</li></ol>');
  expect(await html(page, '1. 一\n2. 二')).toBe('<ol><li>一</li><li>二</li></ol>');
  expect(await html(page, '- a\n  - b\n    1. c\n- d')).toBe('<ul><li>a<ul><li>b<ol><li>c</li></ol></li></ul></li><li>d</li></ul>');
  expect(await html(page, '- a\n  続き\nさらに')).toBe('<ul><li>a<br>続き<br>さらに</li></ul>');
  expect(await html(page, '- a\n\n- b')).toBe('<ul><li><p>a</p></li><li><p>b</p></li></ul>');
  expect(await html(page, '- [ ] 未完了\n- [x] **完了**')).toBe(
    '<ul class="task-list"><li class="task-item"><input type="checkbox" disabled="">未完了</li>' +
    '<li class="task-item"><input type="checkbox" disabled="" checked=""><strong>完了</strong></li></ul>');
  expect(await html(page, '- ```\n  x\n  ```')).toBe('<ul><li>' + code('', 'x') + '</li></ul>');
});

test('GFM tables scroll and keep column alignment', async ({ page }) => {
  expect(await html(page, '| 名前 | 値 | 中央 |\n|:--|--:|:-:|\n| a | `1` | \\| |\n| b')).toBe(
    '<div class="table-scroll"><table><thead><tr><th data-align="left">名前</th><th data-align="right">値</th><th data-align="center">中央</th></tr></thead>' +
    '<tbody><tr><td data-align="left">a</td><td data-align="right"><code>1</code></td><td data-align="center">|</td></tr>' +
    '<tr><td data-align="left">b</td><td data-align="right"></td><td data-align="center"></td></tr></tbody></table></div>');
  expect(await html(page, 'a | b\n--- | ---\n1 | 2\n\n後')).toBe(
    '<div class="table-scroll"><table><thead><tr><th>a</th><th>b</th></tr></thead><tbody><tr><td>1</td><td>2</td></tr></tbody></table></div><p>後</p>');
  expect(await html(page, '| 表ではない |\n| 区切りなし |')).toBe('<p>| 表ではない |<br>| 区切りなし |</p>');
});

test('links allow only web and mail addresses', async ({ page }) => {
  expect(await html(page, '[サイト](https://example.com/a?b=1 "題名") と <https://example.com/x>')).toBe(
    `<p>${link('https://example.com/a?b=1', 'サイト', '題名')} と ${link('https://example.com/x', 'https://example.com/x')}</p>`);
  expect(await html(page, '詳しくは https://example.com/path。続き（https://example.com/a_b）')).toBe(
    `<p>詳しくは ${link('https://example.com/path', 'https://example.com/path')}。続き（${link('https://example.com/a_b', 'https://example.com/a_b')}）</p>`);
  expect(await html(page, '[メール](mailto:me@example.com) と [**強調**リンク](http://example.com)')).toBe(
    `<p>${link('mailto:me@example.com', 'メール')} と ${link('http://example.com', '<strong>強調</strong>リンク')}</p>`);
  expect(await html(page, '![図](https://example.com/a.png)')).toBe(`<p>${link('https://example.com/a.png', '図')}</p>`);
  expect(await html(page, '[相対](/v1/session) と [危険](javascript:alert(1)) と ![x](data:image/png;base64,AAAA)')).toBe('<p>相対 と 危険 と x</p>');
});

test('code paths, fences in lists, URLs in emphasis and table escapes', async ({ page }) => {
  expect(await html(page, '`C:\\`')).toBe('<p><code>C:\\</code></p>');
  expect(await html(page, '- item\n  ```\n  code\n  ```\nOutdented prose')).toBe('<ul><li>item' + code('', 'code') + '</li></ul><p>Outdented prose</p>');
  expect(await html(page, '- item\n  ```\n  code\nOutdented prose')).toBe('<ul><li>item' + code('', 'code') + '</li></ul><p>Outdented prose</p>');
  expect(await html(page, '**https://example.com**です')).toBe(`<p><strong>${link('https://example.com', 'https://example.com')}</strong>です</p>`);
  expect(await html(page, 'https://example.comです https://example.com/パス')).toBe(
    `<p>${link('https://example.com', 'https://example.com')}です ${link('https://example.com/パス', 'https://example.com/パス')}</p>`);
  expect(await html(page, '| `a\\|b` |\n|---|')).toBe('<div class="table-scroll"><table><thead><tr><th><code>a|b</code></th></tr></thead></table></div>');
  expect(await html(page, '- item\n  | column |\n  | --- |\nOutside')).toBe(
    '<ul><li>item<div class="table-scroll"><table><thead><tr><th>column</th></tr></thead></table></div></li></ul><p>Outside</p>');
  expect(await html(page, '- a\n  - b\n    ```\n    code\nOutside')).toBe('<ul><li>a<ul><li>b' + code('', 'code') + '</li></ul></li></ul><p>Outside</p>');
  expect(await html(page, '_https://example.com/my_page_')).toBe(`<p><em>${link('https://example.com/my_page', 'https://example.com/my_page')}</em></p>`);
  expect(await html(page, '[broken](oops[Email](mailto:team@example.com)')).toBe(`<p>[broken](oops${link('mailto:team@example.com', 'Email')}</p>`);
  expect(await html(page, '- item\n  | col |\n  | --- |\n  Alice\nOutside')).toBe(
    '<ul><li>item<div class="table-scroll"><table><thead><tr><th>col</th></tr></thead><tbody><tr><td>Alice</td></tr></tbody></table></div></li></ul><p>Outside</p>');
  expect(await html(page, '- a\n  - ```\n    code\nOutside')).toBe('<ul><li>a<ul><li>' + code('', 'code') + '</li></ul></li></ul><p>Outside</p>');
  expect(await html(page, '- a | b\ncontinued')).toBe('<ul><li>a | b<br>continued</li></ul>');
  expect(await html(page, '_https://example.com/folder_/page_')).toBe(`<p><em>${link('https://example.com/folder_/page', 'https://example.com/folder_/page')}</em></p>`);
  expect(await html(page, '*a**b* **word**x')).toBe('<p><em>a**b</em> <strong>word</strong>x</p>');
  expect(await html(page, 'https://example.com:8443. と [https://example.com]')).toBe(
    `<p>${link('https://example.com:8443', 'https://example.com:8443')}. と [${link('https://example.com', 'https://example.com')}]</p>`);
  expect(await html(page, '- ```\n  ---\n  code\nOutside')).toBe('<ul><li>' + code('', '---\ncode') + '</li></ul><p>Outside</p>');
  expect(await html(page, '- item\n  | col |\n  | --- |\n  # Title\n  para\ncontinued')).toBe(
    '<ul><li>item<div class="table-scroll"><table><thead><tr><th>col</th></tr></thead></table></div><h1>Title</h1>para<br>continued</li></ul>');
});

test('hostile input never creates active content', async ({ page }) => {
  const corpus = [
    '<script>alert(1)</script>',
    '<img src=x onerror=alert(1)>',
    '[x](javascript:alert(1))',
    '[x](JaVaScRiPt:alert(1))',
    '[x](java\tscript:alert(1))',
    '[x](  javascript:alert(1))',
    '[x](vbscript:msgbox(1))',
    '[x](data:text/html,<script>alert(1)</script>)',
    '![x](https://example.com/a.png" onerror="alert(1))',
    '[x](https://example.com "a\\" onmouseover=\\"alert(1))',
    '<a href="javascript:alert(1)">x</a>',
    '<svg onload=alert(1)>',
    '<iframe src="https://example.com"></iframe>',
    '&#106;avascript:alert(1)',
    '[x](&#106;avascript:alert(1))',
    '```html\n<script>alert(1)</script>\n```',
    '| <img src=x onerror=alert(1)> |\n| --- |\n| [x](javascript:alert(1)) |',
  ];
  const report = await page.evaluate(inputs => inputs.map(input => {
    const host = document.createElement('div');
    host.append(window.markdown.renderMarkdown(input));
    const elements = [...host.querySelectorAll('*')];
    return {
      input,
      forbidden: elements.filter(node => ['SCRIPT', 'IMG', 'IFRAME', 'STYLE', 'SVG', 'OBJECT', 'EMBED', 'FORM', 'VIDEO', 'AUDIO', 'LINK', 'META'].includes(node.tagName.toUpperCase())).length,
      handlers: elements.flatMap(node => [...node.attributes].map(attribute => attribute.name)).filter(name => /^on/i.test(name) || ['style', 'id', 'src', 'srcdoc'].includes(name)),
      hrefs: [...host.querySelectorAll('a')].map(anchor => anchor.getAttribute('href')),
    };
  }), corpus);
  for (const result of report) {
    expect(result.forbidden, result.input).toBe(0);
    expect(result.handlers, result.input).toEqual([]);
    for (const href of result.hrefs) expect(href, result.input).toMatch(/^(https?:\/\/|mailto:)/);
  }
});

test('every prefix of a streaming reply renders and the result is stable', async ({ page }) => {
  const sample = [
    '# 旅行の計画', '', '**1日目**は*京都*へ。詳しくは https://example.com を参照。', '',
    '- [x] 予約', '- [ ] 荷造り', '  1. 服', '  2. `充電器`', '', '> 注意: ~~雨~~ 晴れ', '',
    '| 日 | 予定 |', '|---|:-:|', '| 1 | 寺 |', '', '```js', 'const day = 1;', '```', '', '---', '最後',
  ].join('\n');
  const result = await page.evaluate(text => {
    const render = value => {
      const host = document.createElement('div');
      host.append(window.markdown.renderMarkdown(value));
      return host.innerHTML;
    };
    for (let end = 0; end <= text.length; end += 1) render(text.slice(0, end));
    return { first: render(text), second: render(text) };
  }, sample);
  expect(result.first).toBe(result.second);
  expect(result.first).toContain('<h1>旅行の計画</h1>');
  expect(result.first).toContain('<input type="checkbox" disabled="" checked="">予約');
  expect(result.first).toContain('<th data-align="center">予定</th>');
});

test('pathological replies render quickly', async ({ page }) => {
  const timings = await page.evaluate(() => {
    const inputs = {
      stars: '*'.repeat(100_000),
      brackets: '['.repeat(50_000),
      alternating: '*a_'.repeat(30_000),
      quotes: '>'.repeat(10_000) + ' 深い',
      lists: Array.from({ length: 2_000 }, (_, index) => `${' '.repeat(index * 2)}- 項目`).join('\n'),
      backticks: Array.from({ length: 1_500 }, (_, index) => '`'.repeat(index + 1) + 'x').join(' '),
      large: ('通常の段落です。**強調**と`コード`と https://example.com を含みます。\n').repeat(12_000),
      wideTable: '|' + 'a|'.repeat(256) + '\n|' + '-|'.repeat(256) + '\n' + '|\n'.repeat(256),
      openLinks: '[a]('.repeat(30_000),
      spaceRun: 'a' + ' '.repeat(200_000) + 'b',
      headingSpaces: '# a' + ' '.repeat(200_000) + 'b',
      mixedDelimiters: '_a '.repeat(60_000) + '*b*'.repeat(60_000),
      deepEmphasis: '*a '.repeat(70) + 'x '.repeat(300_000) + ' a*'.repeat(70),
      wideHeader: '|' + 'a|'.repeat(10_001) + '\n|' + '-|'.repeat(10_001),
      sameDelimiter: '*a '.repeat(40_000) + 'a**b '.repeat(40_000),
      longRuns: '*'.repeat(400_000) + 'x' + '*'.repeat(400_000),
      angleLinks: '[a](<'.repeat(30_000),
      badAuthorities: 'http://[/.'.repeat(30_000),
      controlPaths: 'https://a.test/'.repeat(30_000) + '\x01',
      headingSeparator: '- a\n      #' + ' '.repeat(64_000) + '\u2028x\nOutside',
      itemSeparator: '-' + ' '.repeat(64_000) + '\u2028x',
      fenceSeparator: '`'.repeat(64_000) + '\u2028x',
    };
    return Object.fromEntries(Object.entries(inputs).map(([name, text]) => {
      const started = performance.now();
      const host = document.createElement('div');
      host.append(window.markdown.renderMarkdown(text));
      // Padding table rows must not amplify a short reply into many cells.
      if (host.querySelectorAll('th, td').length > 10_000) return [name, Infinity];
      return [name, performance.now() - started];
    }));
  });
  for (const [name, elapsed] of Object.entries(timings)) expect(elapsed, name).toBeLessThan(1_000);
});
