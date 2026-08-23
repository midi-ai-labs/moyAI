import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import { renderComposer, renderOverlay, renderTitlebar, synchronizeTitlebarMenuState } from "../src/render.ts";
import type { DesktopViewState } from "../src/types.ts";

function viewState(fields: Record<string, unknown>): DesktopViewState {
  return fields as unknown as DesktopViewState;
}

function elementWith(html: string, attribute: string, value: string): string {
  const match = html.match(new RegExp(`<[^>]+${attribute}="${value}"[^>]*>`));
  assert.ok(match, `expected an element with ${attribute}="${value}"`);
  return match[0];
}

test("titlebar menus expose popup ownership and the active expanded state", () => {
  const inactive = renderTitlebar(false, false, "none");
  const active = renderTitlebar(false, false, "view_menu");

  assert.match(inactive, /<nav class="titlebar-menu" aria-label="アプリケーションメニュー">/);
  for (const menu of ["file", "edit", "view", "help"]) {
    const trigger = elementWith(active, "id", `titlebar-${menu}-menu-trigger`);
    assert.match(trigger, new RegExp(`aria-haspopup="${menu === "view" ? "dialog" : "menu"}"`));
    assert.match(trigger, new RegExp(`aria-controls="titlebar-${menu}-menu"`));
    assert.match(trigger, new RegExp(`aria-expanded="${menu === "view"}"`));
  }
});

test("a preserved titlebar synchronizes every menu trigger after overlay changes", () => {
  const applicationCommands = new AttributeTarget();
  const triggers = new Map(
    ["file", "edit", "view", "help"].map((menu) => [
      `#titlebar-${menu}-menu-trigger`,
      new AttributeTarget({ "aria-expanded": menu === "file" ? "true" : "false" }),
    ]),
  );
  const titlebar = {
    querySelector(selector: string): AttributeTarget | null {
      if (selector === ".titlebar-menu") return applicationCommands;
      return triggers.get(selector) ?? null;
    },
  } as unknown as HTMLElement;

  synchronizeTitlebarMenuState(titlebar, "help_menu", true);
  assert.equal(triggers.get("#titlebar-file-menu-trigger")?.getAttribute("aria-expanded"), "false");
  assert.equal(triggers.get("#titlebar-edit-menu-trigger")?.getAttribute("aria-expanded"), "false");
  assert.equal(triggers.get("#titlebar-view-menu-trigger")?.getAttribute("aria-expanded"), "false");
  assert.equal(triggers.get("#titlebar-help-menu-trigger")?.getAttribute("aria-expanded"), "true");
  assert.equal(applicationCommands.hasAttribute("inert"), true);
  assert.equal(applicationCommands.getAttribute("aria-hidden"), "true");

  synchronizeTitlebarMenuState(titlebar, "none", false);
  for (const trigger of triggers.values()) {
    assert.equal(trigger.getAttribute("aria-expanded"), "false");
  }
  assert.equal(applicationCommands.hasAttribute("inert"), false);
  assert.equal(applicationCommands.hasAttribute("aria-hidden"), false);
});

test("rendered titlebar popover has a stable id and accessible menu name", () => {
  const html = renderOverlay(viewState({ overlay: "help_menu" }));
  const menu = elementWith(html, "id", "titlebar-help-menu");

  assert.match(menu, /role="menu"/);
  assert.match(menu, /aria-label="ヘルプメニュー"/);
  assert.match(menu, /aria-labelledby="titlebar-help-menu-trigger"/);
  assert.match(html, /<button data-action="show-shortcuts" data-titlebar-menu-action tabindex="0" role="menuitem">/);
  assert.match(html, /<button data-action="show-about" data-titlebar-menu-action tabindex="-1" role="menuitem">/);
});

