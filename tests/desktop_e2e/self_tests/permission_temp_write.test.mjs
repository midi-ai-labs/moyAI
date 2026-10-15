import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { EventEmitter } from "node:events";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import path from "node:path";
import { PassThrough } from "node:stream";
import test from "node:test";
import { promisify } from "node:util";
import { fileURLToPath } from "node:url";
import { DesktopE2eError } from "../core/execution.mjs";
import { waitForObservation } from "../core/deadline.mjs";
import * as temp from "../scenarios/permission_temp_write.mjs";
const { PERMISSION_TEMP_WRITE_COMMAND, PERMISSION_TEMP_WRITE_PROMPT, PERMISSION_TEMP_WRITE_RESPONSE,
  PERMISSION_TEMP_WRITE_LIVE_PROMPT, PERMISSION_TEMP_WRITE_LIVE_RESPONSE,
  PERMISSION_TEMP_WRITE_TERMINAL_CONFIRMATION_MS, createPermissionTempWriteTerminalDecision,
  observePermissionTempTerminalDetail, parsePermissionTempWriteCaptureJson,
  permissionTempWriteCaptureFailureObservation, runReadOnlySqlite,
  permissionTempWriteTerminalFailures, permissionTempWriteTerminalFailureEvidence,
  permissionTempWriteWorkspaceFailures, permissionTempWriteLiveFinalFailures,
  permissionTempWriteLiveCleanupFailures }=temp;
const execFileAsync=promisify(execFile);
async function testDirectory(context) {
  const parent = fileURLToPath(new URL("../../../../project_sandbox/desktop-e2e-self-tests/", import.meta.url));
  await mkdir(parent, { recursive: true });
  const root = await mkdtemp(path.join(parent, "temp-write-"));
  context.after(async () => {
    assert.equal(path.dirname(root), path.resolve(parent));
    await rm(root, { recursive: true, force: true });
  });
  return root;
}
const SESSION_ID="01ARZ3NDEKTSV4RRFFQ69G5FAV", TURN_ID="01ARZ3NDEKTSV4RRFFQ69G5FAW";
const completedSummary=projection=>projection.transcript_rows.find(row=>row.row_kind==="work_summary_completed");
function successEvidence(){return {pass:true,command_matches:true,exit_code_zero:true,pytest_passed:true,
  sandbox_note_absent:true,non_success_absent:true,output_size_bytes:192,output_sha256:"a".repeat(64)};}
function responseRow(role,overrides={}){return {route:"responses",method:"POST",pathname:"/v1/responses",
  query_present:false,contract:{pass:true,role,role_evidence:role==="temp_continuation"?{restricted_output:successEvidence()}:{}},
  response_phase:"completed",response_status:200,...overrides};}
function ledger(){return [responseRow("temp_initial"),responseRow("temp_continuation")];}
function baseProjection(overrides={}){return {run_status_key:"completed",task_activity_state:"idle",busy:false,
  agent_tree_active:false,post_run_refresh_pending:false,background_mutation_pending:false,
  async_polling_required:false,pending_async_operations:[],navigation_loading:false,provider_loading:false,
  overlay:"none",confirmation_visible:false,confirmation_id:null,draft_prompt:"",composer_submit_mode:"new_request",can_submit:true,
  progress_text:"Completed",tool_status_text:"- [完了] shell",draft_target:{sessionId:SESSION_ID},
  run_target:{sessionId:SESSION_ID,expectedState:{kind:"idle",latestTurnId:TURN_ID,admissionRevision:"2"}},
  transcript_rows:[{row_kind:"user",body:PERMISSION_TEMP_WRITE_PROMPT},
    {row_kind:"work_summary_completed",body:"### 作業サマリ\n- コマンド/ツール: 1件\n### 作業履歴\n- [待機] shell\n- [完了] shell"},
    {row_kind:"assistant",body:PERMISSION_TEMP_WRITE_RESPONSE}],...overrides};}
function surface(projection=baseProjection(),overrides={}){return {projection,
  users:[{visible:true,text:PERMISSION_TEMP_WRITE_PROMPT}],assistants:[{visible:true,text:PERMISSION_TEMP_WRITE_RESPONSE}],
  completed_summaries:[{visible:true,text:"completed"}],prompt:{count:1,value:"",visible:true,enabled:true},
  send:{count:1,visible:true,enabled:false},visible_fatal_count:0,visible_recoverable_error_count:0,
  visible_dialog_count:0,visible_modal_backdrop_count:0,...overrides};}
