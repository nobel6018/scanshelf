import { useEffect } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { useAppUpdate } from "./useAppUpdate";

type AppUpdate = ReturnType<typeof useAppUpdate>;

// 업데이트 알림(화면 위)과 설정의 업데이트 줄이 함께 쓰는 문구와 버튼. @see docs/update.md

/** 지금 상태를 한 문장으로 */
export function updateMessage(u: AppUpdate): string {
  const s = u.state;
  switch (s.kind) {
    case "checking":
      return "새 버전을 확인하고 있습니다";
    case "latest":
      return `최신 버전(${s.current})입니다`;
    case "ready":
    case "link":
      return `새 버전 ${s.version}이 나왔습니다. 지금은 ${s.current}입니다.`;
    case "downloading":
      return `새 버전 ${s.version}을 내려받는 중${s.percent === null ? "" : ` ${s.percent}%`}`;
    case "installing":
      return `새 버전 ${s.version}을 설치하고 다시 켭니다`;
    case "none":
      return "";
  }
}

/** 상태에 맞는 버튼(업데이트, 내려받기). 없으면 null */
export function UpdateAction({ update, onError }: { update: AppUpdate; onError: (e: string) => void }) {
  const s = update.state;
  if (s.kind === "link") {
    return (
      <button className="primary" onClick={() => openUrl(s.url).catch((e) => onError(String(e)))}>
        내려받기
      </button>
    );
  }
  if (s.kind === "ready" || s.kind === "downloading" || s.kind === "installing") {
    return (
      <button className="primary" disabled={s.kind !== "ready"} onClick={update.install}>
        업데이트하고 다시 켜기
      </button>
    );
  }
  return null;
}

/** 진행 중인 작업 때문에 미뤘거나 실패한 이유 */
export function UpdateNotes({ update }: { update: AppUpdate }) {
  return (
    <>
      {update.blocked && <span className="update-note">{update.blocked}</span>}
      {update.error && <span className="update-note error">{update.error}</span>}
    </>
  );
}

export function UpdateBanner({ update, onError }: { update: AppUpdate; onError: (e: string) => void }) {
  const busy = update.state.kind === "downloading" || update.state.kind === "installing";
  // "최신 버전입니다"는 확인 결과일 뿐이라 잠깐 보이고 닫는다
  const latest = update.state.kind === "latest" && !update.error;
  const { dismiss } = update;
  useEffect(() => {
    if (!latest) return;
    const timer = window.setTimeout(dismiss, 4000);
    return () => window.clearTimeout(timer);
  }, [latest, dismiss]);
  return (
    <div className="update-banner" role="status">
      <span>
        {updateMessage(update)}
        <UpdateNotes update={update} />
      </span>
      <span className="banner-actions">
        <UpdateAction update={update} onError={onError} />
        <button className="ghost" disabled={busy} onClick={update.dismiss}>
          닫기
        </button>
      </span>
    </div>
  );
}
