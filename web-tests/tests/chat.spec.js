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
  // Plain and Markdown replies share one reply bubble.
  const backgrounds = await assistantRows(page).evaluateAll(nodes => nodes.map(node => getComputedStyle(node).backgroundColor));
  expect(new Set(backgrounds).size).toBe(1);
  expect(backgrounds[0]).not.toBe('rgba(0, 0, 0, 0)');
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
  await expect(page.locator('#conversation-list [data-conversation-id]')).toHaveCount(100);
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
    // Simulate the conversation falling outside the server's newest 100.
    await page.route('**/v1/conversations', async route => {
      if (route.request().method() !== 'GET') return route.continue();
      const response = await route.fetch();
      const body = await response.json();
      body.conversations = body.conversations.filter(item => item.id !== id);
      await route.fulfill({ response, json: body });
    });

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
  // The dots sit in the reply bubble.
  expect(await assistantRows(page).last().evaluate(node => getComputedStyle(node).backgroundColor)).not.toBe('rgba(0, 0, 0, 0)');
  await page.getByRole('button', { name: '停止', exact: true }).click();
  await expect.poll(async () => (await run(page, tool.id)).status).toBe('uncertain');
  await expect(page.locator('#run-progress')).toBeHidden();
  await expect(page.locator('#chat-log .typing')).toHaveCount(0);

  await newConversation(page);
  const streaming = await send(page, '停止テスト');
  await expect(page.locator('#run-progress')).toHaveText('応答中');
  await expect(page.locator('#chat-log .typing')).toHaveCount(0);
  // Stop shows at once, before the server acknowledges it.
  const { promise: held, resolve: release } = Promise.withResolvers();
  await page.route(`**/v1/runs/${streaming.id}/cancel`, async route => {
    await held;
    await route.continue();
  });
  try {
    await page.getByRole('button', { name: '停止', exact: true }).click();
    await expect(page.locator('#run-progress')).toHaveText('停止しています');
  } finally {
    release();
  }
  await expect.poll(async () => (await run(page, streaming.id)).status).toBe('cancelled');
  await expect(page.locator('#run-progress')).toBeHidden();
});

// Sends a message whose tool waits for Stop, ends the first event stream
// right after tool.started and checks the label after the reconnect.
async function labelSurvivesReconnect(page, message, label, stopped) {
  await connect(page);
  await newConversation(page);
  const cursors = [];
  let delivered;
  await page.route(/\/v1\/runs\/[^/]+\/events$/, async route => {
    cursors.push(route.request().headers()['last-event-id'] ?? '0');
    if (cursors.length > 1) return route.continue();
    // Deliver the stored events through tool.started, then end this stream.
    const controller = new AbortController();
    const response = await fetch(route.request().url(), { headers: auth, signal: controller.signal });
    const reader = response.body.getReader();
    const decoder = new TextDecoder();
    const isTool = event => /^event:\s*tool\.started$/m.test(event);
    let buffer = '';
    let events = [];
    while (!events.some(isTool)) {
      const { value, done } = await reader.read();
      if (done) throw new Error('the event stream ended before tool.started');
      buffer += decoder.decode(value, { stream: true });
      events = buffer.split(/\r?\n\r?\n/).slice(0, -1);
    }
    controller.abort();
    events = events.slice(0, events.findIndex(isTool) + 1);
    delivered = /^id:\s*(\d+)$/m.exec(events.at(-1))[1];
    await route.fulfill({ status: 200, contentType: 'text/event-stream', body: `${events.join('\n\n')}\n\n` });
  });
  const sent = await send(page, message);
  try {
    // The reconnect resumes after tool.started, so no new event restates it.
    await expect.poll(() => cursors.length).toBe(2);
    expect(cursors[1]).toBe(delivered);
    await expect(page.locator('#run-progress')).toHaveText(label);
  } finally {
    await page.request.post(`/v1/runs/${sent.id}/cancel`, { headers: auth });
    await expect.poll(async () => (await run(page, sent.id)).status).toBe(stopped);
  }
}

test('the tool label survives an event stream reconnect', async ({ page }) => {
  await labelSurvivesReconnect(page, '復旧テスト', 'ツールを実行中', 'uncertain');
});

test('a web search shows its label and the reply links its source', async ({ page }) => {
  await connect(page);
  await newConversation(page);
  const sent = await send(page, '検索テスト');
  const pill = page.locator('#run-progress');
  await expect(pill).toHaveText('Web を検索中');
  await expect(pill).toHaveText('考えています');
  await expect.poll(async () => (await run(page, sent.id)).status).toBe('completed');
  await expect(pill).toBeHidden();
  const source = assistantRows(page).last().getByRole('link', { name: '公式ニュース', exact: true });
  await expect(source).toHaveAttribute('href', 'https://example.com/news');
});

test('a reload during a web search replays the same label', async ({ page }) => {
  await connect(page);
  await newConversation(page);
  const sent = await send(page, '検索停止テスト');
  const pill = page.locator('#run-progress');
  try {
    await expect(pill).toHaveText('Web を検索中');
    await page.reload();
    await expectSignedIn(page);
    // The reloaded page reads the stream from its first event.
    await expect(pill).toHaveText('Web を検索中');
  } finally {
    await page.request.post(`/v1/runs/${sent.id}/cancel`, { headers: auth });
    await expect.poll(async () => (await run(page, sent.id)).status).toBe('cancelled');
  }
});

test('the web search label survives a reconnect and Stop leaves nothing to review', async ({ page }) => {
  await labelSurvivesReconnect(page, '検索停止テスト', 'Web を検索中', 'cancelled');
  // A hosted search has no tool record, so the conversation continues.
  await expect(page.getByRole('button', { name: '実行記録を確認', exact: true })).toHaveCount(0);
  await composer(page).fill('続きをどうぞ');
  await expect(page.getByRole('button', { name: '送信', exact: true })).toBeEnabled();
});

test.describe('in UTC', () => {
  test.use({ timezoneId: 'UTC' });

  test('time separators label turns and a jump button returns to the latest reply', async ({ page }) => {
    // Fixed send times and clock give the same labels on any test date.
    const sentAt = ['2025-12-31T23:50:00Z', '2026-01-01T00:10:00Z', '2026-01-01T00:20:00Z'];
    await page.clock.setFixedTime(new Date('2026-01-02T09:00:00Z'));
    await page.setViewportSize({ width: 1280, height: 520 });
    await connect(page);
    await newConversation(page);
    for (let index = 0; index < sentAt.length; index += 1) {
      const sent = await send(page, 'Markdownテスト');
      await expect.poll(async () => (await run(page, sent.id)).status).toBe('completed');
    }
    await page.route(/\/v1\/conversations\/[^/]+\/snapshot$/, async route => {
      const response = await route.fetch();
      const snapshot = await response.json();
      // Snapshot runs are newest first.
      snapshot.runs.forEach((item, index) => { item.created_at = sentAt.at(-1 - index); });
      await route.fulfill({ response, json: snapshot });
    });
    await page.reload();
    await expectSignedIn(page);
    // An earlier year is named; a new day opens a group even within 30 minutes.
    await expect(page.locator('#chat-log .time-separator')).toHaveText(['2025年12月31日 23:50', '1月1日 00:10']);
    const area = page.locator('#message-area');
    const jump = page.getByRole('button', { name: '最新のメッセージへ移動', exact: true });
    await expect(jump).toBeHidden();
    await area.evaluate(node => { node.scrollTop = 0; });
    await expect(jump).toBeVisible();
    await jump.click();
    await expect.poll(() => area.evaluate(node => node.scrollHeight - node.scrollTop - node.clientHeight)).toBeLessThan(100);
    await expect(jump).toBeHidden();
  });
});

