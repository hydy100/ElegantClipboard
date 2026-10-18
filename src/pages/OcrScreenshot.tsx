import { useState, useRef, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { convertFileSrc } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { logError } from "@/lib/logger";
import { cropScreenRegion, recognizeText } from "@/lib/ocr";
import { registerWindowRequestHandler } from "@/lib/window-request";
import { useOcrSettings } from "@/stores/ocr-settings";

export function OcrScreenshot() {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const imgRef = useRef<HTMLImageElement | null>(null);
  const [drawing, setDrawing] = useState(false);
  const [startPos, setStartPos] = useState({ x: 0, y: 0 });
  const [imagePath, setImagePath] = useState("");
  const cancelScreenshotRef = useRef<(() => void) | null>(null);

  useEffect(() => {
    let cancelLoad: (() => void) | undefined;
    let loadSequence = 0;
    const cancelScreenshot = () => {
      ++loadSequence;
      cancelLoad?.();
      imgRef.current = null;
    };
    cancelScreenshotRef.current = cancelScreenshot;
    const cleanup = registerWindowRequestHandler(async (path) => {
      cancelLoad?.();
      const sequence = ++loadSequence;
      setDrawing(false);
      setStartPos({ x: 0, y: 0 });
      setImagePath("");
      imgRef.current = null;
      const canvas = canvasRef.current;
      const ctx = canvas?.getContext("2d");
      if (!canvas || !ctx) throw new Error("Screenshot canvas unavailable");
      ctx.clearRect(0, 0, canvas.width, canvas.height);
      if (!path) throw new Error("Screenshot path is empty");

      await new Promise<void>((resolve, reject) => {
        const img = new Image();
        let settled = false;
        const detach = () => {
          settled = true;
          img.onload = null;
          img.onerror = null;
          if (cancelLoad === cancel) cancelLoad = undefined;
        };
        const cancel = () => {
          if (settled) return;
          detach();
          img.src = "";
          reject(new Error("Screenshot request cancelled"));
        };
        cancelLoad = cancel;
        img.onerror = () => {
          if (settled) return;
          detach();
          reject(new Error("Failed to load screenshot"));
        };
        img.onload = () => {
          // A queued browser task can retain the old listener after explicit close.
          if (settled) return;
          if (sequence !== loadSequence) {
            cancel();
            return;
          }
          detach();
          try {
            imgRef.current = img;
            canvas.width = img.width;
            canvas.height = img.height;
            ctx.drawImage(img, 0, 0);
            setImagePath(path);
            resolve();
          } catch (error) {
            reject(error);
          }
        };
        // Reload even when the backend overwrites the same file.
        try {
          img.src = convertFileSrc(path) + "?t=" + Date.now() + "-" + sequence;
        } catch (error) {
          detach();
          reject(error);
        }
      });
      if (sequence !== loadSequence) throw new Error("Screenshot request cancelled");
      // Resolves only once the drawn screenshot has been positioned and shown.
      await invoke("ocr_screenshot_ready");
      if (sequence !== loadSequence) {
        // Close may race the native show IPC; keep its eventual result hidden too.
        await invoke("hide_ocr_screenshot_window");
        throw new Error("Screenshot request cancelled");
      }
    });
    void useOcrSettings.getState().loadSettings()
      .catch((error) => logError("Failed to load OCR settings:", error));
    void invoke("managed_window_ready")
      .catch((error) => logError("Failed to initialize screenshot window:", error));
    return () => {
      cleanup();
      cancelScreenshot();
      if (cancelScreenshotRef.current === cancelScreenshot) cancelScreenshotRef.current = null;
    };
  }, []);

  // 重绘 canvas（背景 + 选区）
  const redraw = useCallback(
    (sx: number, sy: number, ex: number, ey: number) => {
      const canvas = canvasRef.current;
      const ctx = canvas?.getContext("2d");
      const img = imgRef.current;
      if (!canvas || !ctx || !img) return;

      // 画背景
      ctx.drawImage(img, 0, 0);
      // 半透明遮罩
      ctx.fillStyle = "rgba(0, 0, 0, 0.4)";
      ctx.fillRect(0, 0, canvas.width, canvas.height);

      // 选区
      const rx = Math.min(sx, ex);
      const ry = Math.min(sy, ey);
      const rw = Math.abs(ex - sx);
      const rh = Math.abs(ey - sy);
      if (rw > 0 && rh > 0) {
        // 清除选区遮罩
        ctx.clearRect(rx, ry, rw, rh);
        ctx.drawImage(img, rx, ry, rw, rh, rx, ry, rw, rh);
        // 选区边框
        ctx.strokeStyle = "#3b82f6";
        ctx.lineWidth = 2;
        ctx.strokeRect(rx, ry, rw, rh);
        // 尺寸标签
        ctx.fillStyle = "#3b82f6";
        ctx.font = "12px sans-serif";
        ctx.fillText(`${rw} × ${rh}`, rx + 4, ry > 18 ? ry - 6 : ry + rh + 14);
      }
    },
    [],
  );

  // 将 CSS 鼠标坐标转换为 canvas 像素坐标（适配 DPI 缩放）
  const toCanvasCoords = (clientX: number, clientY: number) => {
    const canvas = canvasRef.current;
    if (!canvas) return { x: clientX, y: clientY };
    const sx = canvas.width / canvas.clientWidth;
    const sy = canvas.height / canvas.clientHeight;
    return { x: Math.round(clientX * sx), y: Math.round(clientY * sy) };
  };

  const handleMouseDown = (e: React.MouseEvent) => {
    if (e.button !== 0) {
      // 右键 / 中键直接退出
      closeWindow();
      return;
    }
    setDrawing(true);
    setStartPos(toCanvasCoords(e.clientX, e.clientY));
  };

  const handleMouseMove = (e: React.MouseEvent) => {
    if (!drawing) return;
    const { x, y } = toCanvasCoords(e.clientX, e.clientY);
    redraw(startPos.x, startPos.y, x, y);
  };

  const handleMouseUp = async (e: React.MouseEvent) => {
    if (!drawing) return;
    setDrawing(false);

    const { x: ex, y: ey } = toCanvasCoords(e.clientX, e.clientY);
    const rx = Math.min(startPos.x, ex);
    const ry = Math.min(startPos.y, ey);
    const rw = Math.abs(ex - startPos.x);
    const rh = Math.abs(ey - startPos.y);

    if (rw < 10 || rh < 10) {
      // 太小，关闭
      closeWindow();
      return;
    }

    // 立即退出截图状态，打开结果窗口显示"识别中..."
    closeWindow();

    // 先打开结果窗口（空文本 = 加载态）
    invoke("open_ocr_result_window", { text: "" }).catch(() => {});

    // 后台异步裁剪 + 识别
    (async () => {
      try {
        const base64 = await cropScreenRegion(imagePath, rx, ry, rw, rh);
        const text = await recognizeText(base64);
        await invoke("open_ocr_result_window", { text });
      } catch (error) {
        logError("OCR 识别失败:", error);
        await invoke("open_ocr_result_window", { text: `识别失败: ${error}` });
      }
    })();
  };

  const closeWindow = () => {
    cancelScreenshotRef.current?.();
    invoke("hide_ocr_screenshot_window").catch(() => {
      getCurrentWindow().hide();
    });
  };

  // ESC 关闭
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        closeWindow();
      }
    };
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, []);

  return (
    <div
      style={{
        position: "fixed",
        inset: 0,
        cursor: "crosshair",
        userSelect: "none",
        overflow: "hidden",
        background: "transparent",
      }}
      onMouseDown={handleMouseDown}
      onMouseMove={handleMouseMove}
      onMouseUp={handleMouseUp}
      onContextMenu={(e) => e.preventDefault()}
    >
      <canvas
        ref={canvasRef}
        style={{
          position: "absolute",
          top: 0,
          left: 0,
          width: "100%",
          height: "100%",
        }}
      />
      {!drawing && (
        <div
          style={{
            position: "absolute",
            top: 12,
            left: "50%",
            transform: "translateX(-50%)",
            background: "rgba(0,0,0,0.7)",
            color: "#fff",
            padding: "6px 16px",
            borderRadius: 6,
            fontSize: 13,
            pointerEvents: "none",
          }}
        >
          拖拽选择识别区域 · ESC 取消
        </div>
      )}
    </div>
  );
}
