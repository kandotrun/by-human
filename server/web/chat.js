// The browser is a client of the durable API. Closing a stream never stops a
// run, and an unacknowledged submission keeps its original identity.
const $ = (id) => document.getElementById(id);
const encoder = new TextEncoder();
const STORAGE_KEY = "by-human-chat-v1";
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const active = (run) => run && ["queued", "running"].includes(run.status);
const unresolved = (run) => run?.status === "uncertain" && !run.reviewed_at;
const matchesSubmission = (run, pending) => run.id === pending.runId ||
  (run.idempotency_key === pending.key && run.input_text === pending.text);
const SIGNED_OUT = "ログインの有効期限が切れたか、ログアウトしました。サーバートークンを入力してください。";

let signedIn = false;
let signingOut = false;
let connection = 0;
let selection = 0;
let snapshotVersion = 0;
let connectionAbort = new AbortController();
let selectionAbort = new AbortController();
let streamAbort = null;
let conversations = [];
let runs = [];
let olderRunsOmitted = false;
let selectedId = "";
let currentConversation = null;
let loading = false;
let creating = false;
let stopping = "";
let progress = "";
let composition = false;
let review = null;
let reviewRequest = 0;
let resolving = false;
let storageReadable = true;
let storageReady = true;
const drafts = new Map();
const outbox = new Map();
const sending = new Map();
const isSending = () => sending.has(selectedId);

class ApiError extends Error {
  constructor(status) {
    super(`HTTP ${status}`);
    this.status = status;
  }
}

function element(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

function showNotice(text = "", retry = null) {
  const notice = $("notice");
  notice.replaceChildren(text);
  if (text && retry) {
    const button = element("button", "text-button notice-action", "再試行");
    button.type = "button";
    button.addEventListener("click", retry, { once: true });
    notice.append(" ", button);
  }
  notice.hidden = !text;
}

function restoreSession() {
  try {
    const raw = sessionStorage.getItem(STORAGE_KEY);
    if (!raw) return;
    if (encoder.encode(raw).length > 2 * 1024 * 1024) throw new Error("storage");
    const saved = JSON.parse(raw);
    if (saved.version !== 1 || !Array.isArray(saved.drafts) || !Array.isArray(saved.outbox)) throw new Error("storage");
    for (const [id, text] of saved.drafts) {
      if (!UUID.test(id) || typeof text !== "string") throw new Error("storage");
      drafts.set(id, text);
    }
    for (const [id, item] of saved.outbox) {
      if (!UUID.test(id) || !item || !UUID.test(item.key) || typeof item.text !== "string" || !item.text.trim() || encoder.encode(item.text).length > 24576 || (item.runId && !UUID.test(item.runId))) throw new Error("storage");
      outbox.set(id, { key: item.key, text: item.text, createdAt: String(item.createdAt), runId: item.runId || null, rejected: Boolean(item.rejected) });
    }
    if (UUID.test(saved.selectedId)) selectedId = saved.selectedId;
  } catch {
    storageReadable = false;
    storageReady = false;
    showNotice("このタブの送信待ちを読み込めませんでした。重複送信を防ぐため送信を止めています。ブラウザの保存設定を確認してください。");
  }
}

function saveSession() {
  if (!storageReadable) return false;
  try {
    const value = JSON.stringify({ version: 1, selectedId, drafts: [...drafts], outbox: [...outbox] });
    if (encoder.encode(value).length > 2 * 1024 * 1024) throw new Error("storage");
    sessionStorage.setItem(STORAGE_KEY, value);
    storageReady = true;
    return true;
  } catch {
    storageReady = false;
    showNotice("送信待ちをこのタブに保存できません。重複送信を防ぐため、ブラウザの保存設定を確認してからもう一度操作してください。");
    return false;
  }
}

function scope() {
  return { connection, selection, id: selectedId };
}

function current(captured) {
  return signedIn && captured.connection === connection && captured.selection === selection && captured.id === selectedId;
}

async function readJson(response) {
  const reader = response.body?.getReader();
  if (!reader) throw new Error("response");
  const chunks = [];
  let length = 0;
  try {
    for (;;) {
      const { value, done } = await reader.read();
      if (done) break;
      length += value.byteLength;
      if (length > 8 * 1024 * 1024) throw new Error("response");
      chunks.push(value);
    }
  } catch (error) {
    await reader.cancel().catch(() => {});
    throw error;
  }
  const bytes = new Uint8Array(length);
  let offset = 0;
  for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.byteLength; }
  return JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes));
}

