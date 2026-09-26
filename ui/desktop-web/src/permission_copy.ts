const riskLabels: Record<string, string> = {
  destructive_delete: "削除を含む可能性",
  delete: "削除を含む可能性",
  move_or_rename: "移動・名前変更を含む可能性",
  "move/rename": "移動・名前変更を含む可能性",
  network: "ネットワーク通信を含む可能性",
  external_connection: "外部への接続・接続設定を含む可能性",
  "external connection/setup": "外部への接続・接続設定を含む可能性",
  configured_local_service: "設定済みのローカルサービスへの接続",
  "configured local service": "設定済みのローカルサービスへの接続",
  protected_workspace_authority: "保護された設定・指示への変更",
  "protected workspace authority": "保護された設定・指示への変更",
  external_mutation: "外部サービスのデータを変更する操作",
  "external mutation": "外部サービスのデータを変更する操作",
  external_destructive_operation: "外部サービスのデータを削除・破棄する操作",
  "destructive external operation": "外部サービスのデータを削除・破棄する操作",
  unclassified_shell: "影響を自動で判定できないコマンド",
  "unclassified dynamic/indirect shell construct": "影響を自動で判定できないコマンド",
};

export function permissionRiskLabel(risk: string): string {
  return riskLabels[risk] ?? risk;
}

export const guardianReasonPrefix = "代理承認からの確認: ";

export function permissionReviewReason(details: string[]): string | undefined {
  return details.find(detail => detail.startsWith(guardianReasonPrefix))?.slice(guardianReasonPrefix.length);
}

export function permissionDetailLabel(detail: string): string {
  const prefixes: [string, string][] = [
    ["Run shell command: ", "コマンドを実行: "],
    ["Command: ", "実行コマンド: "],
    ["Workdir: ", "実行するフォルダー: "],
    ["Requested sandbox elevation: ", "保護を外す理由（AIの説明）: "],
    ["Canonical executable: ", "実行ファイル: "],
    ["Canonical executable candidate (identity pinned): ", "実行ファイル（確認済み）: "],
  ];
  for (const [prefix, label] of prefixes) {
    if (detail.startsWith(prefix)) return label + detail.slice(prefix.length);
  }
  if (detail.startsWith("Workspace modes run this process in the native workspace-write OS sandbox;")) {
    return "通常は作業フォルダーの保護付きで実行します。保護を外す操作を許可した場合、またはフルアクセスの場合は、このPCにログインしているユーザーの権限で実行します。Windowsでは通信先をファイアウォールで制限しません。";
  }
  if (detail === "execution boundary: approval grants this process effect elevation outside the workspace-write OS sandbox") {
    return "この操作を許可すると、作業フォルダーの保護を外し、このPCにログインしているユーザーの権限で実行します。";
  }
  return detail;
}