test('streamed text below the reader offers the jump button without moving them', async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 520 });
  await connect(page);
  await newConversation(page);
  for (let index = 0; index < 2; index += 1) {
    const sent = await send(page, 'Markdownテスト');
    await expect.poll(async () => (await run(page, sent.id)).status).toBe('completed');
  }
  const { promise: holdEvents, resolve: releaseEvents } = Promise.withResolvers();
  await page.route('**/v1/runs/*/events', async route => {
    await holdEvents;
    await route.continue();
  });
  const sent = await send(page, '停止テスト');
  const reply = page.locator(`#chat-log [data-role="assistant"][data-run-id="${sent.id}"]`);
  const area = page.locator('#message-area');
  const jump = page.getByRole('button', { name: '最新のメッセージへ移動', exact: true });
  try {
    await expect(reply.locator('.typing')).toBeVisible();
    await expect(jump).toBeHidden();
    // After pending scroll events, a shorter viewport, such as an on-screen
    // keyboard, leaves the reader 150 px above the latest text without one.
    await area.evaluate(() => new Promise(resolve => requestAnimationFrame(resolve)));
    await page.setViewportSize({ width: 1280, height: 370 });
    const top = await area.evaluate(node => node.scrollTop);
    releaseEvents();
    await expect(reply).toHaveText('停止操作を待っています。');
    await expect(jump).toBeVisible();
    expect(await area.evaluate(node => node.scrollTop)).toBe(top);
  } finally {
    releaseEvents();
    await page.request.post(`/v1/runs/${sent.id}/cancel`, { headers: auth });
    await expect.poll(async () => (await run(page, sent.id)).status).toBe('cancelled');
  }
});

test('the chat follows the system dark theme', async ({ page }) => {
  const shade = (locator, pseudo) => locator.evaluate((node, pseudo) => {
    const [red, green, blue, alpha = 1] = getComputedStyle(node, pseudo).backgroundColor.match(/[\d.]+/g).map(Number);
    return { brightest: Math.max(red, green, blue), darkest: Math.min(red, green, blue), alpha };
  }, pseudo);
  const canvas = () => shade(page.locator('html'));
  await page.emulateMedia({ colorScheme: 'dark' });
  await page.goto('/');
  await expect(signInDialog(page)).toBeVisible();
  // Dialogs dim the dark page with a dark backdrop.
  const backdrop = await shade(signInDialog(page), '::backdrop');
  expect(backdrop.brightest).toBeLessThan(30);
  expect(backdrop.alpha).toBeGreaterThan(0.5);
  await connect(page, false);
  const dark = await canvas();
  expect(dark.alpha).toBe(1);
  expect(dark.brightest).toBeLessThan(60);
  await page.emulateMedia({ colorScheme: 'light' });
  const light = await canvas();
  expect(light.alpha).toBe(1);
  expect(light.darkest).toBeGreaterThan(200);
});

test('text and controls keep WCAG contrast in light and dark themes', async ({ page }) => {
  await connect(page);
  await newConversation(page);
  // A reply with a quote, quoted code, a link and linked code, from the real renderer.
  await page.evaluate(async () => {
    const { renderMarkdown } = await import('/markdown.js');
    const reply = document.createElement('div');
    reply.className = 'message assistant markdown';
    reply.dataset.contrastSample = '';
    reply.append(renderMarkdown('> 引用と `引用内のコード`\n\n[リンク](https://example.com) と [`リンクのコード`](https://example.com)'));
    document.getElementById('chat-log').append(reply);
  });
  // After a key press, programmatic focus shows the keyboard focus ring.
  await page.keyboard.press('Shift');
  const failures = () => page.evaluate(() => {
    const $ = selector => document.querySelector(selector);
    const channels = color => {
      const srgb = /^color\(srgb ([\d.]+) ([\d.]+) ([\d.]+)(?: \/ ([\d.]+))?\)$/.exec(color);
      if (srgb) return [...srgb.slice(1, 4).map(value => Number(value) * 255), Number(srgb[4] ?? 1)];
      const [red, green, blue, alpha = 1] = color.match(/[\d.]+/g).map(Number);
      return [red, green, blue, alpha];
    };
    const luminance = color => {
      const [red, green, blue, alpha] = channels(color);
      if (alpha !== 1) throw new Error(`translucent colour ${color}`);
      const [r, g, b] = [red, green, blue].map(value => {
        const channel = value / 255;
        return channel <= 0.04045 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4;
      });
      return 0.2126 * r + 0.7152 * g + 0.0722 * b;
    };
    const ratio = (first, second) => {
      const [lighter, darker] = [luminance(first), luminance(second)].sort((a, b) => b - a);
      return (lighter + 0.05) / (darker + 0.05);
    };
    // Text and edges sit on the first opaque background at or above them.
    const backdrop = node => {
      for (let current = node; current; current = current.parentElement) {
        const color = getComputedStyle(current).backgroundColor;
        if (channels(color)[3] > 0) return color;
      }
      throw new Error('no opaque background');
    };
    const sample = $('[data-contrast-sample]');
    const text = (name, node, pseudo = null) => [name, getComputedStyle(node, pseudo).color, backdrop(node), 4.5];
    $('#message').focus();
    const composerEdge = getComputedStyle($('.composer')).borderTopColor;
    $('#new-chat').focus();
    if (!$('#new-chat').matches(':focus-visible')) throw new Error('the focus ring is not shown');
    const ring = getComputedStyle($('#new-chat')).outlineColor;
    const fieldEdge = getComputedStyle($('#server-token')).borderTopColor;
    const surfaces = [['canvas', document.documentElement], ['sidebar', $('.sidebar')], ['surface', $('.composer')], ['reply', sample]];
    const pairs = [
      text('composer placeholder', $('#message'), '::placeholder'),
      text('review note placeholder', $('#review-note'), '::placeholder'),
      text('sidebar status', $('#connection-state')),
      text('sidebar storage link', $('#storage-info')),
      text('composer caption', $('.composer-caption span')),
      text('quote', sample.querySelector('blockquote p')),
      text('quoted code', sample.querySelector('blockquote code')),
      text('link', sample.querySelector('a')),
      text('linked code', sample.querySelector('a code')),
      // Focus indicators and field edges need 3:1 against what they touch.
      ...surfaces.map(([where, node]) => [`focus ring on ${where}`, ring, backdrop(node), 3]),
      ['focused composer edge outside', composerEdge, backdrop($('.composer').parentElement), 3],
      ['focused composer edge inside', composerEdge, backdrop($('.composer')), 3],
      ['dialog field edge outside', fieldEdge, backdrop($('#server-token').parentElement), 3],
      ['dialog field edge inside', fieldEdge, backdrop($('#server-token')), 3],
    ];
    return pairs.filter(([, foreground, background, minimum]) => ratio(foreground, background) < minimum)
      .map(([name, foreground, background]) => `${name}: ${ratio(foreground, background).toFixed(2)} for ${foreground} on ${background}`);
  });
  for (const colorScheme of ['light', 'dark']) {
    await page.emulateMedia({ colorScheme });
    expect.soft(await failures(), colorScheme).toEqual([]);
  }
});

