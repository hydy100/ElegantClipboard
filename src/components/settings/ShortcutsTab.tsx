import { useState, useEffect, useCallback, useRef } from "react";
import {
  ChevronDown16Regular,
  ChevronUp16Regular,
  Delete16Regular,
} from "@fluentui/react-icons";
import { invoke, convertFileSrc } from "@tauri-apps/api/core";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import { logError } from "@/lib/logger";
import { cn } from "@/lib/utils";
import { useUISettings } from "@/stores/ui-settings";

export interface ShortcutSettings {
  shortcut: string;
  winv_replacement: boolean;
}

type ShortcutEditTarget =
  | { type: "main" }
  | { type: "quick-paste"; slot: number }
  | { type: "favorite-paste"; slot: number };

const QUICK_PASTE_SLOT_COUNT = 9;
const QUICK_PASTE_EMPTY_LABEL = "点击设置快捷键";

/** KeyboardEvent.code 到快捷键名称的映射 */
const KEY_CODE_MAP: Record<string, string> = {
  Space: "Space",
  Tab: "Tab",
  Enter: "Enter",
  Backspace: "Backspace",
  Delete: "Delete",
  Escape: "Esc",
  Home: "Home",
  End: "End",
  PageUp: "PageUp",
  PageDown: "PageDown",
  ArrowUp: "Up",
  ArrowDown: "Down",
  ArrowLeft: "Left",
  ArrowRight: "Right",
  Backquote: "`",
};

interface ShortcutsTabProps {
  settings: ShortcutSettings;
  onSettingsChange: (settings: ShortcutSettings) => void;
}

