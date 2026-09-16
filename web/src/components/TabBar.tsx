// Tab bar for the active workspace: one chip per tab with its agent status
// color; click focuses (server-side) and selects (client-side). "+" opens a
// new tab; the active tab offers rename/close.

import type { TabInfo } from "../types.ts";

interface Props {
  tabs: TabInfo[];
  activeTab: string | null;
  dim: boolean;
  onSelect: (tabId: string) => void;
  onNewTab: () => void;
  onRenameTab: (tab: TabInfo) => void;
  onCloseTab: (tab: TabInfo) => void;
}

export function TabBar({ tabs, activeTab, dim, onSelect, onNewTab, onRenameTab, onCloseTab }: Props) {
  return (
    <div className={`tabbar ${dim ? "dim" : ""}`}>
      {tabs.map((t) => (
        <span key={t.tab_id} className={`tab-wrap ${t.tab_id === activeTab ? "active" : ""}`}>
          <button
            className={`tab ${t.tab_id === activeTab ? "active" : ""} st-${t.agent_status}`}
            onClick={() => onSelect(t.tab_id)}
            title={t.label}
          >
            <span className={`dot st-${t.agent_status}`} />
            {t.label}
          </button>
          {t.tab_id === activeTab && (
            <span className="tab-actions">
              <button title="rename tab" onClick={() => onRenameTab(t)}>✎</button>
              <button title="close tab" className="danger" onClick={() => onCloseTab(t)}>×</button>
            </span>
          )}
        </span>
      ))}
      <button className="tab new-tab" title="new tab" onClick={onNewTab}>＋</button>
    </div>
  );
}