const menuButton = (page, id) => page.locator(`#conversation-list [data-menu-for="${id}"]`);
const conversationMenu = page => page.locator('#conversation-menu');
const renameDialog = page => page.getByRole('dialog', { name: '会話の名前を変更', exact: true });
const deleteDialog = page => page.getByRole('dialog', { name: '会話を削除しますか？', exact: true });
const firstListed = page => page.locator('#conversation-list [data-conversation-id]').first();

async function createTitled(page, title) {
  const response = await page.request.post('/v1/conversations', { headers: auth, data: { title } });
  expect(response.status()).toBe(201);
  return (await response.json()).id;
}

async function openMenu(page, id, item) {
  await menuButton(page, id).click();
  await expect(conversationMenu(page)).toBeVisible();
  await conversationMenu(page).getByRole('button', { name: item, exact: true }).click();
}

test.describe('in Tokyo time', () => {
  test.use({ timezoneId: 'Asia/Tokyo' });

  test('the sidebar groups conversations by local date', async ({ page }) => {
    await page.clock.setFixedTime(new Date('2026-11-18T12:00:00+09:00'));
    const groups = [
      ['今日', '2026-11-18T00:00:00+09:00'],
      ['昨日', '2026-11-17T23:59:59+09:00'],
      ['過去 7 日間', '2026-11-11T00:00:00+09:00'],
      ['11月', '2026-11-10T23:59:59+09:00'],
      ['10月', '2026-10-31T12:00:00+09:00'],
      ['2025年12月', '2025-12-31T12:00:00+09:00'],
    ];
    const rows = [];
    for (const [label, at] of groups) {
      // Creation predates every group, so only the activity can place a row.
      rows.push({ id: await createTitled(page, `${label}の会話`), title: `${label}の会話`, created_at: '2024-01-01T12:00:00+09:00', last_activity_at: at });
    }
    await page.route('**/v1/conversations', route => route.request().method() === 'GET'
      ? route.fulfill({ json: { conversations: rows } })
      : route.continue());
    await connect(page);
    await expect(page.locator('#conversation-list h2')).toHaveText(groups.map(([label]) => label));
    for (const [label] of groups) {
      const group = page.getByRole('list', { name: label, exact: true });
      await expect(group.getByRole('button', { name: `${label}の会話`, exact: true })).toBeVisible();
    }
  });
});

test('sending moves a conversation to the top of the list', async ({ page }) => {
  await connect(page);
  const older = await newConversation(page);
  const first = await send(page, '並び順テスト');
  await expect.poll(async () => (await run(page, first.id)).status).toBe('completed');
  const newer = await newConversation(page);
  await expect(firstListed(page)).toHaveAttribute('data-conversation-id', newer);
  await conversationButton(page, older).click();
  await expect(composer(page)).toBeEnabled();
  await send(page, 'もう一度');
  await expect(firstListed(page)).toHaveAttribute('data-conversation-id', older);
  await page.reload();
  await expectSignedIn(page);
  await expect(firstListed(page)).toHaveAttribute('data-conversation-id', older);
});

test('history that confirms a delayed send moves the conversation to the top', async ({ page }) => {
  const id = await createTitled(page, '名前のある会話');
  await connect(page);
  const other = await newConversation(page);
  await conversationButton(page, id).click();
  await expect(composer(page)).toBeEnabled();
  // A named conversation's history can read the conversation before the send
  // commits and its runs after, so only the accepted run shows the activity.
  const { last_activity_at: before } = (await conversation(page, id)).conversation;
  await page.route(`**/v1/conversations/${id}/snapshot`, async route => {
    const response = await route.fetch();
    const body = await response.json();
    body.conversation.last_activity_at = before;
    await route.fulfill({ response, json: body });
  });
  const { promise: held, resolve: release } = Promise.withResolvers();
  const { promise: accepted, resolve: reportAccepted } = Promise.withResolvers();
  await page.route(`**/v1/conversations/${id}/runs`, async route => {
    const response = await route.fetch();
    reportAccepted();
    await held;
    await route.fulfill({ response });
  });
  try {
    await composer(page).fill('履歴で受付を確認');
    await page.getByRole('button', { name: '送信', exact: true }).click();
    await accepted;
    await expect(firstListed(page)).toHaveAttribute('data-conversation-id', other);
    await conversationButton(page, other).click();
    await conversationButton(page, id).click();
    await expect(assistantRows(page)).toHaveText('こんにちは。「履歴で受付を確認」を受け取りました。');
    await expect(firstListed(page)).toHaveAttribute('data-conversation-id', id);
  } finally {
    release();
  }
});

test('a conversation can be renamed from its menu by keyboard', async ({ page }) => {
  await connect(page);
  const id = await newConversation(page);
  const more = menuButton(page, id);
  await expect(more).toHaveAccessibleName('「新しい会話」の操作');
  await more.focus();
  await page.keyboard.press('Enter');
  const rename = conversationMenu(page).getByRole('button', { name: '名前を変更', exact: true });
  await expect(rename).toBeFocused();
  await page.keyboard.press('Escape');
  await expect(conversationMenu(page)).toBeHidden();
  await expect(more).toBeFocused();

  await page.keyboard.press('Enter');
  await expect(rename).toBeFocused();
  await page.keyboard.press('Enter');
  const dialog = renameDialog(page);
  const input = dialog.getByLabel('名前', { exact: true });
  await expect(input).toHaveValue('新しい会話');
  await expect(input).toBeFocused();
  await page.keyboard.press('Escape');
  await expect(dialog).toBeHidden();
  await expect(more).toBeFocused();

  await openMenu(page, id, '名前を変更');
  await input.fill('旅の計画');
  await page.keyboard.press('Enter');
  await expect(dialog).toBeHidden();
  await expect(conversationButton(page, id)).toHaveText('旅の計画');
  await expect(conversationTitle(page)).toHaveText('旅の計画');
  await expect(more).toBeFocused();
  await page.reload();
  await expectSignedIn(page);
  await expect(conversationButton(page, id)).toHaveText('旅の計画');
  await conversationButton(page, id).click();
  await expect(composer(page)).toBeEnabled();
  const sent = await send(page, '自動の名前にならないメッセージ');
  expect(sent.conversation_title).toBe('旅の計画');
  await expect(conversationTitle(page)).toHaveText('旅の計画');
});