function heldSample(){return {surface:surface(baseProjection({run_status_key:"running",task_activity_state:"running",busy:true,
  run_target:{sessionId:SESSION_ID,expectedState:{kind:"turn",turnId:TURN_ID,admissionRevision:"2"}},
  transcript_rows:[{row_kind:"user",body:PERMISSION_TEMP_WRITE_PROMPT},{row_kind:"work_summary_running",body:"Provider request in flight"}]}),
  {assistants:[],completed_summaries:[]}), ledger:[responseRow("temp_initial"),responseRow("temp_continuation",{response_phase:"held",response_status:null})]};}
function liveUser(text){return {type:"message",role:"user",content:[{type:"input_text",text}]};}
function taskBody(input){return {model:"qwen/example",instructions:"bounded TEMP write",input,
  tools:[{type:"function",name:"shell",description:"shell",parameters:{}}],tool_choice:"auto",parallel_tool_calls:false,store:false,stream:true};}
function captures(){const call={type:"function_call",call_id:"call-temp",name:"shell",
  arguments:JSON.stringify({command:PERMISSION_TEMP_WRITE_COMMAND,sandbox_permissions:"use_default"})};
  const output={type:"function_call_output",call_id:call.call_id,output:
    'Command: '+PERMISSION_TEMP_WRITE_COMMAND+'\nExit code: 0\n1 passed in 0.1s'};
  return [{body:taskBody([liveUser(PERMISSION_TEMP_WRITE_LIVE_PROMPT)])},
    {body:taskBody([liveUser(PERMISSION_TEMP_WRITE_LIVE_PROMPT),call,output])}];}
