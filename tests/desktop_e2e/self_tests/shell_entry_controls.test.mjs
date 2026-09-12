import assert from 'node:assert/strict';
import test from 'node:test';
import { actionById } from '../../../ui/desktop-web/src/actions.ts';
import { SHELL_MENU_ENTRY_PLAN,SHELL_PALETTE_ENTRY_PLAN,entryDestinationMatches,emptyShellContentPreserved,createMenuEntryControlsScenario,createPaletteEntryControlsScenario } from '../scenarios/shell_entry_controls.mjs';
const settled={navigation_loading:false,busy:false,background_mutation_pending:false,async_polling_required:false,post_run_refresh_pending:false,pending_async_operations:[]};

test('entry plans target real current menu/palette declarations',()=>{
  assert.equal(SHELL_MENU_ENTRY_PLAN.length,9);
  assert.equal(SHELL_PALETTE_ENTRY_PLAN.length,14);
  for(const entry of SHELL_MENU_ENTRY_PLAN)assert.equal(actionById(entry.action)?.menu,entry.menu,entry.action);
  for(const entry of SHELL_PALETTE_ENTRY_PLAN)assert.equal(actionById(entry.action)?.palette,true,entry.action);
});
test('destination oracle requires the rendered destination and exact control state',()=>{
  const plan={overlay:'hub',heading:'hub-dialog-title'};
  const state={errors:[],projection:{...settled,overlay:'hub',session_search_include_archived:false},headings:['hub-dialog-title'],collapsed:false};
  assert.equal(entryDestinationMatches(state,plan),true);
  assert.equal(entryDestinationMatches({...state,headings:[]},plan),false);
  assert.equal(entryDestinationMatches({...state,errors:['visible error']},plan),false);
  assert.equal(entryDestinationMatches(state,{collapsed:true}),false);
  assert.equal(entryDestinationMatches(state,{archived:true}),false);
});
test('new chat entry rejects the observed intermediate navigation and requires the intended workspace',()=>{
  const options={newChatWorkspace:'C:/fixture/data/quick-chat-workspace'};
  const plan=SHELL_MENU_ENTRY_PLAN[0];
  const state={errors:[],prompt:'',projection:{...settled,overlay:'none',workspace_path:options.newChatWorkspace,
    draft_target:{workspacePath:options.newChatWorkspace,sessionId:null},thread_empty:true},headings:[]};
  assert.equal(entryDestinationMatches(state,plan,options),true);
  for(const projection of [
    {...state.projection,navigation_loading:true,async_polling_required:true,pending_async_operations:['workspace-load']},
    {...state.projection,workspace_path:'C:/fixture/workspace'},
    {...state.projection,draft_target:{workspacePath:'C:/fixture/workspace',sessionId:null}},
    {...state.projection,draft_target:{...state.projection.draft_target,sessionId:'old'}},
    {...state.projection,thread_empty:false}
  ])assert.equal(entryDestinationMatches({...state,projection},plan,options),false);
  assert.equal(entryDestinationMatches(state,plan),false);
});
test('entry content oracle rejects loss, workspace/owner drift and task start',()=>{
  const state={prompt:'',projection:{workspace_path:'fixture',transcript_rows:[],thread_empty:true,draft_target:{id:1},busy:false}};
  assert.equal(emptyShellContentPreserved(state,structuredClone(state)),true);
  for(const after of [{...state,prompt:'lost'},{...state,projection:{...state.projection,workspace_path:'other'}},{...state,projection:{...state.projection,busy:true}},{...state,projection:{...state.projection,draft_target:{id:2}}}])assert.equal(emptyShellContentPreserved(state,after),false);
  assert.equal(emptyShellContentPreserved(state,{...state,projection:{...state.projection,draft_target:{id:2}}},{newChat:true}),true);
  assert.equal(emptyShellContentPreserved(state,{...state,projection:{...state.projection,workspace_path:'quick-chat'}},{newChat:true}),true);
});
test('new chat may change only its empty-placeholder help when leaving a project',()=>{
  const before={prompt:'',projection:{workspace_path:'project',thread_empty:true,busy:false,
    draft_target:{id:1},transcript_rows:[{row_kind:'empty_placeholder',body:'project help'}]}};
  const after={prompt:'',projection:{...before.projection,workspace_path:'quick-chat',
    draft_target:{id:2},transcript_rows:[{row_kind:'empty_placeholder',body:'quick-chat help'}]}};
  assert.equal(emptyShellContentPreserved(before,after,{newChat:true}),true);
  assert.equal(emptyShellContentPreserved(before,after),false);
  assert.equal(emptyShellContentPreserved(before,{...after,projection:{...after.projection,transcript_rows:[{row_kind:'message',body:'unexpected'}]}},{newChat:true}),false);
  assert.equal(emptyShellContentPreserved({...before,projection:{...before.projection,thread_empty:false}},after,{newChat:true}),false);
});
test('menu/palette remain separate reusable common-lifecycle scenarios',()=>{
  for(const [factory,id]of[[createMenuEntryControlsScenario,'navigation.menu-entry-controls'],[createPaletteEntryControlsScenario,'navigation.palette-entry-controls']]){
    const scenario=factory();assert.equal(scenario.id,id);
    for(const name of ['prepare','execute','requestGracefulExit','quiesce','cleanup'])assert.equal(typeof scenario[name],'function');
  }
});
