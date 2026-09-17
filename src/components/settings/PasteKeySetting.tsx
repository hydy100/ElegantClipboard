import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Button } from "@/components/ui/button";

export function PasteKeySetting() {
  const [value, setValue] = useState("ctrl_v");
  const [busy, setBusy] = useState(true);
  const [error, setError] = useState("");
  useEffect(() => {
    let disposed = false;
    invoke<string | null>("get_setting", { key: "paste_key" })
      .then(saved => { if (!disposed) setValue(saved === "shift_insert" ? saved : "ctrl_v"); })
      .catch(() => { if (!disposed) setError("读取粘贴按键失败，请重新打开设置。"); })
      .finally(() => { if (!disposed) setBusy(false); });
    return () => { disposed = true; };
  }, []);
  const save = async (next: string) => {
    if (busy || next === value) return;
    setBusy(true);
    setError("");
    try {
      await invoke("set_setting", { key: "paste_key", value: next });
      setValue(next);
    } catch {
      setError("保存粘贴按键失败，仍使用原设置。");
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="rounded-lg border p-4 space-y-3">
      <div className="text-sm font-medium">粘贴按键</div>
      <p className="text-xs text-muted-foreground">选择向目标应用发送的粘贴按键。默认 Ctrl+V；部分终端支持 Shift+Insert。</p>
      <div className="flex gap-2" role="group" aria-label="粘贴按键">
        {[["ctrl_v", "Ctrl+V"], ["shift_insert", "Shift+Insert"]].map(([key, label]) => (
          <Button key={key} variant={value === key ? "default" : "outline"} size="sm"
            disabled={busy} aria-pressed={value === key} onClick={() => { void save(key); }}>{label}</Button>
        ))}
      </div>
      {error && <p role="alert" className="text-xs text-destructive">{error}</p>}
    </div>
  );
}