export function ShortcutsTab({
  settings,
  onSettingsChange,
}: ShortcutsTabProps) {
  const keyboardNavigation = useUISettings((s) => s.keyboardNavigation);
  const setKeyboardNavigation = useUISettings((s) => s.setKeyboardNavigation);
  const [hotkeyMode, setHotkeyMode] = useState("register");
  const [hotkeySwitching, setHotkeySwitching] = useState(false);
  const [gameModeEnabled, setGameModeEnabled] = useState(false);
  const [winvLoading, setWinvLoading] = useState(false);
  const [winvError, setWinvError] = useState("");
  const [winvConfirmDialogOpen, setWinvConfirmDialogOpen] = useState(false);
  const [winvPendingAction, setWinvPendingAction] = useState<
    "enable" | "disable" | null
  >(null);

  // 快捷键编辑状态
  const [shortcutDialogOpen, setShortcutDialogOpen] = useState(false);
  const [recordingShortcut, setRecordingShortcut] = useState(false);
  const [tempShortcut, setTempShortcut] = useState("");
  const [shortcutError, setShortcutError] = useState("");
  const [editTarget, setEditTarget] = useState<ShortcutEditTarget | null>(null);
  const [quickPasteShortcuts, setQuickPasteShortcuts] = useState<string[]>([]);
  const [quickPasteLoaded, setQuickPasteLoaded] = useState(false);
  const [loadingSlot, setLoadingSlot] = useState<number | null>(null);
  const [slotErrors, setSlotErrors] = useState<Record<number, string>>({});
  const [quickPasteExpanded, setQuickPasteExpanded] = useState(false);
  const [favPasteShortcuts, setFavPasteShortcuts] = useState<string[]>([]);
  const [favPasteLoaded, setFavPasteLoaded] = useState(false);
  const [favPasteExpanded, setFavPasteExpanded] = useState(false);
  const [favSlotErrors, setFavSlotErrors] = useState<Record<number, string>>({});

  // 游戏模式排除列表
  type RunningApp = { name: string; process: string; icon: string | null };
  type AppMeta = { name: string; icon: string | null };
  const [exclusionList, setExclusionList] = useState<string[]>([]);
  const [exclusionInput, setExclusionInput] = useState("");
  const [exclusionRunningApps, setExclusionRunningApps] = useState<RunningApp[]>([]);
  const [showExclusionAppPicker, setShowExclusionAppPicker] = useState(false);
  const exclusionMetaCache = useRef<Map<string, AppMeta>>(new Map());

  // 持久化排除列表元数据到 setting，使重启后图标仍可显示
  const persistExclusionMeta = useCallback(() => {
    const obj: Record<string, AppMeta> = {};
    exclusionMetaCache.current.forEach((v, k) => { obj[k] = v; });
    invoke("set_setting", { key: "game_mode_exclusion_meta", value: JSON.stringify(obj) }).catch((error) => {
      logError("Failed to save game_mode_exclusion_meta:", error);
    });
  }, []);

  // 处理快捷键录入的键盘事件
  const handleKeyDown = useCallback((e: KeyboardEvent) => {
    e.preventDefault();
    e.stopPropagation();

    const parts: string[] = [];

    // 修饰键
    if (e.ctrlKey) parts.push("Ctrl");
    if (e.altKey) parts.push("Alt");
    if (e.shiftKey) parts.push("Shift");
    if (e.metaKey) parts.push("Win");

    // 按键
    let key = "";
    if (e.code.startsWith("Key")) {
      key = e.code.replace("Key", "");
    } else if (e.code.startsWith("Digit")) {
      key = e.code.replace("Digit", "");
    } else if (e.code.startsWith("F") && !isNaN(Number(e.code.slice(1)))) {
      key = e.code; // F1-F12
    } else {
      key = KEY_CODE_MAP[e.code] || "";
    }

    if (key && parts.length > 0) {
      // Shift 不能单独作为全局快捷键修饰键
      const hasNonShiftModifier = e.ctrlKey || e.altKey || e.metaKey;
      if (!hasNonShiftModifier) {
        setShortcutError("Shift 不能单独作为修饰键，请配合 Ctrl/Alt 使用");
        return;
      }
      // 快速粘贴/收藏粘贴禁止使用 Win 键（Win+数字 是系统任务栏快捷键）
      if (e.metaKey && (editTarget?.type === "quick-paste" || editTarget?.type === "favorite-paste")) {
        setShortcutError("快速粘贴不支持 Win 修饰键（Win+数字 是系统任务栏快捷键）");
        return;
      }
      parts.push(key);
      setTempShortcut(parts.join("+"));
      setShortcutError("");
    } else if (!key && parts.length > 0) {
      // 仅按了修饰键，显示提示
      setTempShortcut(parts.join("+") + "+...");
    } else if (key && parts.length === 0) {
      setShortcutError("请至少使用一个修饰键 (Ctrl/Alt)");
    }
  }, []);

  // 开始/停止录入
  useEffect(() => {
    if (recordingShortcut) {
      window.addEventListener("keydown", handleKeyDown);
      return () => window.removeEventListener("keydown", handleKeyDown);
    }
  }, [recordingShortcut, handleKeyDown]);

  useEffect(() => {
    invoke<string>("get_hotkey_mode")
      .then((v) => setHotkeyMode(v))
      .catch((error) => logError("Failed to load hotkey_mode:", error));
    invoke<boolean>("is_game_mode_enabled")
      .then((v) => setGameModeEnabled(v))
      .catch((error) => logError("Failed to load game_mode_enabled:", error));
  }, []);

  const switchHotkeyMode = async (mode: string) => {
    if (mode === hotkeyMode || hotkeySwitching) return;
    setHotkeySwitching(true);
    const prev = hotkeyMode;
    setHotkeyMode(mode);
    try {
      await invoke("set_hotkey_mode", { mode });
    } catch (error) {
      logError("Failed to set hotkey mode:", error);
      setHotkeyMode(prev);
    } finally {
      setHotkeySwitching(false);
    }
  };

  const toggleGameMode = async (enabled: boolean) => {
    setGameModeEnabled(enabled);
    try {
      await invoke("set_game_mode_enabled", { enabled });
    } catch (error) {
      logError("Failed to set game mode:", error);
      setGameModeEnabled(!enabled);
    }
  };

  // 加载排除列表
  useEffect(() => {
    invoke<string[]>("get_game_mode_exclusion_list")
      .then((list) => {
        if (Array.isArray(list)) setExclusionList(list);
      })
      .catch((error) => logError("Failed to load exclusion list:", error));
    // 从持久化设置恢复元数据缓存（保证重启后图标可用）
    invoke<string | null>("get_setting", { key: "game_mode_exclusion_meta" })
      .then((v) => {
        if (v) {
          try {
            const obj = JSON.parse(v) as Record<string, AppMeta>;
            for (const [k, meta] of Object.entries(obj)) {
              exclusionMetaCache.current.set(k, meta);
            }
          } catch { /* ignore */ }
        }
      })
      .catch((error) => logError("Failed to load game_mode_exclusion_meta:", error))
      .finally(() => {
        // 预加载运行中应用来更新图标缓存（覆盖旧数据）
        invoke<RunningApp[]>("get_running_apps")
          .then((apps) => {
            for (const app of apps) {
              exclusionMetaCache.current.set(app.process.toLowerCase(), { name: app.name, icon: app.icon });
            }
          })
          .catch((error) => logError("Failed to preload running apps:", error));
      });
  }, []);

  const addExclusion = useCallback((process: string, meta?: AppMeta) => {
    const trimmed = process.trim();
    if (!trimmed) return;
    if (meta) {
      exclusionMetaCache.current.set(trimmed.toLowerCase(), meta);
      persistExclusionMeta();
    }
    setExclusionList((prev) => {
      if (prev.some((a) => a.toLowerCase() === trimmed.toLowerCase())) return prev;
      const next = [...prev, trimmed];
      invoke("set_game_mode_exclusion_list", { list: next }).catch((error) => {
        logError("Failed to save exclusion list:", error);
        setExclusionList(prev);
      });
      return next;
    });
  }, [persistExclusionMeta]);

  const removeExclusion = useCallback((process: string) => {
    exclusionMetaCache.current.delete(process.toLowerCase());
    persistExclusionMeta();
    setExclusionList((prev) => {
      const next = prev.filter((a) => a !== process);
      invoke("set_game_mode_exclusion_list", { list: next }).catch((error) => {
        logError("Failed to save exclusion list:", error);
        setExclusionList(prev);
      });
      return next;
    });
  }, [persistExclusionMeta]);

  const loadExclusionRunningApps = useCallback(async () => {
    try {
      const apps = await invoke<RunningApp[]>("get_running_apps");
      setExclusionRunningApps(apps);
      for (const app of apps) {
        exclusionMetaCache.current.set(app.process.toLowerCase(), { name: app.name, icon: app.icon });
      }
      setShowExclusionAppPicker(true);
    } catch (error) {
      logError("Failed to load running apps:", error);
    }
  }, []);

  const getExclusionMeta = (process: string): AppMeta | undefined =>
    exclusionMetaCache.current.get(process.toLowerCase());

  useEffect(() => {
    let disposed = false;
    const loadQuickPasteShortcuts = async () => {
      try {
        const shortcuts = await invoke<string[]>("get_quick_paste_shortcuts");
        if (disposed) return;
        if (Array.isArray(shortcuts) && shortcuts.length === QUICK_PASTE_SLOT_COUNT) {
          setQuickPasteShortcuts(shortcuts);
        }
      } catch (error) {
        logError("Failed to load quick paste shortcuts:", error);
      } finally {
        if (!disposed) setQuickPasteLoaded(true);
      }
      try {
        const favShortcuts = await invoke<string[]>("get_favorite_paste_shortcuts");
        if (disposed) return;
        if (Array.isArray(favShortcuts) && favShortcuts.length === QUICK_PASTE_SLOT_COUNT) {
          setFavPasteShortcuts(favShortcuts);
        }
      } catch (error) {
        logError("Failed to load favorite paste shortcuts:", error);
      } finally {
        if (!disposed) setFavPasteLoaded(true);
      }
    };
    loadQuickPasteShortcuts();
    return () => {
      disposed = true;
    };
  }, []);


  const startRecording = () => {
    setRecordingShortcut(true);
    setTempShortcut("");
    setShortcutError("");
  };

  const openEditDialog = (target: ShortcutEditTarget, initialValue: string) => {
    setEditTarget(target);
    setShortcutDialogOpen(true);
    setRecordingShortcut(false);
    setTempShortcut(initialValue);
    setShortcutError("");
  };

  const cancelRecording = () => {
    setRecordingShortcut(false);
    setTempShortcut("");
    setShortcutError("");
    setShortcutDialogOpen(false);
    setEditTarget(null);
  };

  // 标准化快捷键字符串用于比较（顺序无关）
  const normalizeForCompare = (s: string) => s.toLowerCase().split("+").sort().join("+");

  // 当前生效的主快捷键（Win+V 替换开启时为 Win+V）
  const activeMainShortcut = settings.winv_replacement ? "Win+V" : settings.shortcut;

  // 检测快捷键冲突
  const detectConflict = (shortcut: string, target: ShortcutEditTarget): string | null => {
    const normalized = normalizeForCompare(shortcut);
    // 与主快捷键比较
    if (target.type === "quick-paste" && normalized === normalizeForCompare(activeMainShortcut)) {
      return `与呼出快捷键 ${activeMainShortcut} 冲突`;
    }
    // 与快速粘贴槽位比较（编辑时跳过自身）
    for (let i = 0; i < quickPasteShortcuts.length; i++) {
      const s = quickPasteShortcuts[i];
      if (!s) continue;
      if (target.type === "quick-paste" && target.slot === i) continue;
      if (normalized === normalizeForCompare(s)) {
        return `与快捷粘贴位置 ${i + 1} 冲突`;
      }
    }
    // 与收藏快速粘贴槽位比较（编辑时跳过自身）
    for (let i = 0; i < favPasteShortcuts.length; i++) {
      const s = favPasteShortcuts[i];
      if (!s) continue;
      if (target.type === "favorite-paste" && target.slot === i) continue;
      if (normalized === normalizeForCompare(s)) {
        return `与收藏快捷粘贴位置 ${i + 1} 冲突`;
      }
    }
    return null;
  };

  // 通用槽位操作工厂，消除 quick/favorite 重复逻辑
  const createSlotOps = (
    cmd: string,
    setShortcuts: React.Dispatch<React.SetStateAction<string[]>>,
    setErrors: React.Dispatch<React.SetStateAction<Record<number, string>>>,
    slotOffset: number,
  ) => {
    const apply = async (idx: number, shortcut: string) => {
      setLoadingSlot(idx + slotOffset);
      setErrors((prev) => { const { [idx]: _, ...rest } = prev; return rest; });
      await invoke(cmd, { slot: idx + 1, shortcut });
      setShortcuts((prev) => { const next = [...prev]; next[idx] = shortcut; return next; });
    };
    const disable = (idx: number) => {
      apply(idx, "").catch((error) => {
        setErrors((prev) => ({ ...prev, [idx]: String(error) }));
      }).finally(() => setLoadingSlot(null));
    };
    const batchReset = async (defaults: string[], currentShortcuts: string[]) => {
      const mainNorm = normalizeForCompare(activeMainShortcut);
      for (let i = 0; i < QUICK_PASTE_SLOT_COUNT; i++) {
        if (currentShortcuts[i] === defaults[i]) continue;
        if (defaults[i] && normalizeForCompare(defaults[i]) === mainNorm) {
          setErrors((prev) => ({ ...prev, [i]: `${defaults[i]} 与呼出快捷键冲突，已跳过` }));
          continue;
        }
        try { await apply(i, defaults[i]); } catch (error) {
          setErrors((prev) => ({ ...prev, [i]: String(error) }));
        }
      }
      setLoadingSlot(null);
    };
    const batchDisable = async (currentShortcuts: string[]) => {
      for (let i = 0; i < QUICK_PASTE_SLOT_COUNT; i++) {
        if (!currentShortcuts[i]) continue;
        try { await apply(i, ""); } catch (error) {
          setErrors((prev) => ({ ...prev, [i]: String(error) }));
        }
      }
      setLoadingSlot(null);
    };
    return { apply, disable, batchReset, batchDisable };
  };

  const quickOps = createSlotOps("set_quick_paste_shortcut", setQuickPasteShortcuts, setSlotErrors, 0);
  const favOps = createSlotOps("set_favorite_paste_shortcut", setFavPasteShortcuts, setFavSlotErrors, 100);

  const saveShortcut = async () => {
    if (!editTarget) {
      setShortcutError("未选择要编辑的快捷键");
      return;
    }

    if (!tempShortcut || tempShortcut.includes("...")) {
      setShortcutError("请输入完整的快捷键");
      return;
    }

    // 冲突检测
    const conflict = detectConflict(tempShortcut, editTarget);
    if (conflict) {
      setShortcutError(conflict);
      return;
    }

    try {
      if (editTarget.type === "main") {
        await invoke("update_shortcut", { newShortcut: tempShortcut });
        await invoke("set_setting", {
          key: "global_shortcut",
          value: tempShortcut,
        });
        onSettingsChange({ ...settings, shortcut: tempShortcut });
      } else if (editTarget.type === "quick-paste") {
        await quickOps.apply(editTarget.slot, tempShortcut);
      } else {
        await favOps.apply(editTarget.slot, tempShortcut);
      }
      setShortcutDialogOpen(false);
      setRecordingShortcut(false);
      setTempShortcut("");
      setEditTarget(null);
    } catch (error) {
      setShortcutError(`保存失败: ${error}`);
      if (editTarget.type === "quick-paste") {
        setSlotErrors((prev) => ({ ...prev, [editTarget.slot]: String(error) }));
      } else if (editTarget.type === "favorite-paste") {
        setFavSlotErrors((prev) => ({ ...prev, [editTarget.slot]: String(error) }));
      }
    } finally {
      setLoadingSlot(null);
    }
  };

  const QUICK_DEFAULTS = Array.from({ length: QUICK_PASTE_SLOT_COUNT }, (_, i) => `Alt+${i + 1}`);
  const FAV_DEFAULTS = ["Ctrl+Alt+1", "Ctrl+Alt+2", "Ctrl+Alt+3", "", "", "", "", "", ""];

  // 用户确认后执行 Win+V 切换
  const executeWinvToggle = async () => {
    if (!winvPendingAction) return;

    setWinvConfirmDialogOpen(false);
    setWinvLoading(true);
    setWinvError("");

    try {
      if (winvPendingAction === "enable") {
        await invoke("enable_winv_replacement");
        onSettingsChange({ ...settings, winv_replacement: true });
      } else {
        await invoke("disable_winv_replacement");
        onSettingsChange({ ...settings, winv_replacement: false });
      }
    } catch (error) {
      logError("Failed to toggle Win+V replacement:", error);
      setWinvError(String(error));
    } finally {
      setWinvLoading(false);
      setWinvPendingAction(null);
    }
  };

  return (
    <>
      <div className="space-y-4">
        {/* Hotkey Mode Card */}
        <div className="rounded-lg border bg-card p-4">
          <h3 className="text-sm font-medium mb-3">热键注册方式</h3>
          <p className="text-xs text-muted-foreground mb-4">
            选择全局快捷键的注册方式。切换后所有快捷键会自动重新注册。
            如果未开启游戏模式但热键仍无法在某些应用中生效，建议切换为「低级键盘钩子」。
          </p>
          <div className="space-y-3">
            <label
              className={cn(
                "flex items-start gap-3 rounded-md border p-3 cursor-pointer transition-colors",
                hotkeyMode === "register" ? "border-primary bg-primary/5" : "hover:bg-muted/50",
                hotkeySwitching && "opacity-50 pointer-events-none"
              )}
              onClick={() => switchHotkeyMode("register")}
            >
              <div className={cn(
                "mt-0.5 h-4 w-4 rounded-full border-2 flex items-center justify-center flex-shrink-0",
                hotkeyMode === "register" ? "border-primary" : "border-muted-foreground/40"
              )}>
                {hotkeyMode === "register" && <div className="h-2 w-2 rounded-full bg-primary" />}
              </div>
              <div className="space-y-0.5">
                <div className="text-xs font-medium">系统标准API（默认）</div>
                <p className="text-xs text-muted-foreground">
                  在窗口化全屏（无边框窗口）下仍可生效，但在真全屏（独占全屏）应用中不生效。
                </p>
              </div>
            </label>
            <label
              className={cn(
                "flex items-start gap-3 rounded-md border p-3 cursor-pointer transition-colors",
                hotkeyMode === "hook" ? "border-primary bg-primary/5" : "hover:bg-muted/50",
                hotkeySwitching && "opacity-50 pointer-events-none"
              )}
              onClick={() => switchHotkeyMode("hook")}
            >
              <div className={cn(
                "mt-0.5 h-4 w-4 rounded-full border-2 flex items-center justify-center flex-shrink-0",
                hotkeyMode === "hook" ? "border-primary" : "border-muted-foreground/40"
              )}>
                {hotkeyMode === "hook" && <div className="h-2 w-2 rounded-full bg-primary" />}
              </div>
              <div className="space-y-0.5">
                <div className="text-xs font-medium">低级键盘钩子</div>
                <p className="text-xs text-muted-foreground">
                  可穿透真全屏（独占全屏）应用，适合需要在全屏游戏中使用快捷键或截图的场景。
                </p>
              </div>
            </label>
          </div>
        </div>

        {/* Game Mode Card */}
        <div className="rounded-lg border bg-card p-4">
          <h3 className="text-sm font-medium mb-3">游戏模式</h3>
          <p className="text-xs text-muted-foreground mb-4">
            检测到全屏应用（包括窗口化全屏和独占全屏）时，自动暂停剪贴板监控和所有全局快捷键，退出全屏后自动恢复
          </p>
          <div className="flex items-center justify-between">
            <div className="space-y-0.5">
              <Label className="text-xs">启用游戏模式</Label>
              <p className="text-xs text-muted-foreground">
                切换窗口时自动检测，空闲时零开销
              </p>
            </div>
            <Switch checked={gameModeEnabled} onCheckedChange={toggleGameMode} />
          </div>

          {/* 排除列表 */}
          {gameModeEnabled && (
            <>
              <div className="border-t my-4" />
              <div className="space-y-3">
                <div className="flex items-center justify-between">
                  <div className="space-y-0.5">
                    <Label className="text-xs">排除列表</Label>
                    <p className="text-xs text-muted-foreground">
                      以下应用全屏时不进入游戏模式
                    </p>
                  </div>
                  <Button variant="outline" size="sm" onClick={loadExclusionRunningApps} className="h-7 text-xs">
                    选择应用
                  </Button>
                </div>

                <div className="flex gap-2">
                  <Input
                    value={exclusionInput}
                    onChange={(e) => setExclusionInput(e.target.value)}
                    onKeyDown={(e) => {
                      if (e.key === "Enter") {
                        addExclusion(exclusionInput);
                        setExclusionInput("");
                      }
                    }}
                    placeholder="手动输入进程名，如 chrome.exe"
                    className="flex-1 h-8 text-xs"
                  />
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => { addExclusion(exclusionInput); setExclusionInput(""); }}
                    disabled={!exclusionInput.trim()}
                    className="h-8 text-xs"
                  >
                    添加
                  </Button>
                </div>

                {exclusionList.length > 0 ? (
                  <div className="space-y-1">
                    {exclusionList.map((proc) => {
                      const meta = getExclusionMeta(proc);
                      return (
                        <div
                          key={proc}
                          className="group flex items-center gap-2.5 px-2.5 py-2 rounded-md bg-muted/40 hover:bg-muted/70 transition-colors"
                        >
                          {meta?.icon ? (
                            <img
                              src={convertFileSrc(meta.icon)}
                              alt=""
                              className="w-5 h-5 shrink-0 object-contain"
                              onError={(e) => { (e.target as HTMLImageElement).style.display = "none"; }}
                            />
                          ) : (
                            <div className="w-5 h-5 shrink-0 rounded bg-muted flex items-center justify-center text-[10px] text-muted-foreground">
                              ?
                            </div>
                          )}
                          <div className="flex-1 min-w-0">
                            {meta ? (
                              <>
                                <div className="text-xs font-medium truncate">{meta.name}</div>
                                <div className="text-[10px] text-muted-foreground truncate">{proc}</div>
                              </>
                            ) : (
                              <div className="text-xs font-medium truncate">{proc}</div>
                            )}
                          </div>
                          <button
                            type="button"
                            onClick={() => removeExclusion(proc)}
                            className="shrink-0 w-6 h-6 rounded-md flex items-center justify-center text-muted-foreground opacity-0 group-hover:opacity-100 hover:text-destructive transition-all"
                            aria-label={`移除 ${proc}`}
                          >
                            <Delete16Regular className="w-3.5 h-3.5" />
                          </button>
                        </div>
                      );
                    })}
                  </div>
                ) : (
                  <div className="text-center py-4">
                    <p className="text-xs text-muted-foreground/60">暂无排除项</p>
                    <p className="text-[10px] text-muted-foreground/40 mt-1">
                      点击"选择应用"从运行中的应用添加，或手动输入进程名
                    </p>
                  </div>
                )}
              </div>
            </>
          )}
        </div>

        {/* Exclusion App Picker Dialog */}
        <Dialog open={showExclusionAppPicker} onOpenChange={setShowExclusionAppPicker}>
          <DialogContent className="max-w-md max-h-[70vh] flex flex-col">
            <DialogHeader>
              <DialogTitle className="text-sm">选择要排除的应用</DialogTitle>
              <DialogDescription className="text-xs">
                点击应用即可添加到排除列表，该应用全屏时将不触发游戏模式
              </DialogDescription>
            </DialogHeader>
            <div className="flex-1 overflow-y-auto -mx-1 px-1 space-y-0.5">
              {exclusionRunningApps.map((app) => {
                const alreadyAdded = exclusionList.some(
                  (f) => f.toLowerCase() === app.process.toLowerCase()
                );
                return (
                  <button
                    key={app.process}
                    type="button"
                    disabled={alreadyAdded}
                    onClick={() => addExclusion(app.process, { name: app.name, icon: app.icon })}
                    className={`w-full flex items-center gap-2.5 px-2.5 py-2 rounded-md text-left transition-colors ${
                      alreadyAdded
                        ? "opacity-40 cursor-not-allowed"
                        : "hover:bg-accent"
                    }`}
                  >
                    {app.icon ? (
                      <img
                        src={convertFileSrc(app.icon)}
                        alt=""
                        className="w-5 h-5 shrink-0 object-contain"
                        onError={(e) => { (e.target as HTMLImageElement).style.display = "none"; }}
                      />
                    ) : (
                      <div className="w-5 h-5 shrink-0 rounded bg-muted flex items-center justify-center text-[10px] text-muted-foreground">
                        ?
                      </div>
                    )}
                    <div className="flex-1 min-w-0">
                      <div className="text-xs font-medium truncate">{app.name}</div>
                      <div className="text-[10px] text-muted-foreground truncate">{app.process}</div>
                    </div>
                    {alreadyAdded && (
                      <span className="text-[10px] text-muted-foreground shrink-0">已添加</span>
                    )}
                  </button>
                );
              })}
              {exclusionRunningApps.length === 0 && (
                <p className="text-xs text-muted-foreground text-center py-8">加载中...</p>
              )}
            </div>
            <DialogFooter>
              <Button variant="outline" size="sm" onClick={() => setShowExclusionAppPicker(false)} className="text-xs">
                关闭
              </Button>
            </DialogFooter>
          </DialogContent>
        </Dialog>

        {/* Keyboard Navigation Card */}
        <div className="rounded-lg border bg-card p-4">
          <h3 className="text-sm font-medium mb-3">快捷导航</h3>
          <p className="text-xs text-muted-foreground mb-4">使用键盘快速操作剪贴板列表</p>
          <div className="space-y-3">
            <div className="flex items-center justify-between">
              <div className="space-y-0.5">
                <Label className="text-xs">键盘导航</Label>
                <p className="text-xs text-muted-foreground">
                  方向键选择条目和切换分类、Enter 粘贴、Shift+Enter 纯文本粘贴、Delete 删除
                </p>
              </div>
              <Switch
                checked={keyboardNavigation}
                onCheckedChange={setKeyboardNavigation}
              />
            </div>
          </div>
        </div>

        {/* Shortcut Card */}
        <div className="rounded-lg border bg-card p-4">
          <h3 className="text-sm font-medium mb-3">呼出快捷键</h3>
          <p className="text-xs text-muted-foreground mb-4">
            自定义呼出剪贴板的快捷键
          </p>
          <div
            className={cn(
              "space-y-2",
              settings.winv_replacement && "opacity-50",
            )}
          >
            <Label className="text-xs">自定义快捷键</Label>
            <div className="flex gap-2">
              <Input
                value={settings.shortcut}
                readOnly
                className="flex-1 h-8 text-sm font-mono bg-muted"
              />
              <Button
                variant="outline"
                size="sm"
                className="h-8"
                onClick={() => openEditDialog({ type: "main" }, settings.shortcut)}
                disabled={settings.winv_replacement}
              >
                修改
              </Button>
            </div>
            <p className="text-xs text-muted-foreground">
              {settings.winv_replacement
                ? "已启用 Win+V，自定义快捷键已禁用"
                : "点击修改按钮自定义快捷键"}
            </p>
          </div>

          <div className="border-t my-4" />

          <div className="space-y-2">
            <div className="flex items-center justify-between">
              <div className="space-y-0.5">
                <Label className="text-xs">使用 Win+V</Label>
                <p className="text-xs text-muted-foreground">
                  替代系统剪贴板（将禁用自定义快捷键）
                </p>
              </div>
              <Switch
                checked={settings.winv_replacement}
                disabled={winvLoading}
                onCheckedChange={(checked) => {
                  setWinvPendingAction(checked ? "enable" : "disable");
                  setWinvConfirmDialogOpen(true);
                }}
              />
            </div>
            {winvLoading && (
              <p className="text-xs text-muted-foreground">
                正在修改系统设置，请稍候...
              </p>
            )}
            {winvError && (
              <p className="text-xs text-destructive">{winvError}</p>
            )}
            <p className="text-xs text-amber-500">
              注意：此操作会修改注册表并重启 Windows 资源管理器
            </p>
          </div>
        </div>

        {/* Quick Paste Card */}
        <div className="rounded-lg border bg-card p-4">
          <button
            type="button"
            className="flex items-center justify-between w-full text-left"
            onClick={() => setQuickPasteExpanded((v) => !v)}
          >
            <div>
              <h3 className="text-sm font-medium">快捷粘贴位置</h3>
              <p className="text-xs text-muted-foreground mt-1">
                为前 9 条剪贴板记录设置全局快捷键（按默认排序：置顶优先）
              </p>
            </div>
            {quickPasteExpanded
              ? <ChevronUp16Regular className="w-4 h-4 text-muted-foreground shrink-0" />
              : <ChevronDown16Regular className="w-4 h-4 text-muted-foreground shrink-0" />}
          </button>

          <div
            className={cn(
              "grid transition-[grid-template-rows] duration-200 ease-in-out",
              quickPasteExpanded ? "grid-rows-[1fr]" : "grid-rows-[0fr]",
            )}
          >
            <div className="overflow-hidden">
              <div className="pt-4 space-y-2">
                {/* Batch operations */}
                <div className="flex gap-2 mb-3">
                  <Button
                    variant="outline"
                    size="sm"
                    className="h-7 text-xs"
                    disabled={loadingSlot !== null}
                    onClick={() => quickOps.batchReset(QUICK_DEFAULTS, quickPasteShortcuts)}
                  >
                    全部恢复默认
                  </Button>
                  <Button
                    variant="outline"
                    size="sm"
                    className="h-7 text-xs text-muted-foreground"
                    disabled={loadingSlot !== null || quickPasteShortcuts.every((s) => !s)}
                    onClick={() => quickOps.batchDisable(quickPasteShortcuts)}
                  >
                    全部禁用
                  </Button>
                </div>

                {(quickPasteLoaded ? quickPasteShortcuts : Array.from({ length: QUICK_PASTE_SLOT_COUNT }, (_, i) => `Alt+${i + 1}`)).map((shortcut, idx) => (
                  <div key={idx}>
                    <div className="flex items-center gap-2">
                      <Label className="text-xs w-28 shrink-0">{`快速粘贴位置${idx + 1}`}</Label>
                      <Input
                        value={shortcut}
                        placeholder={QUICK_PASTE_EMPTY_LABEL}
                        readOnly
                        className={cn(
                          "h-8 text-sm flex-1 bg-muted",
                          shortcut && "font-mono",
                          slotErrors[idx] && "border-destructive",
                        )}
                      />
                      <Button
                        variant="outline"
                        size="sm"
                        className="h-8"
                        disabled={loadingSlot === idx}
                        onClick={() => openEditDialog({ type: "quick-paste", slot: idx }, shortcut)}
                      >
                        修改
                      </Button>
                      <Button
                        variant="ghost"
                        size="sm"
                        className="h-8 text-muted-foreground"
                        disabled={loadingSlot === idx || !shortcut}
                        onClick={() => quickOps.disable(idx)}
                      >
                        禁用
                      </Button>
                    </div>
                    {slotErrors[idx] && (
                      <p className="text-xs text-destructive mt-1 ml-28 pl-2">{slotErrors[idx]}</p>
                    )}
                  </div>
                ))}
              </div>
              <p className="text-xs text-muted-foreground mt-3">
                建议使用包含修饰键的组合（如 Alt+1、Ctrl+Shift+1）以减少冲突
              </p>
            </div>
          </div>
        </div>

        {/* Favorite Paste Card */}
        <div className="rounded-lg border bg-card p-4">
          <button
            type="button"
            className="flex items-center justify-between w-full text-left"
            onClick={() => setFavPasteExpanded((v) => !v)}
          >
            <div>
              <h3 className="text-sm font-medium">收藏快捷粘贴</h3>
              <p className="text-xs text-muted-foreground mt-1">
                为收藏列表中的前 9 条记录设置全局快捷键
              </p>
            </div>
            {favPasteExpanded
              ? <ChevronUp16Regular className="w-4 h-4 text-muted-foreground shrink-0" />
              : <ChevronDown16Regular className="w-4 h-4 text-muted-foreground shrink-0" />}
          </button>

          <div
            className={cn(
              "grid transition-[grid-template-rows] duration-200 ease-in-out",
              favPasteExpanded ? "grid-rows-[1fr]" : "grid-rows-[0fr]",
            )}
          >
            <div className="overflow-hidden">
              <div className="pt-4 space-y-2">
                {/* Batch operations */}
                <div className="flex gap-2 mb-3">
                  <Button
                    variant="outline"
                    size="sm"
                    className="h-7 text-xs"
                    disabled={loadingSlot !== null}
                    onClick={() => favOps.batchReset(FAV_DEFAULTS, favPasteShortcuts)}
                  >
                    全部恢复默认
                  </Button>
                  <Button
                    variant="outline"
                    size="sm"
                    className="h-7 text-xs text-muted-foreground"
                    disabled={loadingSlot !== null || favPasteShortcuts.every((s) => !s)}
                    onClick={() => favOps.batchDisable(favPasteShortcuts)}
                  >
                    全部禁用
                  </Button>
                </div>

                {(favPasteLoaded ? favPasteShortcuts : ["Ctrl+Alt+1", "Ctrl+Alt+2", "Ctrl+Alt+3", "", "", "", "", "", ""]).map((shortcut, idx) => (
                  <div key={idx}>
                    <div className="flex items-center gap-2">
                      <Label className="text-xs w-28 shrink-0">{`收藏粘贴位置${idx + 1}`}</Label>
                      <Input
                        value={shortcut}
                        placeholder={QUICK_PASTE_EMPTY_LABEL}
                        readOnly
                        className={cn(
                          "h-8 text-sm flex-1 bg-muted",
                          shortcut && "font-mono",
                          favSlotErrors[idx] && "border-destructive",
                        )}
                      />
                      <Button
                        variant="outline"
                        size="sm"
                        className="h-8"
                        disabled={loadingSlot === idx + 100}
                        onClick={() => openEditDialog({ type: "favorite-paste", slot: idx }, shortcut)}
                      >
                        修改
                      </Button>
                      <Button
                        variant="ghost"
                        size="sm"
                        className="h-8 text-muted-foreground"
                        disabled={loadingSlot === idx + 100 || !shortcut}
                        onClick={() => favOps.disable(idx)}
                      >
                        禁用
                      </Button>
                    </div>
                    {favSlotErrors[idx] && (
                      <p className="text-xs text-destructive mt-1 ml-28 pl-2">{favSlotErrors[idx]}</p>
                    )}
                  </div>
                ))}
              </div>
              <p className="text-xs text-muted-foreground mt-3">
                默认前 3 个槽位启用（Ctrl+Alt+1/2/3），粘贴收藏列表中对应位置的记录
              </p>
            </div>
          </div>
        </div>

        {/* Current Active Card */}
        <div className="rounded-lg border bg-card p-4">
          <h3 className="text-sm font-medium mb-3">当前生效</h3>
          <p className="text-xs text-muted-foreground mb-4">
            {settings.winv_replacement
              ? "使用 Win+V 呼出剪贴板"
              : `使用 ${settings.shortcut} 呼出剪贴板`}
          </p>
          <div className="space-y-2">
            <div className="flex items-center justify-between py-2 px-3 rounded-md bg-primary/10 border border-primary/20">
              <span className="text-sm font-medium">呼出/隐藏窗口</span>
              <kbd className="pointer-events-none inline-flex h-6 select-none items-center gap-1 rounded border bg-background px-2 font-mono text-xs font-medium">
                {settings.winv_replacement ? "Win+V" : settings.shortcut}
              </kbd>
            </div>
            {quickPasteLoaded && quickPasteShortcuts.some((s) => s) && (
              <div className="space-y-1">
                {quickPasteShortcuts.map((shortcut, idx) =>
                  shortcut ? (
                    <div key={idx} className="flex items-center justify-between py-1.5 px-3 rounded-md bg-muted/50">
                      <span className="text-xs text-muted-foreground">快捷粘贴 {idx + 1}</span>
                      <kbd className="pointer-events-none inline-flex h-5 select-none items-center gap-1 rounded border bg-background px-1.5 font-mono text-[10px] font-medium">
                        {shortcut}
                      </kbd>
                    </div>
                  ) : null,
                )}
              </div>
            )}
            {favPasteLoaded && favPasteShortcuts.some((s) => s) && (
              <div className="space-y-1 mt-1">
                {favPasteShortcuts.map((shortcut, idx) =>
                  shortcut ? (
                    <div key={`fav-${idx}`} className="flex items-center justify-between py-1.5 px-3 rounded-md bg-muted/50">
                      <span className="text-xs text-muted-foreground">收藏粘贴 {idx + 1}</span>
                      <kbd className="pointer-events-none inline-flex h-5 select-none items-center gap-1 rounded border bg-background px-1.5 font-mono text-[10px] font-medium">
                        {shortcut}
                      </kbd>
                    </div>
                  ) : null,
                )}
              </div>
            )}
          </div>
          <p className="text-xs text-muted-foreground mt-2">
            注：自定义快捷键和 Win+V 只能二选一，不能同时生效
          </p>
        </div>
      </div>

      {/* Shortcut Edit Dialog */}
      <Dialog
        open={shortcutDialogOpen}
        onOpenChange={(open) => {
          if (!open) cancelRecording();
          else setShortcutDialogOpen(open);
        }}
      >
        <DialogContent showCloseButton={false}>
          <DialogHeader>
            <DialogTitle>
              {editTarget?.type === "quick-paste"
                ? `修改快速粘贴位置 ${editTarget.slot + 1}`
                : editTarget?.type === "favorite-paste"
                  ? `修改收藏粘贴位置 ${editTarget.slot + 1}`
                  : "修改快捷键"}
            </DialogTitle>
            <DialogDescription>
              {editTarget?.type === "quick-paste" || editTarget?.type === "favorite-paste"
                ? "按下新的快捷键组合来设置快速粘贴"
                : "按下新的快捷键组合来设置呼出剪贴板的快捷键"}
            </DialogDescription>
          </DialogHeader>

          <div className="space-y-4 py-4">
            <div
              className={cn(
                "h-16 flex items-center justify-center rounded-md border-2 border-dashed transition-colors",
                recordingShortcut
                  ? "border-primary bg-primary/5"
                  : "border-muted",
              )}
              onClick={startRecording}
            >
              {recordingShortcut ? (
                <span className={cn("text-lg font-medium", tempShortcut && "font-mono")}>
                  {tempShortcut || "按下快捷键..."}
                </span>
              ) : (
                <span className="text-sm text-muted-foreground">
                  点击此处开始录入快捷键
                </span>
              )}
            </div>

            {shortcutError && (
              <p className="text-sm text-destructive">{shortcutError}</p>
            )}

            <p className="text-xs text-muted-foreground">
              快捷键必须包含至少一个修饰键 (Ctrl / Alt / Win) 加一个普通按键，Shift 可配合使用
            </p>
          </div>

          <DialogFooter className="flex justify-between sm:justify-between">
            <Button
              variant="ghost"
              onClick={() => {
                if (editTarget?.type === "quick-paste") {
                  setTempShortcut(`Alt+${editTarget.slot + 1}`);
                } else if (editTarget?.type === "favorite-paste") {
                  const favDefaults = ["Ctrl+Alt+1", "Ctrl+Alt+2", "Ctrl+Alt+3"];
                  setTempShortcut(favDefaults[editTarget.slot] || "");
                } else {
                  setTempShortcut("Alt+C");
                }
                setRecordingShortcut(false);
                setShortcutError("");
              }}
              className="text-muted-foreground"
            >
              恢复默认
            </Button>
            <div className="flex gap-2">
              <Button variant="outline" onClick={cancelRecording}>
                取消
              </Button>
              <Button
                onClick={saveShortcut}
                disabled={
                  !tempShortcut || tempShortcut.includes("...") || loadingSlot !== null
                }
              >
                保存
              </Button>
            </div>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* Win+V Confirmation Dialog */}
      <Dialog
        open={winvConfirmDialogOpen}
        onOpenChange={(open) => {
          if (!open) {
            setWinvConfirmDialogOpen(false);
            setWinvPendingAction(null);
          }
        }}
      >
        <DialogContent className="max-w-[400px]" showCloseButton={false}>
          <DialogHeader>
            <DialogTitle>
              {winvPendingAction === "enable" ? "启用 Win+V" : "禁用 Win+V"}
            </DialogTitle>
            <DialogDescription>
              {winvPendingAction === "enable"
                ? "启用 Win+V 需要修改注册表并重启 Windows 资源管理器，桌面会短暂刷新。"
                : "禁用 Win+V 需要恢复注册表并重启 Windows 资源管理器，桌面会短暂刷新。"}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button
              variant="outline"
              onClick={() => {
                setWinvConfirmDialogOpen(false);
                setWinvPendingAction(null);
              }}
            >
              取消
            </Button>
            <Button onClick={executeWinvToggle}>确定</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
}

