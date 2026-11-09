import { useCallback, useEffect, useRef, useState } from "react";
import { getVersion } from "@tauri-apps/api/app";
import { listen } from "@tauri-apps/api/event";
import { relaunch } from "@tauri-apps/plugin-process";
import { check, Update } from "@tauri-apps/plugin-updater";
import { invoke } from "@tauri-apps/api/core";
import { checkForUpdateDetailed } from "./updateCheck";

// 데스크톱 앱 업데이트. 켤 때 확인만 하고, 설치와 재시작은 사람이 버튼을 눌러야 한다.
// 처리, 수신, AI 정리, 모델 받기 같은 작업이 하나라도 진행 중이면 설치하지 않고 무엇이 진행 중인지 알린다.
// @see docs/update.md

export type AppUpdateState =
  | { kind: "none" }
  /** 손으로 확인했다(메뉴, 설정) */
  | { kind: "checking" }
  | { kind: "latest"; current: string }
  /** 앱 안에서 받아 설치할 수 있다(Tauri updater) */
  | { kind: "ready"; version: string; current: string }
  | { kind: "downloading"; version: string; current: string; percent: number | null }
  | { kind: "installing"; version: string; current: string }
  /** updater로 확인하지 못했다(0.9.2 이하 Release 등). Release 페이지로 안내한다 */
  | { kind: "link"; version: string; current: string; url: string };

export type UpdateCheckResult = { kind: "new"; version: string } | { kind: "latest"; current: string } | { kind: "failed" };

export function useAppUpdate() {
  const [state, setState] = useState<AppUpdateState>({ kind: "none" });
  /** 진행 중인 작업이 있어 설치하지 않았을 때 보여 줄 문구 */
  const [blocked, setBlocked] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const update = useRef<Update | null>(null);
  /** 받아 두었으면 다음에 누를 때 다시 받지 않는다 */
  const downloaded = useRef(false);

  /** manual: 메뉴나 설정에서 눌렀다. 새 버전이 없어도 "최신 버전" 결과를 남긴다.
   *  돌려주는 값은 보관함 ⌘R이 알림 문구에 쓴다(새 버전이면 띠도 뜬다) */
  const runCheck = useCallback(async (manual: boolean): Promise<UpdateCheckResult> => {
    if (manual) {
      setState({ kind: "checking" });
      setBlocked(null);
      setError(null);
    }
    try {
      const u = await check();
      if (u) {
        update.current = u;
        downloaded.current = false;
        setState({ kind: "ready", version: u.version, current: u.currentVersion });
        return { kind: "new", version: u.version };
      }
      const current = await getVersion();
      if (manual) setState({ kind: "latest", current });
      return { kind: "latest", current };
    } catch (e) {
      // latest.json을 못 읽었다(새 버전을 올리는 중이거나 네트워크). GitHub API로 다시 묻는다
      console.warn(`업데이트 확인 실패, GitHub API로 다시 확인합니다: ${e}`);
      const info = await checkForUpdateDetailed(/\.dmg$/);
      if (typeof info === "object") {
        setState({ kind: "link", version: info.latest, current: info.current, url: info.url });
        return { kind: "new", version: info.latest };
      }
      if (info === "none") {
        const current = await getVersion();
        if (manual) setState({ kind: "latest", current });
        return { kind: "latest", current };
      }
      if (manual) {
        setState({ kind: "none" });
        setError("업데이트를 확인하지 못했습니다. 인터넷 연결을 확인하거나 잠시 뒤 다시 확인해 주세요");
      }
      return { kind: "failed" };
    }
  }, []);

  useEffect(() => {
    runCheck(false);
    // macOS 앱 메뉴의 "업데이트 확인하기…"(lib.rs add_update_menu)
    const unlisten = listen("check-update", () => runCheck(true));
    return () => {
      unlisten.then((f) => f());
    };
  }, [runCheck]);

  const checkNow = useCallback(() => runCheck(true), [runCheck]);
  /** 띠를 "확인 중", "최신 버전"으로 바꾸지 않고 확인만 한다. 새 버전이면 평소처럼 띠가 뜬다 */
  const checkQuietly = useCallback(() => runCheck(false), [runCheck]);

  /** 진행 중인 작업이 있으면 문구를 띄우고 true */
  const stopIfBusy = async () => {
    const tasks = await invoke<string[]>("busy_tasks");
    if (tasks.length === 0) return false;
    setBlocked(`진행 중인 작업 [${tasks.join(", ")}]을 완료하거나 종료한 뒤 다시 눌러 주세요`);
    return true;
  };

  const install = useCallback(async () => {
    const u = update.current;
    if (!u || state.kind !== "ready") return;
    setBlocked(null);
    setError(null);
    try {
      if (await stopIfBusy()) return;
      const base = { version: u.version, current: u.currentVersion };
      if (!downloaded.current) {
        let total: number | null = null;
        let received = 0;
        setState({ kind: "downloading", ...base, percent: null });
        await u.download((event) => {
          if (event.event === "Started") total = event.data.contentLength ?? null;
          if (event.event === "Progress") {
            received += event.data.chunkLength;
            if (total) setState({ kind: "downloading", ...base, percent: Math.round((received / total) * 100) });
          }
        });
        downloaded.current = true;
      }
      // 받는 동안 iPhone이 문서를 보내기 시작했을 수 있다. 설치 직전에 다시 확인한다
      if (await stopIfBusy()) {
        setState({ kind: "ready", ...base });
        return;
      }
      setState({ kind: "installing", ...base });
      await u.install();
    } catch (e) {
      // plugin-updater 2.14.1은 설치에 실패하면 받은 바이트를 남겨 두지만(Rust install이 성공한 뒤에만 닫는다),
      // 버전이 바뀌어도 재시도가 빈 상태로 install()을 부르지 않도록 다음에 누를 때 다시 받는다
      downloaded.current = false;
      setError(`업데이트하지 못했습니다: ${e}`);
      setState({ kind: "ready", version: u.version, current: u.currentVersion });
      return;
    }
    // 설치가 끝나면 받은 바이트는 닫힌다. 다시 켜기만 실패했으면 다시 설치하지 않고 손으로 켜 달라고 한다
    downloaded.current = false;
    try {
      await relaunch();
    } catch (e) {
      setError(`설치는 끝났습니다. scanshelf를 끄고 다시 켜 주세요(${e})`);
    }
  }, [state.kind]);

  const dismiss = useCallback(() => {
    setState({ kind: "none" });
    setBlocked(null);
    setError(null);
  }, []);

  return { state, blocked, error, install, dismiss, checkNow, checkQuietly };
}