test("TEMP write configs keep ordinary admission and host generation defaults",()=>{
  for(const config of [temp.permissionTempWriteFixtureConfig("http://127.0.0.1:1234/v1"),
    temp.permissionTempWriteLmStudioFixtureConfig({providerBaseUrl:"http://127.0.0.1:1234/v1",model:"qwen/example"})]){
    assert.match(config,/access_mode = "auto_review"/u); assert.doesNotMatch(config,/temperature|max_tokens|reasoning|\[approve\]/u);
  }
  assert.equal(temp.PERMISSION_TEMP_WRITE_TERMINAL_TIMEOUT_MS,150_000);
});
test("TEMP write preflight requires offline-resolved Python identity, 3.13 and existing pytest",()=>{
  const python=path.resolve("python.exe"), observed={executable:python,version:[3,13,5],pytest_version:"9.0.2"};
  assert.deepEqual(temp.permissionTempWritePythonPreflightFailures(python,observed),[]);
  for(const value of [{...observed,executable:path.resolve("other.exe")},{...observed,version:[3,14,7]},
    {...observed,pytest_version:""},{...observed,executable:null}])
    assert.notDeepEqual(temp.permissionTempWritePythonPreflightFailures(python,value),[]);
});
test("TEMP write live option parser rejects credentials and unknown settings",()=>{
  assert.deepEqual(temp.normalizePermissionTempWriteLmStudioOptions({provider_base_url:"http://localhost:1234/v1/",model:" qwen/example "}),
    {providerBaseUrl:"http://localhost:1234/v1",model:"qwen/example"});
  for(const options of [{provider_base_url:"http://user:secret@localhost/v1",model:"a"},
    {provider_base_url:"http://localhost/v1?key=x",model:"a"},
    {provider_base_url:"http://localhost/v1",model:"a",temperature:0}])
    assert.throws(()=>temp.normalizePermissionTempWriteLmStudioOptions(options),TypeError);
});
test("TEMP write Responses captures accept exactly one ordinary call and one successful output",()=>{
  const exact=captures(); assert.deepEqual(temp.permissionTempWriteLiveCaptureContract(exact,{model:"qwen/example"}).failures,[]);
  for(const mutate of [x=>x.push(x[1]),x=>x[1].body.input[1].arguments=JSON.stringify({command:PERMISSION_TEMP_WRITE_COMMAND,sandbox_permissions:"require_escalated"}),
    x=>x[1].body.input[2].call_id="other",x=>x[1].body.input[2].output=x[1].body.input[2].output.replace('Exit code: 0','Exit code: 1'),
    x=>x[1].body.input[2].output+='\nSandbox note: retry',x=>x[1].body.temperature=0,x=>x[1].body.model="other",
    x=>x[1].body.input[1].arguments=JSON.stringify({command:PERMISSION_TEMP_WRITE_COMMAND+' --basetemp=.',sandbox_permissions:"use_default"})]){
    const drift=structuredClone(exact); mutate(drift); assert.notDeepEqual(temp.permissionTempWriteLiveCaptureContract(drift,{model:"qwen/example"}).failures,[]);
  }
  const described=structuredClone(exact); const args=JSON.parse(described[1].body.input[1].arguments); args.description="same command";
  described[1].body.input[1].arguments=JSON.stringify(args);
  assert.deepEqual(temp.permissionTempWriteLiveCaptureContract(described,{model:"qwen/example"}).failures,[]);
});
test("TEMP write held barrier requires successful output and the same active turn",()=>{
  const exact=heldSample(); assert.deepEqual(temp.permissionTempWriteHeldFailures(exact),[]);
  assert.equal(temp.permissionTempWriteHeldDecision(exact),"pass");
  const lag=structuredClone(exact);lag.surface.projection.transcript_rows.pop();assert.equal(temp.permissionTempWriteHeldDecision(lag),"pending");
  for(const mutate of [x=>x.ledger[1].contract.role_evidence.restricted_output.pytest_passed=false,
    x=>x.surface.projection.run_target.expectedState.turnId="invalid",x=>x.surface.projection.transcript_rows.push({row_kind:"assistant",body:"early"}),
    x=>x.ledger.reverse()]){const drift=structuredClone(exact); mutate(drift); assert.notDeepEqual(temp.permissionTempWriteHeldFailures(drift),[]);}
  const dialog=structuredClone(exact);dialog.surface.visible_dialog_count=1;assert.equal(temp.permissionTempWriteHeldDecision(dialog),"fail");
});
test("TEMP write terminal rejects Guardian, errors, retries and changed result authority",()=>{
  const exact={surface:surface(),ledger:ledger()};assert.deepEqual(permissionTempWriteTerminalFailures(exact),[]);
  for(const mutate of [x=>x.ledger.push(responseRow("temp_guardian")),x=>x.ledger[1].contract.role_evidence.restricted_output.pass=false,
    x=>x.surface.projection.transcript_rows.push({row_kind:"error",body:"failed shell"}),
    x=>completedSummary(x.surface.projection).body=completedSummary(x.surface.projection).body.replace('1件','2件'),
    x=>completedSummary(x.surface.projection).body+='\n- [待機] shell\n- [完了] shell',
    x=>completedSummary(x.surface.projection).body=completedSummary(x.surface.projection).body.replace('[完了]','[失敗]'),
    x=>x.surface.projection.transcript_rows.at(-1).body="wrong"]){const drift=structuredClone(exact);mutate(drift);assert.notDeepEqual(permissionTempWriteTerminalFailures(drift),[]);}
  exact.surface.projection.tool_status_text="- [実行中] shell";
  assert.deepEqual(permissionTempWriteTerminalFailures(exact),[]);
});
test("TEMP write actual one-call summary retains waiting and completion despite collapsed DOM",()=>{
  // Public completed summary captured in e2e-20261012t110423z-22f999dc.
  const sample={surface:surface(),ledger:ledger()};
  completedSummary(sample.surface.projection).body="### 作業サマリ\n- 結果: セッションは完了しました。\n- コマンド/ツール: 1件\n\n### 作業履歴\n- [待機] shell\n- [完了] Run shell command: python -B -X utf8 -m pytest -pno:cacheprovider -q --tb=short test_e2e_owner_o...\n  出力: Command: python -B -X utf8 -m pytest -pno:cacheprovider -q --tb=short test_e2e_owner_only_temp.py Exit code: 0 Stdout: . [100%] 1 passed in 0.05s Stderr: (empty)";
  sample.surface.completed_summaries[0].text="";
  assert.deepEqual(permissionTempWriteTerminalFailures(sample),[]);
  assert.equal(createPermissionTempWriteTerminalDecision()(sample),"pass");
  for(const change of [
    body=>body.replace('1件','2件'),
    body=>body.replace('- [待機] shell\n',''),
    body=>body.replace('- [待機] shell\n','- [完了] shell\n- [待機] shell\n'),
    body=>body+'\n- [待機] read\n- [完了] read',
  ]){
    const drift=structuredClone(sample);
    completedSummary(drift.surface.projection).body=change(completedSummary(drift.surface.projection).body);
    assert.ok(permissionTempWriteTerminalFailures(drift).includes("terminal-tool-history-mismatch"));
  }
});
test("TEMP write live terminal preserves exact canonical prompt and normalizes only DOM whitespace",()=>{
  const value=surface();value.projection.transcript_rows[0].body=PERMISSION_TEMP_WRITE_LIVE_PROMPT;
  value.projection.transcript_rows.at(-1).body=PERMISSION_TEMP_WRITE_LIVE_RESPONSE;
  value.users[0].text=PERMISSION_TEMP_WRITE_LIVE_PROMPT.replaceAll('\n',' ');value.assistants[0].text=PERMISSION_TEMP_WRITE_LIVE_RESPONSE;
  assert.deepEqual(temp.permissionTempWriteLiveTerminalFailures(value),[]);
  value.projection.transcript_rows[0].body=value.users[0].text;
  assert.ok(temp.permissionTempWriteLiveTerminalFailures(value).includes("live-terminal-user-authority-mismatch"));
});
function persisted(){return {schema_version:"desktop-e2e.permission-temp-persistence.v1",read_only:true,rows:[{
  history_item_id:"01ARZ3NDEKTSV4RRFFQ69G5FAX",session_id:SESSION_ID,turn_id:TURN_ID,sequence_no:5,kind:"tool_output",
  call_id:"01ARZ3NDEKTSV4RRFFQ69G5FAY",status:"completed",success:1,metadata_success:1,tool_metadata_success:1,sandbox_failure_hint:null}]};}