test('renaming checks the name and skips an unchanged one', async ({ page }) => {
  await connect(page);
  const id = await newConversation(page);
  let patches = 0;
  page.on('request', request => { if (request.method() === 'PATCH') patches += 1; });
  await openMenu(page, id, '名前を変更');
  const dialog = renameDialog(page);
  await dialog.getByRole('button', { name: '保存', exact: true }).click();
  await expect(dialog).toBeHidden();
  await openMenu(page, id, '名前を変更');
  await dialog.getByLabel('名前', { exact: true }).fill('   ');
  await dialog.getByRole('button', { name: '保存', exact: true }).click();
  await expect(dialog.getByRole('alert')).toHaveText('名前を入力してください。');
  await dialog.getByRole('button', { name: 'キャンセル', exact: true }).click();
  await expect(dialog).toBeHidden();
  expect(patches).toBe(0);
  await expect(conversationButton(page, id)).toHaveText('新しい会話');
});

test('saving an untouched name keeps a title that arrived while renaming', async ({ page }) => {
  await connect(page);
  const id = await newConversation(page);
  const path = `/v1/conversations/${id}/runs`;
  const { promise: holdSend, resolve: releaseSend } = Promise.withResolvers();
  await page.route(`**${path}`, async route => {
    await holdSend;
    await route.continue();
  });
  let patches = 0;
  page.on('request', request => { if (request.method() === 'PATCH') patches += 1; });
  try {
    const sending = page.waitForRequest(request => request.url().endsWith(path));
    await composer(page).fill('自動で付く名前');
    await page.getByRole('button', { name: '送信', exact: true }).click();
    await sending;
    await openMenu(page, id, '名前を変更');
    const dialog = renameDialog(page);
    await expect(dialog.getByLabel('名前', { exact: true })).toHaveValue('新しい会話');
    releaseSend();
    await expect(conversationButton(page, id)).toHaveText('自動で付く名前');
    await dialog.getByRole('button', { name: '保存', exact: true }).click();
    await expect(dialog).toBeHidden();
    await expect(conversationButton(page, id)).toHaveText('自動で付く名前');
    expect(patches).toBe(0);
    expect((await conversation(page, id)).conversation.title).toBe('自動で付く名前');
  } finally {
    releaseSend();
  }
});

test('deleting a conversation asks first and forgets it in this tab', async ({ page }) => {
  await connect(page);
  const keep = await newConversation(page);
  const id = await newConversation(page);
  await composer(page).fill('消える下書き');
  await openMenu(page, id, '削除');
  const dialog = deleteDialog(page);
  await expect(dialog).toContainText('「新しい会話」');
  await expect(dialog).toContainText('このタブの下書きと送信待ちのメッセージも削除されます。');
  await expect(dialog.getByRole('button', { name: 'キャンセル', exact: true })).toBeFocused();
  await page.keyboard.press('Escape');
  await expect(dialog).toBeHidden();
  await expect(conversationButton(page, id)).toBeVisible();

  await openMenu(page, id, '削除');
  await dialog.getByRole('button', { name: '削除', exact: true }).click();
  await expect(dialog).toBeHidden();
  await expect(conversationButton(page, id)).toHaveCount(0);
  await expect(page.locator('#empty-state h2')).toHaveText('何から始めますか？');
  await expect(conversationTitle(page)).toHaveText('会話');
  await expect(conversationButton(page, keep)).toBeFocused();
  const stored = await page.evaluate(() => sessionStorage.getItem('by-human-chat-v1'));
  expect(stored).not.toContain(id);
  expect(stored).not.toContain('消える下書き');
  expect((await page.request.get(`/v1/conversations/${id}`, { headers: auth })).status()).toBe(404);
});

test('a busy conversation cannot be deleted', async ({ page }) => {
  await connect(page);
  const other = await newConversation(page);
  const background = await page.request.post(`/v1/conversations/${other}/runs`, {
    headers: auth,
    data: { message: '停止テスト', idempotency_key: `delete-busy-other-${Date.now()}` },
  });
  expect(background.status()).toBe(202);
  const id = await newConversation(page);
  const sent = await send(page, '停止テスト');
  try {
    await openMenu(page, id, '削除');
    const dialog = deleteDialog(page);
    const busy = '実行中の処理か、確認が必要な実行結果があるため削除できません。';
    await expect(dialog.getByRole('alert')).toHaveText(busy);
    await expect(dialog.getByRole('button', { name: '削除', exact: true })).toBeDisabled();
    await dialog.getByRole('button', { name: 'キャンセル', exact: true }).click();

    await openMenu(page, other, '削除');
    await expect(dialog.getByRole('button', { name: '削除', exact: true })).toBeEnabled();
    await dialog.getByRole('button', { name: '削除', exact: true }).click();
    await expect(dialog.getByRole('alert')).toHaveText(busy);
    await dialog.getByRole('button', { name: 'キャンセル', exact: true }).click();
    await expect(conversationButton(page, other)).toBeVisible();
  } finally {
    const { id: otherRun } = await background.json();
    for (const runId of [sent.id, otherRun]) {
      await page.request.post(`/v1/runs/${runId}/cancel`, { headers: auth });
      await expect.poll(async () => (await run(page, runId)).status).toBe('cancelled');
    }
  }
});

test('the delete dialog follows work that ends or starts while it is open', async ({ page }) => {
  await connect(page);
  const id = await newConversation(page);
  const first = await send(page, '停止テスト');
  await expect(assistantRows(page)).toHaveText('停止操作を待っています。');
  await openMenu(page, id, '削除');
  const dialog = deleteDialog(page);
  const confirm = dialog.getByRole('button', { name: '削除', exact: true });
  const busy = '実行中の処理か、確認が必要な実行結果があるため削除できません。';
  await expect(confirm).toBeDisabled();
  let second;
  try {
    await page.request.post(`/v1/runs/${first.id}/cancel`, { headers: auth });
    await expect(confirm).toBeEnabled();
    await expect(dialog.getByRole('alert')).toBeEmpty();
    // Another client starts work before this tab asks to delete.
    const started = await page.request.post(`/v1/conversations/${id}/runs`, {
      headers: auth,
      data: { message: '停止テスト', idempotency_key: `delete-dialog-${Date.now()}` },
    });
    expect(started.status()).toBe(202);
    second = (await started.json()).id;
    await confirm.click();
    await expect(dialog.getByRole('alert')).toHaveText(busy);
    await expect(confirm).toBeDisabled();
    // The refreshed history follows that work, so its end answers the refusal.
    await page.request.post(`/v1/runs/${second}/cancel`, { headers: auth });
    await expect(confirm).toBeEnabled();
    await expect(dialog.getByRole('alert')).toBeEmpty();
  } finally {
    for (const runId of [first.id, second]) {
      if (!runId) continue;
      await page.request.post(`/v1/runs/${runId}/cancel`, { headers: auth });
      await expect.poll(async () => (await run(page, runId)).status).toBe('cancelled');
    }
  }
});

