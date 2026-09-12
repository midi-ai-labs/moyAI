import {isDeepStrictEqual as same} from 'node:util';
import {DesktopE2eError} from '../core/execution.mjs';
import {assertExactDesktopCommandSequence} from '../drivers/desktop_command_probe.mjs';
import {captureScenarioScreenshot,invokeDesktopCommand} from './observations.mjs';
import {trustedClick,wait} from './hub_browser_enrollment.mjs';

const OWNER='scenario:agent.interrupt';
const rootHistory=p=>p.transcript_rows.filter(row=>['user','assistant','work_summary_completed'].includes(row.row_kind)).map(row=>({kind:row.row_kind,identity:row.stable_history_identity??null,title:row.title,body:row.body}));
export function agentHistoryLedger(ledger,count,terminal=false){
  const expected=['root_initial','root_continuation',...Array.from({length:count},(_,i)=>`child_tool_${i}`),'child_held'];
  return Array.isArray(ledger)&&ledger.length===expected.length&&expected.every(role=>{
    const rows=ledger.filter(row=>row.contract?.role===role),row=rows[0];
    return rows.length===1&&row.route==='responses'&&row.method==='POST'&&row.pathname==='/v1/responses'&&row.query_present===false&&row.contract.pass===true
      &&row.response_phase===(role==='child_held'?(terminal?'peer_closed':'held'):'completed')&&row.response_status===(role==='child_held'?null:200);
  });
}
export function agentHistoryOwnerPreserved(p,root){return p?.workspace_path===root.workspace&&p.run_target?.sessionId===root.sessionId&&p.draft_target?.sessionId===root.sessionId
  &&p.run_status_key==='completed'&&!p.busy&&same(rootHistory(p),root.history);}
export function agentHistoryPrepended(after,before,toolCalls){
  const ids=after?.historyIds??[],prior=before?.historyIds??[],rows=after?.rows??[];
  return after?.previous===0&&after.agentPath===before.agentPath&&after.errors===0
    &&before.count===1&&after.count===2&&after.meta==='2件の履歴'
    &&prior.length===1&&same(ids,prior)&&rows.length===2
    &&rows[0].kind==='system'&&rows[0].identity===null&&rows[0].title==='Agent間の追加指示'
    &&rows[0].body===before.expectedTask
    &&rows[1].kind==='work_summary_running'&&rows[1].identity===prior[0]
    &&after.toolCount===toolCalls*2;
}
async function surface(cdp,agentPath){const p=await invokeDesktopCommand(cdp,'desktop_state');return {p,...await cdp.evaluate(String.raw`(()=>{
  const section=document.querySelector('aside#sub-agent-inspector section.agent-execution[data-agent-path='+CSS.escape(${JSON.stringify(agentPath)})+']');
  const composer=document.querySelector('section.composer');
  const meta=section?.querySelector('.agent-execution-meta small')?.textContent.trim()??'';
  return {composerRunTarget:composer?.dataset.runTarget?JSON.parse(composer.dataset.runTarget):null,agentPath:section?.dataset.agentPath??null,meta,count:Number(meta.match(/^([0-9]+)件/)?.[1]??0),status:section?.querySelector('.agent-execution-meta > span')?.textContent.trim()??'',
    previous:section?.querySelectorAll('button[data-action="load-previous-agent-execution-page"]').length??0,
    historyIds:Array.from(section?.querySelectorAll('.agent-execution-scroll [data-history-identity]')??[]).map(n=>n.dataset.historyIdentity),historyText:section?.querySelector('.agent-execution-scroll')?.textContent??'',
    rows:Array.from(section?.querySelectorAll('.agent-execution-scroll > article.message')??[]).map(n=>({kind:n.classList.contains('system')?'system':n.classList.contains('work_summary_running')?'work_summary_running':'other',identity:n.dataset.historyIdentity??null,title:n.querySelector('h2')?.textContent.trim()??'',body:n.querySelector('.markdown-body')?.textContent.replace(/\s+/g,' ').trim()??''})),
    toolCount:Number(Array.from(section?.querySelectorAll('.work-history-meta > span')??[]).find(n=>n.querySelector('strong')?.textContent==='コマンド/ツール')?.querySelector('small')?.textContent.match(/^([0-9]+)件$/)?.[1]??-1),
    interrupt:section?.querySelectorAll('button[data-action="interrupt-agent"]').length??0,errors:document.querySelectorAll('.fatal,.ui-error-notice,.agent-execution-error').length};})()`)};}