test("TEMP write persistence requires one completed success at all three typed layers",()=>{
  const owner={sessionId:SESSION_ID,turnId:TURN_ID}, exact=persisted();assert.deepEqual(temp.permissionTempWritePersistenceFailures(exact,owner),[]);
  for(const mutate of [x=>x.rows[0].success=0,x=>x.rows[0].metadata_success=0,x=>x.rows[0].tool_metadata_success=0,
    x=>x.rows[0].sandbox_failure_hint="denied",x=>x.rows[0].status="running",x=>x.rows[0].turn_id=SESSION_ID,
    x=>x.rows.push(x.rows[0])]){const drift=structuredClone(exact);mutate(drift);assert.notDeepEqual(temp.permissionTempWritePersistenceFailures(drift,owner),[]);}
});
test("TEMP write persistence reader reads the exact completed turn without mutating the database",async context=>{
  const root=await testDirectory(context);
  const database=path.join(root,"moyai.sqlite3"),q=x=>"'"+x.replaceAll("'","''")+"'";
  const payload=JSON.stringify({kind:"tool_output",call_id:"01ARZ3NDEKTSV4RRFFQ69G5FAY",status:"completed",success:true,
    metadata:{success:true,tool_metadata:{success:true}}});
  await execFileAsync("sqlite3.exe",["-batch","-bail",database,
    'CREATE TABLE protocol_history_items(id TEXT,session_id TEXT,turn_id TEXT,sequence_no INTEGER,payload_json TEXT);'+
    'INSERT INTO protocol_history_items VALUES('+q("01ARZ3NDEKTSV4RRFFQ69G5FAX")+','+q(SESSION_ID)+','+q(TURN_ID)+',5,'+q(payload)+');'],{windowsHide:true});
  const owner={sessionId:SESSION_ID,turnId:TURN_ID};
  assert.deepEqual(temp.permissionTempWritePersistenceFailures(await temp.readPermissionTempWritePersistence({database,owner}),owner),[]);
});

async function writeCaptures(directory, values, mutate = () => {}) {
  for (const [index, value] of values.entries()) {
    const stem = `capture-${index}`, request = `${stem}.request.json`;
    const bytes = Buffer.from(JSON.stringify(value.body), "utf8");
    const metadata = { api_mode: "responses", capture_stage: "prepared", captured_at_unix_ms: 100 + index,
      endpoint_path: "v1/responses", process_id: 42, request_body_bytes: bytes.byteLength,
      request_body_file: request, request_id: `request-${index}`, schema_version: 2,
      sequence: index, transport: "http" };
    mutate(metadata, index);
    await writeFile(path.join(directory, request), bytes);
    await writeFile(path.join(directory, `${stem}.metadata.json`), JSON.stringify(metadata));
  }
}

