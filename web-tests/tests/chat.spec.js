import { test, expect } from '@playwright/test';

const TOKEN = 'by-human-web-contract-test-token-2026';
const auth = { Authorization: `Bearer ${TOKEN}` };
const composer = page => page.getByRole('textbox', { name: 'メッセージ', exact: true });
const userRows = page => page.locator('#chat-log [data-role="user"]');
const assistantRows = page => page.locator('#chat-log [data-role="assistant"]');

async function connect(page, navigate = true) {
  if (navigate) await page.goto('/');
  const dialog = page.getByRole('dialog', { name: 'by-human に接続', exact: true });
  await dialog.getByLabel('サーバートークン', { exact: true }).fill(TOKEN);
  await dialog.getByRole('button', { name: '接続', exact: true }).click();
  await expect(dialog).toBeHidden();
}

async function newConversation(page, title) {
  await page.getByRole('button', { name: '新しい会話', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: '新しい会話', exact: true });
  await dialog.getByLabel('会話の名前', { exact: true }).fill(title);
  const response = page.waitForResponse(response =>
    response.url().endsWith('/v1/conversations') && response.request().method() === 'POST');
  await dialog.getByRole('button', { name: '作成', exact: true }).click();
  const created = await (await response).json();
  await expect(dialog).toBeHidden();
  await expect(page.getByRole('heading', { name: title, exact: true })).toBeVisible();
  await expect(composer(page)).toBeEnabled();
  return created.id;
}

async function send(page, text) {
  await composer(page).fill(text);
  const response = page.waitForResponse(response =>
    /\/v1\/conversations\/[^/]+\/runs$/.test(response.url()) && response.request().method() === 'POST');
  await page.getByRole('button', { name: '送信', exact: true }).click();
  return (await response).json();
}

async function run(page, id) {
  const response = await page.request.get(`/v1/runs/${id}`, { headers: auth });
  expect(response.ok()).toBeTruthy();
  return (await response.json()).run;
}

async function conversation(page, id) {
  const response = await page.request.get(`/v1/conversations/${id}`, { headers: auth });
  expect(response.ok()).toBeTruthy();
  return response.json();
}

test('Japanese send has one transcript row, clears the draft and never stores the token', async ({ page }, testInfo) => {
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  await connect(page);
  const id = await newConversation(page, '日本語で会話');
  const sent = await send(page, '今日は何をしよう？');
  await expect(composer(page)).toHaveValue('');
  await expect(userRows(page)).toHaveCount(1);
  await expect(userRows(page)).toHaveText('今日は何をしよう？');
  await expect(assistantRows(page)).toHaveText('こんにちは。「今日は何をしよう？」を受け取りました。');
  await expect.poll(async () => (await run(page, sent.id)).status).toBe('completed');
  expect((await conversation(page, id)).runs).toHaveLength(1);
  const storage = await page.evaluate(() => ({
    local: JSON.stringify(localStorage), session: JSON.stringify(sessionStorage), url: location.href,
  }));
  expect(JSON.stringify(storage)).not.toContain(TOKEN);
  expect(errors).toEqual([]);
  await page.screenshot({ path: testInfo.outputPath('desktop-chat.png'), fullPage: true });
});

test('IME confirmation Enter does not submit and untrusted HTML stays text', async ({ page }) => {
  await connect(page);
  const id = await newConversation(page, '変換と表示');
  await composer(page).fill('にほんご');
  await composer(page).dispatchEvent('compositionstart');
  await composer(page).dispatchEvent('keydown', { key: 'Enter', code: 'Enter', isComposing: true, keyCode: 229 });
  await composer(page).dispatchEvent('compositionend');
  await expect(composer(page)).toHaveValue('にほんご');
  expect((await conversation(page, id)).runs).toHaveLength(0);
  const text = '<img src=x onerror="window.__injected=1"> 日本語';
  const sent = await send(page, text);
  await expect.poll(async () => (await run(page, sent.id)).status).toBe('completed');
  await expect(userRows(page)).toHaveText(text);
  await expect(assistantRows(page)).toContainText(text);
  await expect(page.locator('#chat-log img')).toHaveCount(0);
  expect(await page.evaluate(() => window.__injected)).toBeUndefined();
});

