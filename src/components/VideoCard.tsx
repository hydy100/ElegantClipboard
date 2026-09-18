// 视频内容卡片渲染器（从 CardContentRenderers.tsx 提取）

import { memo, useEffect, useState } from "react";
import {
  Video16Regular,
  Play16Filled,
} from "@fluentui/react-icons";
import { convertFileSrc } from "@tauri-apps/api/core";
import { CardFooter } from "@/components/CardContentRenderers";
import { FileIconLayout } from "@/components/FileIconLayout";
import { useVideoPreview } from "@/hooks/useVideoPreview";
import { getFileNameFromPath } from "@/lib/format";

// ============ 视频缩略图 LRU 缓存 ============

const THUMB_CACHE_MAX = 32;
const THUMB_MAX_EDGE = 320;
const thumbCache = new Map<string, string>();
type ThumbnailTask = {
  cancelled: boolean;
  finished: boolean;
  done?: () => void;
  start: (done: () => void) => void;
};
const thumbnailQueue: ThumbnailTask[] = [];
let activeThumbnailTasks = 0;

function pumpThumbnailQueue(): void {
  if (activeThumbnailTasks > 0) return;
  const task = thumbnailQueue.shift();
  if (!task) return;
  if (task.cancelled) {
    pumpThumbnailQueue();
    return;
  }

  activeThumbnailTasks = 1;
  const done = () => {
    if (task.finished) return;
    task.finished = true;
    task.done = undefined;
    activeThumbnailTasks = 0;
    pumpThumbnailQueue();
  };
  task.done = done;
  try {
    task.start(done);
  } catch {
    done();
  }
}

function enqueueThumbnail(start: ThumbnailTask["start"]): () => void {
  const task: ThumbnailTask = {
    cancelled: false,
    finished: false,
    start,
  };
  thumbnailQueue.push(task);
  pumpThumbnailQueue();
  return () => {
    task.cancelled = true;
    task.done?.();
  };
}

function getThumbFromCache(path: string): string | null {
  const url = thumbCache.get(path);
  if (url) {
    thumbCache.delete(path);
    thumbCache.set(path, url);
    return url;
  }
  return null;
}

function setThumbToCache(path: string, url: string): void {
  if (thumbCache.size >= THUMB_CACHE_MAX) {
    const oldest = thumbCache.keys().next().value;
    if (oldest !== undefined) {
      const oldUrl = thumbCache.get(oldest);
      thumbCache.delete(oldest);
      if (oldUrl?.startsWith("blob:")) URL.revokeObjectURL(oldUrl);
    }
  }
  const previous = thumbCache.get(path);
  if (previous && previous !== url && previous.startsWith("blob:")) {
    URL.revokeObjectURL(previous);
  }
  thumbCache.set(path, url);
}

// ============ 视频内容 ============

interface VideoContentProps {
  filePaths: string[];
  filesInvalid: boolean;
  preview: string | null;
  metaItems: string[];
  index?: number;
  showBadge?: boolean;
  isDragOverlay?: boolean;
  sourceAppName?: string | null;
  sourceAppIcon?: string | null;
}

