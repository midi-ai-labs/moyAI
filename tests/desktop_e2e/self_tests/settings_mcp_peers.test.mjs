import assert from 'node:assert/strict';
import test from 'node:test';
import { startScriptedProvider } from '../drivers/scripted_provider.mjs';
import { createSettingsMcpPeerControlsScenario, peerRowsMatch, exactMcpCheckLedger } from '../scenarios/settings_mcp_peers.mjs';

const token='moyai_e2e_direct_peer_0123456789abcdef';
const payload={jsonrpc:'2.0',id:17,method:'tools/list',params:{}};
async function rpc(provider,authorization,body=payload,path='/mcp') {
  const response=await fetch(provider.baseUrl+path,{method:'POST',headers:{'content-type':'application/json',...(authorization?{Authorization:authorization}:{})},body:JSON.stringify(body)});
  return {status:response.status,body:await response.json()};
}
test('MCP option defaults to absent and retains ordinary model discovery',async t=>{
  const provider=await startScriptedProvider();t.after(()=>provider.close());
  assert.equal((await rpc(provider,`Bearer ${token}`)).status,404);
  const response=await fetch(provider.baseUrl+'/v1/models');assert.equal(response.status,200);await response.json();
  assert.equal(provider.requestLedger[0].route,'unknown');
});
test('isolated MCP endpoint checks saved bearer and lists agent tools without allowing execution',async t=>{
  const provider=await startScriptedProvider({mcpPeerToken:token});t.after(()=>provider.close());
  const good=await rpc(provider,`Bearer ${token}`);
  assert.equal(good.status,200);assert.equal(good.body.id,17);
  assert.deepEqual(good.body.result.tools.map(row=>row.name),['delegate_task','task_status','task_artifacts','cancel_task']);
  assert.equal((await rpc(provider,'Bearer another_fixture_0123456789abcdef')).status,401);
  assert.equal(exactMcpCheckLedger(provider.requestLedger,[200,401]),true);
  const call=await rpc(provider,`Bearer ${token}`,{...payload,method:'tools/call'});
  assert.equal(call.status,422);
  assert.equal((await rpc(provider,`Bearer ${token}`,payload,'/mcp?x=1')).status,404);
  const serialized=JSON.stringify({ledger:provider.requestLedger,resource:provider.resourceObservation()});
  assert.equal(serialized.includes(token),false);
  assert.equal(serialized.includes('another_fixture'),false);
  const close=await provider.close();assert.equal(close.pass,true);assert.equal(close.forced_connection_count,0);
});
test('MCP fixture rejects invalid configured tokens before binding',async()=>{
  for(const value of ['', 'short', 'x'.repeat(257), 'bad token'.repeat(8), 3]) await assert.rejects(startScriptedProvider({mcpPeerToken:value}),/mcpPeerToken/);
});
test('saved peer and wire oracles reject omitted rows, wrong target, untrusted request and extra calls',()=>{
  const surface={peers:[{id:'a'},{id:'b'}],rows:[{id:'a'},{id:'b'}]};
  assert.equal(peerRowsMatch(surface,['a','b']),true);assert.equal(peerRowsMatch(surface,['a']),false);
  assert.equal(peerRowsMatch({...surface,rows:[{id:'a'},{id:'a'}]},['a','b']),false);
  const row={route:'mcp_peer',method:'POST',pathname:'/mcp',query_present:false,contract:{pass:true,rpc_method:'tools/list'},response_phase:'completed',response_status:200};
  assert.equal(exactMcpCheckLedger([row],[200]),true);
  for(const bad of [{...row,route:'responses'},{...row,contract:{pass:false,rpc_method:'tools/list'}},{...row,response_status:401},{...row,pathname:'/v1/mcp'}]) assert.equal(exactMcpCheckLedger([bad],[200]),false);
  assert.equal(exactMcpCheckLedger([row,row],[200]),false);
});
test('direct MCP scenario keeps the common host lifecycle contract',()=>{
  const scenario=createSettingsMcpPeerControlsScenario();assert.equal(scenario.id,'settings.mcp-peer-controls');
  for(const name of ['prepare','execute','requestGracefulExit','quiesce','cleanup'])assert.equal(typeof scenario[name],'function');
});