test("the mixed-widget View popover is a dialog with ordinary actions and a native range", () => {
  const html = renderOverlay(viewState({
    overlay: "view_menu",
    window_opacity_percent: 85,
  }));
  const popup = elementWith(html, "id", "titlebar-view-menu");

  assert.match(popup, /role="dialog"/);
  assert.match(popup, /aria-label="表示メニュー"/);
  assert.doesNotMatch(html, /data-titlebar-menu-action[^>]*role="menuitem"/);
  assert.match(html, /<button data-action="refresh" data-titlebar-menu-action>/);
  assert.match(html, /<button data-action="show-provider" data-titlebar-menu-action>/);
  assert.match(html, /<button data-action="show-config" data-titlebar-menu-action>/);
  const viewActions = Array.from(html.matchAll(/<button[^>]*data-titlebar-menu-action[^>]*>/g), (match) => match[0]);
  assert.equal(viewActions.length, 3);
  for (const action of viewActions) assert.doesNotMatch(action, /tabindex=/);
  assert.match(html, /<input id="opacity-input" type="range"[^>]*aria-valuetext="85%"/);
  const tabOrder = ["data-action=\"refresh\"", "data-action=\"show-provider\"", "data-action=\"show-config\"", "id=\"opacity-input\""]
    .map((marker) => html.indexOf(marker));
  assert.ok(tabOrder.every((position) => position >= 0));
  assert.deepEqual(tabOrder, [...tabOrder].sort((left, right) => left - right));
});

test("view opacity and workspace path controls are bound to visible labels", () => {
  const viewMenu = renderOverlay(viewState({
    overlay: "view_menu",
    window_opacity_percent: 85,
  }));
  const workspace = renderOverlay(viewState({
    overlay: "workspace",
    workspace_input: "C:/workspace",
  }));

  assert.match(viewMenu, /<label class="field-label" for="opacity-input">ウィンドウ透過率<\/label>/);
  assert.match(viewMenu, /<input id="opacity-input" type="range"/);
  assert.match(workspace, /<label class="field-label" for="workspace-input">パス<\/label>/);
  assert.match(workspace, /<input id="workspace-input"/);
});

test("provider mode and prompt review controls expose accessible group and field names", () => {
  const provider = renderOverlay(viewState({
    overlay: "provider",
    provider_selected_model_summary: [],
    provider_status: { kind: "idle", title: "待機中", hint: "", details: "" },
    startup: { initial_setup_required: false, action_overlay: "none" },
    provider_base_url: "http://127.0.0.1:1234",
    provider_metadata_mode: "openai_compatible_only",
    provider_catalog_base_url: null,
    provider_catalog_metadata_mode: null,
    provider_context_window: "32768",
    provider_max_output_tokens: "4096",
    provider_loading: false,
    provider_apply_enabled: false,
    provider_models: [],
    provider_model_ids: [],
    provider_selected_index: -1,
    config_draft: { external_owner_mutation_open: true },
  }));
  const review = renderOverlay(viewState({
    overlay: "prompt_review",
    review_raw_text: "原文",
    review_draft_text: "推敲文",
    review_status_text: "",
    send_raw_enabled: true,
    send_enhanced_enabled: true,
  }));

  assert.match(provider, /id="provider-mode-label">Provider mode<\/span>/);
  assert.match(provider, /class="segmented-control provider-mode-control" role="group" aria-labelledby="provider-mode-label"/);
  assert.match(review, /<label class="sr-only" for="review-draft">推敲文<\/label>/);
  assert.match(review, /<textarea id="review-draft">推敲文<\/textarea>/);
});

test("composer and overlay text entry controls have stable explicit labels", () => {
  const composer = renderComposer(viewState({
    selected_project_index: -1,
    composer_submit_mode: "blocked",
    navigation_loading: false,
    busy: false,
    draft_prompt: "",
    image_input: "",
    image_input_enabled: true,
    attached_images: [],
    enhance_enabled: false,
    can_submit: false,
    workspace_path: "C:/workspace",
    token_meter_label: "",
  }));
  const workspace = renderOverlay(viewState({
    overlay: "workspace",
    workspace_input: "C:/workspace",
  }));
  const review = renderOverlay(viewState({
    overlay: "prompt_review",
    review_raw_text: "原文",
    review_draft_text: "推敲文",
    review_status_text: "",
    send_raw_enabled: true,
    send_enhanced_enabled: true,
  }));
  const palette = renderOverlay(viewState({
    overlay: "command_palette",
    local_search_text: "__no_matching_action__",
    local_search_results_text: "",
    command_rows: [],
    session_rows: [],
    config_draft: { dirty: false },
  }));

  assert.match(composer, /<label class="sr-only" for="prompt">moyAIへの依頼<\/label>/);
  assert.match(composer, /<textarea id="prompt"/);
  assert.match(workspace, /<label class="field-label" for="workspace-input">パス<\/label>/);
  assert.match(workspace, /<input id="workspace-input"/);
  assert.match(review, /<label class="sr-only" for="review-draft">推敲文<\/label>/);
  assert.match(review, /<textarea id="review-draft"/);
  assert.match(palette, /<label class="sr-only" for="local-search">アクション、セッション、コマンドを検索<\/label>/);
  assert.match(palette, /<input id="local-search"/);
});