test('a conversation deleted elsewhere disappears when opened', async ({ page }) => {
  await connect(page);
  const gone = await newConversation(page);
  await newConversation(page);
  expect((await page.request.delete(`/v1/conversations/${gone}`, { headers: auth })).status()).toBe(204);
  await conversationButton(page, gone).click();
  await expect(conversationButton(page, gone)).toHaveCount(0);
  await expect(page.locator('#notice')).toHaveText('この会話は削除されています。');
});

test('deleting a conversation already deleted elsewhere says so and focuses its neighbour', async ({ page }) => {
  await connect(page);
  const keep = await newConversation(page);
  const gone = await newConversation(page);
  expect((await page.request.delete(`/v1/conversations/${gone}`, { headers: auth })).status()).toBe(204);
  await openMenu(page, gone, '削除');
  const dialog = deleteDialog(page);
  await dialog.getByRole('button', { name: '削除', exact: true }).click();
  await expect(dialog).toBeHidden();
  await expect(conversationButton(page, gone)).toHaveCount(0);
  await expect(page.locator('#notice')).toHaveText('この会話は削除されています。');
  await expect(conversationButton(page, keep)).toBeFocused();
});

test('a message sent to a conversation deleted elsewhere can be returned', async ({ page }) => {
  await connect(page);
  const id = await newConversation(page);
  expect((await page.request.delete(`/v1/conversations/${id}`, { headers: auth })).status()).toBe(204);
  await composer(page).fill('届かないメッセージ');
  await page.getByRole('button', { name: '送信', exact: true }).click();
  await expect(page.locator('#notice')).toHaveText('この会話は削除されています。');
  await page.getByRole('button', { name: '入力欄に戻す', exact: true }).click();
  await expect(composer(page)).toHaveValue('届かないメッセージ');
});