// The HttpOnly session cookie authenticates every same-origin request; the
// page never holds the server token after signing in.
async function api(path, options = {}) {
  const capturedConnection = connection;
  const headers = { ...options.headers };
  if (options.body !== undefined) headers["Content-Type"] = "application/json";
  const response = await fetch(`/v1${path}`, { ...options, headers, body: options.body === undefined ? undefined : JSON.stringify(options.body), cache: "no-store", redirect: "error", credentials: "same-origin", signal: options.signal || connectionAbort.signal });
  if (response.status === 401 && capturedConnection === connection) {
    disconnect(SIGNED_OUT);
    throw new ApiError(401);
  }
  if (!response.ok) throw new ApiError(response.status);
  return response.status === 204 ? null : readJson(response);
}

function friendlyError(error) {
  if (error instanceof ApiError) {
    if (error.status === 401) return SIGNED_OUT;
    if (error.status === 403) return "このアドレスからはログインできません。サーバーと同じアドレス（例: http://127.0.0.1:4310）で開いてください。";
    if (error.status === 409) return "会話の状態が変わりました。実行中の処理や確認待ちの内容を確認してください。";
    if (error.status === 404) return "会話または処理が見つかりません。会話を選び直してください。";
    if (error.status === 413) return "メッセージが長すぎます。短く分けて送信してください。";
    if (error.status === 400 || error.status === 422) return "入力内容を確認してください。";
  }
  return "サーバーに接続できませんでした。接続を確認して、もう一度操作してください。";
}

function setDrawer(open) {
  $("sidebar").classList.toggle("open", open);
  $("drawer-backdrop").hidden = !open;
  $("open-sidebar").setAttribute("aria-expanded", String(open));
  if (open) $("close-sidebar").focus();
}

function renderSidebar() {
  $("new-chat").disabled = !signedIn || creating;
  $("empty-new-chat").disabled = !signedIn || creating;
  $("disconnect").disabled = !signedIn || signingOut;
  $("connection-state").textContent = signedIn ? "ログイン中" : "未ログイン";
  $("connection-state").classList.toggle("connected", signedIn);
  const list = $("conversation-list");
  list.replaceChildren();
  for (const conversation of conversations) {
    const button = element("button", "", conversation.title);
    button.type = "button";
    button.dataset.conversationId = conversation.id;
    if (conversation.id === selectedId) button.setAttribute("aria-current", "page");
    button.addEventListener("click", () => selectConversation(conversation.id));
    list.append(button);
  }
  if (signedIn && !conversations.length) list.append(element("p", "sidebar-empty", "会話はまだありません"));
}

function runStatus(run) {
  if (run.status === "uncertain") return run.reviewed_at ? "結果不明・確認済み" : "実行結果の確認が必要です";
  if (run.status === "failed") return run.error_code === "CONTEXT_LIMIT_REACHED" ? "会話が長くなったため続けられません。新しい会話を作成してください。" : "返信を完了できませんでした";
  if (run.status === "cancelled") return "停止しました";
  if (active(run)) return run.cancel_requested ? "停止を待っています" : "応答を待っています";
  return "";
}

