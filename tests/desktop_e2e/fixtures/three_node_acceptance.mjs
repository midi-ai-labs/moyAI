import { createHash } from "node:crypto";
import path from "node:path";

export const ANALYSIS_INPUT = "Alpha beta beta.\nGamma alpha!\nDelta epsilon zeta.";
export const ANALYSIS_EXPECTED = Object.freeze({
  word_count: 8, line_count: 3,
  top5: [{ word: "alpha", count: 2 }, { word: "beta", count: 2 }, { word: "delta", count: 1 }, { word: "epsilon", count: 1 }, { word: "gamma", count: 1 }],
  sha256: "8722c2be8fc47db6d4dd8dd386181d113db9668d393ec104b3abe5353e34b02f",
});
export const MODEL_PROMPT = "回答と利用者に見せる説明は日本語にしてください。";
export const INDEPENDENT_PROMPT = "このプロジェクトの定義を確認し、目的・各環境の役割・接続関係を短く報告してください。ファイルやサービスは変更しないでください。";

// This is an observable acceptance contract, not a prewritten implementation or
// an agent/tool plan. Placement comes only from the Hub project definition.
export const APPLICATION_PROMPT = `プロジェクトの定義を確認して、複数の実行環境にまたがる非同期テキスト解析Webアプリを作り、実際に起動して動作確認してください。環境ごとの役割と接続はプロジェクト定義に従って判断してください。
画面から文章を投入するとすぐにjob IDを返し、Workerが後で解析します。DBへjobと状態PENDING/RUNNING/COMPLETED/FAILED、結果、エラーを永続化し、画面で状態と結果を確認できるようにしてください。処理結果は単語数、行数、頻度上位5語、入力のSHA-256です。単語は[A-Za-z]+を小文字化し、同頻度はASCII昇順。行数は空文字0、それ以外はLF数+1（末尾LFなら+1しない）。SHA-256は受信した文字列を変更せずUTF-8化したbytesに対して計算します。
UI/API・Worker・DBを定義された別々の環境で実行し、実際の通信を通してください。1つの環境にDocker/VMや全componentを集約する方法は不可です。各環境のファイルはその環境に置き、他環境の保存先を直接読み書きしないでください。既存の許可範囲とインストール済みランタイムを使い、新しい外部依存のdownloadは不要な実装にしてください。
Worker停止中に新しいjobを受け付けてもPENDINGで残り、再開すると同じIDが完了すること、DB再起動後も履歴と結果が残ることを確認してください。隔離試験で指定した1jobだけRUNNINGに移った直後に例外を起こし、通常の例外処理でFAILEDと理由がDBへ残ることも確認できるようにしてください。正常入力へ隠し文字列を仕込まず、DB停止や直接UPDATEで失敗状態を偽装しないでください。
利用者の追加作業なしで検証できる成果物として、起動状態のacceptance.jsonをUI/APIの作業フォルダーへ保存し、会話の成果として公開してください。観測用の最小HTTP契約は次のとおりです。
- POST /jobs のJSON {text}で{id,status}を返す。GET /jobs/{id}で{id,status,result,error}。resultは{word_count,line_count,top5:[{word,count}],sha256}。word_count、line_count、countはJSON数値。idは空でない文字列または安全な整数とし、同じjobでは同じ値を維持する。sha256は64桁の16進文字列で、英字の大小は自由。
- Web画面の入力欄・投入ボタン・選択job ID・状態/結果にdata-testid=text-input/submit-job/job-id/job-resultを付ける。job-id要素のtextContentはIDだけとし、job-resultにはCOMPLETEDなどのstatusコードも含める。日本語の説明を併記してよい。
- 隔離試験専用の制御URLへPOST {action}。actionはworker_stop、worker_start、database_restart。{action:"worker_fail_job",job_id}はその1jobだけに前述の例外を設定する。GET同URLは{api:{pid},worker:{pid,running},database:{pid,running}}を返す。停止はWorkerの実process終了、再開とDB再起動は新しいprocess PIDで確認できること。制御手段の実装方式は自由で、DBとUI/APIはworker_stop中も利用可能にする。
- acceptance.jsonは{version:1,components:{api:{environment_id,base_url,entrypoint},worker:{environment_id,base_url,entrypoint},database:{environment_id,base_url,entrypoint,database_file}},ui_url,control_url}。entrypoint/database_fileは各担当環境内の実ファイルの絶対path。URLはHTTPの到達可能なloopbackアドレス。PIDはcontrol_urlのGETで実行中の値を返す。
起動したアプリは結果確認まで保持し、最後の通常の会話全停止でその会話が起動したprocessが終了するようにしてください。作成したソース、接続情報、検証結果を報告してください。`;