test("quiet text meets normal-text contrast and reduced-motion disables continuous effects", () => {
  const css = readFileSync(new URL("../src/styles.css", import.meta.url), "utf8");
  const quiet = css.match(/--quiet:\s*(#[0-9a-f]{6})/i)?.[1];
  const background = css.match(/--bg:\s*(#[0-9a-f]{6})/i)?.[1];
  assert.ok(quiet);
  assert.ok(background);
  assert.ok(contrastRatio(quiet, background) >= 4.5);

  const reducedMotionStart = css.indexOf("@media (prefers-reduced-motion: reduce)");
  const nextMedia = css.indexOf("@media (max-width", reducedMotionStart);
  assert.notEqual(reducedMotionStart, -1);
  const reducedMotion = css.slice(reducedMotionStart, nextMedia < 0 ? undefined : nextMedia);
  assert.match(reducedMotion, /\*::before/);
  assert.match(reducedMotion, /\*::after/);
  assert.match(reducedMotion, /animation-iteration-count:\s*1\s*!important/);
  assert.match(reducedMotion, /transition-duration:\s*0\.01ms\s*!important/);
  assert.match(reducedMotion, /\.task-activity-indicator\s*\{[\s\S]*animation:\s*none\s*!important/);
  assert.match(reducedMotion, /\.task-activity-indicator\[data-task-activity="running"\]/);
  assert.match(reducedMotion, /\.task-activity-indicator\[data-task-activity="finalizing"\]/);
  assert.match(reducedMotion, /\.task-activity-indicator\[data-task-activity="attention"\]/);
  assert.match(reducedMotion, /border-style:\s*double/);
  assert.match(reducedMotion, /border-radius:\s*5px/);
});

test("task activity keeps distinct shapes and selected versus background emphasis", () => {
  const css = readFileSync(new URL("../src/styles.css", import.meta.url), "utf8");
  assert.match(
    css,
    /\.task-activity-indicator\[data-task-activity="running"\]\s*\{[^}]*border:\s*2px solid rgb\(121 170 255 \/ 38%\)[^}]*border-radius:\s*999px[^}]*background:\s*rgb\(85 142 236 \/ 18%\)[^}]*animation:\s*moyai-task-activity-running 900ms linear infinite/s,
  );
  assert.match(
    css,
    /\.task-activity-indicator\[data-task-activity="running"\]::after\s*\{[^}]*width:\s*4px[^}]*height:\s*4px[^}]*border-radius:\s*999px[^}]*background:\s*#dceaff/s,
  );
  assert.match(
    css,
    /\.task-activity-indicator\[data-task-activity="finalizing"\]\s*\{[^}]*border:\s*3px double #c4a9ff[^}]*border-radius:\s*5px[^}]*animation:\s*moyai-task-activity-finalizing 1600ms ease-in-out infinite/s,
  );
  assert.match(
    css,
    /\.task-activity-indicator\[data-task-activity="finalizing"\]::after\s*\{[^}]*width:\s*4px[^}]*height:\s*4px[^}]*border-radius:\s*1px[^}]*background:\s*#e3d7ff[^}]*rotate\(45deg\)/s,
  );
  assert.match(
    css,
    /\.task-activity-indicator\[data-task-activity="attention"\]\s*\{[^}]*border:\s*2px solid #ffc766[^}]*border-radius:\s*2px[^}]*transform:\s*rotate\(45deg\) scale\(0\.78\)/s,
  );
  assert.match(
    css,
    /\.task-activity-indicator\[data-task-activity="attention"\]::after\s*\{[^}]*content:\s*"!"[^}]*color:\s*#fff4d6/s,
  );
  assert.match(
    css,
    /\.task-activity-indicator\.small\s*\{[^}]*width:\s*14px[^}]*min-width:\s*14px[^}]*height:\s*14px/s,
  );
  assert.match(
    css,
    /\.nav-row-wrap:not\(\.selected\) \.task-activity-indicator\[data-task-activity="running"\]\s*\{[^}]*box-shadow:\s*none[^}]*animation-duration:\s*1200ms/s,
  );
  assert.match(
    css,
    /\.nav-row-wrap:not\(\.selected\) \.task-activity-indicator\[data-task-activity="finalizing"\]\s*\{[^}]*box-shadow:\s*none[^}]*animation-duration:\s*1900ms/s,
  );
});

