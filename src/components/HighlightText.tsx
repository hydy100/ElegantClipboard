import { memo, useMemo } from "react";
import { useClipboardStore } from "@/stores/clipboard";

interface HighlightTextProps {
  text: string;
}

// 模块级正则缓存：所有 HighlightText 实例共享同一编译结果
let _cachedQuery = "";
let _cachedRegex: RegExp | null = null;

function getHighlightRegex(query: string): RegExp | null {
  if (!query || query.trim().length === 0) return null;
  if (query === _cachedQuery && _cachedRegex) return _cachedRegex;
  const escaped = query.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  _cachedRegex = new RegExp(`(${escaped})`, "gi");
  _cachedQuery = query;
  return _cachedRegex;
}

/** 渲染文本并高亮搜索匹配项 */
export const HighlightText = memo(function HighlightText({ text }: HighlightTextProps) {
  const searchQuery = useClipboardStore((s) => s.searchQuery);

  const parts = useMemo(() => {
    const regex = getHighlightRegex(searchQuery);
    if (!regex) return null;
    return text.split(regex);
  }, [text, searchQuery]);

  if (!parts) return <>{text}</>;

  return (
    <>
      {parts.map((part, i) =>
        i % 2 === 1 ? (
          <mark key={i} className="search-highlight">
            {part}
          </mark>
        ) : (
          part
        ),
      )}
    </>
  );
});