export function resultAccepted(job) {
  return job?.status === "COMPLETED" && job.result?.word_count === ANALYSIS_EXPECTED.word_count
    && job.result?.line_count === ANALYSIS_EXPECTED.line_count && typeof job.result?.sha256 === "string"
    && /^[0-9a-f]{64}$/i.test(job.result.sha256) && job.result.sha256.toLowerCase() === ANALYSIS_EXPECTED.sha256
    && Array.isArray(job.result.top5) && job.result.top5.length === ANALYSIS_EXPECTED.top5.length
    && job.result.top5.every((entry, index) => entry?.word === ANALYSIS_EXPECTED.top5[index].word && entry.count === ANALYSIS_EXPECTED.top5[index].count);
}
export function analysisJobId(value) {
  if (typeof value === "string" && value.length > 0) return value;
  return Number.isSafeInteger(value) ? String(value) : null;
}
export function sameAnalysisJobId(actual, expected) {
  const id = analysisJobId(actual);
  return id !== null && id === analysisJobId(expected);
}
export function loopbackUrl(value) {
  const url = new URL(value);
  if (url.protocol !== "http:" || !["127.0.0.1", "localhost", "[::1]"].includes(url.hostname) || url.username || url.password || url.hash) throw new TypeError("Acceptance URLs must be unauthenticated loopback HTTP URLs");
  return url.href;
}
function absolutePath(value) {
  if (typeof value !== "string") return null;
  const flavor = /^(?:[a-z]:[\\/]|\\\\)/i.test(value) ? path.win32 : path;
  if (!flavor.isAbsolute(value)) return null;
  // This only equates path spelling. Callers still enforce realpath and symlink boundaries.
  const absolute = flavor.toNamespacedPath(flavor.resolve(value));
  return { flavor, key: flavor === path.win32 ? absolute.toLowerCase() : absolute };
}
export function sameFilePath(left, right) {
  const a = absolutePath(left), b = absolutePath(right);
  return Boolean(a && b && a.flavor === b.flavor && a.key === b.key);
}
export function inside(root, file) {
  const parent = absolutePath(root), child = absolutePath(file);
  if (!parent || !child || parent.flavor !== child.flavor) return false;
  const relative = parent.flavor.relative(parent.key, child.key);
  return relative !== "" && !parent.flavor.isAbsolute(relative) && relative !== ".." && !relative.startsWith(`..${parent.flavor.sep}`);
}
export function validateManifest(manifest, environments) {
  if (manifest?.version !== 1) throw new TypeError("Missing acceptance manifest version 1");
  for (const role of ["api", "worker", "database"]) {
    const component = manifest.components?.[role], expected = environments[role];
    if (!component || component.environment_id !== expected.environment_id || !inside(expected.directory, component.entrypoint)) throw new TypeError(`Component ${role} is not deployed to its defined environment`);
    loopbackUrl(component.base_url);
  }
  if (new Set(Object.values(manifest.components).map(row => row.environment_id)).size !== 3) throw new TypeError("Components collapsed onto one environment");
  if (!inside(environments.database.directory, manifest.components.database.database_file)) throw new TypeError("Database file escaped its environment");
  loopbackUrl(manifest.ui_url); loopbackUrl(manifest.control_url);
  return manifest;
}
export function inputHash() { return createHash("sha256").update(ANALYSIS_INPUT, "utf8").digest("hex"); }