test('Stop and conversation switching preserve distinct drafts and isolate streamed text', async ({ page }) => {
  await connect(page);
  await newConversation(page, '作業 A');
  await newConversation(page, '作業 B');
  const nav = page.getByRole('navigation', { name: '会話一覧', exact: true });
  await nav.getByRole('button', { name: '作業 A', exact: true }).click();
  const sent = await send(page, '停止テスト');
  await expect(assistantRows(page)).toHaveText('停止操作を待っています。');
  await composer(page).fill('A の下書き');
  await nav.getByRole('button', { name: '作業 B', exact: true }).click();
  await expect(assistantRows(page)).toHaveCount(0);
  await composer(page).fill('B の下書き');
  await nav.getByRole('button', { name: '作業 A', exact: true }).click();
  await expect(composer(page)).toHaveValue('A の下書き');
  await expect(assistantRows(page)).toHaveText('停止操作を待っています。');
  await page.getByRole('button', { name: '停止', exact: true }).click();
  await expect.poll(async () => (await run(page, sent.id)).status).toBe('cancelled');
  await nav.getByRole('button', { name: '作業 B', exact: true }).click();
  await expect(composer(page)).toHaveValue('B の下書き');
  await expect(assistantRows(page)).toHaveCount(0);
});

test('lost send acknowledgment survives reload and reuses the original identity', async ({ page }) => {
  await connect(page);
  const id = await newConversation(page, '送信結果の確認');
  const attempts = [];
  let first = true;
  await page.route(/\/v1\/conversations\/[^/]+\/runs$/, async route => {
    attempts.push(route.request().postDataJSON());
    if (attempts.length <= 2) {
      if (first) await route.fetch(); // The real PostgreSQL enqueue succeeds; only its ACK is lost.
      first = false;
      await route.abort('failed');
    } else {
      await route.continue();
    }
  });
  await composer(page).fill('同じ送信を二重にしない');
  await page.getByRole('button', { name: '送信', exact: true }).click();
  await expect(page.getByRole('button', { name: '送信結果を確認', exact: true })).toBeVisible();
  await expect(composer(page)).toHaveValue('');
  // Exercise an explicit confirmation while the original outcome is unknown.
  await page.getByRole('button', { name: '送信結果を確認', exact: true }).click();
  await expect.poll(() => attempts.length).toBe(2);
  await expect(page.getByRole('button', { name: '送信結果を確認', exact: true })).toBeVisible();
  await composer(page).fill('確認中に書いた次の下書き');
  await page.reload();
  await connect(page, false);
  await expect(composer(page)).toHaveValue('確認中に書いた次の下書き');
  // Exact-key history is already proof of acceptance after reload.
  await expect(page.getByRole('button', { name: '送信結果を確認', exact: true })).toBeHidden();
  await expect(userRows(page)).toHaveCount(1);
  await expect(assistantRows(page)).toHaveText('こんにちは。「同じ送信を二重にしない」を受け取りました。');
  await expect(composer(page)).toHaveValue('確認中に書いた次の下書き');
  expect(attempts).toHaveLength(2);
  expect(attempts[1]).toEqual(attempts[0]);
  expect((await conversation(page, id)).runs).toHaveLength(1);
});

test('failed browser persistence keeps the draft and does not dispatch a new run', async ({ page }) => {
  await connect(page);
  const id = await newConversation(page, '保存できない場合');
  await page.evaluate(() => {
    Storage.prototype.setItem = () => { throw new DOMException('Synthetic quota boundary', 'QuotaExceededError'); };
  });
  await composer(page).fill('保存される前には送信しない');
  await expect(page.getByRole('button', { name: '送信', exact: true })).toBeDisabled();
  await composer(page).press('Enter');
  await expect(page.getByRole('status').filter({ hasText: /保存/ })).toBeVisible();
  await expect(composer(page)).toHaveValue('保存される前には送信しない');
  expect((await conversation(page, id)).runs).toHaveLength(0);
});

