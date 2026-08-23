export type FocusIntentPriority =
  | "modal-containment"
  | "explicit-transfer"
  | "exact-restore"
  | "operation-return"
  | "pane-navigation"
  | "fallback";

export type FocusIntentSource =
  | "modal-primary"
  | "new-session"
  | "command-palette"
  | "settings-action"
  | "titlebar-menu"
  | "modal-return"
  | "focus-snapshot"
  | "history-prepend"
  | "agent-execution-prepend"
  | "attachment"
  | "quick-chat-delete"
  | "main-run"
  | "side-chat"
  | "refresh-prompt"
  | "agent-pane"
  | "artifact-pane"
  | "composer-request"
  | "initial-composer";

/**
 * Structural focus target used by the arbiter. Keeping this as a small port instead of requiring
 * an `HTMLElement` lets the arbitration contract be tested without constructing a DOM fixture.
 */
export interface FocusTargetElement {
  readonly isConnected: boolean;
  readonly disabled?: boolean;
  readonly hidden?: boolean;
  readonly inert?: boolean;
  matches?(selector: string): boolean;
  getAttribute?(name: string): string | null;
  closest?(selector: string): unknown | null;
  focus(options?: { preventScroll?: boolean }): void;
}

export interface FocusTargetCandidate {
  /** Resolve after the committed markup is connected, never against the pre-render DOM. */
  resolve(): FocusTargetElement | null;
  /** Selection, scroll, or roving-tabindex settlement for this exact target. Must not focus. */
  settle?(target: FocusTargetElement): void;
}

export type FocusClaim =
  | { readonly kind: "unowned" }
  | {
      readonly kind: "yield-from";
      readonly owners: readonly FocusTargetElement[];
    }
  | { readonly kind: "force" };

export interface PostRenderFocusIntent {
  readonly source: FocusIntentSource;
  readonly priority: FocusIntentPriority;
  readonly claim: FocusClaim;
  readonly candidates: readonly FocusTargetCandidate[];
  /** Revalidate the domain's exact owner after the animation-frame boundary. */
  readonly isCurrent: () => boolean;
}

export interface FocusArbiterCommit {
  readonly renderCommit: number;
  readonly interactionEpoch: bigint;
  readonly intents: readonly PostRenderFocusIntent[];
  readonly onResult?: (result: FocusArbiterResult) => void;
}

export type FocusArbiterResultKind =
  | "focused"
  | "already-focused"
  | "no-intent"
  | "superseded"
  | "stale-render"
  | "stale-interaction"
  | "interaction-active"
  | "stale-intent"
  | "owned"
  | "unavailable";

export interface FocusArbiterResult {
  readonly kind: FocusArbiterResultKind;
  readonly source: FocusIntentSource | null;
}

export interface FocusArbiterScheduler<Handle> {
  schedule(callback: () => void): Handle;
  cancel(handle: Handle): void;
}

export interface FocusArbiterEnvironment {
  currentRenderCommit(): number;
  currentInteractionEpoch(): bigint;
  interactionActive(): boolean;
  activeElement(): FocusTargetElement | null;
  bodyElement(): FocusTargetElement | null;
  documentElement(): FocusTargetElement | null;
}

interface PendingFocusCommit<Handle> {
  readonly token: object;
  readonly commit: FocusArbiterCommit;
  handle: Handle | null;
}

const FOCUS_PRIORITY_RANK: Readonly<Record<FocusIntentPriority, number>> = Object.freeze({
  "modal-containment": 6,
  "explicit-transfer": 5,
  "exact-restore": 4,
  "operation-return": 3,
  "pane-navigation": 2,
  fallback: 1,
});

/**
 * The sole post-render focus writer.
 *
 * Arbitration selects one intent before checking its owner or target. A stale or unavailable
 * winning intent therefore cannot accidentally activate an unrelated lower-priority fallback.
 * Candidate fallback is allowed only inside that winning intent and only before a focus attempt.
 */
export class PostRenderFocusArbiter<Handle> {
  private pending: PendingFocusCommit<Handle> | null = null;
  private readonly scheduler: FocusArbiterScheduler<Handle>;
  private readonly environment: FocusArbiterEnvironment;

  constructor(
    scheduler: FocusArbiterScheduler<Handle>,
    environment: FocusArbiterEnvironment,
  ) {
    this.scheduler = scheduler;
    this.environment = environment;
  }

  schedule(commit: FocusArbiterCommit): void {
    this.cancelPending("superseded");
    const pending: PendingFocusCommit<Handle> = {
      token: {},
      commit,
      handle: null,
    };
    this.pending = pending;
    const handle = this.scheduler.schedule(() => this.execute(pending));
    if (this.pending?.token === pending.token) pending.handle = handle;
  }

  cancel(): void {
    this.cancelPending("superseded");
  }

