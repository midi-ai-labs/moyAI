import assert from 'node:assert/strict';
import test from 'node:test';
import { workspaceSelectionSucceeded } from '../scenarios/workspace_controls.mjs';

test('workspace success requires the selected path and its affirmative status, not merely closed navigation',()=>{
  const ready={fatal:0,errors:[],dialog:false,p:{overlay:'none',navigation_loading:false,navigation_admission_open:true,
    workspace_path:'C:/fixture/日本語 別プロジェクト',status_message:'作業フォルダー: C:/fixture/日本語 別プロジェクト'}};
  const target=ready.p.workspace_path;
  assert.equal(workspaceSelectionSucceeded(ready,target),true);
  for(const change of [v=>v.p.status_message='the request draft owner changed before the action was applied',
    v=>v.p.status_message='作業フォルダー: C:/other',v=>v.p.workspace_path='C:/other',v=>v.p.navigation_loading=true,
    v=>v.p.status_message='作業フォルダーを切り替えられませんでした',v=>v.p.overlay='workspace_picker',
    v=>v.p.status_message='作業フォルダーを入力してください。',
    v=>v.p.status_message='作業フォルダーを開けません: C:/missing (not found)',
    v=>v.p.status_message='フォルダーを指定してください: C:/file.txt',
    v=>v.p.navigation_admission_open=false,v=>v.dialog=true,v=>v.errors=['error'],v=>v.fatal=1]){
    const invalid=structuredClone(ready);change(invalid);assert.equal(workspaceSelectionSucceeded(invalid,target),false);
  }
});
