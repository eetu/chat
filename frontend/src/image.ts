// Long-edge ceiling for vision-chat attachments — keeps enough detail for
// multimodal models to read fine print / small UI elements.
const MAX_EDGE_CHAT = 1568;
// Long-edge ceiling for image-mode (Kontext img2img) attachments. The
// backend workflow further downscales the input to ~0.59 MP (≈768²) before
// VAE-encoding, so anything above ~1024 just bloats the wire payload
// without affecting output quality. Leaves a small buffer above 768 so
// non-square aspects don't get clipped twice.
const MAX_EDGE_IMAGE = 1024;
const JPEG_QUALITY = 0.82;

export type ResizedImage = {
  base64: string;
  preview: string;
};

export type ResizeMode = "chat" | "image";

export async function resizeImageForUpload(
  file: File,
  mode: ResizeMode = "chat",
): Promise<ResizedImage> {
  const bitmap = await createImageBitmap(file);
  try {
    const { width, height } = bitmap;
    const longEdge = Math.max(width, height);
    const maxEdge = mode === "image" ? MAX_EDGE_IMAGE : MAX_EDGE_CHAT;
    const scale = longEdge > maxEdge ? maxEdge / longEdge : 1;
    const w = Math.max(1, Math.round(width * scale));
    const h = Math.max(1, Math.round(height * scale));

    const canvas = document.createElement("canvas");
    canvas.width = w;
    canvas.height = h;
    const ctx = canvas.getContext("2d");
    if (!ctx) throw new Error("2d context unavailable");
    ctx.drawImage(bitmap, 0, 0, w, h);

    const blob = await new Promise<Blob | null>((resolve) =>
      canvas.toBlob(resolve, "image/jpeg", JPEG_QUALITY),
    );
    if (!blob) throw new Error("toBlob failed");

    const dataUrl = await blobToDataUrl(blob);
    const comma = dataUrl.indexOf(",");
    const base64 = comma >= 0 ? dataUrl.slice(comma + 1) : dataUrl;
    return { base64, preview: dataUrl };
  } finally {
    bitmap.close();
  }
}

function blobToDataUrl(blob: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(String(reader.result));
    reader.onerror = () => reject(reader.error);
    reader.readAsDataURL(blob);
  });
}