function renderMessages(forceScroll = false) {
  const area = $("message-area");
  const nearBottom = area.scrollHeight - area.scrollTop - area.clientHeight < 100;
  const oldTop = area.scrollTop;
  const log = $("chat-log");
  const fragment = document.createDocumentFragment();
  if (olderRunsOmitted) fragment.append(element("p", "message-status", "古い履歴の一部は省略されています。"));
  for (const run of [...runs].reverse()) {
    const turn = element("div", "turn");
    const user = element("div", "message user", run.input_text);
    user.dataset.role = "user";
    user.dataset.runId = run.id;
    turn.append(user);
    if (run.output_text) {
      const assistant = element("div", "message assistant", run.output_text);
      assistant.dataset.role = "assistant";
      assistant.dataset.runId = run.id;
      turn.append(assistant);
    }
    const status = runStatus(run);
    if (status) turn.append(element("div", `message-status${unresolved(run) ? " warning" : ""}`, status));
    if (run.reviewed_at && run.review_note) turn.append(element("p", "review-note", `確認メモ：${run.review_note}`));
    fragment.append(turn);
  }
  // Unacknowledged text stays in tab storage but is shown only while signed in.
  const pending = signedIn ? outbox.get(selectedId) : null;
  const matched = pending && runs.some((run) => matchesSubmission(run, pending));
  if (pending && !matched) {
    const turn = element("div", "turn");
    const user = element("div", "message user pending", pending.text);
    user.dataset.role = "user";
    user.dataset.submissionKey = pending.key;
    turn.append(user, element("div", "message-status", isSending() ? "送信しています" : "送信結果を確認してください"));
    fragment.append(turn);
  }
  log.replaceChildren(fragment);
  const empty = !runs.length && !pending;
  $("empty-state").hidden = !empty;
  const failedLoad = selectedId && !currentConversation && !loading;
  $("empty-state").querySelector("h2").textContent = loading ? "会話を読み込んでいます" : failedLoad ? "会話を読み込めませんでした" : "何から始めますか？";
  const emptyHint = $("empty-state").querySelector("p");
  emptyHint.hidden = Boolean(selectedId) && !currentConversation;
  emptyHint.textContent = selectedId ? "メッセージを送って、会話を始めてください。" : "新しい会話を作成して、メッセージを送ってください。";
  $("empty-new-chat").hidden = Boolean(selectedId);
  if (forceScroll || nearBottom) area.scrollTop = area.scrollHeight;
  else area.scrollTop = oldTop;
}

function renderControls() {
  const busy = runs.find(active);
  const unknown = runs.find(unresolved);
  const pending = signedIn ? outbox.get(selectedId) : null;
  const text = $("message").value;
  $("message").disabled = !signedIn || !currentConversation || loading;
  $("send").disabled = !signedIn || !currentConversation || loading || isSending() || Boolean(busy || unknown || pending) || !text.trim() || !storageReady;
  $("send").hidden = Boolean(busy);
  $("stop").hidden = !busy;
  $("stop").disabled = !signedIn || Boolean(busy?.cancel_requested) || stopping === busy?.id;
  $("run-progress").textContent = busy ? (busy.cancel_requested ? "停止を待っています" : progress || "応答を待っています") : "";
  const listedTitle = conversations.find((item) => item.id === selectedId)?.title;
  $("conversation-title").textContent = currentConversation?.title ?? listedTitle ?? "会話";
  const actions = $("run-actions");
  actions.replaceChildren();
  if (pending && !isSending() && !loading) {
    actions.append(element("p", "", pending.rejected ? "メッセージは受け付けられませんでした。内容を戻して確認できます。" : "送信結果が未確認です。同じメッセージの結果を確認できます。"));
    const retry = element("button", "secondary-button", pending.rejected ? "入力欄に戻す" : "送信結果を確認");
    retry.type = "button";
    retry.addEventListener("click", () => pending.rejected ? returnRejected(pending) : dispatch(pending));
    actions.append(retry);
  }
  if (unknown) {
    actions.append(element("p", "", "処理の結果を確認してから、この会話を続けられます。"));
    const inspect = element("button", "secondary-button", "実行記録を確認");
    inspect.type = "button";
    inspect.addEventListener("click", () => openReview(unknown.id));
    actions.append(inspect);
  }
}

function render(forceScroll = false) {
  renderSidebar();
  renderMessages(forceScroll);
  renderControls();
}

function resizeComposer() {
  $("message").style.height = "auto";
  $("message").style.height = `${$("message").scrollHeight}px`;
}

function cancelSelection() {
  selection += 1;
  selectionAbort.abort();
  selectionAbort = new AbortController();
  streamAbort?.abort();
  streamAbort = null;
  closeReview();
  progress = "";
  stopping = "";
  composition = false;
}

async function refreshConversations(capturedConnection = connection) {
  const result = await api("/conversations");
  if (capturedConnection !== connection || !signedIn) return;
  conversations = result.conversations.filter((item) => UUID.test(item.id));
  renderSidebar();
}

function reconcileOutbox() {
  const pending = outbox.get(selectedId);
  if (!pending) return;
  const found = runs.some((run) => matchesSubmission(run, pending));
  if (found) {
    if (sending.get(selectedId) === pending.key) sending.delete(selectedId);
    outbox.delete(selectedId);
    saveSession();
  }
}

