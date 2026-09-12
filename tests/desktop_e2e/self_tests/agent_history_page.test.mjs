import assert from 'node:assert/strict';
import test from 'node:test';
import {agentHistoryLedger,agentHistoryOwnerPreserved,agentHistoryPrepended} from '../scenarios/agent_history_page.mjs';
import {createAgentInterruptScenario} from '../scenarios/agent_interrupt.mjs';
test('optional previous-page scenario keeps the original default and accepts only a bounded long child fixture',()=>{
  assert.equal(createAgentInterruptScenario().id,'agent.interrupt');assert.equal(createAgentInterruptScenario({childToolCalls:42}).manualGate,'pending');
  for(const childToolCalls of [null,-1,1,40,65,1.2,'42'])assert.throws(()=>createAgentInterruptScenario({childToolCalls}),/childToolCalls/);
  assert.throws(()=>createAgentInterruptScenario({seedDatabase:true}),/only childToolCalls/);
});
const ledger=(count,terminal=false)=>['root_initial','root_continuation',...Array.from({length:count},(_,i)=>`child_tool_${i}`),'child_held'].map(role=>({route:'responses',method:'POST',pathname:'/v1/responses',query_present:false,contract:{pass:true,role},response_phase:role==='child_held'?(terminal?'peer_closed':'held'):'completed',response_status:role==='child_held'?null:200}));
test('child history accepts each finite role once and requires actual peer cancellation',()=>{
  assert.equal(agentHistoryLedger(ledger(42),42),true);assert.equal(agentHistoryLedger(ledger(42,true),42,true),true);
  for(const mutate of [v=>v.pop(),v=>v.push(v[0]),v=>v[3].contract.role='child_tool_0',v=>v[3].contract.pass=false,v=>v[3].response_phase='held',v=>v[3].response_status=500,v=>v[3].query_present=true,v=>v.at(-1).response_phase='completed']){const v=ledger(42);mutate(v);assert.equal(agentHistoryLedger(v,42),false);}
  assert.equal(agentHistoryLedger(ledger(42),42,true),false);assert.equal(agentHistoryLedger(ledger(42,true),42),false);
});
test('prepend accepts the identity-less System instruction while preserving the exact running summary and complete tool total',()=>{
  const before={agentPath:'/root/child',historyIds:['summary'],count:1,expectedTask:'Message Type: NEW_TASK exact task'};
  const after=()=>({agentPath:'/root/child',historyIds:['summary'],count:2,meta:'2件の履歴',previous:0,errors:0,toolCount:84,
    rows:[{kind:'system',identity:null,title:'Agent間の追加指示',body:before.expectedTask},{kind:'work_summary_running',identity:'summary'}]});
  assert.equal(agentHistoryPrepended(after(),before,42),true);
  for(const mutate of [v=>v.historyIds=['other'],v=>v.historyIds=['summary','summary'],v=>v.count=1,v=>v.meta='2件を表示 · 以前の実行履歴あり',v=>v.previous=1,v=>v.errors=1,v=>v.agentPath='/root/other',v=>v.rows.reverse(),v=>v.rows[0].body='unrelated NEW_TASK',v=>v.rows[0].title='unrelated',v=>v.rows[1].identity='other',v=>v.toolCount=53,v=>v.rows.pop()]){const v=after();mutate(v);assert.equal(agentHistoryPrepended(v,before,42),false);}
});
test('reading child pages cannot change Main owner, draft owner or canonical answer',()=>{
  const p={workspace_path:'C:\\owned',run_target:{sessionId:'main'},draft_target:{sessionId:'main'},run_status_key:'completed',busy:false,transcript_rows:[{row_kind:'assistant',title:'',body:'answer'}]};
  const root={workspace:'C:\\owned',sessionId:'main',history:[{kind:'assistant',identity:null,title:'',body:'answer'}]};
  assert.equal(agentHistoryOwnerPreserved(p,root),true);
  for(const mutate of [v=>v.draft_target.sessionId='child',v=>v.run_target.sessionId='child',v=>v.busy=true,v=>v.transcript_rows[0].body='changed']){const v=structuredClone(p);mutate(v);assert.equal(agentHistoryOwnerPreserved(v,root),false);}
});