test('a reconnecting stream stops and forgets a conversation deleted elsewhere', async ({ page }) => {
  await connect(page);
  const id = await newConversation(page);
  let streams = 0;
  const { promise: reconnecting, resolve: reportReconnect } = Promise.withResolvers();
  const { promise: holdReconnect, resolve: releaseReconnect } = Promise.withResolvers();
  await page.route(/\/v1\/runs\/[^/]+\/events$/, async route => {
    streams += 1;
    if (streams > 1) {
      reportReconnect();
      await holdReconnect;
      return route.continue();
    }
    // The first stream ends after one event, so the client reconnects.
    const response = await route.fetch();
    const [first] = (await response.text()).split(/\r?\n\r?\n/).filter(event => /^id:/m.test(event));
    await route.fulfill({ status: 200, contentType: 'text/event-stream', body: `${first}\n\n` });
  });
  try {
    const sent = await send(page, '別のタブで消される会話');
    await reconnecting;
    await expect.poll(async () => (await run(page, sent.id)).status).toBe('completed');
    expect((await page.request.delete(`/v1/conversations/${id}`, { headers: auth })).status()).toBe(204);
    releaseReconnect();
    await expect(conversationButton(page, id)).toHaveCount(0);
    await expect(page.locator('#notice')).toHaveText('この会話は削除されています。');
    await expect(page.locator('#run-progress')).toBeHidden();
    // A retry would follow within a second of the failed reconnect.
    const retried = await page.waitForRequest(/\/v1\/runs\//, { timeout: 1_500 }).then(() => true, () => false);
    expect(retried).toBe(false);
    expect(streams).toBe(2);
  } finally {
    releaseReconnect();
  }
});

test('the list reloads when the tab becomes visible again', async ({ page }) => {
  await connect(page);
  const created = await createTitled(page, '別のタブで作成');
  await expect(conversationButton(page, created)).toHaveCount(0);
  await page.evaluate(() => document.dispatchEvent(new Event('visibilitychange')));
  await expect(conversationButton(page, created)).toHaveText('別のタブで作成');
});

test('a list reload during creation keeps one row for the new conversation', async ({ page }) => {
  await connect(page);
  const { promise: held, resolve: release } = Promise.withResolvers();
  const { promise: committed, resolve: reportCommitted } = Promise.withResolvers();
  await page.route('**/v1/conversations', async route => {
    if (route.request().method() !== 'POST') return route.continue();
    const response = await route.fetch();
    reportCommitted((await response.json()).id);
    await held;
    await route.fulfill({ response });
  });
  try {
    await newChat(page).click();
    const id = await committed;
    await page.evaluate(() => document.dispatchEvent(new Event('visibilitychange')));
    await expect(conversationButton(page, id)).toHaveCount(1);
    release();
    await expect(conversationTitle(page)).toHaveText('新しい会話');
    await expect(composer(page)).toBeFocused();
    await expect(conversationButton(page, id)).toHaveCount(1);
    await expect(conversationButton(page, id)).toHaveAttribute('aria-current', 'page');
  } finally {
    release();
  }
});

test('a conversation deleted before its creation response arrives stays deleted', async ({ page }) => {
  await connect(page);
  const other = await newConversation(page);
  await newConversation(page);
  const { promise: held, resolve: release } = Promise.withResolvers();
  const { promise: committed, resolve: reportCommitted } = Promise.withResolvers();
  await page.route('**/v1/conversations', async route => {
    if (route.request().method() !== 'POST') return route.continue();
    const response = await route.fetch();
    reportCommitted((await response.json()).id);
    await held;
    await route.fulfill({ response });
  });
  try {
    await newChat(page).click();
    const id = await committed;
    await conversationButton(page, other).click();
    await expect(composer(page)).toBeEnabled();
    await page.evaluate(() => document.dispatchEvent(new Event('visibilitychange')));
    await expect(conversationButton(page, id)).toHaveCount(1);
    await openMenu(page, id, '削除');
    await deleteDialog(page).getByRole('button', { name: '削除', exact: true }).click();
    await expect(deleteDialog(page)).toBeHidden();
    await expect(conversationButton(page, id)).toHaveCount(0);
    release();
    // New Chat turns on again once the creation response has been handled.
    await expect(newChat(page)).toBeEnabled();
    await expect(conversationButton(page, id)).toHaveCount(0);
    await expect(conversationButton(page, other)).toHaveAttribute('aria-current', 'page');
  } finally {
    release();
  }
});

test('a conversation created before an accepted send is listed below it', async ({ page }) => {
  await connect(page);
  const id = await newConversation(page);
  const { promise: held, resolve: release } = Promise.withResolvers();
  const { promise: committed, resolve: reportCommitted } = Promise.withResolvers();
  await page.route('**/v1/conversations', async route => {
    if (route.request().method() !== 'POST') return route.continue();
    const response = await route.fetch();
    reportCommitted((await response.json()).id);
    await held;
    await route.fulfill({ response });
  });
  try {
    await newChat(page).click();
    const created = await committed;
    await send(page, '作成中に送信');
    // The send's own response has moved its row, with its first-message title.
    await expect(conversationButton(page, id)).toHaveText('作成中に送信');
    release();
    await expect(conversationButton(page, created)).toHaveAttribute('aria-current', 'page');
    const rows = page.locator('#conversation-list [data-conversation-id]');
    await expect(rows.nth(0)).toHaveAttribute('data-conversation-id', id);
    await expect(rows.nth(1)).toHaveAttribute('data-conversation-id', created);
  } finally {
    release();
  }
});

const searchBox = page => page.getByRole('searchbox', { name: '会話を検索', exact: true });

test('search finds conversations by title and message text', async ({ page }) => {
  await connect(page);
  const suffix = Date.now().toString(36);
  const titled = await createTitled(page, `検索タイトル${suffix}`);
  const id = await newConversation(page);
  const sent = await send(page, `本文の目印${suffix}`);
  await expect.poll(async () => (await run(page, sent.id)).status).toBe('completed');
  const listed = page.locator('#conversation-list [data-conversation-id]');
  await searchBox(page).fill(`目印${suffix}`);
  await expect(listed).toHaveCount(1);
  await expect(conversationButton(page, id)).toBeVisible();
  // Both messages match; the reply is the newer one.
  await expect(page.locator('#conversation-list .conversation-snippet')).toHaveText(`こんにちは。「本文の目印${suffix}」を受け取りました。`);
  await searchBox(page).fill(`検索タイトル${suffix}`);
  await expect(listed).toHaveCount(1);
  await expect(conversationButton(page, titled)).toBeVisible();
  await expect(page.locator('#conversation-list .conversation-snippet')).toHaveCount(0);
  await conversationButton(page, titled).click();
  await expect(conversationTitle(page)).toHaveText(`検索タイトル${suffix}`);
  await expect(searchBox(page)).toHaveValue(`検索タイトル${suffix}`);
  await searchBox(page).fill(`該当なし${suffix}`);
  await expect(page.locator('#conversation-list')).toHaveText(`「該当なし${suffix}」に一致する会話はありません`);
  await searchBox(page).press('Escape');
  await expect(searchBox(page)).toHaveValue('');
  await expect(conversationButton(page, id)).toBeVisible();
  await expect(page.locator('#conversation-list h2').first()).toHaveText('今日');
});

test('search waits for Japanese input to finish', async ({ page }) => {
  await connect(page);
  const requests = [];
  page.on('request', request => { if (request.url().includes('/v1/conversations/search')) requests.push(request.url()); });
  await searchBox(page).focus();
  await searchBox(page).dispatchEvent('compositionstart');
  await searchBox(page).fill('にほんご');
  await page.waitForTimeout(600);
  expect(requests).toHaveLength(0);
  await searchBox(page).dispatchEvent('compositionend');
  await expect.poll(() => requests.length).toBe(1);
  await page.waitForTimeout(500);
  expect(requests).toHaveLength(1);
  expect(new URL(requests[0]).searchParams.get('q')).toBe('にほんご');
});

test('search snippets show reply text as plain text', async ({ page }) => {
  await connect(page);
  await newConversation(page);
  const marker = `検索XSS${Date.now().toString(36)}`;
  const sent = await send(page, `${marker}<img src=x onerror="window.injected=1">`);
  await expect.poll(async () => (await run(page, sent.id)).status).toBe('completed');
  await searchBox(page).fill(marker);
  await expect(page.locator('#conversation-list .conversation-snippet')).toContainText(`${marker}<img src=x onerror="window.injected=1">`);
  await expect(page.locator('#conversation-list img')).toHaveCount(0);
  expect(await page.evaluate(() => window.injected)).toBeUndefined();
});

test('a search snippet begins near the match so its two lines show it', async ({ page }) => {
  await connect(page);
  await newConversation(page);
  const marker = `見える目印${Date.now().toString(36)}`;
  const sent = await send(page, `${'長い前置きの文章です。'.repeat(6)}${marker}`);
  await expect.poll(async () => (await run(page, sent.id)).status).toBe('completed');
  await searchBox(page).fill(marker);
  const snippet = page.locator('#conversation-list .conversation-snippet');
  await expect(snippet).toContainText(marker);
  // The snippet is clamped to two lines; the match must start on the first.
  const line = await snippet.evaluate((node, text) => {
    const range = document.createRange();
    const start = node.textContent.indexOf(text);
    range.setStart(node.firstChild, start);
    range.setEnd(node.firstChild, start + 1);
    const top = range.getBoundingClientRect().top - node.getBoundingClientRect().top;
    return Math.floor(top / parseFloat(getComputedStyle(node).lineHeight));
  }, marker);
  expect(line).toBe(0);
});

test('the list says when older conversations exist', async ({ page }) => {
  await page.route('**/v1/conversations', async route => {
    if (route.request().method() !== 'GET') return route.continue();
    const response = await route.fetch();
    await route.fulfill({ response, json: { ...(await response.json()), has_more: true } });
  });
  await connect(page);
  await expect(page.locator('#conversation-list .sidebar-more')).toHaveText('これより古い会話は検索で探せます。');
});

const searchRequests = url => url.pathname === '/v1/conversations/search';

test('renaming or deleting during a search refreshes its results', async ({ page }) => {
  const suffix = Date.now().toString(36);
  const query = `書き換え${suffix}`;
  const renamed = await createTitled(page, `${query}の前`);
  const kept = await createTitled(page, `${query}を残す`);
  const removed = await createTitled(page, `${query}を削除`);
  await connect(page);
  // The next search answer is fetched at once and delivered when released.
  const releases = [];
  let gate = null;
  let held = 0;
  await page.route(searchRequests, async route => {
    const wait = gate;
    gate = null;
    if (!wait) return route.continue();
    const response = await route.fetch();
    held += 1;
    await wait;
    await route.fulfill({ response });
  });
  const holdNextSearch = () => {
    const { promise, resolve } = Promise.withResolvers();
    gate = promise;
    releases.push(resolve);
    return resolve;
  };
  const rows = page.locator('#conversation-list [data-conversation-id]');
  try {
    const releaseBeforeRename = holdNextSearch();
    await searchBox(page).fill(query);
    await expect.poll(() => held).toBe(1);
    await openMenu(page, renamed, '名前を変更');
    await renameDialog(page).getByLabel('名前', { exact: true }).fill(`改名後${suffix}`);
    await page.keyboard.press('Enter');
    await expect(renameDialog(page)).toBeHidden();
    releaseBeforeRename();
    // Neither the old title nor the new one, which no longer matches, stays.
    await expect(rows).toHaveCount(2);
    await expect(conversationButton(page, renamed)).toHaveCount(0);

    await searchBox(page).press('Escape');
    const releaseBeforeDelete = holdNextSearch();
    await searchBox(page).fill(query);
    await expect.poll(() => held).toBe(2);
    await openMenu(page, removed, '削除');
    await deleteDialog(page).getByRole('button', { name: '削除', exact: true }).click();
    await expect(deleteDialog(page)).toBeHidden();
    releaseBeforeDelete();
    // The deleted row does not come back.
    await expect(rows).toHaveCount(1);
    await expect(conversationButton(page, kept)).toBeVisible();
  } finally {
    for (const release of releases) release();
  }
});

test('renaming a search result by keyboard so it no longer matches moves focus to the search field', async ({ page }) => {
  const suffix = Date.now().toString(36);
  const id = await createTitled(page, `外れる名前${suffix}`);
  await connect(page);
  await searchBox(page).fill(`外れる名前${suffix}`);
  await expect(page.locator('#conversation-list [data-conversation-id]')).toHaveCount(1);
  await menuButton(page, id).focus();
  await page.keyboard.press('Enter');
  await expect(conversationMenu(page).getByRole('button', { name: '名前を変更', exact: true })).toBeFocused();
  await page.keyboard.press('Enter');
  const input = renameDialog(page).getByLabel('名前', { exact: true });
  await expect(input).toBeFocused();
  await input.fill(`別の名前${suffix}`);
  await page.keyboard.press('Enter');
  await expect(renameDialog(page)).toBeHidden();
  // The refreshed results drop the row whose menu button had focus.
  await expect(conversationButton(page, id)).toHaveCount(0);
  await expect(searchBox(page)).toBeFocused();
});

test('a changed query ignores the answer for the previous one', async ({ page }) => {
  const suffix = Date.now().toString(36);
  const before = await createTitled(page, `前の語${suffix}`);
  const after = await createTitled(page, `次の語${suffix}`);
  await connect(page);
  const releases = [];
  await page.route(searchRequests, async route => {
    const response = await route.fetch();
    const { promise, resolve } = Promise.withResolvers();
    releases.push(resolve);
    await promise;
    await route.fulfill({ response });
  });
  try {
    // The previous answer arrives while Japanese input is converting.
    await searchBox(page).fill(`前の語${suffix}`);
    await expect.poll(() => releases.length).toBe(1);
    await searchBox(page).dispatchEvent('compositionstart');
    await searchBox(page).fill(`次の語${suffix}`);
    releases[0]();
    await searchBox(page).dispatchEvent('compositionend');
    await expect.poll(() => releases.length).toBe(2);
    await expect(conversationButton(page, after)).toBeVisible();
    releases[1]();
    await expect(conversationButton(page, before)).toHaveCount(0);

    // The previous answer arrives during the pause after typing.
    await searchBox(page).fill(`前の語${suffix}`);
    await expect.poll(() => releases.length).toBe(3);
    await searchBox(page).fill(`次の語${suffix}`);
    releases[2]();
    await expect.poll(() => releases.length).toBe(4);
    await expect(conversationButton(page, after)).toBeVisible();
    await expect(conversationButton(page, before)).toHaveCount(0);
    releases[3]();
    await expect(conversationButton(page, after)).toBeVisible();
  } finally {
    for (const release of releases) release();
  }
});

test('creating a conversation ends a search that has not answered yet', async ({ page }) => {
  await connect(page);
  const { promise: hold, resolve: release } = Promise.withResolvers();
  let held = false;
  await page.route(searchRequests, async route => {
    held = true;
    const response = await route.fetch();
    await hold;
    await route.fulfill({ response });
  });
  try {
    await searchBox(page).fill(`該当なし${Date.now().toString(36)}`);
    await expect.poll(() => held).toBe(true);
    const created = await newConversation(page);
    await expect(searchBox(page)).toHaveValue('');
    release();
    await expect(conversationButton(page, created)).toHaveAttribute('aria-current', 'page');
    await expect(page.locator('#conversation-list .sidebar-empty')).toHaveCount(0);
  } finally {
    release();
  }
});

test('Escape during Japanese conversion in the search field keeps the drawer open', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await connect(page);
  await page.getByRole('button', { name: '会話一覧を開く', exact: true }).click();
  const list = page.getByRole('navigation', { name: '会話一覧', exact: true });
  await expect(list).toBeVisible();
  await searchBox(page).fill('にほんご');
  await searchBox(page).dispatchEvent('compositionstart');
  // Escape's own keyCode, so only isComposing marks it as part of conversion.
  await searchBox(page).dispatchEvent('keydown', { key: 'Escape', code: 'Escape', isComposing: true, keyCode: 27 });
  await expect(list).toBeVisible();
  await expect(searchBox(page)).toHaveValue('にほんご');
  await searchBox(page).dispatchEvent('compositionend');
  // Safari ends composition first, so the key that ends it arrives as 229.
  await searchBox(page).dispatchEvent('keydown', { key: 'Escape', code: 'Escape', isComposing: false, keyCode: 229 });
  await expect(list).toBeVisible();
  await expect(searchBox(page)).toHaveValue('にほんご');
});