test("TEMP write capture reader binds both body files, byte counts, sequence and successful pair", async context => {
  const directory = await testDirectory(context);
  await writeCaptures(directory, captures());
  const result = await temp.readPermissionTempWriteLiveRequestCaptures(directory, { model: "qwen/example" });
  assert.equal(result.capture_count, 2);
  assert.deepEqual(result.request_sequences, [0, 1]);
  assert.deepEqual(result.failures, []);
  assert.equal(result.request_body_hashes.length, 2);
});

test("TEMP write capture reader rejects byte ownership, missing pair, process and sequence drift", async context => {
  for (const [change, code] of [
    [metadata => { metadata.request_body_bytes += 1; }, "permission-temp-live-request-capture-size"],
    [metadata => { metadata.request_body_file = "other.request.json"; }, "permission-temp-live-request-capture-pair"],
    [(metadata, index) => { metadata.process_id += index; }, "permission-temp-live-request-contract"],
    [(metadata, index) => { metadata.sequence += index; }, "permission-temp-live-request-contract"],
  ]) {
    const directory = await testDirectory(context);
    await writeCaptures(directory, captures(), change);
    await assert.rejects(temp.readPermissionTempWriteLiveRequestCaptures(directory, { model: "qwen/example" }),
      error => error.owner === "product" && error.code === code);
  }
  const directory = await testDirectory(context);
  await writeCaptures(directory, captures());
  await writeFile(path.join(directory, "orphan.request.json"), "{}");
  await assert.rejects(temp.readPermissionTempWriteLiveRequestCaptures(directory, { model: "qwen/example" }),
    error => error.code === "permission-temp-live-request-capture-orphan");
});

test("permission TEMP capture JSON treats invalid UTF-8 or JSON as product evidence", () => {
  assert.deepEqual(parsePermissionTempWriteCaptureJson(
    Buffer.from('{"schema_version":2}', "utf8"),
    { file: "capture.metadata.json", kind: "metadata" },
  ), { schema_version: 2 });

  for (const [bytes, file, kind] of [
    [Buffer.from("{", "utf8"), "capture.metadata.json", "metadata"],
    [Buffer.from([0xff]), "capture.request.json", "request"],
  ]) {
    assert.throws(
      () => parsePermissionTempWriteCaptureJson(bytes, { file, kind }),
      (error) => error?.owner === "product"
        && error?.code === "permission-temp-live-request-capture-json"
        && error?.evidence?.file === file
        && error?.evidence?.kind === kind,
    );
  }
});

test("permission TEMP final capture observation preserves product and harness owners", () => {
  let productError;
  try {
    parsePermissionTempWriteCaptureJson(Buffer.from("{"), {
      file: "capture.metadata.json",
      kind: "metadata",
    });
  } catch (error) {
    productError = error;
  }
  assert.deepEqual(permissionTempWriteCaptureFailureObservation(productError), {
    name: "DesktopE2eError",
    code: "permission-temp-live-request-capture-json",
    message: "one prepared request capture metadata file is not valid UTF-8 JSON",
    evidence: productError.evidence,
  });

  const harnessError = new DesktopE2eError(
    "harness",
    "permission-temp-live-request-capture-read",
    "capture bytes could not be read",
  );
  assert.throws(
    () => permissionTempWriteCaptureFailureObservation(harnessError),
    (error) => error === harnessError,
  );
  const unexpected = new Error("unexpected parser failure");
  assert.throws(
    () => permissionTempWriteCaptureFailureObservation(unexpected),
    (error) => error === unexpected,
  );
});

