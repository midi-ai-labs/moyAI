import test from 'node:test';
import assert from 'node:assert/strict';
import { disclosureRoundTrip, temporaryConfigViewMatches, createGlobalAdditionalControlsScenario, createInitialAdditionalControlsScenario, createSessionDiscardCloseScenario } from '../scenarios/settings_additional_controls.mjs';
test('disclosure draft comparison rejects removal and changed values',()=>{
  const before=[{key:'Main',value:'日本\n保持'},{key:'Side',value:'side'}];
  assert.equal(disclosureRoundTrip(before,[...before].reverse()),true);
  assert.equal(disclosureRoundTrip(before,before.slice(0,1)),false);
  assert.equal(disclosureRoundTrip(before,[before[0],{key:'Side',value:'changed'}]),false);
});
test('additional settings cases share the existing lifecycle and fixture contract',()=>{
  const cases=[createGlobalAdditionalControlsScenario(),createInitialAdditionalControlsScenario(),createSessionDiscardCloseScenario()];
  assert.equal(new Set(cases.map(s=>s.id)).size,3);
  for(const scenario of cases){assert.equal(scenario.databaseRequired,true);for(const method of ['prepare','execute','requestGracefulExit','quiesce','cleanup'])assert.equal(typeof scenario[method],'function');}
});
test('temporary Apply compares persisted fields separately from effective projection',()=>{
  const original=new Map([['model.model','saved'],['model.context_window','65536']]);
  const effective=new Map([['model.model','temporary'],['model.context_window','32768']]);
  const surface={dirty:false,projection:{config_fields:[...original].map(([key,value])=>({key,value})),model_label:'temporary',provider_effective_context_window:'32768'}};
  assert.equal(temporaryConfigViewMatches(surface,original,effective),true);
  assert.equal(temporaryConfigViewMatches(surface,original,original),false);
  assert.equal(temporaryConfigViewMatches({...surface,projection:{...surface.projection,provider_context_window:'32768',provider_effective_context_window:'65536'}},original,effective),false);
  assert.equal(temporaryConfigViewMatches({...surface,dirty:true},original,effective),false);
  assert.equal(temporaryConfigViewMatches({...surface,projection:{...surface.projection,config_fields:[...effective].map(([key,value])=>({key,value}))}},original,effective),false);
});