  private execute(pending: PendingFocusCommit<Handle>): void {
    if (this.pending?.token !== pending.token) return;
    this.pending = null;

    const intent = highestPriorityIntent(pending.commit.intents);
    if (!intent) {
      settleCommit(pending.commit, "no-intent", null);
      return;
    }
    if (this.environment.currentRenderCommit() !== pending.commit.renderCommit) {
      settleCommit(pending.commit, "stale-render", intent.source);
      return;
    }
    if (this.environment.currentInteractionEpoch() !== pending.commit.interactionEpoch) {
      settleCommit(pending.commit, "stale-interaction", intent.source);
      return;
    }
    if (this.environment.interactionActive()) {
      settleCommit(pending.commit, "interaction-active", intent.source);
      return;
    }
    if (!intent.isCurrent()) {
      settleCommit(pending.commit, "stale-intent", intent.source);
      return;
    }

    const targetCandidate = firstEligibleCandidate(intent.candidates);
    if (!targetCandidate) {
      settleCommit(pending.commit, "unavailable", intent.source);
      return;
    }
    const target = targetCandidate.target;
    const active = this.environment.activeElement();
    if (active === target) {
      targetCandidate.candidate.settle?.(target);
      settleCommit(pending.commit, "already-focused", intent.source);
      return;
    }
    if (!focusClaimAllowed(intent.claim, active, this.environment)) {
      settleCommit(pending.commit, "owned", intent.source);
      return;
    }

    // Exactly one focus call is allowed for a render commit. If the browser refuses it, do not
    // try another candidate or another intent; a later render/user action must create a new owner.
    target.focus({ preventScroll: true });
    if (this.environment.activeElement() !== target) {
      settleCommit(pending.commit, "unavailable", intent.source);
      return;
    }
    targetCandidate.candidate.settle?.(target);
    settleCommit(pending.commit, "focused", intent.source);
  }

  private cancelPending(kind: Extract<FocusArbiterResultKind, "superseded">): void {
    const pending = this.pending;
    if (!pending) return;
    this.pending = null;
    if (pending.handle !== null) this.scheduler.cancel(pending.handle);
    const source = highestPriorityIntent(pending.commit.intents)?.source ?? null;
    settleCommit(pending.commit, kind, source);
  }
}

export function animationFrameFocusScheduler(
  target: Pick<Window, "requestAnimationFrame" | "cancelAnimationFrame">,
): FocusArbiterScheduler<number> {
  return {
    schedule: (callback) => target.requestAnimationFrame(callback),
    cancel: (handle) => target.cancelAnimationFrame(handle),
  };
}

export function focusTargetEligible(target: FocusTargetElement | null): target is FocusTargetElement {
  if (!target || !target.isConnected || target.disabled === true || target.hidden === true || target.inert === true) {
    return false;
  }
  if (safeMatches(target, ":disabled")) return false;
  if (normalizedAttribute(target, "aria-disabled") === "true") return false;
  if (normalizedAttribute(target, "aria-hidden") === "true") return false;
  if (hasAttribute(target, "hidden")) return false;
  if (hasAttribute(target, "inert")) return false;
  return !safeClosest(target, "[hidden], [aria-hidden='true'], [inert]");
}

function highestPriorityIntent(
  intents: readonly PostRenderFocusIntent[],
): PostRenderFocusIntent | null {
  let selected: PostRenderFocusIntent | null = null;
  for (const intent of intents) {
    if (
      selected === null
      || FOCUS_PRIORITY_RANK[intent.priority] > FOCUS_PRIORITY_RANK[selected.priority]
    ) {
      selected = intent;
    }
  }
  return selected;
}

function firstEligibleCandidate(
  candidates: readonly FocusTargetCandidate[],
): { candidate: FocusTargetCandidate; target: FocusTargetElement } | null {
  for (const candidate of candidates) {
    const target = candidate.resolve();
    if (focusTargetEligible(target)) return { candidate, target };
  }
  return null;
}

function focusClaimAllowed(
  claim: FocusClaim,
  active: FocusTargetElement | null,
  environment: FocusArbiterEnvironment,
): boolean {
  if (claim.kind === "force") return true;
  if (focusIsUnowned(active, environment)) return true;
  return claim.kind === "yield-from" && claim.owners.includes(active!);
}

function focusIsUnowned(
  active: FocusTargetElement | null,
  environment: FocusArbiterEnvironment,
): boolean {
  return active === null
    || active === environment.bodyElement()
    || active === environment.documentElement()
    || !active.isConnected;
}

function safeMatches(target: FocusTargetElement, selector: string): boolean {
  try {
    return target.matches?.(selector) === true;
  } catch {
    return false;
  }
}

function safeClosest(target: FocusTargetElement, selector: string): boolean {
  try {
    return target.closest?.(selector) != null;
  } catch {
    return false;
  }
}

function normalizedAttribute(target: FocusTargetElement, name: string): string {
  return target.getAttribute?.(name)?.trim().toLowerCase() ?? "";
}

function hasAttribute(target: FocusTargetElement, name: string): boolean {
  const value = target.getAttribute?.(name);
  return value !== null && value !== undefined;
}

function settleCommit(
  commit: FocusArbiterCommit,
  kind: FocusArbiterResultKind,
  source: FocusIntentSource | null,
): void {
  commit.onResult?.({ kind, source });
}