export async function executeAgentHistoryPage({cdp,input,commandProbe,provider,sink,count,agentPath,selectors}){
  const held=await wait('Finite child tool history settles while only its last model request remains active',async()=>({surface:await surface(cdp,agentPath),ledger:provider.requestLedger}),v=>{
    const p=v.surface.p,row=p.agent_activity_rows.find(row=>row.agent_path===agentPath);
    return same(v.surface.composerRunTarget,p.run_target)&&v.surface.errors===0&&p.run_status_key==='completed'&&!p.busy&&p.can_submit&&p.post_run_refresh_pending===false&&p.pending_async_operations.length===0
      &&p.run_target?.expectedState?.kind==='idle'&&row?.status==='running'&&row.interrupt_target&&agentHistoryLedger(v.ledger,count);
  },60000);
  const p=held.surface.p,target=structuredClone(p.agent_activity_rows.find(row=>row.agent_path===agentPath).interrupt_target);
  const root={workspace:p.workspace_path,sessionId:target.rootSessionId,history:rootHistory(p)};
  const executionTarget={workspacePath:target.workspacePath,rootSessionId:target.rootSessionId,agentPath:target.agentPath,childSessionId:target.childSessionId};
  await trustedClick(input,cdp,selectors.output,sink);await trustedClick(input,cdp,selectors.card,sink);
  const latest=await invokeDesktopCommand(cdp,'load_agent_execution',{expectedTarget:executionTarget});
  if(!latest.turn_page_has_previous||latest.turn_page_offset<=0||latest.turn_page_offset>=80||latest.turn_page_end!==latest.turn_page_total){throw new DesktopE2eError('harness','agent-history-fixture-too-short','Finite child tools did not create one older history page',{count,latest,ledger:provider.requestLedger});}
  const before=await wait('Child inspector exposes actual previous-history button and latest range',()=>surface(cdp,agentPath),v=>v.agentPath===agentPath&&v.previous===1&&v.interrupt===1&&v.errors===0
    &&v.meta===`${latest.transcript_rows.length}件を表示 · 以前の実行履歴あり`&&v.historyIds.length>0&&agentHistoryOwnerPreserved(v.p,root));
  before.expectedTask='Message Type: NEW_TASK Task name: '+agentPath+' Sender: /root Payload: remain active until the user interrupts this exact child';
  await sink.record('agent-history-before-previous-page',{count,target,latest,before},{phase:'executing',owner:OWNER});
  await captureScenarioScreenshot({cdp,sink,name:'agent-history-latest-range',owner:OWNER});
  const previous={selector:`aside#sub-agent-inspector button[data-action="load-previous-agent-execution-page"][data-agent-path=${JSON.stringify(agentPath)}]`,identity:{tag:'BUTTON',action:'load-previous-agent-execution-page'}};
  await commandProbe.install();await trustedClick(input,cdp,previous,sink);
  const after=await wait('Previous-history click prepends older child records and keeps the entire prior range',()=>surface(cdp,agentPath),v=>agentHistoryPrepended(v,before,count)&&agentHistoryOwnerPreserved(v.p,root));
  const expected=[{command:'load_previous_agent_execution_page',args:{expectedTarget:executionTarget,expectedOffset:latest.turn_page_offset,expectedEnd:latest.turn_page_end}}];
  const prependProof=assertExactDesktopCommandSequence(await commandProbe.snapshot(),{expected});
  await captureScenarioScreenshot({cdp,sink,name:'agent-history-older-range-prepended',owner:OWNER});
  await trustedClick(input,cdp,selectors.interrupt,sink);
  const terminal=await wait('Exact child stop terminates the held request without changing Main',async()=>({surface:await surface(cdp,agentPath),ledger:provider.requestLedger,resource:provider.resourceObservation()}),v=>{
    const p=v.surface.p,row=p.agent_activity_rows.find(row=>row.agent_path===agentPath);
    return row?.session_id===target.childSessionId&&row.status==='interrupted'&&row.active_turn_id===null&&row.interrupt_target===null&&row.result_preview==='Interrupted'
      &&p.agent_tree_active===false&&p.task_activity_state==='idle'&&p.post_run_refresh_pending===false&&p.background_mutation_pending===false&&p.async_polling_required===false&&p.pending_async_operations.length===0&&p.navigation_loading===false
      &&v.surface.interrupt===0&&v.surface.errors===0&&v.surface.status==='中断'&&same(v.surface.composerRunTarget,p.run_target)
      &&agentHistoryOwnerPreserved(p,root)&&agentHistoryLedger(v.ledger,count,true)&&v.resource.active_request_count===0;
  },30000);
  const terminalPage=await invokeDesktopCommand(cdp,'load_agent_execution',{expectedTarget:executionTarget});
  if(terminalPage.session_id!==target.childSessionId||terminalPage.transcript_rows.filter(row=>row.row_kind==='work_summary_cancelled').length!==1
    ||terminalPage.transcript_rows.some(row=>['assistant','work_summary_completed','work_summary_failed','error'].includes(row.row_kind))){throw new DesktopE2eError('product','agent-history-terminal-mismatch','Interrupted child canonical history did not retain a single cancelled terminal',{target,terminalPage});}
  const proof=assertExactDesktopCommandSequence(await commandProbe.snapshot(),{expected:[...expected,{command:'interrupt_agent',args:{expectedTarget:target}}]});
  await captureScenarioScreenshot({cdp,sink,name:'agent-history-terminal-owner-preserved',owner:OWNER});
  await sink.record('agent-child-history-page-result',{count,target,root,latest,before,after,prependProof,terminal,terminalPage,proof},{phase:'executing',owner:OWNER});
  return {acceptedLedger:structuredClone(terminal.ledger),result:{acquisition:'pass',oracle:'pass',manual:'pending'}};
}