test("permission TEMP SQLite timeout waits for close and has a bounded termination deadline", async () => {
  const fakeChild = (kill) => {
    const child = new EventEmitter();
    child.stdout = new PassThrough();
    child.stderr = new PassThrough();
    child.kill = kill;
    return child;
  };

  let observeKill;
  const killed = new Promise((resolve) => { observeKill = resolve; });
  let closingChild;
  const closingQuery = runReadOnlySqlite("fixture.sqlite3", "SELECT 1", 1, {
    spawnProcess: () => {
      closingChild = fakeChild(() => {
        observeKill();
        return true;
      });
      return closingChild;
    },
    terminationTimeoutMs: 1_000,
  });
  await killed;
  let settledBeforeClose = false;
  closingQuery.then(
    () => { settledBeforeClose = true; },
    () => { settledBeforeClose = true; },
  );
  await Promise.resolve();
  assert.equal(settledBeforeClose, false);
  closingChild.emit("close", null);
  await assert.rejects(closingQuery, /SQLite query timed out/u);

  const neverClosingQuery = runReadOnlySqlite("fixture.sqlite3", "SELECT 1", 1, {
    spawnProcess: () => fakeChild(() => true),
    terminationTimeoutMs: 20,
  });
  await assert.rejects(neverClosingQuery, /did not close after termination/u);
});
function detailObservationClock() {
  let now = 0;
  return { observe: options => waitForObservation({ ...options,
    now: () => now, sleep: async milliseconds => { now += milliseconds; } }) };
}

test("permission TEMP detail diagnosis observes convergence without changing terminal acceptance", async () => {
  const initial = surface(), samples = [];
  initial.projection.tool_status_text = "- [実行中] shell";
  for (let index = 0; index < 3; index += 1) {
    const next = structuredClone(initial);
    next.visible_tool_detail = { present_count: 0, visible_count: 0, text: "" };
    if (index === 2) next.projection.tool_status_text = "- [完了] shell";
    samples.push(next);
  }
  const result = await observePermissionTempTerminalDetail(initial, async () => samples.shift(), detailObservationClock());
  assert.equal(result.status, "converged");
  assert.equal(result.same_owner, true);
  assert.equal(result.attempts, 3);
  assert.equal(result.elapsed_ms, 200);
  assert.equal(result.typed_pending, false);
  assert.equal(result.visible_pending, false);
  assert.deepEqual(permissionTempWriteTerminalFailures({ surface: initial, ledger: ledger() }), []);
});

test("permission TEMP detail timeout separates hidden typed activity from visible activity", async () => {
  for (const visible of [false, true]) {
    const initial = surface();
    initial.projection.tool_status_text = "- [実行中] shell";
    const next = { ...initial, visible_tool_detail: { present_count: Number(visible),
      visible_count: Number(visible), text: visible ? initial.projection.tool_status_text : "" } };
    const result = await observePermissionTempTerminalDetail(initial, async () => next, detailObservationClock());
    assert.equal(result.status, "not_converged");
    assert.equal(result.elapsed_ms, 3000);
    assert.equal(result.typed_pending, true);
    assert.equal(result.visible_pending, visible);
    assert.deepEqual(permissionTempWriteTerminalFailures({ surface: initial, ledger: ledger() }), []);
  }
});

test("permission TEMP detail diagnosis cannot call a changed session, turn or generation converged", async () => {
  for (const mutate of [
    target => { target.sessionId = TURN_ID; },
    target => { target.expectedState.latestTurnId = SESSION_ID; },
    target => { target.runtimeOwnerToken = "root:2"; },
  ]) {
    const initial = surface();
    initial.projection.run_target.runtimeOwnerToken = "root:1";
    const next = structuredClone(initial);
    next.visible_tool_detail = { present_count: 0, visible_count: 0, text: "" };
    mutate(next.projection.run_target);
    const result = await observePermissionTempTerminalDetail(initial, async () => next, detailObservationClock());
    assert.equal(result.status, "owner_changed");
    assert.equal(result.same_owner, false);
    assert.equal(result.attempts, 1);
  }
});

