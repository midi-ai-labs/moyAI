import test from 'node:test';
import assert from 'node:assert/strict';
import { ScriptedProvider } from '../drivers/scripted_provider.mjs';
import { createMainSteerControlsScenario,createHistoryRailControlsScenario,createMainGoalQueryScenario,createShortcutRowControlsScenario,createPaletteRunControlsScenario,
  mainControlProviderOptions,completedMainTurnFailures,heldMainTurnFailures } from '../scenarios/main_runtime_controls.mjs';
import { RAIL_FIXTURE_TURNS,steerQueueFailures,railNavigationFailures,classifyGoalQuery,GOAL_EMPTY_TEXT,GOAL_STATUS_TEXT,stoppedSteerFailures } from '../scenarios/main_remaining_controls.mjs';

const root={workspacePath:'C:/owned/workspace',sessionId:'owned-session',expectedState:{kind:'turn',turnId:'turn-1',admissionRevision:'1'}};
const ledger=[{method:'POST',contract:{pass:true},response_phase:'held',response_status:null}];
const queue=()=>({p:{run_target:structuredClone(root),composer_submit_mode:'steer',pending_turn_inputs:[{id:'input-1',turn_id:'turn-1',text:'first direction'},{id:'input-2',turn_id:'turn-1',text:'second direction'}]},
  pending:[{id:'input-1',turn:'turn-1',text:'first direction'},{id:'input-2',turn:'turn-1',text:'second direction'}],prompt:{value:''},visibleErrors:[]});
const queueArgs=()=>({base:{p:{run_target:root}},texts:['first direction','second direction'],ledger:structuredClone(ledger),currentLedger:structuredClone(ledger)});
test('bounded runner scenarios reuse existing lifecycle and retain manual visual gate',()=>{
  for(const [factory,id] of [[createMainSteerControlsScenario,'main.steer-controls'],[createHistoryRailControlsScenario,'history.rail-controls'],[createMainGoalQueryScenario,'main.goal-query'],[createShortcutRowControlsScenario,'navigation.shortcut-row-controls'],[createPaletteRunControlsScenario,'main.palette-run-controls']]) {
    const scenario=factory(); assert.equal(scenario.id,id);assert.equal(scenario.manualGate,'pending');
    for(const method of ['prepare','execute','requestGracefulExit','quiesce','cleanup'])assert.equal(typeof scenario[method],'function');
    assert.equal('launch' in scenario,false);
  }
});
test('all bounded provider plans are accepted by the existing fixture without scripts or network',()=>{
  for(const kind of ['steer','rail','goal','shortcuts','palette']) assert.doesNotThrow(()=>new ScriptedProvider(mainControlProviderOptions(kind)));
  assert.equal(mainControlProviderOptions('steer').responseBehavior,'hold_until_peer_close');
  assert.equal(mainControlProviderOptions('rail').orderedConversation,true);
  assert.equal(RAIL_FIXTURE_TURNS.length,2);assert.notEqual(RAIL_FIXTURE_TURNS[0].responseText,RAIL_FIXTURE_TURNS[1].responseText);
  assert.equal(RAIL_FIXTURE_TURNS.every(t=>t.responseText.split('\n\n').length===35),true);
  assert.throws(()=>mainControlProviderOptions('other'));
});
test('accepted steer requires unique ordered pending inputs for the same live owner and visible queue',()=>{
  assert.deepEqual(steerQueueFailures(queue(),queueArgs()),[]);
  for(const mutate of [v=>v.p.pending_turn_inputs.pop(),v=>{v.p.pending_turn_inputs[1].id='input-1';},v=>{v.p.pending_turn_inputs[1].turn_id='other-turn';},
    v=>{v.p.pending_turn_inputs.reverse();},v=>{v.p.run_target.sessionId='other-session';},v=>{v.p.composer_submit_mode='new_request';},
    v=>{v.pending[1].id='stale-input';},v=>{v.pending[1].turn='other-turn';},v=>{v.prompt.value='unsent direction';},v=>{v.visibleErrors=['error'];}]){
    const value=queue();mutate(value);assert.notEqual(steerQueueFailures(value,queueArgs()).length,0);
  }
});
test('an extra or changed provider request is never mistaken for same-turn acceptance',()=>{
  for(const mutate of [a=>a.currentLedger.push(structuredClone(ledger[0])),a=>{a.currentLedger[0].response_phase='completed';}]) {
    const args=queueArgs();mutate(args);assert.ok(steerQueueFailures(queue(),args).includes('additional-provider-request'));
  }
});
test('semantic owner and ledger equality does not depend on JSON key insertion order',()=>{
  const value=queue(),args=queueArgs();
  value.p.run_target={expectedState:{admissionRevision:'1',turnId:'turn-1',kind:'turn'},sessionId:'owned-session',workspacePath:'C:/owned/workspace'};
  args.currentLedger=[{response_status:null,response_phase:'held',contract:{pass:true},method:'POST'}];
  assert.deepEqual(steerQueueFailures(value,args),[]);
});
const rail=()=>({p:{run_target:structuredClone(root),draft_target:{sessionId:'owned-session',revision:'2'},composer_commit_generation:'3',turn_page_offset:0,selected_session_index:0},
  rows:[{anchor:'anchor-1',identity:'turn:turn-1:work-summary',targetCount:1,focused:true,destinationVisible:true,open:true}],prompt:'unsent text',selection:[3,6],errors:0});