test('a conversation found only by search joins the list when it gets a new message', async ({ page }) => {
  const suffix = Date.now().toString(36);
  const title = `一覧の外${suffix}`;
  const id = await createTitled(page, title);
  // The list leaves it out, as if it were older than the newest 100.
  await page.route('**/v1/conversations', async route => {
    if (route.request().method() !== 'GET') return route.continue();
    const response = await route.fetch();
    const body = await response.json();
    body.conversations = body.conversations.filter(item => item.id !== id);
    await route.fulfill({ response, json: body });
  });
  await connect(page);
  await expect(conversationButton(page, id)).toHaveCount(0);
  await searchBox(page).fill(title);
  await conversationButton(page, id).click();
  await expect(composer(page)).toBeEnabled();
  await send(page, '新しいメッセージ');
  await searchBox(page).press('Escape');
  await expect(firstListed(page)).toHaveAttribute('data-conversation-id', id);
});

test('a late search answer cannot bring back a conversation deleted elsewhere', async ({ page }) => {
  const suffix = Date.now().toString(36);
  const title = `遅い検索${suffix}`;
  const id = await createTitled(page, title);
  await connect(page);
  const { promise: hold, resolve: release } = Promise.withResolvers();
  const { promise: answered, resolve: markAnswered } = Promise.withResolvers();
  await page.route(url => searchRequests(url), async route => {
    const response = await route.fetch();
    markAnswered();
    await hold;
    await route.fulfill({ response });
  });
  try {
    await searchBox(page).fill(title);
    // The server found it before the delete; the page has not seen the answer yet.
    await answered;
    expect((await page.request.delete(`/v1/conversations/${id}`, { headers: auth })).status()).toBe(204);
    // Opening it from the list finds it gone, while the search answer is still on its way.
    await conversationButton(page, id).click();
    await expect(page.locator('#notice')).toHaveText('この会話は削除されています。');
    release();
    await expect(page.locator('#conversation-list')).toContainText(`「${title}」に一致する会話はありません`);
    await expect(conversationButton(page, id)).toHaveCount(0);
  } finally {
    release();
  }
});