export const VideoContent = memo(function VideoContent({
  filePaths,
  filesInvalid,
  preview,
  metaItems,
  index,
  showBadge,
  isDragOverlay,
  sourceAppName,
  sourceAppIcon,
}: VideoContentProps) {
  const isMultiple = filePaths.length > 1;
  const firstPath = filePaths[0] || "";
  const fileName = getFileNameFromPath(firstPath || preview || "");
  const [thumbUrl, setThumbUrl] = useState<string | null>(() => getThumbFromCache(firstPath));
  const [thumbError, setThumbError] = useState(false);

  // 从视频文件生成缩略图（优先使用缓存）
  useEffect(() => {
    if (filesInvalid || !firstPath || isMultiple) {
      setThumbUrl(null);
      setThumbError(false);
      return;
    }

    const cached = getThumbFromCache(firstPath);
    if (cached) {
      setThumbUrl(cached);
      setThumbError(false);
      return;
    }

    let cancelled = false;
    let video: HTMLVideoElement | null = null;
    const cancelTask = enqueueThumbnail((done) => {
      if (cancelled) {
        done();
        return;
      }

      video = document.createElement("video");
      video.preload = "metadata";
      video.muted = true;
      video.playsInline = true;
      video.src = convertFileSrc(firstPath);

      video.addEventListener("loadeddata", () => {
        if (!cancelled && video) {
          video.currentTime = Math.min(1, video.duration * 0.1);
        }
      });

      video.addEventListener("seeked", () => {
        if (cancelled || !video) {
          done();
          return;
        }
        try {
          const sourceWidth = Math.max(1, video.videoWidth);
          const sourceHeight = Math.max(1, video.videoHeight);
          const scale = Math.min(1, THUMB_MAX_EDGE / Math.max(sourceWidth, sourceHeight));
          const canvas = document.createElement("canvas");
          canvas.width = Math.max(1, Math.round(sourceWidth * scale));
          canvas.height = Math.max(1, Math.round(sourceHeight * scale));
          const ctx = canvas.getContext("2d");
          if (!ctx) {
            setThumbError(true);
            done();
            return;
          }
          ctx.drawImage(video, 0, 0, canvas.width, canvas.height);
          canvas.toBlob((blob) => {
            if (!blob || cancelled) {
              if (!cancelled) setThumbError(true);
              done();
              return;
            }
            const objectUrl = URL.createObjectURL(blob);
            setThumbToCache(firstPath, objectUrl);
            setThumbUrl(objectUrl);
            canvas.width = 1;
            canvas.height = 1;
            done();
          }, "image/jpeg", 0.7);
        } catch {
          if (!cancelled) setThumbError(true);
          done();
        }
      });

      video.addEventListener("error", () => {
        if (!cancelled) setThumbError(true);
        done();
      });
    });

    return () => {
      cancelled = true;
      cancelTask();
      if (video) {
        video.src = "";
        video.load();
      }
    };
  }, [firstPath, filesInvalid, isMultiple]);

  // 视频悬浮预览 hook（单个有效视频文件时启用）
  const videoPreviewPath = (!isMultiple && !filesInvalid && firstPath) ? firstPath : undefined;
  const {
    containerRef: videoPreviewRef,
    handleMouseEnter: handleVideoMouseEnter,
    hidePreview: hideVideoPreview,
  } = useVideoPreview(videoPreviewPath);

  // 有缩略图且单个视频文件时显示预览
  if (!isMultiple && !filesInvalid && thumbUrl && !thumbError) {
    return (
      <div
        ref={videoPreviewRef}
        className="flex-1 min-w-0 px-3 py-2.5"
        onMouseEnter={handleVideoMouseEnter}
        onMouseLeave={hideVideoPreview}
      >
        <div className="relative w-full rounded-sm overflow-hidden bg-muted/30">
          <img
            src={thumbUrl}
            alt={fileName}
            className="w-full h-auto max-h-48 object-cover"
          />
          <div className="absolute inset-0 flex items-center justify-center">
            <div className="w-10 h-10 rounded-full bg-black/50 flex items-center justify-center">
              <Play16Filled className="w-5 h-5 text-white ml-0.5" />
            </div>
          </div>
          <div className="absolute bottom-0 left-0 right-0 bg-linear-to-t from-black/50 to-transparent px-2 py-1">
            <p className="text-[11px] text-white truncate">{fileName}</p>
          </div>
        </div>
        <CardFooter
          metaItems={metaItems}
          index={index}
          showBadge={showBadge}
          isDragOverlay={isDragOverlay}
          sourceAppName={sourceAppName}
          sourceAppIcon={sourceAppIcon}
        />
      </div>
    );
  }

  // 无缩略图、多文件、或失效时回退为图标+文件名样式
  return (
    <div
      ref={videoPreviewRef}
      onMouseEnter={handleVideoMouseEnter}
      onMouseLeave={hideVideoPreview}
    >
      <FileIconLayout
        filePaths={filePaths}
        filesInvalid={filesInvalid}
        preview={preview}
        metaItems={metaItems}
        index={index}
        showBadge={showBadge}
        isDragOverlay={isDragOverlay}
        sourceAppName={sourceAppName}
        sourceAppIcon={sourceAppIcon}
        colorScheme="purple"
        singleIcon={Video16Regular}
        multiLabel="个视频"
      />
    </div>
  );
});
