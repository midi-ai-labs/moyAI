import test from "node:test";
import assert from "node:assert/strict";
import { createHubOfflineResetScenario, resetAccepted } from "../scenarios/hub_offline_reset.mjs";

test("offline recovery requires empty device authority and both independent model selections cleared",()=>{
  const network={enrollment:"unconfigured",hub_url:"",device_id:null,request_id:null,peers:[],receiver:{enabled:false}};
  const hub={main_mode:"direct",side_chat_mode:"direct",main_review:null,side_chat_review:null};
  assert.equal(resetAccepted(network,hub),true);
  for(const changed of [{device_id:"old"},{hub_url:"https://old"},{request_id:"pending"},{peers:[{}]},{receiver:{enabled:true}}]) assert.equal(resetAccepted({...network,...changed},hub),false);
  assert.equal(resetAccepted(network,{...hub,side_chat_review:{}}),false);
  assert.equal(resetAccepted(network,{...hub,main_mode:"hub"}),false);
  assert.equal(createHubOfflineResetScenario({headed:true}).id,"hub.offline-reset");
});
