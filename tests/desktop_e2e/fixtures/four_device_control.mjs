// Deterministic harness qualification only. This never generates the acceptance
// application and must not count as live placement or application evidence.
export const CONTROL_PROMPT = "隔離された4端末の制御経路を確認してください。これは固定応答による試験であり、アプリ開発の受入試験ではありません。";
export const KEEPALIVE_COMMAND = "Write-Output 'four-device controlled service'; while ($true) { Start-Sleep -Seconds 1 }";
export const INDEPENDENT_COMMAND = "Write-Output 'four-device independent inspection'";
export function controlReply(request, environments) {
  const messages = request.messages;
  const users = JSON.stringify(messages.filter(row => row.role === "user"));
  const done = id => messages.some(row => row.role === "tool" && row.tool_call_id === id);
  const call = (id, name, args) => ({ delta: { role: "assistant", tool_calls: [{ index: 0, id, type: "function", function: { name, arguments: JSON.stringify(args) } }] }, finish: "tool_calls" });
  const answer = text => ({ delta: { role: "assistant", content: text }, finish: "stop" });
  if (users.includes("ファイルやサービスは変更しないでください")) {
    if (done("independent-inspection")) return answer("定義を確認しました。UI/API、Worker、DBが別環境で実行されるプロジェクトです。");
    return call("independent-inspection", "shell", { command: INDEPENDENT_COMMAND, sandbox_permissions: "require_escalated", justification: "Print only the controlled independent-inspection marker to verify approval belongs to its own requesting PC." });
  }
  if (users.includes("four-device-child:")) {
    if (done("controlled-service")) return answer("この環境の制御試験用プロセスを保持しました。");
    return call("controlled-service", "shell_start", { command: KEEPALIVE_COMMAND, retain_after_turn: true, sandbox_permissions: "require_escalated", justification: "Start only the isolated acceptance harness keepalive process; normal conversation stop will terminate it." });
  }
  for (const role of ["worker", "database"]) {
    if (!environments[role]?.environment_id) throw new Error("Control fixture has no GUI-prepared environments");
    if (!done(`delegate-${role}`)) return call(`delegate-${role}`, "shared_delegate", { environment_id: environments[role].environment_id, title: `${role}の制御確認`, prompt: `four-device-child:${role} 隔離試験用の保持プロセスを起動してください。` });
  }
  if (!done("controlled-service")) return call("controlled-service", "shell_start", { command: KEEPALIVE_COMMAND, retain_after_turn: true, sandbox_permissions: "require_escalated", justification: "Start only the isolated acceptance harness keepalive process; normal conversation stop will terminate it." });
  return answer("3環境の制御試験用プロセスを保持しました。これはアプリ受入結果ではありません。");
}