test("permission TEMP terminal waits for DOM convergence and confirms canonical mismatch", () => {
  const exact = { surface: surface(), ledger: ledger() };
  const domLag = structuredClone(exact);
  domLag.surface.assistants = [];
  domLag.surface.completed_summaries = [];
  assert.deepEqual(permissionTempWriteTerminalFailures(domLag), [
    "terminal-dom-conversation-mismatch",
  ]);
  let observedAt = 1_000;
  const domDecision = createPermissionTempWriteTerminalDecision({
    now: () => observedAt,
  });
  assert.equal(domDecision(domLag), "pending");
  observedAt += PERMISSION_TEMP_WRITE_TERMINAL_CONFIRMATION_MS * 2;
  assert.equal(domDecision(domLag), "pending");
  assert.equal(domDecision(exact), "pass");

  const canonicalMismatch = structuredClone(exact);
  const wrongSummary = completedSummary(canonicalMismatch.surface.projection);
  wrongSummary.body = wrongSummary.body.replace(
      "- コマンド/ツール: 1件",
      "- コマンド/ツール: 2件",
    );
  assert.ok(permissionTempWriteTerminalFailureEvidence(canonicalMismatch)
    .terminal_oracle_failures.includes("terminal-tool-history-mismatch"));
  observedAt = 2_000;
  const canonicalDecision = createPermissionTempWriteTerminalDecision({
    now: () => observedAt,
  });
  assert.equal(canonicalDecision(canonicalMismatch), "pending");
  observedAt += PERMISSION_TEMP_WRITE_TERMINAL_CONFIRMATION_MS - 1;
  assert.equal(canonicalDecision(canonicalMismatch), "pending");
  observedAt += 1;
  assert.equal(canonicalDecision(canonicalMismatch), "fail");
});

test("permission TEMP write workspace oracle rejects every fixture or top-level drift", () => {
  const baseline = {
    schema_version: "desktop-e2e.workspace-snapshot.v1",
    entries: [
      {
        name: "E2E_PERMISSION_TEMP_WRITE.txt",
        kind: "file",
        size_bytes: 8,
        sha256: "a".repeat(64),
      },
      {
        name: "test_e2e_owner_only_temp.py",
        kind: "file",
        size_bytes: 72,
        sha256: "b".repeat(64),
      },
    ],
  };
  assert.deepEqual(permissionTempWriteWorkspaceFailures(baseline, baseline), []);
  assert.deepEqual(permissionTempWriteWorkspaceFailures({
    ...baseline,
    entries: [...baseline.entries, {
      name: ".pytest_tmp",
      kind: "directory",
      size_bytes: null,
      sha256: null,
    }],
  }, baseline), [
    "workspace-top-level-drift",
    "workspace-temp-workaround-present",
    "workspace-fixture-mutated",
  ]);
  const modified = structuredClone(baseline);
  modified.entries[1].sha256 = "c".repeat(64);
  assert.deepEqual(permissionTempWriteWorkspaceFailures(modified, baseline), [
    "workspace-fixture-mutated",
  ]);
});

test("permission TEMP live final and cleanup gates reject late workspace or request drift", () => {
  const capture = {
    schema_version: "desktop-e2e.permission-temp-live-captures.v1",
    capture_count: 2,
    request_body_hashes: ["a", "b"],
  };
  const exact = {
    workspace: { entries: [] },
    workspace_failures: [],
    terminal_workspace_matches: true,
    prepared_request_capture: capture,
    prepared_request_capture_failure: null,
    prepared_request_capture_matches: true,
  };
  assert.deepEqual(permissionTempWriteLiveFinalFailures(exact), []);
  assert.deepEqual(permissionTempWriteLiveCleanupFailures({
    quiesceInput: "pass",
    quiesceFinalObservation: { ...exact, failures: [] },
    cleanupFinalObservation: { ...exact, failures: [] },
  }), []);

  assert.deepEqual(permissionTempWriteLiveFinalFailures({
    ...exact,
    workspace_failures: ["workspace-fixture-mutated"],
    terminal_workspace_matches: false,
    prepared_request_capture_failure: { code: "invalid-json" },
    prepared_request_capture_matches: false,
  }), [
    "live-final-workspace-drift",
    "live-final-terminal-workspace-mismatch",
    "live-final-request-capture-read-failed",
    "live-final-request-capture-drift",
  ]);

  const lateCapture = structuredClone({ ...exact, failures: [] });
  lateCapture.prepared_request_capture.capture_count = 3;
  assert.deepEqual(permissionTempWriteLiveCleanupFailures({
    quiesceInput: "pass",
    quiesceFinalObservation: { ...exact, failures: [] },
    cleanupFinalObservation: lateCapture,
  }), ["live-cleanup-final-observation-drift"]);
  assert.deepEqual(permissionTempWriteLiveCleanupFailures({
    quiesceInput: "fail",
    quiesceFinalObservation: null,
    cleanupFinalObservation: null,
  }), [
    "live-cleanup-quiesce-failed",
    "live-cleanup-final-observation-drift",
  ]);
});
