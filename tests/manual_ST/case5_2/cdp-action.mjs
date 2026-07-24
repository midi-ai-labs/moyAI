import { readFile, writeFile } from "node:fs/promises";

const [, , portText, action, argument = ""] = process.argv;
const port = Number(portText);
if (!Number.isInteger(port) || !action) {
  throw new Error("usage: node cdp-action.mjs <port> <action> [argument]");
}

let pages;
for (let attempt = 0; attempt < 300; attempt += 1) {
  try {
    pages = await fetch(`http://127.0.0.1:${port}/json/list`).then((response) => response.json());
    if (pages.some((candidate) => candidate.webSocketDebuggerUrl)) break;
  } catch {
    // Visible WebView is still starting.
  }
  await new Promise((resolve) => setTimeout(resolve, 100));
}
const page = pages?.find((candidate) => candidate.title === "moyAI" && candidate.webSocketDebuggerUrl)
  ?? pages?.find((candidate) => candidate.webSocketDebuggerUrl);
if (!page?.webSocketDebuggerUrl) {
  throw new Error(`No moyAI WebView2 page exposed through CDP port ${port}`);
}

const socket = new WebSocket(page.webSocketDebuggerUrl);
await new Promise((resolve, reject) => {
  socket.addEventListener("open", resolve, { once: true });
  socket.addEventListener("error", reject, { once: true });
});
let nextId = 0;
const pending = new Map();
socket.addEventListener("message", (event) => {
  const payload = JSON.parse(event.data);
  const waiter = pending.get(payload.id);
  if (!waiter) return;
  pending.delete(payload.id);
  if (payload.error) waiter.reject(new Error(JSON.stringify(payload.error)));
  else waiter.resolve(payload.result);
});

function call(method, params = {}) {
  const id = ++nextId;
  return new Promise((resolve, reject) => {
    pending.set(id, { resolve, reject });
    socket.send(JSON.stringify({ id, method, params }));
  });
}

async function evaluate(expression) {
  const result = await call("Runtime.evaluate", {
    expression,
    returnByValue: true,
    awaitPromise: true,
    userGesture: true,
  });
  if (result.exceptionDetails) {
    throw new Error(result.exceptionDetails.text ?? "Runtime.evaluate failed");
  }
  return result.result.value;
}

async function clickPoint(point) {
  if (!point || point.error) throw new Error(JSON.stringify(point));
  await call("Input.dispatchMouseEvent", {
    type: "mousePressed",
    x: point.x,
    y: point.y,
    button: "left",
    clickCount: 1,
  });
  await call("Input.dispatchMouseEvent", {
    type: "mouseReleased",
    x: point.x,
    y: point.y,
    button: "left",
    clickCount: 1,
  });
}

async function key(key, code = key, modifiers = 0) {
  await call("Input.dispatchKeyEvent", { type: "keyDown", key, code, modifiers });
  await call("Input.dispatchKeyEvent", { type: "keyUp", key, code, modifiers });
}

try {
  if (action === "controls") {
    const controls = await evaluate(`(() => [...document.querySelectorAll('textarea, input, [contenteditable="true"], button')]
      .map((element) => {
        const rect = element.getBoundingClientRect();
        return {
          tag: element.tagName,
          text: (element.innerText || element.value || '').trim().slice(0, 160),
          aria: element.getAttribute('aria-label'),
          title: element.getAttribute('title'),
          placeholder: element.getAttribute('placeholder'),
          disabled: Boolean(element.disabled),
          visible: rect.width > 0 && rect.height > 0,
          x: rect.left + rect.width / 2,
          y: rect.top + rect.height / 2,
        };
      }))()`);
    const rendered = JSON.stringify(controls, null, 2);
    if (argument) await writeFile(argument, rendered, "utf8");
    console.log(rendered);
  } else if (action === "snapshot") {
    const snapshot = await evaluate(`(() => {
      const body = document.body.innerText || '';
      const composer = document.querySelector('textarea:not([disabled]), [contenteditable="true"]');
      return {
        title: document.title,
        bodyTail: body.slice(-8000),
        buttons: [...document.querySelectorAll('button')].map((button) => ({
          text: button.innerText.trim(),
          aria: button.getAttribute('aria-label'),
          title: button.getAttribute('title'),
          disabled: button.disabled,
        })).filter((item) => item.text || item.aria || item.title),
        composer: composer ? {
          tag: composer.tagName,
          valueLength: (composer.value || composer.innerText || '').length,
          disabled: Boolean(composer.disabled),
          placeholder: composer.getAttribute('placeholder'),
        } : null,
      };
    })()`);
    const rendered = JSON.stringify(snapshot, null, 2);
    if (argument) await writeFile(argument, rendered, "utf8");
    console.log(rendered);
  } else if (action === "fill-composer") {
    const content = await readFile(argument, "utf8");
    const point = await evaluate(`(() => {
      const element = document.querySelector('textarea:not([disabled]), [contenteditable="true"]');
      if (!element) return { error: 'composer not found' };
      const rect = element.getBoundingClientRect();
      if (rect.width <= 0 || rect.height <= 0) return { error: 'composer not visible' };
      return { x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 };
    })()`);
    await clickPoint(point);
    await key("a", "KeyA", 2);
    await key("Backspace", "Backspace");
    await call("Input.insertText", { text: content });
    console.log(JSON.stringify({ characters: content.length, point }));
  } else if (action === "click-button" || action === "click-button-contains") {
    const label = JSON.stringify(argument);
    const exact = action === "click-button";
    const point = await evaluate(`(() => {
      const label = ${label};
      const matches = [...document.querySelectorAll('button')].filter((button) => {
        const values = [button.innerText.trim(), button.getAttribute('aria-label') || '', button.getAttribute('title') || ''];
        return !button.disabled && values.some((value) => ${exact ? "value === label" : "value.includes(label)"});
      });
      const visible = matches.filter((button) => {
        const rect = button.getBoundingClientRect();
        return rect.width > 0 && rect.height > 0;
      });
      if (visible.length !== 1) return { error: 'expected one enabled visible button', count: visible.length, label };
      const rect = visible[0].getBoundingClientRect();
      return { x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 };
    })()`);
    await clickPoint(point);
    console.log(JSON.stringify({ label: argument, point }));
  } else if (action === "keypress") {
    await key(argument, argument);
    console.log(JSON.stringify({ key: argument }));
  } else if (action === "screenshot") {
    await call("Page.enable");
    const capture = await call("Page.captureScreenshot", { format: "png", fromSurface: true });
    const buffer = Buffer.from(capture.data, "base64");
    await writeFile(argument, buffer);
    console.log(JSON.stringify({ path: argument, bytes: buffer.length }));
  } else {
    throw new Error(`unknown action: ${action}`);
  }
} finally {
  socket.close();
}