test('rail oracle requires exact visible focused destination and preserved canonical owner and draft',()=>{
  const base=rail(),wanted=base.rows[0];assert.deepEqual(railNavigationFailures(rail(),base,wanted),[]);
  for(const mutate of [v=>{v.rows[0].anchor='wrong';},v=>{v.rows[0].identity='wrong';},v=>{v.rows[0].targetCount=2;},v=>{v.rows[0].focused=false;},
    v=>{v.rows[0].destinationVisible=false;},v=>{v.rows[0].open=false;},v=>{v.p.run_target.sessionId='other';},v=>{v.p.turn_page_offset=20;},
    v=>{v.p.draft_target.revision='4';},v=>{v.p.composer_commit_generation='4';},v=>{v.prompt='';},v=>{v.selection=[0,0];},v=>{v.errors=1;}]){
    const value=rail();mutate(value);assert.notEqual(railNavigationFailures(value,base,wanted).length,0);
  }
});
const goal=()=>({before:{p:{run_target:{sessionId:'s1',expectedState:{kind:'idle',latestTurnId:'t1',admissionRevision:'1'}}}},
  result:{p:{run_target:{sessionId:'s1',expectedState:{kind:'idle',latestTurnId:'t1',admissionRevision:'1'}},status_message:'run failed: run command completed without a terminal turn summary'}},
  providerBefore:[{response_phase:'completed'}],providerAfter:[{response_phase:'completed'}]});
