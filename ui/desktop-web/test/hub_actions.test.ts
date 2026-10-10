import assert from "node:assert/strict";
import test from "node:test";
import type { ActionContext } from "../src/actions.ts";
import { connectHub, refreshHub, saveHubReview, setHubRouteMode } from "../src/hub_actions.ts";
import { acceptHubProjection, createHubUiState, editHubField, hubCanSave, type HubProjection, type HubSelection } from "../src/hub_state.ts";

function projection(overrides: Partial<HubProjection> = {}): HubProjection {
  return {
    settings_revision: "0", connection_generation: "0", status: "disconnected",
    endpoint: "http://127.0.0.1:9470", label: "moyAI Desktop", hub_id: null,
    catalog: null, main_review: null, side_chat_review: null, approve_review: null,
    main_confirmation: "unconfirmed", side_chat_confirmation: "unconfirmed", approve_confirmation: "unconfirmed", error: null,
    main_mode: "direct", side_chat_mode: "direct", approve_mode: "direct", active_main: null, active_side_chat: null, active_approve: null,
    can_enable_main_hub: false, can_enable_side_chat_hub: false, can_enable_approve_hub: false,
    can_change_main_mode: true, can_change_side_chat_mode: true, can_change_approve_mode: true,
    ...overrides,
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((accept, fail) => { resolve = accept; reject = fail; });
  return { promise, resolve, reject };
}

async function withContext(
  invoke: (name: string, args: Record<string, unknown>) => Promise<HubProjection>,
  run: (fixture: ReturnType<typeof fixtureContext>) => Promise<void>,
): Promise<void> {
  const fixture = fixtureContext();
  const previousWindow = Object.getOwnPropertyDescriptor(globalThis, "window");
  const previousDocument = Object.getOwnPropertyDescriptor(globalThis, "document");
  Object.defineProperty(globalThis, "window", { configurable: true, value: { __TAURI_INTERNALS__: { invoke } } });
  Object.defineProperty(globalThis, "document", { configurable: true, value: {
    querySelector: (selector: string) => selector === "#hub-token" ? fixture.token : null,
  } });
  try { await run(fixture); }
  finally {
    if (previousWindow) Object.defineProperty(globalThis, "window", previousWindow);
    else delete (globalThis as Record<string, unknown>).window;
    if (previousDocument) Object.defineProperty(globalThis, "document", previousDocument);
    else delete (globalThis as Record<string, unknown>).document;
  }
}

function fixtureContext() {
  const local = createHubUiState();
  acceptHubProjection(local, projection());
  const token = { value: "first-token" };
  const view = { overlay: "hub" };
  let renders = 0;
  const context = {
    uiState: { hub: local }, getViewState: () => view,
    rerender: () => { renders += 1; },
  } as unknown as ActionContext;
  return { context, local, token, view, renderCount: () => renders };
}

test("route mode save sends the exact channel and revision without optimistic fallback or discarding reviews", async () => {
  const saved = deferred<HubProjection>();
  const calls: { name: string; args: Record<string, unknown> }[] = [];
  await withContext(async (name, args) => { calls.push({ name, args }); return saved.promise; }, async ({ context, local }) => {
    acceptHubProjection(local, projection({ main_mode: "hub", settings_revision: "5", connection_generation: "7" }));
    editHubField(local, "side_chat:affinity", "8", false);
    const sideDraft = structuredClone(local.drafts.side_chat);
    const request = setHubRouteMode(context, "main", "direct");
    assert.equal(local.projection!.main_mode, "hub");
    assert.equal(local.pending, "main_mode");
    assert.deepEqual(calls, [{ name: "hub_set_route_mode", args: {
      context: "main", mode: "direct", expectedSettingsRevision: "5", expectedConnectionGeneration: "7",
    } }]);
    saved.resolve(projection({ main_mode: "direct", settings_revision: "6", connection_generation: "7" }));
    await request;
    assert.equal(local.projection!.main_mode, "direct");
    assert.deepEqual(local.drafts.side_chat, sideDraft);
  });
});

test("delayed catalog refresh keeps newer runtime start and stop projections for every role", async () => {
  for (const channel of ["main", "side_chat", "approve"] as const) {
    for (const started of [true, false]) {
      const refresh = deferred<HubProjection>();
      await withContext(async () => refresh.promise, async ({ context, local }) => {
        const active = { turn_id: "live-turn", phase: "running" as const, logical_model_id: "model-a" };
        const before = projection({ status: "connected", hub_id: "hub-a", connection_generation: "2",
          catalog: { hub_id: "hub-a", software_version: "1", revision: "3", changes: [], models: [] },
          [`active_${channel}`]: started ? null : active, [`can_change_${channel}_mode`]: started });
        acceptHubProjection(local, before);
        const request = refreshHub(context);
        acceptHubProjection(local, { ...before, [`active_${channel}`]: started ? active : null,
          [`can_change_${channel}_mode`]: !started });
        refresh.resolve({ ...before, catalog: { ...before.catalog!, revision: "4" } });
        await request;
        assert.deepEqual(local.projection![`active_${channel}`], started ? active : null);
        assert.equal(local.projection![`can_change_${channel}_mode`], !started);
        assert.equal(local.projection!.catalog?.revision, "4");
      });
    }
  }
});

test("an in-flight refresh cannot restore a run after start and stop return to the initial state", async () => {
  for (const channel of ["main", "side_chat", "approve"] as const) {
    const refresh = deferred<HubProjection>();
    await withContext(async () => refresh.promise, async ({ context, local }) => {
      const before = projection({ status: "connected", hub_id: "hub-a", connection_generation: "2" });
      const running = { ...before, [`active_${channel}`]: {
        turn_id: "ended-turn", phase: "running" as const, logical_model_id: "model-a",
      }, [`can_change_${channel}_mode`]: false };
      acceptHubProjection(local, before);
      const request = refreshHub(context);
      acceptHubProjection(local, running);
      acceptHubProjection(local, structuredClone(before));
      refresh.resolve(running);
      await request;
      assert.equal(local.projection![`active_${channel}`], null);
      assert.equal(local.projection![`can_change_${channel}_mode`], true);
    });
  }
  await withContext(async () => projection({ status: "connected", hub_id: "hub-a", connection_generation: "2",
    active_main: { turn_id: "new-turn", phase: "running", logical_model_id: "model-a" }, can_change_main_mode: false }),
  async ({ context, local }) => {
    acceptHubProjection(local, projection({ status: "connected", hub_id: "hub-a", connection_generation: "2" }));
    await refreshHub(context);
    assert.equal(local.projection!.active_main?.turn_id, "new-turn", "a read without an intervening projection still adopts its runtime");
  });
});

test("both edited contexts can be saved in either order without a catalog request", async () => {
  for (const order of [["main", "side_chat"], ["side_chat", "main"]] as const) {
    const calls: { name: string; args: Record<string, unknown> }[] = [];
    let remote = projection({ status: "connected", settings_revision: "1", connection_generation: "2", hub_id: "hub-a",
      catalog: { hub_id: "hub-a", software_version: "0.1.0", revision: "7", changes: [],
        models: [{ id: "model-a", label: "Model A", capabilities: [] }] } });
    await withContext(async (name, args) => {
      calls.push({ name, args });
      assert.equal(name, "hub_save_review");
      assert.equal(args.expectedSettingsRevision, remote.settings_revision);
      assert.equal(args.expectedCatalogRevision, "7");
      const channel = args.context as "main" | "side_chat";
      remote = { ...remote, settings_revision: String(BigInt(remote.settings_revision) + 1n),
        [`${channel}_review`]: { hub_id: "hub-a", reviewed_revision: "7", selection: args.selection as HubSelection },
        [`${channel}_confirmation`]: "confirmed" };
      return structuredClone(remote);
    }, async ({ context, local }) => {
      acceptHubProjection(local, remote);
      for (const channel of order) editHubField(local, `${channel}:model:model-a`, "", true);
      editHubField(local, `${order[1]}:affinity`, "9", false);
      await saveHubReview(context, order[0]);
      assert.equal(local.drafts[order[1]].affinityText, "9");
      assert.equal(hubCanSave(local, order[0]), false);
      assert.equal(hubCanSave(local, order[1]), true);
      await saveHubReview(context, order[1]);
      assert.equal(calls.length, 2);
      assert.equal(local.drafts.main.dirty, false);
      assert.equal(local.drafts.side_chat.dirty, false);
      assert.equal(local.projection!.main_confirmation, "confirmed");
      assert.equal(local.projection!.side_chat_confirmation, "confirmed");
      await saveHubReview(context, order[1]);
      assert.equal(calls.length, 2, "unchanged confirmed selection cannot be saved repeatedly");
    });
  }
});

test("acknowledged route mode save keeps a separately edited review ready without network refresh", async () => {
  let remote = projection({ status: "connected", settings_revision: "4", connection_generation: "2", hub_id: "hub-a", main_mode: "hub",
    catalog: { hub_id: "hub-a", software_version: "0.1.0", revision: "7", changes: [], models: [{ id: "model-a", label: "Model A", capabilities: [] }] } });
  await withContext(async (name) => {
    assert.equal(name, "hub_set_route_mode");
    return { ...remote, main_mode: "direct", settings_revision: "5" };
  }, async ({ context, local }) => {
    acceptHubProjection(local, remote);
    editHubField(local, "side_chat:model:model-a", "", true);
    await setHubRouteMode(context, "main", "direct");
    assert.equal(hubCanSave(local, "side_chat"), true);
    assert.deepEqual(local.drafts.side_chat.selection.allowed_model_ids, ["model-a"]);
  });
});

test("refreshing unchanged Hub defaults leaves Side ready after a Main save and poll", async () => {
  const recommendation: HubSelection = { allowed_model_ids: ["model-a", "model-b"], preferred_model_id: "model-a",
    required_capabilities: ["tools"], wait_policy: "allow_selected_fallback", affinity_turns: 3 };
  let remote = projection({ status: "connected", settings_revision: "1", connection_generation: "2", hub_id: "hub-a",
    catalog: { hub_id: "hub-a", software_version: "0.1.0", revision: "7", changes: [], models: ["model-a", "model-b"].map(id => ({id,label:id,capabilities:["tools"]})) },
    main_mode: "hub", side_chat_mode: "hub", main_confirmation: "confirmed", side_chat_confirmation: "confirmed",
    main_uses_default: true, side_chat_uses_default: true, recommended_main_selection: recommendation,
    main_review: {hub_id:"hub-a",reviewed_revision:"7",selection:structuredClone(recommendation)},
    side_chat_review: {hub_id:"hub-a",reviewed_revision:"7",selection:structuredClone(recommendation)},
  });
  const calls: {name:string;args:Record<string,unknown>}[] = [];
  let poll: (p:HubProjection)=>void = () => {};
  await withContext(async (name,args) => {
    calls.push({name,args});
    if (name === "hub_refresh") return structuredClone(remote);
    assert.equal(name,"hub_save_review");
    assert.equal(args.expectedSettingsRevision,remote.settings_revision);
    const channel = args.context as "main" | "side_chat";
    remote = {...remote,settings_revision:String(BigInt(remote.settings_revision)+1n),
      [`${channel}_review`]:{hub_id:"hub-a",reviewed_revision:"7",selection:args.selection as HubSelection},[`${channel}_uses_default`]:false};
    poll(structuredClone(remote));
    return structuredClone(remote);
  },async ({context,local,view}) => {
    view.overlay = "config";
    acceptHubProjection(local,remote);
    await refreshHub(context);
    assert.equal(local.drafts.side_chat.dirty,false,"refreshing the same default is not a Side edit");
    poll = p => acceptHubProjection(local,p);
    editHubField(local,"main:choice","model-a",false);
    await saveHubReview(context,"main");
    assert.equal(local.drafts.side_chat.target?.expectedSettingsRevision,"2");
    editHubField(local,"side_chat:choice","model-b",false);
    assert.equal(hubCanSave(local,"side_chat"),true);
    await saveHubReview(context,"side_chat");
    assert.deepEqual(calls.map(call=>call.name),["hub_refresh","hub_save_review","hub_save_review"]);
    assert.equal(local.projection?.side_chat_review?.selection.preferred_model_id,"model-b");
    assert.equal(local.projection?.side_chat_uses_default,false);
  });
});

test("failed review save retains both drafts and does not treat recovery as a local successful save", async () => {
  const before = projection({ status: "connected", settings_revision: "4", connection_generation: "2", hub_id: "hub-a",
    catalog: { hub_id: "hub-a", software_version: "0.1.0", revision: "7", changes: [], models: [{ id: "model-a", label: "Model A", capabilities: [] }] } });
  await withContext(async (name) => {
    if (name === "hub_save_review") throw "settings_changed";
    assert.equal(name, "hub_projection");
    return { ...before, settings_revision: "5" };
  }, async ({ context, local }) => {
    acceptHubProjection(local, before);
    editHubField(local, "main:model:model-a", "", true);
    editHubField(local, "side_chat:model:model-a", "", true);
    editHubField(local, "approve:model:model-a", "", true);
    const drafts = structuredClone(local.drafts);
    await saveHubReview(context, "main");
    assert.deepEqual(local.drafts, drafts);
    assert.equal(hubCanSave(local, "side_chat"), false);
    assert.equal(local.errorContext, "main");
  });
});

test("route activation is not sent before review or while the backend closes its admission", async () => {
  const calls: string[] = [];
  await withContext(async (name) => { calls.push(name); return projection(); }, async ({ context, local }) => {
    await setHubRouteMode(context, "main", "hub");
    acceptHubProjection(local, projection({ status: "connected", main_confirmation: "confirmed", can_enable_main_hub: true, can_change_main_mode: false }));
    await setHubRouteMode(context, "main", "hub");
    assert.deepEqual(calls, []);
  });
});

test("Main, Sub and Approve selections save in every order with exact independent command targets", async () => {
  const orders = [["main", "side_chat", "approve"], ["main", "approve", "side_chat"],
    ["side_chat", "main", "approve"], ["side_chat", "approve", "main"],
    ["approve", "main", "side_chat"], ["approve", "side_chat", "main"]] as const;
  const ids = { main: "main-model", side_chat: "sub-model", approve: "approve-model" };
  for (const order of orders) {
    let remote = projection({ status: "connected", settings_revision: "1", connection_generation: "2", hub_id: "hub-a",
      catalog: { hub_id: "hub-a", software_version: "0.1.0", revision: "7", changes: [],
        models: Object.values(ids).map(id => ({ id, label: id, capabilities: [] })) } });
    const calls: string[] = [];
    await withContext(async (name, args) => {
      assert.equal(name, "hub_save_review");
      const channel = args.context as typeof order[number];
      calls.push(channel);
      assert.equal(args.expectedSettingsRevision, remote.settings_revision);
      assert.equal(args.expectedConnectionGeneration, "2");
      assert.equal(args.expectedHubId, "hub-a");
      assert.equal(args.expectedCatalogRevision, "7");
      assert.equal((args.selection as HubSelection).preferred_model_id, ids[channel]);
      remote = { ...remote, settings_revision: String(Number(remote.settings_revision) + 1),
        [`${channel}_review`]: { hub_id: "hub-a", reviewed_revision: "7", selection: structuredClone(args.selection) },
        [`${channel}_confirmation`]: "confirmed" };
      return remote;
    }, async ({ context, local }) => {
      acceptHubProjection(local, remote);
      for (const channel of order) editHubField(local, `${channel}:choice`, ids[channel], false);
      for (const channel of order) { assert.equal(hubCanSave(local, channel), true); await saveHubReview(context, channel); }
      assert.deepEqual(calls, order);
      for (const channel of order) {
        assert.equal(local.projection![`${channel}_review`]!.selection.preferred_model_id, ids[channel]);
        assert.equal(local.drafts[channel].dirty, false);
      }
    });
  }
});

test("Approve route mode sends its own target and an active review blocks its selection", async () => {
  const calls: { name: string; args: Record<string, unknown> }[] = [];
  await withContext(async (name, args) => { calls.push({ name, args }); return projection({ settings_revision: "6", connection_generation: "7" }); }, async ({ context, local }) => {
    acceptHubProjection(local, projection({ approve_mode: "hub", settings_revision: "5", connection_generation: "7" }));
    await setHubRouteMode(context, "approve", "direct");
    assert.deepEqual(calls, [{ name: "hub_set_route_mode", args: {
      context: "approve", mode: "direct", expectedSettingsRevision: "5", expectedConnectionGeneration: "7",
    } }]);
    const current = projection({ status: "connected", hub_id: "hub-a",
      catalog: { hub_id: "hub-a", software_version: "0.1.0", revision: "7", changes: [], models: [{ id: "approve-model", label: "Approve", capabilities: [] }] } });
    acceptHubProjection(local, { ...current, settings_revision: "6", connection_generation: "7" });
    editHubField(local, "approve:choice", "approve-model", false);
    acceptHubProjection(local, { ...local.projection!, active_approve: { turn_id: "review-turn", phase: "running", logical_model_id: "approve-model" } });
    await saveHubReview(context, "approve");
    assert.equal(calls.length, 1);
    assert.equal(local.drafts.approve.dirty, true);
  });
});

test("failed Hub connect retains pending until the fresh owner arrives and permits immediate corrected-token retry", async () => {
  const recovery = deferred<HubProjection>();
  const calls: { name: string; args: Record<string, unknown> }[] = [];
  await withContext(async (name, args) => {
    calls.push({ name, args });
    if (name === "hub_projection") return recovery.promise;
    if (args.token === "first-token") throw "unauthorized";
    assert.equal(args.expectedConnectionGeneration, "1");
    return projection({ connection_generation: "2", status: "connected" });
  }, async ({ context, local, token }) => {
    editHubField(local, "endpoint", "127.0.0.1:9500", false);
    editHubField(local, "label", "編集中の端末名", false);
    const failed = connectHub(context);
    await new Promise((resolve) => setImmediate(resolve));
    assert.equal(local.pending, "connect", "retry stays locked until the rejected request's new owner is acquired");
    assert.deepEqual(calls.map((call) => call.name), ["hub_connect", "hub_projection"]);
    await connectHub(context);
    assert.equal(calls.length, 2, "a second request cannot bypass recovery settlement");
    recovery.resolve(projection({ connection_generation: "1", status: "error", error: "unauthorized" }));
    await failed;
    assert.equal(local.pending, null);
    assert.equal(local.projection?.connection_generation, "1");
    assert.equal(local.endpoint, "127.0.0.1:9500");
    assert.equal(local.label, "編集中の端末名");
    assert.equal(token.value, "first-token");
    assert.match(local.error, /参加許可/);
    token.value = "corrected-token";
    await connectHub(context);
    assert.equal(calls.length, 3);
    assert.equal(local.projection?.status, "connected");
    assert.equal(token.value, "");
  });
});

test("failed refresh reacquires connection state without rebasing a dirty review target", async () => {
  await withContext(async (name) => {
    if (name === "hub_refresh") throw "unavailable";
    return projection({ settings_revision: "9", connection_generation: "5", status: "stale" });
  }, async ({ context, local, token }) => {
    acceptHubProjection(local, projection({
      connection_generation: "4", settings_revision: "8", status: "connected", hub_id: "hub-a",
      catalog: { hub_id: "hub-a", software_version: "0.1.0", revision: "3", models: [], changes: [] },
    }));
    editHubField(local, "main:affinity", "7", false);
    const originalTarget = structuredClone(local.drafts.main.target);
    await refreshHub(context);
    assert.equal(local.projection?.connection_generation, "5");
    assert.equal(local.drafts.main.affinityText, "7");
    assert.equal(local.drafts.main.dirty, true);
    assert.deepEqual(local.drafts.main.target, originalTarget);
    assert.equal(token.value, "first-token");
    assert.match(local.error, /接続できません/);
  });
});

test("a delayed failure-recovery projection cannot overwrite a newer request or its draft", async () => {
  const recovery = deferred<HubProjection>();
  await withContext(async (name) => {
    if (name === "hub_connect") throw "unauthorized";
    return recovery.promise;
  }, async ({ context, local, token }) => {
    const failed = connectHub(context);
    await new Promise((resolve) => setImmediate(resolve));
    assert.equal(local.pending, "connect");
    local.requestSerial += 1;
    local.pending = "refresh";
    local.endpoint = "127.0.0.1:9600";
    local.connectionTouched = true;
    local.drafts.main.affinityText = "11";
    local.drafts.main.dirty = true;
    local.error = "newer request status";
    const accepted = projection({ connection_generation: "4", status: "connected" });
    acceptHubProjection(local, accepted);
    recovery.resolve(projection({ connection_generation: "99", status: "error" }));
    await failed;
    assert.equal(local.projection, accepted);
    assert.equal(local.pending, "refresh");
    assert.equal(local.endpoint, "127.0.0.1:9600");
    assert.equal(local.drafts.main.affinityText, "11");
    assert.equal(local.error, "newer request status");
    assert.equal(token.value, "first-token");
  });
});

test("failed recovery retains the original connection error and input without repeated local loads", async () => {
  const calls: string[] = [];
  await withContext(async (name) => {
    calls.push(name);
    throw name === "hub_connect" ? "unauthorized" : "settings_unavailable";
  }, async ({ context, local, token }) => {
    editHubField(local, "endpoint", "127.0.0.1:9500", false);
    await connectHub(context);
    assert.deepEqual(calls, ["hub_connect", "hub_projection"]);
    assert.match(local.error, /参加許可/);
    assert.equal(local.endpoint, "127.0.0.1:9500");
    assert.equal(token.value, "first-token");
    assert.equal(local.pending, null);
  });
});

test("successful device import retires a failed Hub refresh without another Hub form action", async () => {
  const offline = projection({ status: "error", error: "unavailable", connection_generation: "1", main_mode: "hub", side_chat_mode: "hub" });
  await withContext(async (name) => {
    if (name === "hub_refresh") throw "unavailable";
    assert.equal(name, "hub_projection");
    return offline;
  }, async ({ context, local }) => {
    acceptHubProjection(local, offline);
    await refreshHub(context);
    assert.equal(local.errorCode, "unavailable");
    assert.equal(local.errorContext, "connection");
    assert.notEqual(local.error, "");
    const selection: HubSelection = { allowed_model_ids: ["model-a"], preferred_model_id: "model-a",
      required_capabilities: [], wait_policy: "wait_for_preferred", affinity_turns: 1 };
    const review = { hub_id: "hub-a", reviewed_revision: "1", selection };
    acceptHubProjection(local, projection({ status: "connected", settings_revision: "1", connection_generation: "2",
      hub_id: "hub-a", catalog: { hub_id: "hub-a", software_version: "0.1.0", revision: "1", changes: [],
        models: [{ id: "model-a", label: "Model A", capabilities: [] }] },
      main_mode: "hub", side_chat_mode: "hub", main_review: review, side_chat_review: review,
      main_uses_default: true, side_chat_uses_default: true, main_confirmation: "confirmed", side_chat_confirmation: "confirmed",
      can_enable_main_hub: true, can_enable_side_chat_hub: true,
    }));
    assert.equal(local.error, "");
    assert.equal(local.errorCode, null);
    assert.equal(local.drafts.main.dirty, false);
    assert.equal(hubCanSave(local, "main"), false);
  });
});

test("recovery respects the active overlay and a failed projection load does not recurse", async () => {
  const recovery = deferred<HubProjection>();
  await withContext(async (name) => {
    if (name === "hub_connect") throw "unauthorized";
    return recovery.promise;
  }, async ({ context, local, view }) => {
    const failed = connectHub(context);
    await new Promise((resolve) => setImmediate(resolve));
    view.overlay = "none";
    const previous = local.projection;
    recovery.resolve(projection({ connection_generation: "1", status: "error" }));
    await failed;
    assert.equal(local.projection, previous);
    assert.equal(local.pending, null, "the settled request releases its lock even after the overlay leaves");
  });
  const calls: string[] = [];
  await withContext(async (name) => { calls.push(name); throw "settings_unavailable"; }, async ({ context, local }) => {
    await refreshHub(context);
    assert.deepEqual(calls, ["hub_projection"], "a failed local load cannot recursively reload itself");
    assert.match(local.error, /保存できません/);
    assert.equal(local.pending, null);
  });
});