function updateConversationTitle(id, title) {
  if (typeof title !== "string") return;
  const listed = conversations.find((item) => item.id === id);
  if (listed) listed.title = title;
  if (currentConversation?.id === id) currentConversation.title = title;
}

async function refreshConversationTitle(id, capturedConnection) {
  const listed = conversations.find((item) => item.id === id);
  if (!listed) return;
  const previousTitle = listed.title;
  try {
    const result = await api("/conversations");
    if (capturedConnection !== connection || listed.title !== previousTitle) return;
    let latest = result.conversations.find((item) => item.id === id);
    if (!latest) {
      // Locally prepended conversations can push a visible one out of the
      // server's newest list window; its bounded snapshot still has the title.
      latest = (await api(`/conversations/${id}/snapshot`)).conversation;
      if (capturedConnection !== connection || listed.title !== previousTitle) return;
    }
    updateConversationTitle(id, latest?.title);
    renderSidebar();
    if (selectedId === id) renderControls();
  } catch {
    // Selecting the conversation will try loading its title again.
  }
}

async function selectConversation(id, focusComposer = false) {
  if (!signedIn || !UUID.test(id)) return;
  cancelSelection();
  selectedId = id;
  currentConversation = null;
  runs = [];
  olderRunsOmitted = false;
  loading = true;
  $("message").value = drafts.get(id) || "";
  saveSession();
  resizeComposer();
  setDrawer(false);
  if (storageReady) showNotice();
  render(true);
  await loadConversationSnapshot({ focusComposer });
}

async function loadConversationSnapshot({ focusComposer = false, refresh = false } = {}) {
  const captured = scope();
  const version = ++snapshotVersion;
  const ownsSnapshot = () => current(captured) && version === snapshotVersion;
  try {
    const result = await api(`/conversations/${captured.id}/snapshot`, { signal: selectionAbort.signal });
    if (!ownsSnapshot()) return;
    currentConversation = result.conversation;
    updateConversationTitle(captured.id, currentConversation.title);
    const displayedRuns = new Map(runs.map((run) => [run.id, run]));
    runs = result.runs.map((run) => {
      const displayed = displayedRuns.get(run.id);
      return displayed && displayed.event_seq > run.event_seq ? displayed : run;
    });
    olderRunsOmitted = result.older_runs_omitted;
    reconcileOutbox();
    loading = false;
    render(!refresh);
    if (focusComposer) $("message").focus();
    const run = runs.find(active);
    if (run) startStream(run.id, captured);
  } catch (error) {
    if (!ownsSnapshot() || error.name === "AbortError") return;
    loading = false;
    // The displayed runs can predate an accepted send or conflict.
    currentConversation = null;
    showNotice(`${friendlyError(error)} サイドバーから会話を選び直してください。`);
    render();
  }
}

function refreshCurrentConversation() {
  loading = true;
  renderControls();
  return loadConversationSnapshot({ refresh: true });
}

function disconnect(message = "") {
  connection += 1;
  connectionAbort.abort();
  connectionAbort = new AbortController();
  cancelSelection();
  sending.clear();
  signedIn = false;
  signingOut = false;
  conversations = [];
  runs = [];
  olderRunsOmitted = false;
  currentConversation = null;
  loading = false;
  creating = false;
  $("message").value = "";
  $("server-token").value = "";
  $("connect-submit").disabled = false;
  $("connect-error").textContent = message;
  setDrawer(false);
  render();
  if (!$("connect-dialog").open) $("connect-dialog").showModal();
}

async function connect(event) {
  event.preventDefault();
  if (!secureOrigin()) return;
  const candidate = $("server-token").value;
  $("server-token").value = "";
  if (!/^[\x21-\x7e]{32,512}$/.test(candidate)) {
    $("connect-error").textContent = "32〜512文字のサーバートークンを入力してください。";
    return;
  }
  connection += 1;
  const capturedConnection = connection;
  $("connect-submit").disabled = true;
  $("connect-error").textContent = "ログインしています…";
  try {
    const response = await fetch("/v1/sessions", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ token: candidate }), cache: "no-store", redirect: "error", credentials: "same-origin", signal: connectionAbort.signal });
    if (!response.ok) throw new ApiError(response.status);
  } catch (error) {
    if (capturedConnection !== connection) return;
    $("connect-error").textContent = error instanceof ApiError && error.status === 401 ? "サーバートークンが正しくありません。" : friendlyError(error);
    $("connect-submit").disabled = false;
    return;
  }
  if (capturedConnection !== connection) return;
  // The cookie is valid now, so loading follows the same retrying path as a
  // reload. The button stays disabled until that settles, so a second sign-in
  // cannot supersede the first; disconnect() owns the dialog if it ends.
  await resumeSession(true);
  if (capturedConnection !== connection) return;
  $("connect-submit").disabled = false;
  $("connect-error").textContent = "";
}