test('Goal observation identifies normal control completion incorrectly converted into run failure',()=>{
  assert.equal(classifyGoalQuery(goal()),'goal-control-reported-as-run-failure');
  const value=goal();value.result.p.status_message='run completed';
  assert.equal(classifyGoalQuery(value),'goal-result-needs-visible-feedback-oracle');
});
test('Goal query must not contact provider or acquire another turn and never claims PASS from missing feedback',()=>{
  const extra=goal();extra.providerAfter.push({response_phase:'held'});assert.equal(classifyGoalQuery(extra),'goal-query-contacted-provider');
  const owner=goal();owner.result.p.run_target.expectedState.latestTurnId='t2';assert.equal(classifyGoalQuery(owner),'goal-query-changed-turn-owner');
  const session=goal();session.result.p.run_target.sessionId='s2';assert.equal(classifyGoalQuery(session),'goal-query-changed-turn-owner');
});
const successfulGoal=()=>{
  const value=goal();
  Object.assign(value.before.p,{run_status_key:'completed',composer_commit_generation:'1',transcript_rows:[{row_kind:'user',body:'original'},{row_kind:'assistant',body:'original answer'}]});
  Object.assign(value.result.p,{status_code:'goal_control',status_detail:GOAL_EMPTY_TEXT,status_message:GOAL_STATUS_TEXT,
    run_status_key:'completed',composer_commit_generation:'2',transcript_rows:structuredClone(value.before.p.transcript_rows)});
  Object.assign(value.result,{status:{text:GOAL_STATUS_TEXT,detailText:GOAL_EMPTY_TEXT,detailCount:1},
    composerRunTarget:structuredClone(value.result.p.run_target),prompt:{value:'',disabled:false},runStrip:false,visibleErrors:[]});
  return value;
};
test('Goal PASS requires a real typed and rendered result with unchanged history and one input commit',()=>{
  assert.equal(classifyGoalQuery(successfulGoal()),'goal-query-visible-success');
  for(const mutate of [v=>{v.result.p.status_code='general';},v=>{v.result.p.status_detail='';},v=>{v.result.status.detailText='';},
    v=>{v.result.status.detailCount=2;},v=>{v.result.status.text='完了';},v=>{v.result.p.transcript_rows.push({row_kind:'user',body:'/goal'});},
    v=>{v.result.p.run_status_key='failed';},v=>{v.result.p.composer_commit_generation='1';},v=>{v.result.p.composer_commit_generation='3';},
    v=>{v.result.prompt.value='/goal';},v=>{v.result.prompt.disabled=true;},v=>{v.result.composerRunTarget.expectedState.kind='turn';},
    v=>{v.result.runStrip=true;},v=>{v.result.visibleErrors=['error'];}]) {
    const value=successfulGoal();mutate(value);assert.notEqual(classifyGoalQuery(value),'goal-query-visible-success');
  }
});
test('Steer Stop requires both settled canonical owner and retired running DOM',()=>{
  const fixture=()=>({surface:{p:{run_status_key:'cancelled',busy:false,post_run_refresh_pending:false,can_submit:true,
    run_target:{sessionId:'s1',expectedState:{kind:'idle'}}},composerRunTarget:{sessionId:'s1',expectedState:{kind:'idle'}},
    prompt:{disabled:false},runStrip:false,visibleErrors:[]},ledger:[{response_phase:'peer_closed'}]});
  assert.deepEqual(stoppedSteerFailures(fixture(),'s1'),[]);
  for(const mutate of [v=>{v.surface.runStrip=true;},v=>{v.surface.composerRunTarget.expectedState.kind='turn';},
    v=>{v.surface.p.run_status_key='running';},v=>{v.surface.p.run_target.sessionId='other';},v=>{v.surface.prompt.disabled=true;},
    v=>{v.ledger[0].response_phase='held';},v=>{v.surface.visibleErrors=['error'];}]) {
    const value=fixture();mutate(value);assert.notEqual(stoppedSteerFailures(value,'s1').length,0);
  }
});
const completed=()=>({surface:{p:{busy:false,post_run_refresh_pending:false,run_status_key:'completed',composer_submit_mode:'new_request',can_submit:true,
  run_target:{sessionId:'session-1',expectedState:{kind:'idle'}},transcript_rows:[
    {row_kind:'user',body:'one'},{row_kind:'work_summary_completed',body:''},{row_kind:'assistant',body:'ONE_OK'}]},prompt:{value:'',disabled:false},visibleErrors:[]},
  ledger:[{method:'POST',contract:{pass:true},response_phase:'completed',response_status:200}]});
test('fixture setup advances only after canonical response and UI owner settle, not raw provider completion',()=>{
  const fixture=()=>{const value=completed();value.surface.composerRunTarget=structuredClone(value.surface.p.run_target);return value;};
  const turns=[{prompt:'one',responseText:'ONE_OK'}];assert.deepEqual(completedMainTurnFailures(fixture(),turns,'session-1'),[]);
  for(const mutate of [v=>{v.surface.p.post_run_refresh_pending=true;},v=>{v.surface.p.run_status_key='running';},v=>{v.surface.p.run_target.sessionId='other';},
    v=>{v.surface.p.transcript_rows[2].body='wrong';},v=>{v.surface.prompt.disabled=true;},v=>{v.surface.visibleErrors=['error'];},v=>{v.ledger[0].contract.pass=false;},
    v=>{v.surface.composerRunTarget.expectedState={kind:'turn',turnId:'previous-turn'};}]){
    const value=fixture();mutate(value);assert.notEqual(completedMainTurnFailures(value,turns,'session-1').length,0);
  }
});
test('live fixture waits for committed session/turn identity on the actual composer before typing',()=>{
  const fixture=()=>({surface:{p:{run_status_key:'running',composer_submit_mode:'steer',run_target:structuredClone(root)},composerRunTarget:structuredClone(root),
    prompt:{value:'',disabled:false},runStrip:true},ledger:structuredClone(ledger)});
  assert.deepEqual(heldMainTurnFailures(fixture(),0),[]);
  for(const mutate of [v=>{v.surface.composerRunTarget.sessionId=null;},v=>{v.surface.composerRunTarget.expectedState={kind:'idle'};},
    v=>{v.surface.prompt.value='old submitted input';},v=>{v.surface.prompt.disabled=true;},v=>{v.surface.p.composer_submit_mode='blocked';},
    v=>{v.surface.runStrip=false;},v=>{v.ledger[0].response_phase='completed';}]){
    const value=fixture();mutate(value);assert.notEqual(heldMainTurnFailures(value,0).length,0);
  }
});
