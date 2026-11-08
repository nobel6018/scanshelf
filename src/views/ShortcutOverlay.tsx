import { SCREEN_LABEL, shortcutLabel, shortcutsFor, type Screen } from "../shortcuts";

// 단축키 목록. ⌘를 길게 누르면 누르는 동안, ? 키나 도움말 메뉴로 열면 Esc나 바깥을 누를 때까지 보인다.
// 목록은 src/shortcuts.ts SHORTCUTS를 그대로 읽어 실제 단축키와 어긋나지 않는다.
// @see docs/shortcuts-and-deep-links.md

export function ShortcutOverlay({ screen, sticky, onClose }: { screen: Screen; sticky: boolean; onClose: () => void }) {
  const groups = shortcutsFor(screen);
  return (
    <div className="shortcut-overlay" role="dialog" aria-label="단축키" onMouseDown={onClose}>
      <div className="shortcut-card" onMouseDown={(e) => e.stopPropagation()}>
        <div className="shortcut-head">
          <h3>단축키</h3>
          <span className="hint">{SCREEN_LABEL[screen]} 화면</span>
        </div>
        <div className="shortcut-groups">
          {groups.map((g) => (
            <section key={g.group}>
              <h4>{g.group}</h4>
              <ul>
                {g.items.map((s) => (
                  <li key={`${s.run}-${s.key}`}>
                    <span>{s.label}</span>
                    <kbd>{shortcutLabel(s)}</kbd>
                  </li>
                ))}
              </ul>
            </section>
          ))}
        </div>
        <p className="hint shortcut-foot">{sticky ? "Esc나 바깥을 누르면 닫힙니다" : "⌘에서 손을 떼면 닫힙니다"}</p>
      </div>
    </div>
  );
}
