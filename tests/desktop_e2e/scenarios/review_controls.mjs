import path from "node:path";
import { writeFile, readFile, mkdir } from "node:fs/promises";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { DesktopE2eError } from "../core/execution.mjs";
import { WebviewInput } from "../drivers/webview_input.mjs";
import { startScriptedProvider } from "../drivers/scripted_provider.mjs";
import { prepareDesktopFixture } from "./fixture.mjs";
import { providerRestartFixtureConfig } from "./provider_restart.mjs";
import { acquireInteractiveShell, requestGracefulExit } from "./shell_baseline.mjs";
import { captureScenarioScreenshot, invokeDesktopCommand } from "./observations.mjs";
import { byId, action, wait, trustedClick } from "./hub_browser_enrollment.mjs";

const OWNER="scenario:review.uncommitted-controls", PROMPT="Review fixture changes.", FINAL="GUI_REVIEW_FINISHED";
// Observable review prompt contract for one untracked file in an unborn fixture repository.
const REVIEW_PROMPT = ["Review the current uncommitted workspace changes.", "Base ref: HEAD", "Head ref: gui-controls",
  "Git summary: untracked: 1 file(s)", "Changed files:", "- E2E_REVIEW.txt", "", "Authority-scoped Git inspection:",
  "- Keep the shell workdir at the selected workspace directory.",
  "- The trailing `-- .` is the mandatory literal pathspec; do not omit it or replace it with a repository-wide Git command.",
  "- Inventory: `git --literal-pathspecs --no-optional-locks -c core.fsmonitor=false -c submodule.recurse=false status --short --untracked-files=all --ignore-submodules=all -- .`",
  "- Unstaged diff: `git --literal-pathspecs --no-optional-locks -c core.fsmonitor=false -c submodule.recurse=false diff --no-ext-diff --no-textconv --ignore-submodules=all -- .`",
  "- Staged diff: `git --literal-pathspecs --no-optional-locks -c core.fsmonitor=false -c submodule.recurse=false diff --cached --no-ext-diff --no-textconv --ignore-submodules=all -- .`",
  "Additional review request:", PROMPT, "",
  "Inspect only this scope, gather evidence, and report findings first with severity, path, rationale, and impact. If no material issue is found, say so explicitly."].join("\n");
export function createReviewControlsScenario() {
  const state={provider:null,input:null,close:null,failures:[]};
  return Object.freeze({id:"review.uncommitted-controls",productOracle:"pass",manualGate:"pending",databaseRequired:true,
    async prepare({context,sink,phase}) {
      state.provider=await startScriptedProvider({expectedPrompt:REVIEW_PROMPT,responseText:FINAL});
      await prepareDesktopFixture({context,sink,phase,owner:OWNER,configText:providerRestartFixtureConfig(state.provider.baseUrl),
        sentinelName:"E2E_REVIEW.txt",sentinelText:"Review this one isolated untracked file.\n"});
      await promisify(execFile)("git",["-C",context.paths.workspace,"-c","init.templateDir=","init","--initial-branch=gui-controls"],{windowsHide:true,timeout:5000});
      await mkdir(path.join(context.paths.workspace,".git","info"),{recursive:true});
      await writeFile(path.join(context.paths.workspace,".git","info","exclude"),".moyai/\n");
    },
    async execute({context,driver:cdp,sink}) {
      await acquireInteractiveShell({context,driver:cdp,sink},{evidenceOwner:OWNER,screenshotStem:"review-shell"});
      const input=state.input=new WebviewInput(cdp,{probeId:"review-controls"}); await input.installProbe();
      await trustedClick(input,cdp,byId("prompt","TEXTAREA"),sink); await input.insertText(byId("prompt","TEXTAREA"),PROMPT);
      await trustedClick(input,cdp,action("show-command-palette","section.composer"),sink);
      await trustedClick(input,cdp,byId("local-search","INPUT"),sink); await input.insertText(byId("local-search","INPUT"),"未コミット差分をレビュー");
      await trustedClick(input,cdp,action("review-uncommitted",".command"),sink);
      const result=await wait("Review action starts and completes the scoped canonical review",async()=>({p:await invokeDesktopCommand(cdp,"desktop_state"),ledger:state.provider.requestLedger}),
        value=>value.ledger.some(row=>row.contract?.pass&&row.response_phase==="completed")&&value.p.run_status_key==="completed"&&value.p.can_submit);
      if(result.ledger.filter(row=>row.route==="responses").length!==1)throw new DesktopE2eError("product","review-duplicate-request","Review produced an unexpected model request count",result);
      if(await cdp.evaluate(`document.body.textContent.includes(${JSON.stringify(FINAL)})`)!==true)throw new DesktopE2eError("product","review-result-missing","Completed review result is not visible",result);
      await captureScenarioScreenshot({cdp,sink,name:"review-uncommitted-finished",owner:OWNER});
      if(await readFile(path.join(context.paths.workspace,"E2E_REVIEW.txt"),"utf8")!=="Review this one isolated untracked file.\n")throw new Error("Review modified the sentinel");
      await sink.record("uncommitted-review-control",result,{phase:"executing",owner:OWNER});
      return{acquisition:"pass",oracle:"pass",manual:"pending"};
    },
    async requestGracefulExit(cdp){if(state.input){try{await state.input.cleanup();}catch{state.failures.push("input-cleanup");}state.input=null;}return requestGracefulExit(cdp);},
    async quiesce(){if(state.input){try{await state.input.cleanup();}catch{state.failures.push("input-cleanup");}state.input=null;}const provider=state.provider?await state.provider.close():{pass:true};state.close={pass:provider.pass&&!state.failures.length,provider,failures:[...state.failures]};return{input:state.close.pass?"pass":"fail",resources:[{kind:"review-controls",...state.close}]};},
    async cleanup(){return{input:state.close?.pass?"pass":"fail",resources:[]};},
  });
}
