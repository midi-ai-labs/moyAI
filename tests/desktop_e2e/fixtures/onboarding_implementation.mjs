// A deterministic tool plan; execution and files still travel through the real Runner.
export const INPUT_NAME = "onboarding-numbers.csv";
export const SCRIPT_NAME = "Summarize-Numbers.ps1";
export const RESULT_NAME = "onboarding-result.md";
export function onboardingImplementationReply(messages) {
  const task = JSON.stringify(messages.filter(message => message.role === "user"));
  if (!task.includes(INPUT_NAME)) return null;
  const results = id => messages.filter(message => message.role === "tool" && message.tool_call_id === id);
  const call = (id, name, args) => ({ delta: { role: "assistant", tool_calls: [{ index: 0, id, type: "function", function: { name, arguments: JSON.stringify(args) } }] }, finish: "tool_calls" });
  const input = task.match(/\.moyai-shared-inputs-[^/]+\/onboarding-numbers\.csv/)?.[0];
  if (!input) throw new Error("Implementation has no materialized CSV input");
  if (results("implementation-read-result").length) {
    const result = results("implementation-read-result")[0].content;
    if (!result.includes("Count: 3") || !result.includes("Sum: 60")) throw new Error("The real script did not produce the expected CSV result");
    if (!results("implementation-publish-result").length) return call("implementation-publish-result", "shared_publish_artifact", { path: RESULT_NAME, name: RESULT_NAME });
    if (!results("implementation-publish-result")[0].content.includes("Saved shared artifact")) throw new Error("The executed result was not published to the shared job");
    return { delta: { role: "assistant", content: "CSVを集計する Summarize-Numbers.ps1 を作成し、実行しました。件数は3、合計は60です。スクリプトと onboarding-result.md を保存できます。" }, finish: "stop" };
  }
  if (results("implementation-execute").length) return call("implementation-read-result", "read", { path: RESULT_NAME });
  if (results("implementation-script").length) {
    const quote = value => "'" + value.replaceAll("'", "''") + "'";
    return call("implementation-execute", "shell", { command: `powershell.exe -NoProfile -ExecutionPolicy Bypass -File ./${SCRIPT_NAME} -InputPath ${quote(input)} -OutputPath ./${RESULT_NAME}` });
  }
  if (results("implementation-read-input").length) {
    if (!["10", "20", "30"].every(value => results("implementation-read-input")[0].content.includes(value))) throw new Error("Implementation CSV content was not read");
    const script = ["param([Parameter(Mandatory=$true)][string]$InputPath, [Parameter(Mandatory=$true)][string]$OutputPath)", "$ErrorActionPreference = 'Stop'", "$rows = @(Import-Csv -LiteralPath $InputPath)", "$sum = ($rows | Measure-Object -Property value -Sum).Sum", 'Set-Content -LiteralPath $OutputPath -Value @("Count: $($rows.Count)", "Sum: $sum") -Encoding UTF8', 'Get-Content -LiteralPath $OutputPath'];
    return call("implementation-script", "apply_patch", { patch_text: `*** Begin Patch\n*** Add File: ${SCRIPT_NAME}\n${script.map(line => "+" + line).join("\n")}\n*** End Patch` });
  }
  return call("implementation-read-input", "read", { path: input });
}
