import { test, expect } from '@playwright/test';

const TOKEN = 'by-human-web-contract-test-token-2026';
const auth = { Authorization: `Bearer ${TOKEN}` };
const composer = page => page.getByRole('textbox', { name: 'メッセージ', exact: true });
const userRows = page => page.locator('#chat-log [data-role="user"]');
const assistantRows = page => page.locator('#chat-log [data-role="assistant"]');
const newChat = page => page.locator('#new-chat');
const conversationButton = (page, id) => page.locator(`#conversation-list [data-conversation-id="${id}"]`);
const conversationTitle = page => page.locator('#conversation-title');
const signInDialog = page => page.getByRole('dialog', { name: 'by-human にログイン', exact: true });

async function connect(page, navigate = true) {
  if (navigate) await page.goto('/');
  const dialog = signInDialog(page);
  await dialog.getByLabel('サーバートークン', { exact: true }).fill(TOKEN);
  await dialog.getByRole('button', { name: 'ログイン', exact: true }).click();
  await expect(dialog).toBeHidden();
}

async function expectSignedIn(page) {
  await expect(signInDialog(page)).toBeHidden();
  await expect(page.locator('#connection-state')).toHaveText('ログイン中');
}

async function newConversation(page) {
  const response = page.waitForResponse(response =>
    response.url().endsWith('/v1/conversations') && response.request().method() === 'POST');
  await newChat(page).click();
  const created = await (await response).json();
  await expect(conversationButton(page, created.id)).toHaveAttribute('aria-current', 'page');
  await expect(conversationTitle(page)).toHaveText('新しい会話');
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

test('large stored histories open and allow a new message', async ({ page }) => {
  await connect(page);
  const title = '大きな履歴';
  await page.getByRole('button', { name: title, exact: true }).click();
  await expect(page.getByRole('heading', { name: title, exact: true })).toBeVisible();
  await expect(composer(page)).toBeEnabled();
  await expect(page.getByText('古い履歴の一部は省略されています。')).toHaveCount(1);
  await expect(userRows(page).first()).not.toHaveText('依頼 1');
  await expect(userRows(page).last()).toHaveText('依頼 9');
  // Replies this long stay one plain text node instead of rendering Markdown.
  const plain = await assistantRows(page).evaluateAll(nodes => nodes.map(node =>
    node.classList.contains('plain') && node.childNodes.length === 1 && node.firstChild.nodeType === Node.TEXT_NODE));
  expect(plain).toEqual(Array(await userRows(page).count()).fill(true));
  const next = await send(page, 'ここから続けて');
  await expect.poll(async () => (await run(page, next.id)).status).toBe('completed');
  await expect(assistantRows(page).last()).toHaveText('こんにちは。「ここから続けて」を受け取りました。');
});

test('Japanese send has one transcript row, clears the draft and never stores the token', async ({ page }, testInfo) => {
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  await connect(page);
  const id = await newConversation(page);
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

test('a new conversation takes its title from the first message and keeps it after reload', async ({ page }) => {
  await connect(page);
  const id = await newConversation(page);
  await expect(page.getByRole('dialog')).toHaveCount(0);
  await expect(composer(page)).toBeFocused();
  const first = await send(page, '京都への旅行\n日程を考えて');
  const title = '京都への旅行 日程を考えて';
  await expect(conversationTitle(page)).toHaveText(title);
  await expect(conversationButton(page, id)).toHaveText(title);
  await expect.poll(async () => (await run(page, first.id)).status).toBe('completed');

  const second = await send(page, '宿泊先も教えて');
  await expect.poll(async () => (await run(page, second.id)).status).toBe('completed');
  await expect(conversationTitle(page)).toHaveText(title);
  await expect(conversationButton(page, id)).toHaveText(title);

  await page.reload();
  await expectSignedIn(page);
  await expect(conversationButton(page, id)).toHaveAttribute('aria-current', 'page');
  await expect(conversationTitle(page)).toHaveText(title);
  await expect(conversationButton(page, id)).toHaveText(title);
  expect((await conversation(page, id)).conversation.title).toBe(title);
});

test('a competing first send with the same key keeps different rejected text', async ({ page }) => {
  await connect(page);
  const id = await newConversation(page);
  const path = `/v1/conversations/${id}/runs`;
  let runId;
  await page.route(`**${path}`, async route => {
    const { idempotency_key } = route.request().postDataJSON();
    const winner = await page.request.post(path, {
      headers: auth,
      data: { message: '停止テスト', idempotency_key },
    });
    expect(winner.status()).toBe(202);
    runId = (await winner.json()).id;
    await route.continue();
  });
  try {
    const rejected = page.waitForResponse(response =>
      response.url().endsWith(path) && response.request().method() === 'POST');
    await composer(page).fill('別のタブで書いたメッセージ');
    await page.getByRole('button', { name: '送信', exact: true }).click();
    expect((await rejected).status()).toBe(409);
    await expect(conversationTitle(page)).toHaveText('停止テスト');
    await expect(conversationButton(page, id)).toHaveText('停止テスト');
    await expect(page.getByRole('button', { name: '停止', exact: true })).toBeVisible();
    await page.getByRole('button', { name: '入力欄に戻す', exact: true }).click();
    await expect(composer(page)).toHaveValue('別のタブで書いたメッセージ');
    await expect(userRows(page)).toHaveText(['停止テスト']);
  } finally {
    if (runId) {
      await page.request.post(`/v1/runs/${runId}/cancel`, { headers: auth });
      await expect.poll(async () => (await run(page, runId)).status).toBe('cancelled');
    }
  }
});

test('an offscreen first-send conflict updates only that sidebar title', async ({ page }) => {
  await connect(page);
  const other = await newConversation(page);
  await composer(page).fill('別の会話の下書き');
  const id = await newConversation(page);
  const path = `/v1/conversations/${id}/runs`;
  const winner = await page.request.post(path, {
    headers: auth,
    data: { message: '停止テスト', idempotency_key: 'offscreen-first-send' },
  });
  expect(winner.status()).toBe(202);
  const { id: runId } = await winner.json();
  const { promise: holdConflict, resolve: releaseConflict } = Promise.withResolvers();
  await page.route(`**${path}`, async route => {
    const response = await route.fetch();
    expect(response.status()).toBe(409);
    await holdConflict;
    await route.fulfill({ response });
  });
  try {
    const sending = page.waitForRequest(request => request.url().endsWith(path));
    await composer(page).fill('競合するメッセージ');
    await page.getByRole('button', { name: '送信', exact: true }).click();
    await sending;
    await conversationButton(page, other).click();
    await expect(composer(page)).toBeEnabled();

    releaseConflict();
    await expect(conversationButton(page, id)).toHaveText('停止テスト');
    await expect(conversationButton(page, other)).toHaveAttribute('aria-current', 'page');
    await expect(conversationTitle(page)).toHaveText('新しい会話');
    await expect(composer(page)).toHaveValue('別の会話の下書き');
    await expect(userRows(page)).toHaveCount(0);
  } finally {
    releaseConflict();
    await page.request.post(`/v1/runs/${runId}/cancel`, { headers: auth });
    await expect.poll(async () => (await run(page, runId)).status).toBe('cancelled');
  }
});

test('refreshing a conflicting send does not interrupt opening a new conversation', async ({ page }) => {
  await connect(page);
  const id = await newConversation(page);
  const path = `/v1/conversations/${id}/runs`;
  const winner = await page.request.post(path, {
    headers: auth,
    data: { message: '停止テスト', idempotency_key: 'other-client-first-send' },
  });
  expect(winner.status()).toBe(202);
  const { id: runId } = await winner.json();
  const { promise: holdConflict, resolve: releaseConflict } = Promise.withResolvers();
  const { promise: holdCreate, resolve: releaseCreate } = Promise.withResolvers();
  await page.route(`**${path}`, async route => {
    const response = await route.fetch();
    expect(response.status()).toBe(409);
    await holdConflict;
    await route.fulfill({ response });
  });
  await page.route('**/v1/conversations', async route => {
    if (route.request().method() === 'POST') await holdCreate;
    await route.continue();
  });
  try {
    await composer(page).fill('競合するメッセージ');
    const sending = page.waitForRequest(request => request.url().endsWith(path));
    await page.getByRole('button', { name: '送信', exact: true }).click();
    await sending;
    const creating = page.waitForRequest(request =>
      request.url().endsWith('/v1/conversations') && request.method() === 'POST');
    await newChat(page).click();
    await creating;
    releaseConflict();
    await expect(conversationTitle(page)).toHaveText('停止テスト');
    const created = page.waitForResponse(response =>
      response.url().endsWith('/v1/conversations') && response.status() === 201);
    releaseCreate();
    const { id: nextId } = await (await created).json();
    await expect(conversationButton(page, nextId)).toHaveAttribute('aria-current', 'page');
    await expect(conversationTitle(page)).toHaveText('新しい会話');
    await expect(composer(page)).toBeFocused();
  } finally {
    releaseConflict();
    releaseCreate();
    await page.request.post(`/v1/runs/${runId}/cancel`, { headers: auth });
    await expect.poll(async () => (await run(page, runId)).status).toBe('cancelled');
  }
});

test('conversation creation is available after reconnecting from a rejected token', async ({ page }) => {
  await connect(page);
  let rejected = false;
  await page.route('**/v1/conversations', route => {
    if (route.request().method() === 'POST' && !rejected) {
      rejected = true;
      return route.fulfill({ status: 401 });
    }
    return route.continue();
  });
  await newChat(page).click();
  await expect(signInDialog(page)).toBeVisible();
  await connect(page, false);
  await newConversation(page);
});

test('IME confirmation Enter does not submit and untrusted HTML stays text', async ({ page }) => {
  await connect(page);
  const id = await newConversation(page);
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
  await expect(page.locator('#conversation-title img, #conversation-list img')).toHaveCount(0);
  expect(await page.evaluate(() => window.__injected)).toBeUndefined();
});

test('Stop and conversation switching preserve distinct drafts and isolate streamed text', async ({ page }) => {
  await connect(page);
  const first = await newConversation(page);
  const second = await newConversation(page);
  await conversationButton(page, first).click();
  const sent = await send(page, '停止テスト');
  await expect(assistantRows(page)).toHaveText('停止操作を待っています。');
  await composer(page).fill('A の下書き');
  await conversationButton(page, second).click();
  await expect(assistantRows(page)).toHaveCount(0);
  await composer(page).fill('B の下書き');
  await conversationButton(page, first).click();
  await expect(composer(page)).toHaveValue('A の下書き');
  await expect(assistantRows(page)).toHaveText('停止操作を待っています。');
  await page.getByRole('button', { name: '停止', exact: true }).click();
  await expect.poll(async () => (await run(page, sent.id)).status).toBe('cancelled');
  await conversationButton(page, second).click();
  await expect(composer(page)).toHaveValue('B の下書き');
  await expect(assistantRows(page)).toHaveCount(0);
});

test('lost send acknowledgment survives reload and reuses the original identity', async ({ page }) => {
  await connect(page);
  const id = await newConversation(page);
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
  await expectSignedIn(page);
  await expect(composer(page)).toHaveValue('確認中に書いた次の下書き');
  // Exact-key history is already proof of acceptance after reload.
  await expect(page.getByRole('button', { name: '送信結果を確認', exact: true })).toBeHidden();
  await expect(userRows(page)).toHaveCount(1);
  await expect(assistantRows(page)).toHaveText('こんにちは。「同じ送信を二重にしない」を受け取りました。');
  await expect(conversationTitle(page)).toHaveText('同じ送信を二重にしない');
  await expect(conversationButton(page, id)).toHaveText('同じ送信を二重にしない');
  await expect(composer(page)).toHaveValue('確認中に書いた次の下書き');
  expect(attempts).toHaveLength(2);
  expect(attempts[1]).toEqual(attempts[0]);
  expect((await conversation(page, id)).runs).toHaveLength(1);
});

test('failed browser persistence keeps the draft and does not dispatch a new run', async ({ page }) => {
  await connect(page);
  const id = await newConversation(page);
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
  const other = await newConversation(page);
  const id = await newConversation(page);
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
    await conversationButton(page, other).click();
    await conversationButton(page, id).click();
    await composer(page).fill('次の下書き');
    await expect(page.getByRole('button', { name: '送信結果を確認', exact: true })).toBeHidden();
    await expect(page.getByText('送信しています', { exact: true })).toBeVisible();
    release();
    await expect(assistantRows(page)).toHaveText('こんにちは。「受付を待つメッセージ」を受け取りました。');
    await expect(conversationTitle(page)).toHaveText('受付を待つメッセージ');
    await expect(conversationButton(page, id)).toHaveText('受付を待つメッセージ');
    await expect(conversationButton(page, other)).toHaveText('新しい会話');
    await expect(userRows(page)).toHaveCount(1);
    await expect(composer(page)).toHaveValue('次の下書き');
    await expect(page.getByRole('button', { name: '送信', exact: true })).toBeEnabled();
  } finally {
    release();
  }
});

test('a failed refresh leaves sending disabled until the selected history loads', async ({ page }) => {
  await connect(page);
  const other = await newConversation(page);
  const id = await newConversation(page);
  const base = `/v1/conversations/${id}`;
  const { promise: holdAck, resolve: releaseAck } = Promise.withResolvers();
  const { promise: accepted, resolve: reportAccepted } = Promise.withResolvers();
  const { promise: holdSnapshot, resolve: releaseSnapshot } = Promise.withResolvers();
  const { promise: snapshotArrived, resolve: reportSnapshot } = Promise.withResolvers();
  let snapshotRequests = 0;
  let runId;
  await page.route(`**${base}/runs`, async route => {
    const response = await route.fetch();
    runId = (await response.json()).id;
    reportAccepted();
    await holdAck;
    await route.fulfill({ response });
  });
  await page.route(`**${base}/snapshot`, async route => {
    snapshotRequests += 1;
    if (snapshotRequests === 1) {
      reportSnapshot();
      await holdSnapshot;
      return route.continue();
    }
    if (snapshotRequests === 2) return route.fulfill({ status: 503 });
    return route.continue();
  });
  try {
    await composer(page).fill('停止テスト');
    await page.getByRole('button', { name: '送信', exact: true }).click();
    await accepted;
    await composer(page).fill('残しておきたい下書き');
    await conversationButton(page, other).click();
    await conversationButton(page, id).click();
    await snapshotArrived;
    releaseAck();
    await expect(page.locator('#notice')).toContainText('サーバーに接続できませんでした');
    await expect(composer(page)).toBeDisabled();
    await expect(page.getByRole('button', { name: '送信', exact: true })).toBeDisabled();
    await expect(composer(page)).toHaveValue('残しておきたい下書き');

    releaseSnapshot();
    await conversationButton(page, id).click();
    await expect(userRows(page)).toHaveText(['停止テスト']);
    await expect(page.getByRole('button', { name: '停止', exact: true })).toBeVisible();
    await expect(composer(page)).toBeEnabled();
    await expect(composer(page)).toHaveValue('残しておきたい下書き');
  } finally {
    releaseAck();
    releaseSnapshot();
    if (runId) {
      await page.request.post(`/v1/runs/${runId}/cancel`, { headers: auth });
      await expect.poll(async () => (await run(page, runId)).status).toBe('cancelled');
    }
  }
});

test('a failed refresh cannot enable Send on history that predates acceptance', async ({ page }) => {
  await connect(page);
  const other = await newConversation(page);
  const id = await newConversation(page);
  const base = `/v1/conversations/${id}`;
  const { promise: holdSend, resolve: releaseSend } = Promise.withResolvers();
  const { promise: sendArrived, resolve: reportSend } = Promise.withResolvers();
  let snapshots = 0;
  let runId;
  await page.route(`**${base}/runs`, async route => {
    reportSend();
    await holdSend;
    const response = await route.fetch();
    runId = (await response.json()).id;
    await route.fulfill({ response });
  });
  await page.route(`**${base}/snapshot`, route => {
    if (++snapshots === 2) return route.fulfill({ status: 503 });
    return route.continue();
  });
  try {
    await composer(page).fill('停止テスト');
    await page.getByRole('button', { name: '送信', exact: true }).click();
    await sendArrived;
    await conversationButton(page, other).click();
    await conversationButton(page, id).click();
    await composer(page).fill('次の下書き');

    releaseSend();
    await expect(page.locator('#notice')).toContainText('サーバーに接続できませんでした');
    await expect(conversationTitle(page)).toHaveText('停止テスト');
    await expect(page.getByRole('button', { name: '送信', exact: true })).toBeDisabled();
    await expect(composer(page)).toHaveValue('次の下書き');

    await conversationButton(page, id).click();
    await expect(userRows(page)).toHaveText(['停止テスト']);
    await expect(page.getByRole('button', { name: '停止', exact: true })).toBeVisible();
    await expect(composer(page)).toBeEnabled();
    await expect(composer(page)).toHaveValue('次の下書き');
  } finally {
    releaseSend();
    if (runId) {
      await page.request.post(`/v1/runs/${runId}/cancel`, { headers: auth });
      await expect.poll(async () => (await run(page, runId)).status).toBe('cancelled');
    }
  }
});

test('history settles an accepted send while its response is still delayed', async ({ page }) => {
  await connect(page);
  const other = await newConversation(page);
  const id = await newConversation(page);
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
    await conversationButton(page, other).click();
    await conversationButton(page, id).click();
    await expect(userRows(page)).toHaveText(['先に受け付けた送信']);
    await expect(conversationButton(page, id)).toHaveText('先に受け付けた送信');
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
  await newConversation(page);
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
  await newConversation(page);
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
  const id = await newConversation(page);
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

test('a delayed snapshot preserves a newer outcome review', async ({ page }) => {
  await connect(page);
  const id = await newConversation(page);
  const base = `/v1/conversations/${id}`;
  const { promise: holdSend, resolve: releaseSend } = Promise.withResolvers();
  const { promise: holdSnapshot, resolve: releaseSnapshot } = Promise.withResolvers();
  const { promise: capturedSnapshot, resolve: reportSnapshot } = Promise.withResolvers();
  await page.route(`**${base}/runs`, async route => {
    await holdSend;
    await route.continue();
  });
  try {
    const sending = page.waitForRequest(request => request.url().endsWith(`${base}/runs`));
    await composer(page).fill('競合するメッセージ');
    await page.getByRole('button', { name: '送信', exact: true }).click();
    await sending;
    const winner = await page.request.post(`${base}/runs`, {
      headers: auth,
      data: { message: '復旧テスト', idempotency_key: 'other-client-uncertain' },
    });
    expect(winner.status()).toBe(202);
    const { id: runId } = await winner.json();
    await conversationButton(page, id).click();
    await expect.poll(async () => {
      const response = await page.request.get(`/v1/runs/${runId}/review`, { headers: auth });
      return (await response.json()).tools.length;
    }).toBe(1);
    await page.getByRole('button', { name: '停止', exact: true }).click();
    const inspect = page.getByRole('button', { name: '実行記録を確認', exact: true });
    await expect(inspect).toBeVisible();

    await page.route(`**${base}/snapshot`, async route => {
      const response = await route.fetch();
      const snapshot = await response.json();
      expect(snapshot.runs.find(item => item.id === runId)?.reviewed_at).toBeNull();
      reportSnapshot();
      await holdSnapshot;
      await route.fulfill({ response });
    });
    releaseSend();
    await capturedSnapshot;
    await inspect.click();
    const dialog = page.getByRole('dialog', { name: '実行結果の確認', exact: true });
    await dialog.getByLabel('確認メモ', { exact: true }).fill('実行結果を確認しました。');
    await dialog.getByLabel('実行記録を確認しました。この処理は再実行しません。', { exact: true }).check();
    await dialog.getByRole('button', { name: '確認して会話を続ける', exact: true }).click();
    await expect(dialog).toBeHidden();

    releaseSnapshot();
    await expect(composer(page)).toBeEnabled();
    await expect(inspect).toBeHidden();
    await expect(page.locator('#chat-log')).toContainText('確認メモ：実行結果を確認しました。');
  } finally {
    releaseSend();
    releaseSnapshot();
  }
});

test('mobile drawer works with reduced motion and Japanese input stays visible', async ({ page }, testInfo) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await connect(page);
  await page.getByRole('button', { name: '会話一覧を開く', exact: true }).click();
  await newConversation(page);
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

test('an offscreen title refresh finds a conversation outside the newest list window', async ({ page }) => {
  const createConversation = async () => {
    const response = await page.request.post('/v1/conversations', { headers: auth, data: {} });
    expect(response.status()).toBe(201);
    return (await response.json()).id;
  };
  const id = await createConversation();
  for (let index = 0; index < 99; index += 1) await createConversation();
  await connect(page);
  await expect(page.locator('#conversation-list button')).toHaveCount(100);
  await conversationButton(page, id).click();
  await expect(conversationButton(page, id)).toHaveAttribute('aria-current', 'page');
  await expect(composer(page)).toBeEnabled();
  const path = `/v1/conversations/${id}/runs`;
  const winner = await page.request.post(path, {
    headers: auth,
    data: { message: '停止テスト', idempotency_key: 'offscreen-outside-list-window' },
  });
  expect(winner.status()).toBe(202);
  const { id: runId } = await winner.json();
  const { promise: holdConflict, resolve: releaseConflict } = Promise.withResolvers();
  await page.route(`**${path}`, async route => {
    const response = await route.fetch();
    expect(response.status()).toBe(409);
    await holdConflict;
    await route.fulfill({ response });
  });
  try {
    const sending = page.waitForRequest(request => request.url().endsWith(path));
    await composer(page).fill('競合するメッセージ');
    await page.getByRole('button', { name: '送信', exact: true }).click();
    await sending;
    const created = await newConversation(page);
    // The newly created conversation pushes the older one out of the server's newest 100.
    const listed = await page.request.get('/v1/conversations', { headers: auth });
    expect((await listed.json()).conversations.some(item => item.id === id)).toBeFalsy();

    releaseConflict();
    await expect(conversationButton(page, id)).toHaveText('停止テスト');
    await expect(conversationButton(page, created)).toHaveAttribute('aria-current', 'page');
    await expect(conversationTitle(page)).toHaveText('新しい会話');
  } finally {
    releaseConflict();
    await page.request.post(`/v1/runs/${runId}/cancel`, { headers: auth });
    await expect.poll(async () => (await run(page, runId)).status).toBe('cancelled');
  }
});

test('sign-in survives reload and sign-out returns to the dialog', async ({ page, context }) => {
  await connect(page);
  const id = await newConversation(page);
  await page.reload();
  await expectSignedIn(page);
  await expect(conversationButton(page, id)).toHaveAttribute('aria-current', 'page');
  const session = (await context.cookies()).find(cookie => cookie.name === 'by_human_session');
  expect(session).toMatchObject({ httpOnly: true, sameSite: 'Strict', path: '/' });
  expect(session.value).not.toContain(TOKEN);
  expect(await page.evaluate(() => document.cookie)).not.toContain('by_human_session');
  await page.getByRole('button', { name: 'ログアウト', exact: true }).click();
  await expect(signInDialog(page)).toBeVisible();
  await page.reload();
  await expect(signInDialog(page)).toBeVisible();
  expect((await context.cookies()).some(cookie => cookie.name === 'by_human_session')).toBeFalsy();
});

test('an unknown session cookie asks to sign in again', async ({ page, context }) => {
  await context.addCookies([{ name: 'by_human_session', value: 'A'.repeat(43), url: 'http://127.0.0.1:43219' }]);
  await page.goto('/');
  await expect(signInDialog(page)).toBeVisible();
  await connect(page, false);
  await expectSignedIn(page);
});

test('temporary loading failures keep the session and offer a retry', async ({ page }) => {
  await connect(page);
  const id = await newConversation(page);
  for (const path of ['**/v1/session', '**/v1/conversations']) {
    let failures = 1;
    await page.route(path, route => {
      if (route.request().method() !== 'GET' || failures === 0) return route.continue();
      failures -= 1;
      return route.fulfill({ status: 503, contentType: 'application/json', body: '{"error":"storage_unavailable"}' });
    });
    await page.reload();
    await expect(page.getByRole('button', { name: '再試行', exact: true })).toBeVisible();
    await expect(signInDialog(page)).toBeHidden();
    // A conversation created before the list loads could be dropped by it.
    await expect(newChat(page)).toBeDisabled();
    await page.getByRole('button', { name: '再試行', exact: true }).click();
    await expectSignedIn(page);
    await expect(conversationButton(page, id)).toHaveAttribute('aria-current', 'page');
    await expect(page.locator('#notice')).toBeHidden();
    await expect(newChat(page)).toBeEnabled();
    await page.unroute(path);
  }
});

test('a retry keeps the reason sending is disabled by unreadable tab storage', async ({ page }) => {
  await connect(page);
  await newConversation(page);
  await page.evaluate(() => sessionStorage.setItem('by-human-chat-v1', '{broken'));
  let failures = 1;
  await page.route('**/v1/session', route => {
    if (failures === 0) return route.continue();
    failures -= 1;
    return route.fulfill({ status: 503, contentType: 'application/json', body: '{"error":"storage_unavailable"}' });
  });
  await page.reload();
  await page.getByRole('button', { name: '再試行', exact: true }).click();
  await expectSignedIn(page);
  await expect(page.locator('#notice')).toContainText('このタブの送信待ちを読み込めませんでした');
});

test('pending text stays hidden while signed out and returns after signing in', async ({ page }) => {
  await connect(page);
  const id = await newConversation(page);
  const runs = /\/v1\/conversations\/[^/]+\/runs$/;
  await page.route(runs, route => route.abort('failed'));
  await composer(page).fill('ログアウト後に見せない本文');
  await page.getByRole('button', { name: '送信', exact: true }).click();
  await expect(page.getByRole('button', { name: '送信結果を確認', exact: true })).toBeVisible();
  await expect(userRows(page)).toHaveText(['ログアウト後に見せない本文']);
  await page.getByRole('button', { name: 'ログアウト', exact: true }).click();
  await expect(signInDialog(page)).toBeVisible();
  await expect(page.getByText('ログアウト後に見せない本文')).toHaveCount(0);
  await expect(page.getByRole('button', { name: '送信結果を確認', exact: true })).toHaveCount(0);
  await page.reload();
  await expect(signInDialog(page)).toBeVisible();
  await expect(page.getByText('ログアウト後に見せない本文')).toHaveCount(0);
  await page.unroute(runs);
  await connect(page, false);
  await expect(conversationButton(page, id)).toHaveAttribute('aria-current', 'page');
  await expect(userRows(page)).toHaveText(['ログアウト後に見せない本文']);
  await expect(page.getByRole('button', { name: '送信結果を確認', exact: true })).toBeVisible();
});

test('a retry that finds the session ended returns to a clean sign-in', async ({ page, context }) => {
  await connect(page);
  await newConversation(page);
  const runs = /\/v1\/conversations\/[^/]+\/runs$/;
  await page.route(runs, route => route.abort('failed'));
  await composer(page).fill('再試行で隠す本文');
  await page.getByRole('button', { name: '送信', exact: true }).click();
  await expect(page.getByRole('button', { name: '送信結果を確認', exact: true })).toBeVisible();
  let failures = 1;
  await page.route('**/v1/conversations', route => {
    if (route.request().method() !== 'GET' || failures === 0) return route.continue();
    failures -= 1;
    return route.fulfill({ status: 503, contentType: 'application/json', body: '{"error":"storage_unavailable"}' });
  });
  await page.reload();
  await expect(page.getByRole('button', { name: '再試行', exact: true })).toBeVisible();
  // The session ends before the owner retries.
  await context.clearCookies();
  await page.getByRole('button', { name: '再試行', exact: true }).click();
  await expect(signInDialog(page)).toBeVisible();
  await expect(page.locator('#connection-state')).toHaveText('未ログイン');
  await expect(page.getByText('再試行で隠す本文')).toHaveCount(0);
});

test('a loading failure right after signing in keeps the new session', async ({ page }) => {
  const seeded = await page.request.post('/v1/conversations', { headers: auth, data: { title: '再試行で表示する会話' } });
  expect(seeded.status()).toBe(201);
  const { id } = await seeded.json();
  let failures = 1;
  await page.route('**/v1/conversations', route => {
    if (route.request().method() !== 'GET' || failures === 0) return route.continue();
    failures -= 1;
    return route.fulfill({ status: 503, contentType: 'application/json', body: '{"error":"storage_unavailable"}' });
  });
  await page.goto('/');
  const dialog = signInDialog(page);
  await dialog.getByLabel('サーバートークン', { exact: true }).fill(TOKEN);
  await dialog.getByRole('button', { name: 'ログイン', exact: true }).click();
  await expect(dialog).toBeHidden();
  await expect(page.locator('#connection-state')).toHaveText('ログイン中');
  await expect(conversationButton(page, id)).toHaveCount(0);
  await page.getByRole('button', { name: '再試行', exact: true }).click();
  await expect(conversationButton(page, id)).toBeVisible();
  await expect(page.locator('#notice')).toBeHidden();
});

test('the sign-in button stays disabled until the first load settles', async ({ page }) => {
  const { promise: holdList, resolve: releaseList } = Promise.withResolvers();
  await page.route('**/v1/conversations', async route => {
    if (route.request().method() === 'GET') await holdList;
    await route.continue();
  });
  await page.goto('/');
  const dialog = signInDialog(page);
  await dialog.getByLabel('サーバートークン', { exact: true }).fill(TOKEN);
  const listing = page.waitForRequest(request => request.url().endsWith('/v1/conversations') && request.method() === 'GET');
  await dialog.getByRole('button', { name: 'ログイン', exact: true }).click();
  // The session exists and the first list load is in progress.
  await listing;
  await expect(dialog.getByRole('button', { name: 'ログイン', exact: true })).toBeDisabled();
  await expect(dialog.getByRole('alert')).toHaveText('ログインしています…');
  releaseList();
  await expect(dialog).toBeHidden();
  await expectSignedIn(page);
});

test('a sign-in whose cookie is not kept explains the problem', async ({ page }) => {
  // Simulates a browser that drops the session cookie.
  await page.route('**/v1/sessions', route => route.fulfill({ status: 200, contentType: 'application/json', body: '{"expires_at":"2026-12-18T00:00:00Z"}' }));
  await page.goto('/');
  const dialog = signInDialog(page);
  await dialog.getByLabel('サーバートークン', { exact: true }).fill(TOKEN);
  await dialog.getByRole('button', { name: 'ログイン', exact: true }).click();
  await expect(dialog.getByRole('alert')).toHaveText('ログイン状態を保存できませんでした。ブラウザのCookie設定を確認してください。');
  await expect(dialog).toBeVisible();
});

test('replies render as Markdown with copy buttons', async ({ page, context }) => {
  await context.grantPermissions(['clipboard-read', 'clipboard-write']);
  await connect(page);
  await newConversation(page);
  const sent = await send(page, 'Markdownテスト');
  await expect.poll(async () => (await run(page, sent.id)).status).toBe('completed');
  const reply = assistantRows(page).last();
  await expect(reply.getByRole('heading', { name: '旅の準備', level: 2 })).toBeVisible();
  await expect(reply.locator('li.task-item input[type="checkbox"]')).toHaveCount(2);
  await expect(reply.locator('li.task-item').nth(1).locator('strong')).toHaveText('荷造り');
  await expect(reply.locator('.code-block[data-language="js"] code')).toHaveText('const day = 1;');
  const guide = reply.getByRole('link', { name: '案内', exact: true });
  await expect(guide).toHaveAttribute('href', 'https://example.com/guide');
  await expect(guide).toHaveAttribute('target', '_blank');
  await reply.getByRole('button', { name: 'コードをコピー', exact: true }).click();
  // The button keeps its name, so a status outside the log announces the result.
  await expect(page.getByRole('status').filter({ hasText: 'コピーしました' })).toHaveCount(1);
  await expect.poll(() => page.evaluate(() => navigator.clipboard.readText())).toBe('const day = 1;');
  await page.getByRole('button', { name: '返信をコピー', exact: true }).last().click();
  await expect.poll(() => page.evaluate(() => navigator.clipboard.readText())).toContain('## 旅の準備\n\n- [x] 予約');
});

test('the first streamed text updates only the reply in progress', async ({ page }) => {
  await connect(page);
  await newConversation(page);
  const first = await send(page, '最初のメッセージ');
  await expect.poll(async () => (await run(page, first.id)).status).toBe('completed');
  const { promise: holdEvents, resolve: releaseEvents } = Promise.withResolvers();
  await page.route('**/v1/runs/*/events', async route => {
    await holdEvents;
    await route.continue();
  });
  const sent = await send(page, '停止テスト');
  // The accepted run renders before the stream starts; capture after that.
  await expect(page.locator(`#chat-log [data-role="user"][data-run-id="${sent.id}"]`)).toBeVisible();
  const earlier = await userRows(page).first().elementHandle();
  releaseEvents();
  await expect(assistantRows(page).last()).toHaveText('停止操作を待っています。');
  expect(await earlier.evaluate(node => node.isConnected)).toBe(true);
  await page.getByRole('button', { name: '停止', exact: true }).click();
  await expect.poll(async () => (await run(page, sent.id)).status).toBe('cancelled');
});

test('a status pill and typing dots show what the agent is doing', async ({ page }) => {
  await connect(page);
  await newConversation(page);
  const tool = await send(page, '復旧テスト');
  await expect(page.locator('#run-progress')).toHaveText('ツールを実行中');
  await expect(page.locator('#chat-log .typing')).toBeVisible();
  await page.getByRole('button', { name: '停止', exact: true }).click();
  await expect.poll(async () => (await run(page, tool.id)).status).toBe('uncertain');
  await expect(page.locator('#run-progress')).toBeHidden();
  await expect(page.locator('#chat-log .typing')).toHaveCount(0);

  await newConversation(page);
  const streaming = await send(page, '停止テスト');
  await expect(page.locator('#run-progress')).toHaveText('応答中');
  await expect(page.locator('#chat-log .typing')).toHaveCount(0);
  await page.getByRole('button', { name: '停止', exact: true }).click();
  await expect.poll(async () => (await run(page, streaming.id)).status).toBe('cancelled');
  await expect(page.locator('#run-progress')).toBeHidden();
});

test('time separators label turns and a jump button returns to the latest reply', async ({ page }) => {
  await page.clock.setFixedTime(new Date(Date.now() + 3 * 24 * 60 * 60 * 1000));
  await page.setViewportSize({ width: 1280, height: 520 });
  await connect(page);
  await newConversation(page);
  for (let index = 0; index < 3; index += 1) {
    const sent = await send(page, 'Markdownテスト');
    await expect.poll(async () => (await run(page, sent.id)).status).toBe('completed');
  }
  const separators = page.locator('#chat-log .time-separator');
  await expect(separators).toHaveCount(1);
  await expect(separators.first()).toHaveText(/^\d{1,2}月\d{1,2}日 \d{2}:\d{2}$/);
  const area = page.locator('#message-area');
  const jump = page.getByRole('button', { name: '最新のメッセージへ移動', exact: true });
  await expect(jump).toBeHidden();
  await area.evaluate(node => { node.scrollTop = 0; });
  await expect(jump).toBeVisible();
  await jump.click();
  await expect.poll(() => area.evaluate(node => node.scrollHeight - node.scrollTop - node.clientHeight)).toBeLessThan(100);
  await expect(jump).toBeHidden();
});

test('the chat follows the system dark theme', async ({ page }) => {
  const canvas = () => page.evaluate(() => {
    const [red, green, blue, alpha = 1] = getComputedStyle(document.documentElement).backgroundColor.match(/[\d.]+/g).map(Number);
    return { brightest: Math.max(red, green, blue), darkest: Math.min(red, green, blue), alpha };
  });
  await page.emulateMedia({ colorScheme: 'dark' });
  await connect(page);
  const dark = await canvas();
  expect(dark.alpha).toBe(1);
  expect(dark.brightest).toBeLessThan(60);
  await page.emulateMedia({ colorScheme: 'light' });
  const light = await canvas();
  expect(light.alpha).toBe(1);
  expect(light.darkest).toBeGreaterThan(200);
});
