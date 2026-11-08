import { useLayoutEffect, useState } from "react";
import { availableOn, SHORTCUTS, shortcutLabel, type Screen } from "../shortcuts";

// ⌘를 길게 누르는 동안 화면의 단추, 탭 위에 그 단추의 단축키를 겹쳐 띄운다.
// 단추는 data-shortcut="<명령 이름>"만 붙이면 된다. 키 표기는 src/shortcuts.ts SHORTCUTS에서 읽어 실제 단축키와 어긋나지 않는다.
// 보이는 단추만 그린다: 크기가 0, 화면 밖, 다른 창(모달)에 가린 단추는 건너뛴다.
// @see docs/shortcuts-and-deep-links.md

interface Badge {
  key: string;
  label: string;
  left: number;
  top: number;
  disabled: boolean;
}

/** 단추의 가운데가 실제로 보이는지. 모달이나 다른 층에 가렸으면 그 자리의 맨 위 요소가 이 단추가 아니다 */
function visible(el: HTMLElement, rect: DOMRect): boolean {
  if (rect.width === 0 || rect.height === 0) return false;
  if (rect.bottom < 0 || rect.right < 0 || rect.top > window.innerHeight || rect.left > window.innerWidth) return false;
  const style = getComputedStyle(el);
  if (style.visibility === "hidden" || el.closest("[hidden]")) return false;
  const x = Math.min(Math.max(rect.left + rect.width / 2, 0), window.innerWidth - 1);
  const y = Math.min(Math.max(rect.top + rect.height / 2, 0), window.innerHeight - 1);
  const top = document.elementFromPoint(x, y);
  return !!top && (el === top || el.contains(top));
}

export function measureBadges(screen: Screen): Badge[] {
  const badges: Badge[] = [];
  document.querySelectorAll<HTMLElement>("[data-shortcut]").forEach((el, i) => {
    const command = el.dataset.shortcut;
    const shortcut = SHORTCUTS.find((s) => s.run === command && !s.passive && availableOn(s, screen));
    if (!shortcut) return;
    const rect = el.getBoundingClientRect();
    if (!visible(el, rect)) return;
    badges.push({
      key: `${command}-${i}`,
      label: shortcutLabel(shortcut),
      // 단추 왼쪽 위 모서리. 사이드바처럼 넓은 단추에서도 글자 바로 옆에 온다. 창 밖으로 나가지 않게 붙잡는다
      left: Math.min(Math.max(rect.left + 4, 4), window.innerWidth - 60),
      top: Math.max(rect.top, 12),
      disabled: el instanceof HTMLButtonElement && el.disabled,
    });
  });
  return badges;
}

export function ShortcutBadges({ screen }: { screen: Screen }) {
  const [badges, setBadges] = useState<Badge[]>([]);

  useLayoutEffect(() => {
    const update = () => setBadges(measureBadges(screen));
    update();
    // 누르고 있는 동안 스크롤하거나 창 크기를 바꾸면 다시 잰다
    window.addEventListener("scroll", update, true);
    window.addEventListener("resize", update);
    return () => {
      window.removeEventListener("scroll", update, true);
      window.removeEventListener("resize", update);
    };
  }, [screen]);

  return (
    <div className="shortcut-badges" aria-hidden>
      {badges.map((b) => (
        <kbd key={b.key} className={b.disabled ? "shortcut-badge off" : "shortcut-badge"} style={{ left: b.left, top: b.top }}>
          {b.label}
        </kbd>
      ))}
      <div className="shortcut-badges-hint">⌘를 누른 채 /를 누르면 전체 목록을 봅니다</div>
    </div>
  );
}