test("responsive output visibility owns its cascade", () => {
  const css = readFileSync(new URL("../src/styles.css", import.meta.url), "utf8");

  const baseToggle = css.match(/\.icon-button\.responsive-output-toggle\s*\{([^}]*)\}/)?.[1];
  assert.ok(baseToggle);
  assert.match(baseToggle, /display:\s*none/);
  const responsiveStart = css.indexOf("@media (max-width: 1220px)");
  assert.notEqual(responsiveStart, -1);
  const responsiveCss = css.slice(responsiveStart);
  assert.match(
    responsiveCss,
    /\.icon-button\.responsive-output-toggle\s*\{[^}]*display:\s*grid/s,
  );
});

test("history prepend thread fallback has an unclipped keyboard focus indicator", () => {
  const css = readFileSync(new URL("../src/styles.css", import.meta.url), "utf8");
  const rule = css.match(/\.thread:focus-visible\s*\{([^}]*)\}/)?.[1];

  assert.ok(rule);
  assert.match(rule, /outline:\s*2px solid rgba\(141, 185, 255, 0\.9\)/);
  assert.match(rule, /outline-offset:\s*-2px/);
});

test("initial setup primary action has a distinct accessible contrast treatment", () => {
  const css = readFileSync(new URL("../src/styles.css", import.meta.url), "utf8");
  const rule = css.match(
    /\.settings-header-actions button\.setup-primary-action,\s*\.split-actions button\.setup-primary-action\s*\{([^}]*)\}/,
  )?.[1];
  assert.ok(rule);
  const background = rule.match(/background:\s*(#[0-9a-f]{6})/i)?.[1];
  const foreground = rule.match(/(?:^|\n)\s*color:\s*(#[0-9a-f]{6})/i)?.[1];
  assert.ok(background);
  assert.ok(foreground);
  assert.ok(contrastRatio(foreground, background) >= 4.5);
  assert.match(rule, /font-weight:\s*650/);
});

test("agent inspector long names wrap without changing compact preview rows", () => {
  const css = readFileSync(new URL("../src/styles.css", import.meta.url), "utf8");
  const listNameRule = css.match(
    /\.agent-inspector-list \.sub-agent-list-card \.agent-job-copy strong\s*\{([^}]*)\}/,
  )?.[1];
  const detailNameRule = css.match(/\.agent-pane-identity strong\s*\{([^}]*)\}/)?.[1];
  const compactCopyRule = css.match(
    /\.agent-job-copy,\s*\.agent-job-copy strong,\s*\.agent-job-copy small\s*\{([^}]*)\}/,
  )?.[1];

  for (const rule of [listNameRule, detailNameRule]) {
    assert.ok(rule);
    assert.match(rule, /overflow-wrap:\s*anywhere/);
    assert.match(rule, /text-overflow:\s*clip/);
    assert.match(rule, /white-space:\s*normal/);
  }
  assert.ok(compactCopyRule);
  assert.match(compactCopyRule, /text-overflow:\s*ellipsis/);
  assert.match(compactCopyRule, /white-space:\s*nowrap/);
});