async function openSignedIn(capturedConnection) {
  signedIn = true;
  await refreshConversations(capturedConnection);
  if (capturedConnection !== connection) return;
  if ($("connect-dialog").open) $("connect-dialog").close();
  if (storageReady) showNotice();
  render();
  const target = conversations.some((item) => item.id === selectedId) ? selectedId : conversations[0]?.id;
  if (target) await selectConversation(target);
}

// A reload resumes the browser session; only a missing or ended session asks
// for the server token again.
async function resumeSession(afterSignIn = false) {
  const capturedConnection = connection;
  try {
    const response = await fetch("/v1/session", { cache: "no-store", redirect: "error", credentials: "same-origin", signal: connectionAbort.signal });
    if (capturedConnection !== connection) return;
    if (response.status === 401) {
      // A retry can find the session ended after the page was signed in, so
      // reset that state and any pending text before asking for the token.
      disconnect(signedIn ? SIGNED_OUT : afterSignIn ? "ログイン状態を保存できませんでした。ブラウザのCookie設定を確認してください。" : "");
      return;
    }
    if (!response.ok) throw new ApiError(response.status);
    await openSignedIn(capturedConnection);
  } catch (error) {
    // Only an ended session needs the token; api() already handled a 401.
    // Transport and server failures keep the cookie and offer a retry.
    if (capturedConnection !== connection || error.name === "AbortError") return;
    if ($("connect-dialog").open) $("connect-dialog").close();
    render();
    showNotice(friendlyError(error), () => {
      showNotice();
      resumeSession();
    });
  }
}

async function signOut() {
  if (!signedIn || signingOut) return;
  signingOut = true;
  renderSidebar();
  try {
    await api("/session", { method: "DELETE" });
    disconnect();
  } catch (error) {
    // A 401 already returned to the sign-in dialog inside api().
    if (error instanceof ApiError && error.status === 401) return;
    signingOut = false;
    showNotice(friendlyError(error));
    renderSidebar();
  }
}

async function createConversation() {
  if (!signedIn || creating) return;
  creating = true;
  const captured = scope();
  renderSidebar();
  try {
    const result = await api("/conversations", { method: "POST", body: {} });
    if (captured.connection !== connection || !signedIn) return;
    conversations.unshift(result);
    if (current(captured)) await selectConversation(result.id, true);
  } catch (error) {
    if (current(captured)) {
      setDrawer(false);
      showNotice(friendlyError(error));
    }
  } finally {
    if (captured.connection === connection) {
      creating = false;
      renderSidebar();
    }
  }
}

function submit(event) {
  event.preventDefault();
  if ($("send").disabled) return;
  const text = $("message").value;
  if (encoder.encode(text).length > 24576 || text.includes("\0")) {
    showNotice("メッセージが長すぎます。24 KiB以内に分けて送信してください。");
    return;
  }
  const pending = { key: crypto.randomUUID(), text, createdAt: new Date().toISOString(), runId: null, rejected: false };
  outbox.set(selectedId, pending);
  drafts.set(selectedId, "");
  // A durable identity is reserved before any network operation or suspension.
  if (!saveSession()) {
    outbox.delete(selectedId);
    drafts.set(selectedId, text);
    renderControls();
    return;
  }
  $("message").value = "";
  resizeComposer();
  showNotice();
  dispatch(pending);
}

