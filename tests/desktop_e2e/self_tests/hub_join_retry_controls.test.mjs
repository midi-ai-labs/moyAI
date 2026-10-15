import assert from 'node:assert/strict';
import test from 'node:test';
import { failedJoinReady, explicitRetryObserved, acceptedRetryIdentity, createHubJoinRetryControlsScenario } from '../scenarios/hub_join_retry_controls.mjs';

const URL='https://127.0.0.1:9471';
test('failed import acceptance requires a saved but unregistered endpoint and a visible enabled Retry',()=>{
  const ready={network:{enrollment:'error',hub_url:URL,device_id:null,request_id:null,can_join:true},join:{count:1,visible:true,enabled:true},errorVisible:true};
  assert.equal(failedJoinReady(ready,URL),true);
  for(const change of [v=>v.network.enrollment='pending',v=>v.network.device_id='registered',v=>v.network.request_id='request',
    v=>v.network.hub_url='https://other',v=>v.network.can_join=false,v=>v.join.enabled=false,v=>v.join.visible=false,v=>v.errorVisible=false]){
    const invalid=structuredClone(ready);change(invalid);assert.equal(failedJoinReady(invalid,URL),false);
  }
});
test('automatic enrollment or Refresh is never accepted as explicit Retry',()=>{
  const call={command:'device_network_request_join',args:{expectedRevision:'1',expectedGeneration:'2'}};
  assert.equal(explicitRetryObserved([call]),true);
  for(const calls of [[],[{command:'device_network_refresh'}],[call,call],[call,{command:'submit_prompt'}]])assert.equal(explicitRetryObserved(calls),false);
});
test('approved request must produce one stable registered ID and leave no pending request',()=>{
  const network={enrollment:'active',device_id:'device-a',can_join:false},snapshot={devices:[{device_id:'device-a'}],join_requests:[]};
  assert.equal(acceptedRetryIdentity(network,snapshot),true);
  assert.equal(acceptedRetryIdentity(network,snapshot,'device-a'),true);
  assert.equal(acceptedRetryIdentity(network,snapshot,'device-b'),false);
  assert.equal(acceptedRetryIdentity({...network,can_join:true},snapshot),false);
  assert.equal(acceptedRetryIdentity(network,{...snapshot,devices:[{device_id:'device-a'},{device_id:'device-b'}]}),false);
  assert.equal(acceptedRetryIdentity(network,{...snapshot,join_requests:[{request_id:'pending'}]}),false);
});
test('retry factory retains the common Desktop and real Hub resource lifecycle',()=>{
  const scenario=createHubJoinRetryControlsScenario();assert.equal(scenario.id,'hub.join-retry-controls');
  assert.equal(scenario.databaseRequired,true);assert.equal(scenario.manualGate,'pending');
  for(const key of ['prepare','execute','requestGracefulExit','quiesce','cleanup'])assert.equal(typeof scenario[key],'function');
});
