// Пустой список-заглушка лончера (шаг 5): 5 приглушённых skeleton-строк —
// заготовка фазы 2. Реальные результаты появятся в фазе 2.
import type { CSSProperties } from "react";

interface Props {
  onOpenSettings: () => void;
}

export default function ResultList({ onOpenSettings }: Props) {
  return (
    <main className="launcher">
      <header className="topbar" data-tauri-drag-region>
        <span className="brand">Iskra</span>
        <span className="hint">скелет фазы 1</span>
        <span className="spacer" />
        <button
          className="icon-btn"
          title="Настройки"
          aria-label="Настройки"
          onClick={onOpenSettings}
        >
          ⚙
        </button>
      </header>
      <ul className="results" aria-label="results">
        {Array.from({ length: 5 }, (_, i) => (
          <li key={i} className="result-skeleton" style={{ "--i": i } as CSSProperties}>
            <span className="skeleton-icon" />
            <span className="skeleton-line" />
          </li>
        ))}
      </ul>
      <footer className="statusbar">Alt+Space — показать/скрыть · клик мимо — скрыть</footer>
    </main>
  );
}