async function dispatch(pending) {
  if (!signedIn || !selectedId || isSending() || outbox.get(selectedId) !== pending) return;
  const captured = scope();
  sending.set(captured.id, pending.key);
  const ownsSend = () => captured.connection === connection && sending.get(captured.id) === pending.key;
  render(true);
  try {
    const result = await api(`/conversations/${captured.id}/runs`, { method: "POST", body: { message: pending.text, idempotency_key: pending.key } });
    if (!ownsSend()) return;
    sending.delete(captured.id);
    if (outbox.get(captured.id)?.key !== pending.key) {
      if (selectedId === captured.id) renderControls();
      return;
    }
    updateConversationTitle(captured.id, result.conversation_title);
    outbox.delete(captured.id);
    saveSession();
    if (!current(captured)) {
      if (selectedId === captured.id) await refreshCurrentConversation();
      else renderSidebar();
      return;
    }
    replaceRun(result);
    render(true);
    if (active(result)) startStream(result.id, captured);
  } catch (error) {
    if (!ownsSend()) return;
    sending.delete(captured.id);
    if (outbox.get(captured.id)?.key === pending.key) {
      // A definitive input/conflict response did not accept this payload.
      // Transport failures remain unknown and retain the exact same identity.
      pending.rejected = error instanceof ApiError && [400, 409, 413, 422].includes(error.status);
      saveSession();
    }
    if (error.name === "AbortError") return;
    if (error instanceof ApiError && error.status === 409) {
      // Another client may already have submitted the first message.
      if (selectedId === captured.id) await refreshCurrentConversation();
      else await refreshConversationTitle(captured.id, captured.connection);
      return;
    }
    if (selectedId !== captured.id) return;
    showNotice(friendlyError(error));
    render();
  }
}

function returnRejected(pending) {
  if (!pending.rejected || outbox.get(selectedId) !== pending) return;
  const existing = $("message").value;
  const restored = existing ? `${pending.text}\n\n${existing}` : pending.text;
  outbox.delete(selectedId);
  drafts.set(selectedId, restored);
  if (!saveSession()) {
    outbox.set(selectedId, pending);
    drafts.set(selectedId, existing);
    return;
  }
  $("message").value = restored;
  resizeComposer();
  showNotice();
  render();
  $("message").focus();
}

async function stop() {
  const run = runs.find(active);
  if (!run || stopping === run.id || run.cancel_requested) return;
  const captured = scope();
  stopping = run.id;
  const ownsStop = () => current(captured) && stopping === run.id;
  renderControls();
  try {
    const result = await api(`/runs/${run.id}/cancel`, { method: "POST" });
    if (!ownsStop()) return;
    replaceRun(result.run);
  } catch (error) {
    if (ownsStop()) showNotice(friendlyError(error));
  } finally {
    if (ownsStop()) { stopping = ""; render(); }
  }
}

function replaceRun(run) {
  const index = runs.findIndex((item) => item.id === run.id);
  if (index < 0) {
    runs.unshift(run);
    return;
  }
  const previous = runs[index];
  if (run.event_seq < previous.event_seq) return;
  if (active(run)) run.output_text = previous.output_text;
  runs[index] = run;
}

function delay(ms, signal) {
  return new Promise((resolve, reject) => {
    const abort = () => { clearTimeout(timer); reject(new DOMException("Aborted", "AbortError")); };
    const timer = setTimeout(() => { signal.removeEventListener("abort", abort); resolve(); }, ms);
    if (signal.aborted) abort();
    else signal.addEventListener("abort", abort, { once: true });
  });
}

async function startStream(id, captured) {
  streamAbort?.abort();
  const controller = new AbortController();
  streamAbort = controller;
  const valid = () => current(captured) && streamAbort === controller && !controller.signal.aborted;
  let cursor = 0;
  let text = "";
  let textBytes = 0;
  let attempts = 0;
  while (valid()) {
    try {
      const response = await fetch(`/v1/runs/${id}/events`, { headers: { "Last-Event-ID": String(cursor) }, cache: "no-store", redirect: "error", credentials: "same-origin", signal: controller.signal });
      if (!valid()) return;
      if (response.status === 401) { disconnect(SIGNED_OUT); return; }
      if (!response.ok || !response.headers.get("content-type")?.toLowerCase().startsWith("text/event-stream") || !response.body) throw new Error("stream");
      progress = "応答を待っています";
      renderControls();
      await readEvents(response.body, controller.signal, (record) => {
        if (!valid()) return;
        if (record.event === "stream_error") throw new Error("stream");
        const seq = Number(record.id);
        if (!/^\d+$/.test(record.id) || !Number.isSafeInteger(seq) || seq < 1) throw new Error("cursor");
        if (seq <= cursor) return;
        if (seq !== cursor + 1) throw new Error("cursor");
        const data = JSON.parse(record.data);
        const run = runs.find((item) => item.id === id);
        if (!run) throw new Error("run");
        if (record.event === "text_delta") {
          if (typeof data.delta !== "string") throw new Error("text");
          textBytes += encoder.encode(data.delta).length;
          if (textBytes > 1048576) throw new Error("text");
          text += data.delta;
          if (active(run)) {
            run.output_text = text;
            progress = "応答中";
            renderMessages();
          }
        } else if (record.event === "tool.started") progress = "ツールを実行中";
        else if (record.event === "tool.completed") progress = "応答を準備中";
        else if (record.event === "run.cancel_requested") run.cancel_requested = true;
        cursor = seq;
        attempts = 0;
        renderControls();
      });
      if (!valid()) return;
      const result = await api(`/runs/${id}`, { signal: controller.signal });
      if (!valid()) return;
      if (!active(result.run) && cursor >= result.run.event_seq) {
        replaceRun(result.run);
        progress = "";
        render();
        return;
      }
      throw new Error("stream");
    } catch (error) {
      if (!valid() || error.name === "AbortError") return;
      progress = "再接続しています";
      renderControls();
      // Only the event stream is retried. The POST and external tools are not.
      try { await delay(Math.min(500 * 2 ** Math.min(attempts++, 4), 5000), controller.signal); }
      catch { return; }
    }
  }
}

