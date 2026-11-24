// Images pasted into a composer and sent with the message, for a host
// with the `images` capability. Each image shows as a `[Image #n]`
// placeholder in the text and as a chip under it; the message goes out
// with its images in order, its placeholders numbered from 1 as Agentty
// numbers them. Only PNG and JPEG files go, within the limits the server
// keeps.

// File types a message carries; the server recognizes no others.
export const IMAGE_TYPES = ["image/png", "image/jpeg"];
// Most images one message carries.
export const MAX_IMAGES = 8;
// Most image bytes one message carries together.
export const MAX_IMAGE_BYTES = 6 * 1024 * 1024;

// Why a paste is refused, as the server words it.
export const IMAGE_TYPE_REFUSAL = "Only PNG and JPEG images can be attached";
export const IMAGE_COUNT_REFUSAL = "A message carries at most 8 images";
export const IMAGE_SIZE_REFUSAL = "A message's images are at most 6 MB together";

// Bytes turned into characters at a time while encoding, below the
// argument limit of `String.fromCharCode`.
const ENCODE_CHUNK_BYTES = 0x8000;

// Whether the host takes images in messages.
export function offersImages(capabilities) {
  return Array.isArray(capabilities) && capabilities.includes("images");
}

// The placeholder of image `number` in the text.
export function imagePlaceholder(number) {
  return "[Image #" + number + "]";
}

// The image files among the files of a clipboard `transfer`.
export function transferImages(transfer) {
  return Array.from(transfer?.files ?? []).filter(
    (file) => typeof file?.type === "string" && file.type.startsWith("image/"),
  );
}

// Why `files` cannot join the attached `images`, or `null`.
export function imageRefusal(images, files) {
  if (files.some((file) => !IMAGE_TYPES.includes(file.type))) {
    return IMAGE_TYPE_REFUSAL;
  }
  if (images.length + files.length > MAX_IMAGES) {
    return IMAGE_COUNT_REFUSAL;
  }
  const bytes = [...images.map((image) => image.file), ...files].reduce((total, file) => total + file.size, 0);

  return bytes > MAX_IMAGE_BYTES ? IMAGE_SIZE_REFUSAL : null;
}

// The number the next pasted image gets: one past the highest attached.
export function nextImageNumber(images) {
  return images.reduce((highest, image) => Math.max(highest, image.number), 0) + 1;
}

// `text` as it goes out with `images`: each image's placeholder renumbered
// by its place in `images`, from 1. Numbers only ever fall, and images are
// in the order they were pasted, so no renumbered placeholder is renumbered
// again.
export function numberedText(text, images) {
  return images.reduce(
    (result, image, index) => result.split(imagePlaceholder(image.number)).join(imagePlaceholder(index + 1)),
    text,
  );
}

// `text` without the placeholder of image `number`, and the space that
// separated it from what follows.
export function withoutPlaceholder(text, number) {
  const placeholder = imagePlaceholder(number);
  const start = text.indexOf(placeholder);
  if (start < 0) {
    return text;
  }
  const end = start + placeholder.length;

  return text.slice(0, start) + text.slice(text[end] === " " ? end + 1 : end);
}

// The `images` of a `send_message` request for `images`: each file in
// standard base64.
export async function encodeImages(images) {
  return Promise.all(images.map(async (image) => ({ data: await encodeImage(image.file) })));
}

async function encodeImage(file) {
  const bytes = new Uint8Array(await file.arrayBuffer());
  let binary = "";
  for (let offset = 0; offset < bytes.length; offset += ENCODE_CHUNK_BYTES) {
    binary += String.fromCharCode(...bytes.subarray(offset, offset + ENCODE_CHUNK_BYTES));
  }

  return btoa(binary);
}

// A file size as the chips show it, as `240 KB` or `1.5 MB`.
export function imageSize(bytes) {
  if (bytes < 1024 * 1024) {
    return Math.max(1, Math.round(bytes / 1024)) + " KB";
  }

  return (bytes / (1024 * 1024)).toFixed(1) + " MB";
}