test("agent execution content wraps unbroken identities inside the narrow pane", () => {
  const css = readFileSync(new URL("../src/styles.css", import.meta.url), "utf8");
  const scrollRule = css.match(/\.agent-execution-scroll\s*\{([^}]*)\}/)?.[1];
  const messageRule = css.match(/\.agent-execution-scroll \.message\s*\{([^}]*)\}/)?.[1];
  const markdownRule = [...css.matchAll(/\.agent-execution-scroll \.markdown-body\s*\{([^}]*)\}/g)]
    .map((match) => match[1])
    .find((rule) => /overflow-wrap:/.test(rule));
  const preRule = css.match(/\.agent-execution-scroll \.markdown-body pre\s*\{([^}]*)\}/)?.[1];

  assert.ok(scrollRule);
  assert.match(scrollRule, /min-width:\s*0/);
  assert.ok(messageRule);
  assert.match(messageRule, /max-width:\s*100%/);
  assert.ok(markdownRule);
  assert.match(markdownRule, /overflow-wrap:\s*anywhere/);
  assert.match(markdownRule, /word-break:\s*break-word/);
  assert.ok(preRule);
  assert.match(preRule, /max-width:\s*100%/);
  assert.match(preRule, /overflow-x:\s*auto/);
});

test("agent pane header keeps both action groups inside a narrow drawer", () => {
  const css = readFileSync(new URL("../src/styles.css", import.meta.url), "utf8");
  const headerRule = css.match(/\.agent-pane-title\s*\{([^}]*)\}/)?.[1];

  assert.ok(headerRule);
  assert.match(headerRule, /grid-template-columns:\s*auto minmax\(0, 1fr\) auto/);
  assert.match(
    css,
    /\.agent-pane-title > \.pin,\s*\.agent-pane-title > \.pane-actions\s*\{[^}]*justify-self:\s*end/,
  );
});

test("responsive output drawer leaves the main composer controls pointer-visible", () => {
  const css = readFileSync(new URL("../src/styles.css", import.meta.url), "utf8");
  const composerRule = css.match(
    /\.app-frame:not\(\.artifact-collapsed\):not\(\.side-chat-open\) \.composer\s*\{([^}]*)\}/,
  )?.[1];

  assert.match(css, /--responsive-pane-width:\s*min\(352px, calc\(100vw - 24px\)\)/);
  assert.ok(composerRule);
  assert.match(composerRule, /left:\s*calc\(\(100% - var\(--responsive-pane-width\)\) \/ 2\)/);
  assert.match(
    composerRule,
    /width:\s*min\(760px, calc\(100% - var\(--responsive-pane-width\) - 70px\)\)/,
  );
  assert.match(
    css,
    /\.app-frame:not\(\.artifact-collapsed\):not\(\.side-chat-open\) \.run-strip\s*\{[^}]*padding-right:\s*calc\(var\(--responsive-pane-width\) \+ 28px\)/,
  );
});

function contrastRatio(foreground: string, background: string): number {
  const foregroundLuminance = relativeLuminance(foreground);
  const backgroundLuminance = relativeLuminance(background);
  return (Math.max(foregroundLuminance, backgroundLuminance) + 0.05)
    / (Math.min(foregroundLuminance, backgroundLuminance) + 0.05);
}

function relativeLuminance(hex: string): number {
  const channels = [1, 3, 5].map((offset) => Number.parseInt(hex.slice(offset, offset + 2), 16) / 255);
  const [red, green, blue] = channels.map((channel) =>
    channel <= 0.04045 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4
  );
  return 0.2126 * red + 0.7152 * green + 0.0722 * blue;
}

class AttributeTarget {
  private readonly attributes = new Map<string, string>();

  constructor(attributes: Record<string, string> = {}) {
    for (const [name, value] of Object.entries(attributes)) this.attributes.set(name, value);
  }

  setAttribute(name: string, value: string): void {
    this.attributes.set(name, value);
  }

  removeAttribute(name: string): void {
    this.attributes.delete(name);
  }

  toggleAttribute(name: string, force: boolean): boolean {
    if (force) this.attributes.set(name, "");
    else this.attributes.delete(name);
    return force;
  }

  hasAttribute(name: string): boolean {
    return this.attributes.has(name);
  }

  getAttribute(name: string): string | null {
    return this.attributes.get(name) ?? null;
  }
}