async function readEvents(body, signal, onRecord) {
  const reader = body.getReader();
  const decoder = new TextDecoder("utf-8", { fatal: true });
  let buffer = "";
  let event = "message";
  let id = "";
  let data = [];
  let recordBytes = 0;
  function line(value) {
    if (!value) {
      if (data.length) onRecord({ event, id, data: data.join("\n") });
      event = "message"; id = ""; data = []; recordBytes = 0;
      return;
    }
    if (value.startsWith(":")) return;
    recordBytes += encoder.encode(value).length;
    if (recordBytes > 262144) throw new Error("event_size");
    const colon = value.indexOf(":");
    const field = colon < 0 ? value : value.slice(0, colon);
    let content = colon < 0 ? "" : value.slice(colon + 1);
    if (content.startsWith(" ")) content = content.slice(1);
    if (field === "event") event = content;
    else if (field === "id") id = content;
    else if (field === "data") data.push(content);
  }
  try {
    for (;;) {
      const { value, done } = await reader.read();
      if (signal.aborted) throw new DOMException("Aborted", "AbortError");
      buffer += done ? decoder.decode() : decoder.decode(value, { stream: true });
      let start = 0;
      for (let index = 0; index < buffer.length; index += 1) {
        if (buffer[index] !== "\n" && buffer[index] !== "\r") continue;
        if (buffer[index] === "\r" && index === buffer.length - 1 && !done) break;
        line(buffer.slice(start, index));
        if (buffer[index] === "\r" && buffer[index + 1] === "\n") index += 1;
        start = index + 1;
      }
      buffer = buffer.slice(start);
      if (encoder.encode(buffer).length > 262144) throw new Error("event_size");
      if (done) break; // An unterminated event is never published.
    }
  } finally {
    await reader.cancel().catch(() => {});
    reader.releaseLock();
  }
}

function updateReviewButton() {
  const note = $("review-note").value.trim();
  $("resolve").disabled = resolving || !unresolved(review?.run) || !note
    || encoder.encode(note).length > 4096 || !$("review-acknowledgment").checked;
}

function closeReview() {
  reviewRequest += 1;
  review = null;
  resolving = false;
  $("review-dialog").close();
  $("review-records").replaceChildren();
  $("review-note").value = "";
}

async function openReview(id) {
  const captured = scope();
  const requestId = ++reviewRequest;
  const valid = () => requestId === reviewRequest && current(captured) && $("review-dialog").open;
  $("review-records").replaceChildren(element("p", "", "実行記録を読み込んでいます…"));
  $("review-note").value = "";
  $("review-acknowledgment").checked = false;
  $("review-error").textContent = "";
  $("resolve").disabled = true;
  $("review-dialog").showModal();
  try {
    const result = await api(`/runs/${id}/review`, { signal: selectionAbort.signal });
    if (!valid()) return;
    review = { ...result, captured };
    const records = $("review-records");
    records.replaceChildren();
    const request = element("section", "review-record");
    request.append(element("h3", "", "送信したメッセージ"), element("p", "", result.run.input_text));
    records.append(request);
    if (!result.tools.length) records.append(element("p", "", "保存されたツールの実行記録はありません。返信は自動で再生成しません。"));
    for (const tool of result.tools) {
      const record = element("section", "review-record");
      record.append(element("h3", "", tool.name), element("p", "", tool.status === "completed" ? "結果を受信済み" : "結果は不明です。実行先でも確認してください。"));
      const details = element("details");
      details.append(element("summary", "", "引数と記録された結果を見る"), element("pre", "", JSON.stringify({ arguments: tool.arguments, output: tool.output }, null, 2)));
      record.append(details);
      records.append(record);
    }
    if (result.run.reviewed_at) {
      replaceRun(result.run);
      render();
      $("review-note").value = result.run.review_note || "";
      $("review-error").textContent = "この処理はすでに確認済みです。会話を選び直してください。";
    }
    updateReviewButton();
  } catch (error) {
    if (valid()) $("review-error").textContent = friendlyError(error);
  }
}