test('a desktop window shorter than the layout still scrolls to the composer', async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 200 });
  await connect(page);
  await page.evaluate(() => window.scrollTo(0, document.documentElement.scrollHeight));
  await expect(page.getByRole('button', { name: '送信', exact: true })).toBeInViewport();
});

test.describe('on a phone', () => {
  test.use({ viewport: { width: 393, height: 852 }, hasTouch: true, isMobile: true });
  // iOS keeps the layout viewport and shrinks only the visual viewport.
  const emulateKeyboard = page => page.addInitScript(() => {
    const visual = new EventTarget();
    let height = null;
    for (const [name, read] of [['height', () => height ?? window.innerHeight], ['width', () => window.innerWidth], ['scale', () => 1], ['offsetTop', () => 0]]) {
      Object.defineProperty(visual, name, { get: read });
    }
    Object.defineProperty(window, 'visualViewport', { get: () => visual, configurable: true });
    window.showKeyboard = value => { height = value; visual.dispatchEvent(new Event('resize')); };
  });

  test('Return adds a line and the send button sends', async ({ page }) => {
    await connect(page);
    await page.locator('#open-sidebar').tap();
    // Waits for the new conversation, not the one opened at sign-in.
    await newConversation(page);
    let sent = 0;
    page.on('request', request => { if (/\/v1\/conversations\/[^/]+\/runs$/.test(request.url())) sent += 1; });
    await composer(page).fill('一行目');
    await composer(page).press('Enter');
    await composer(page).pressSequentially('二行目');
    await expect(composer(page)).toHaveValue('一行目\n二行目');
    expect(sent).toBe(0);
    await expect(page.locator('.composer-caption')).toBeHidden();
    const response = page.waitForResponse(response => /\/v1\/conversations\/[^/]+\/runs$/.test(response.url()));
    await page.getByRole('button', { name: '送信', exact: true }).tap();
    expect((await response).status()).toBe(202);
    await expect(userRows(page).last()).toHaveText('一行目\n二行目');
  });

  test('fields do not trigger iOS zoom and dialogs stay above the keyboard', async ({ page }) => {
    await page.goto('/');
    const dialog = signInDialog(page);
    await expect(dialog).toBeVisible();
    // iOS zooms into focused fields under 16px and keeps the page zoomed.
    for (const selector of ['#server-token', '#message', '#conversation-query', '#rename-input', '#review-note']) {
      expect(await page.locator(selector).evaluate(node => getComputedStyle(node).fontSize), selector).toBe('16px');
    }
    expect((await dialog.boundingBox()).y).toBeLessThan(40);
  });

  test('a tall dialog below the notch stays above the keyboard', async ({ page }) => {
    await emulateKeyboard(page);
    // A Dynamic Island iPhone's safe area pushes the dialog 59px down.
    const session = await page.context().newCDPSession(page);
    await session.send('Emulation.setSafeAreaInsetsOverride', { insets: { top: 59, bottom: 34, left: 0, right: 0 } });
    await page.goto('/');
    const dialog = signInDialog(page);
    await expect(dialog).toBeVisible();
    await page.evaluate(() => window.showKeyboard(300));
    const box = await dialog.boundingBox();
    expect(box.y).toBe(59);
    // It keeps a 12px gap above the keyboard.
    expect(box.y + box.height).toBeLessThanOrEqual(300 - 12);
  });

  test('the layout fits the visible area above the keyboard', async ({ page }) => {
    await emulateKeyboard(page);
    await connect(page);
    const shellHeight = () => page.locator('.app-shell').evaluate(node => node.getBoundingClientRect().height);
    await expect.poll(shellHeight).toBe(852);
    await page.evaluate(() => window.showKeyboard(500));
    await expect.poll(shellHeight).toBe(500);
    expect(await page.evaluate(() => window.innerHeight)).toBe(852);
    await expect(page.locator('html')).toHaveClass(/keyboard-open/);
    // A small phone can leave less than the 320px desktop minimum visible,
    // with a draft of several lines from Return.
    await expect(composer(page)).toBeEnabled();
    await composer(page).fill(Array.from({ length: 12 }, (_, line) => `行${line + 1}`).join('\n'));
    await page.evaluate(() => window.showKeyboard(250));
    const send = await page.getByRole('button', { name: '送信', exact: true }).boundingBox();
    expect(send.y + send.height).toBeLessThanOrEqual(250);
    await expect.poll(shellHeight).toBe(250);
    await page.evaluate(() => window.showKeyboard(null));
    await expect.poll(shellHeight).toBe(852);
    await expect(page.locator('html')).not.toHaveClass(/keyboard-open/);
  });

  test('the drawer fits the visible area above the search keyboard', async ({ page }) => {
    await emulateKeyboard(page);
    await connect(page);
    await page.locator('#open-sidebar').tap();
    await page.locator('#conversation-query').tap();
    await page.evaluate(() => window.showKeyboard(400));
    // The list then scrolls within the visible area, not behind the keyboard.
    await expect.poll(() => page.locator('#sidebar').evaluate(node => node.getBoundingClientRect().bottom)).toBe(400);
  });

  test('a search result opens from the drawer when the keyboard leaves 250px', async ({ page }) => {
    await emulateKeyboard(page);
    const suffix = Date.now().toString(36);
    const title = `鍵盤の上の検索${suffix}`;
    const id = await createTitled(page, title);
    // Sign-in opens the newest conversation, not the one searched for.
    await createTitled(page, `後から作った会話${suffix}`);
    await connect(page);
    await expect(conversationTitle(page)).toHaveText(`後から作った会話${suffix}`);
    await page.locator('#open-sidebar').tap();
    await searchBox(page).tap();
    // The drawer's own controls are taller than what is left visible.
    await page.evaluate(() => window.showKeyboard(250));
    await searchBox(page).fill(title);
    await expect(page.locator('#conversation-list [data-conversation-id]')).toHaveCount(1);
    // Chromium would accept a tap under the emulated keyboard, so the result
    // must first scroll into the 250px that stay visible.
    await conversationButton(page, id).scrollIntoViewIfNeeded();
    const result = await conversationButton(page, id).boundingBox();
    expect(result.y + result.height).toBeLessThanOrEqual(250);
    await conversationButton(page, id).tap();
    await expect(conversationTitle(page)).toHaveText(title);
  });
});
