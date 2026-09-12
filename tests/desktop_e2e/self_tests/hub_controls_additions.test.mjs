import test from 'node:test';
import assert from 'node:assert/strict';
import {hubRouteModeFailures,hubCapabilitiesReady,assertHubTrustedClear} from '../scenarios/hub_connection_settings.mjs';
import {receiverDiagnosticFailures,receiverRecommendationReady} from '../scenarios/mcp_receiver_connection_controls.mjs';

test('clearing a capability requires real Backspace deletion at the same focused empty input',()=>{
  const identity={tag:'INPUT',id:'hub-main-capabilities'},target={identity};
  const snapshot={found:true,sequence:3,dropped_through:0,events:[
    {sequence:1,type:'keydown',isTrusted:true,...identity,key:'Backspace'},
    {sequence:2,type:'input',isTrusted:true,...identity,inputType:'deleteContentBackward',data:null},
    {sequence:3,type:'keyup',isTrusted:true,...identity,key:'Backspace'},
  ]};
  const observation={count:1,value:'',focused:true};
  assert.doesNotThrow(()=>assertHubTrustedClear(snapshot,0,target,observation));
  for(const mutate of [v=>v.events[1].isTrusted=false,v=>v.events[1].id='hub-side_chat-capabilities',v=>v.events[1].inputType='insertText',v=>v.events.splice(1,1)]){
    const bad=structuredClone(snapshot);mutate(bad);assert.throws(()=>assertHubTrustedClear(bad,0,target,observation));
  }
  for(const bad of [{...observation,value:'tools'},{...observation,focused:false},{...observation,count:2}])assert.throws(()=>assertHubTrustedClear(snapshot,0,target,bad));
});

test('route controls reflect both independent modes and preserve both saved selections',()=>{
  const reviews={main:{selection:{preferred_model_id:'main'}},side_chat:{selection:{preferred_model_id:'side'}}};
  const modes={main:'hub',side_chat:'direct'};
  const surface={fatal_count:0,hub:{status:'connected',main_mode:'hub',side_chat_mode:'direct',main_review:reviews.main,side_chat_review:reviews.side_chat},main:{routes:{hub:{pressed:'true',enabled:false},direct:{pressed:'false',enabled:true}}},side_chat:{routes:{direct:{pressed:'true',enabled:false},hub:{pressed:'false',enabled:true}}}};
  assert.deepEqual(hubRouteModeFailures(surface,modes,reviews),[]);
  for(const mutate of [v=>v.hub.side_chat_mode='hub',v=>v.main.routes.direct.pressed='true',v=>v.main.routes.direct.enabled=false,v=>v.hub.side_chat_review={selection:{preferred_model_id:'main'}},v=>v.fatal_count=1]){const bad=structuredClone(surface);mutate(bad);assert.notDeepEqual(hubRouteModeFailures(bad,modes,reviews),[]);}
});
test('capability validation requires exact visible draft, explanatory feedback and disabled Save',()=>{
  const surface={fatal_count:0,main:{capabilities:'Tools',save_enabled:false,feedback:'選択モデルに必要な機能を満たすものがありません。'}};
  assert.equal(hubCapabilitiesReady(surface,'main','Tools',{valid:false,feedback:'必要な機能'}),true);
  for(const mutate of [v=>v.main.save_enabled=true,v=>v.main.capabilities='tools',v=>v.main.feedback='',v=>v.fatal_count=1]){const bad=structuredClone(surface);mutate(bad);assert.equal(hubCapabilitiesReady(bad,'main','Tools',{valid:false,feedback:'必要な機能'}),false);}
});
const before={revision:'3',generation:'2',device_id:'receiver',hub_url:'https://127.0.0.1:40001',receiver:{profile_id:'pub',enabled:true,endpoint:'https://127.0.0.1:40002/mcp',status:'receiving'}};
function receiverDiagnostic(){return {projection:structuredClone(before),count:1,open:true,buttonEnabled:true,warnings:0,text:'診断日時: 2026/9/12',stages:[{label:'ローカルIPv4',status:'pass',detail:'127.0.0.1'},{label:'待受IPv4と証明書',status:'pass',detail:'現在のIPv4が端末証明書に含まれています。'},{label:'この端末の待受',status:'pass',detail:before.receiver.endpoint+' / receiving'},{label:'別端末からの到達',status:'skipped',detail:'この端末内の確認だけでは、別端末からの到達は確定できません。'}]};}
test('receiver diagnostic preserves publication and distinguishes legitimate physical reachability limit',()=>{
  assert.deepEqual(receiverDiagnosticFailures(receiverDiagnostic(),'receiver',before),[]);
  for(const mutate of [v=>v.stages[3].status='pass',v=>v.stages[1].status='fail',v=>v.stages[2].detail='another listener',v=>v.projection.revision='4',v=>v.projection.receiver.enabled=false,v=>v.open=false,v=>v.count=2,v=>v.warnings=1]){const bad=receiverDiagnostic();mutate(bad);assert.notDeepEqual(receiverDiagnosticFailures(bad,'receiver',before),[]);}
});
test('Hub diagnostic requires current working TLS Gateway and all stages rather than accepting any result',()=>{
  const value=receiverDiagnostic();value.stages=['ローカルIPv4','HubへのTCP接続','HubのTLS・端末認可','Hub証明書の自動更新','Gateway証明書の自動更新','モデルGatewayへのTLS接続'].map(label=>({label,status:'pass',detail:'confirmed'}));
  assert.deepEqual(receiverDiagnosticFailures(value,'hub',before),[]);
  for(const mutate of [v=>v.stages.pop(),v=>v.stages.reverse(),v=>v.stages[5].status='skipped',v=>v.stages[2].detail='']){const bad=structuredClone(value);mutate(bad);assert.notDeepEqual(receiverDiagnosticFailures(bad,'hub',before),[]);}
});
test('recommendation changes only draft selection and requires the independent saved owners to stay unchanged',()=>{
  const hub={settings_revision:'5',main_mode:'direct',side_chat_mode:'direct',main_review:null,side_chat_review:null,recommended_main_selection:{allowed_model_ids:['a'],preferred_model_id:'a',required_capabilities:['tools'],wait_policy:'wait_for_preferred',affinity_turns:3}};
  const surface={fatal:0,selected:['a'],preferred:'a',wait:'wait_for_preferred',affinity:'3',capabilities:'tools',saveEnabled:true,hub:structuredClone(hub)};
  assert.equal(receiverRecommendationReady(surface,hub),true);
  for(const mutate of [v=>v.selected=['a','a'],v=>v.capabilities='vision',v=>v.hub.main_mode='hub',v=>v.hub.side_chat_review={},v=>v.hub.settings_revision='6',v=>v.saveEnabled=false]){const bad=structuredClone(surface);mutate(bad);assert.equal(receiverRecommendationReady(bad,hub),false);}
});
