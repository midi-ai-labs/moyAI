import assert from 'node:assert/strict';
import test from 'node:test';
import {outgoingPeerSearchMatches} from '../scenarios/outgoing_peer_search.mjs';
const key='["device","profile"]',baseline=[{key,selected:true}];
function value(query='match'){return {query,focused:true,visibleKeys:query==='none'?[]:[key],network:{peers:[{device_id:'device',profile_id:'profile',selected:true}]},emptyText:'検索条件を確認してください',errors:0};}
test('peer search matches displayed owner without changing persisted selection',()=>{
  for(const query of ['match','none',''])assert.equal(outgoingPeerSearchMatches(value(query),{query,visibleKeys:query==='none'?[]:[key],baseline}),true);
  for(const mutate of [v=>v.visibleKeys=['wrong'],v=>v.network.peers[0].selected=false,v=>v.query='wrong',v=>v.focused=false,v=>v.errors=1]){const v=value();mutate(v);assert.equal(outgoingPeerSearchMatches(v,{query:'match',visibleKeys:[key],baseline}),false);}
});