test('a delayed send settles after leaving and returning to the conversation', async ({ page }) => {
  await connect(page);
  await newConversation(page, '別の会話');
  const id = await newConversation(page, '送信先');
  let release;
  const held = new Promise(resolve => { release = resolve; });
  const routePattern = `**/v1/conversations/${id}/runs`;
  await page.route(routePattern, async route => {
    await held;
    await route.continue();
  });
  try {
    await composer(page).fill('受付を待つメッセージ');
    const request = page.waitForRequest(req => req.url().endsWith(`/conversations/${id}/runs`));
    await page.getByRole('button', { name: '送信', exact: true }).click();
    await request;
    const nav = page.getByRole('navigation', { name: '会話一覧', exact: true });
    await nav.getByRole('button', { name: '別の会話', exact: true }).click();
    await nav.getByRole('button', { name: '送信先', exact: true }).click();
    await composer(page).fill('次の下書き');
    await expect(page.getByRole('button', { name: '送信結果を確認', exact: true })).toBeHidden();
    await expect(page.getByText('送信しています', { exact: true })).toBeVisible();
    release();
    await expect(assistantRows(page)).toHaveText('こんにちは。「受付を待つメッセージ」を受け取りました。');
    await expect(userRows(page)).toHaveCount(1);
    await expect(composer(page)).toHaveValue('次の下書き');
    await expect(page.getByRole('button', { name: '送信', exact: true })).toBeEnabled();
  } finally {
    release();
  }
});

test('history settles an accepted send while its response is still delayed', async ({ page }) => {
  await connect(page);
  await newConversation(page, '切替先');
  const id = await newConversation(page, '受付済みの会話');
  const { promise: held, resolve: release } = Promise.withResolvers();
  const { promise: accepted, resolve: reportAccepted } = Promise.withResolvers();
  await page.route(`**/v1/conversations/${id}/runs`, async route => {
    if (route.request().postDataJSON().message !== '先に受け付けた送信') return route.continue();
    const response = await route.fetch();
    reportAccepted(await response.json());
    await held;
    await route.fulfill({ response });
  });
  try {
    await composer(page).fill('先に受け付けた送信');
    await page.getByRole('button', { name: '送信', exact: true }).click();
    const first = await accepted;
    await expect.poll(async () => (await run(page, first.id)).status).toBe('completed');
    const nav = page.getByRole('navigation', { name: '会話一覧', exact: true });
    await nav.getByRole('button', { name: '切替先', exact: true }).click();
    await nav.getByRole('button', { name: '受付済みの会話', exact: true }).click();
    await expect(userRows(page)).toHaveText(['先に受け付けた送信']);
    const next = await send(page, '次の送信');
    await expect.poll(async () => (await run(page, next.id)).status).toBe('completed');
    const late = page.waitForResponse(response => response.url().endsWith(`/conversations/${id}/runs`));
    release();
    await late;
    await expect(userRows(page)).toHaveText(['先に受け付けた送信', '次の送信']);
    expect((await conversation(page, id)).runs).toHaveLength(2);
  } finally {
    release();
  }
});

test('the next run can be stopped while an earlier Stop response is delayed', async ({ page }) => {
  await connect(page);
  await newConversation(page, '続けて停止');
  const first = await send(page, '停止テスト');
  const { promise: held, resolve: release } = Promise.withResolvers();
  await page.route(`**/v1/runs/${first.id}/cancel`, async route => {
    const response = await route.fetch();
    await held;
    await route.fulfill({ response });
  });
  const stop = page.getByRole('button', { name: '停止', exact: true });
  try {
    await stop.click();
    await expect.poll(async () => (await run(page, first.id)).status).toBe('cancelled');
    const next = await send(page, '停止テスト');
    await expect(stop).toBeEnabled();
    await stop.click();
    await expect.poll(async () => (await run(page, next.id)).status).toBe('cancelled');
    release();
    await expect(stop).toBeHidden();
  } finally {
    release();
  }
});