async function resolve(event) {
  event.preventDefault();
  if (!review || !current(review.captured) || $("resolve").disabled) return;
  const note = $("review-note").value.trim();
  if (!note || encoder.encode(note).length > 4096 || !$("review-acknowledgment").checked) {
    $("review-error").textContent = "確認メモを4 KiB以内で入力し、確認済みのチェックを付けてください。";
    return;
  }
  const pendingReview = review;
  resolving = true;
  updateReviewButton();
  try {
    const result = await api(`/runs/${pendingReview.run.id}/resolve`, { method: "POST", body: { note, expected_event_seq: pendingReview.run.event_seq, acknowledge_unknown: true } });
    if (!current(pendingReview.captured)) return;
    replaceRun(result.run);
    closeReview();
    showNotice();
    render();
    $("message").focus();
  } catch (error) {
    if (review === pendingReview && current(pendingReview.captured)) {
      $("review-error").textContent = friendlyError(error);
      resolving = false;
      updateReviewButton();
    }
  }
}

function secureOrigin() {
  const safe = location.protocol === "https:" || (location.protocol === "http:" && ["localhost", "127.0.0.1", "[::1]"].includes(location.hostname));
  if (!safe) {
    $("connect-error").textContent = "トークンを保護するため、HTTPSまたはこのPCのlocalhostで開いてください。";
    $("connect-submit").disabled = true;
  }
  return safe;
}

$("connect-form").addEventListener("submit", connect);
$("connect-dialog").addEventListener("cancel", (event) => event.preventDefault());
$("disconnect").addEventListener("click", signOut);
$("new-chat").addEventListener("click", createConversation);
$("empty-new-chat").addEventListener("click", createConversation);
$("composer-form").addEventListener("submit", submit);
$("message").addEventListener("input", () => {
  if (selectedId) { drafts.set(selectedId, $("message").value); saveSession(); }
  resizeComposer();
  renderControls();
});
$("message").addEventListener("compositionstart", () => { composition = true; });
$("message").addEventListener("compositionend", () => { composition = false; });
$("message").addEventListener("keydown", (event) => {
  if (event.key === "Enter" && !event.shiftKey && !event.altKey && !event.ctrlKey && !event.metaKey && !event.isComposing && !composition && event.keyCode !== 229) {
    event.preventDefault();
    $("composer-form").requestSubmit();
  }
});
$("stop").addEventListener("click", stop);
$("open-sidebar").addEventListener("click", () => setDrawer(true));
$("close-sidebar").addEventListener("click", () => { setDrawer(false); $("open-sidebar").focus(); });
$("drawer-backdrop").addEventListener("click", () => { setDrawer(false); $("open-sidebar").focus(); });
document.addEventListener("keydown", (event) => {
  if (event.key === "Escape" && $("sidebar").classList.contains("open")) { setDrawer(false); $("open-sidebar").focus(); }
});
$("close-review").addEventListener("click", () => { if (!resolving) closeReview(); });
$("review-dialog").addEventListener("cancel", (event) => {
  event.preventDefault();
  if (!resolving) closeReview();
});
$("review-note").addEventListener("input", updateReviewButton);
$("review-acknowledgment").addEventListener("change", updateReviewButton);
$("review-form").addEventListener("submit", resolve);
$("storage-info").addEventListener("click", () => $("storage-dialog").showModal());
$("close-storage").addEventListener("click", () => $("storage-dialog").close());
restoreSession();
render();
if (secureOrigin()) resumeSession();
else $("connect-dialog").showModal();
