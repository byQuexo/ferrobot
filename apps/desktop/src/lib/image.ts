// Avatar uploads are normalized client-side to a 256x256 PNG (center crop),
// which keeps them well under the daemon's 512 KiB limit.
const SIZE = 256;

export async function toAvatarPng(file: Blob): Promise<Blob> {
  const url = URL.createObjectURL(file);
  try {
    const img = new Image();
    img.src = url;
    await img.decode();
    const side = Math.min(img.naturalWidth, img.naturalHeight);
    const canvas = document.createElement("canvas");
    canvas.width = SIZE;
    canvas.height = SIZE;
    const ctx = canvas.getContext("2d");
    if (!ctx) throw new Error("canvas unavailable");
    ctx.imageSmoothingQuality = "high";
    ctx.drawImage(
      img,
      (img.naturalWidth - side) / 2,
      (img.naturalHeight - side) / 2,
      side,
      side,
      0,
      0,
      SIZE,
      SIZE,
    );
    return await new Promise<Blob>((resolve, reject) => {
      canvas.toBlob((b) => { if (b) resolve(b); else reject(new Error("could not encode image")); }, "image/png");
    });
  } finally {
    URL.revokeObjectURL(url);
  }
}