test('an interrupted event stream resumes with its durable cursor without duplicate text', async ({ page }) => {
  await connect(page);
  await newConversation(page, '接続の再開');
  const cursors = [];
  let first = true;
  let deliveredCursor;
  await page.route(/\/v1\/runs\/[^/]+\/events$/, async route => {
    cursors.push(route.request().headers()['last-event-id'] ?? '0');
    if (!first) return route.continue();
    first = false;
    const response = await route.fetch();
    const events = (await response.text()).split(/\r?\n\r?\n/).filter(Boolean);
    const textIndex = events.findIndex(event => /^event:\s*text_delta$/m.test(event));
    expect(textIndex).toBeGreaterThanOrEqual(0);
    deliveredCursor = /^id:\s*(\d+)$/m.exec(events[textIndex])[1];
    await route.fulfill({
      status: 200, contentType: 'text/event-stream',
      body: `${events.slice(0, textIndex + 1).join('\n\n')}\n\n`,
    });
  });
  await send(page, '再接続テスト');
  await expect(assistantRows(page)).toHaveText('こんにちは。「再接続テスト」を受け取りました。');
  await expect.poll(() => cursors.length).toBeGreaterThanOrEqual(2);
  expect(cursors[1]).toBe(deliveredCursor);
  await expect(userRows(page)).toHaveCount(1);
});

test('explicit review preserves uncertainty, shows records separately and releases the conversation', async ({ page }, testInfo) => {
  await connect(page);
  const id = await newConversation(page, '処理の確認');
  const sent = await send(page, '復旧テスト');
  await expect.poll(async () => {
    const response = await page.request.get(`/v1/runs/${sent.id}/review`, { headers: auth });
    return (await response.json()).tools.length;
  }).toBe(1);
  await page.getByRole('button', { name: '停止', exact: true }).click();
  await expect.poll(async () => (await run(page, sent.id)).status).toBe('uncertain');
  await expect(page.getByRole('button', { name: '実行記録を確認', exact: true })).toBeVisible();
  await expect(page.locator('#chat-log')).not.toContainText('synthetic operation only');
  await page.getByRole('button', { name: '実行記録を確認', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: '実行結果の確認', exact: true });
  await expect(dialog).toContainText('fixture_unknown_action');
  const resolve = dialog.getByRole('button', { name: '確認して会話を続ける', exact: true });
  await expect(resolve).toBeDisabled();
  await dialog.getByLabel('確認メモ', { exact: true }).fill('結果は未確認です。この処理を繰り返さず、次の会話へ進みます。');
  await expect(resolve).toBeDisabled();
  await dialog.getByLabel('実行記録を確認しました。この処理は再実行しません。', { exact: true }).check();
  const resolved = page.waitForResponse(response => response.url().endsWith(`/v1/runs/${sent.id}/resolve`));
  await resolve.click();
  expect((await resolved).ok()).toBeTruthy();
  await expect(dialog).toBeHidden();
  const reviewed = await run(page, sent.id);
  expect(reviewed.status).toBe('uncertain');
  expect(reviewed.reviewed_at).not.toBeNull();
  expect(reviewed.error_code).toBe('TOOL_OUTCOME_UNKNOWN');
  const next = await send(page, 'ここから別の話をします');
  await expect.poll(async () => (await run(page, next.id)).status).toBe('completed');
  const details = await (await page.request.get(`/v1/runs/${sent.id}/review`, { headers: auth })).json();
  expect(details.tools).toHaveLength(1);
  expect(details.tools[0].status).toBe('uncertain');
  expect(details.tools[0].output).toBeNull();
  expect((await conversation(page, id)).runs).toHaveLength(2);
  await expect(page.locator('#chat-log')).not.toContainText('synthetic operation only');
  await page.screenshot({ path: testInfo.outputPath('reviewed-conversation.png'), fullPage: true });
});

test('mobile drawer works with reduced motion and Japanese input stays visible', async ({ page }, testInfo) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await connect(page);
  await page.getByRole('button', { name: '会話一覧を開く', exact: true }).click();
  await newConversation(page, 'スマートフォン');
  await composer(page).fill('日本語の下書きです。');
  await expect(composer(page)).toBeInViewport();
  await page.getByRole('button', { name: '会話一覧を開く', exact: true }).click();
  await expect(page.getByRole('navigation', { name: '会話一覧', exact: true })).toBeVisible();
  await page.getByRole('complementary', { name: 'サイドバー', exact: true })
    .getByRole('button', { name: '会話一覧を閉じる', exact: true }).click();
  await expect(composer(page)).toHaveValue('日本語の下書きです。');
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBeTruthy();
  await page.screenshot({ path: testInfo.outputPath('mobile-chat.png'), fullPage: true });
});
