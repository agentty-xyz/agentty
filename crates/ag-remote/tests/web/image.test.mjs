import assert from "node:assert/strict";
import test from "node:test";

import {
  IMAGE_COUNT_REFUSAL,
  IMAGE_SIZE_REFUSAL,
  IMAGE_TYPE_REFUSAL,
  MAX_IMAGES,
  encodeImages,
  imagePlaceholder,
  imageRefusal,
  imageSize,
  nextImageNumber,
  numberedText,
  offersImages,
  transferImages,
  withoutPlaceholder,
} from "../../assets/image.mjs";

function file(bytes, type = "image/png") {
  return new File([bytes], "image", { type: type });
}

test("images are offered only by a host with the images capability", () => {
  assert.equal(offersImages(["images"]), true);
  assert.equal(offersImages(["graphs"]), false);
  assert.equal(offersImages(undefined), false);
});

test("a transfer yields its image files and nothing without files", () => {
  const png = file(new Uint8Array(1));
  const text = new File(["notes"], "notes.txt", { type: "text/plain" });

  assert.deepEqual(transferImages({ files: [text, png] }), [png]);
  assert.deepEqual(transferImages(null), []);
  assert.deepEqual(transferImages({ files: [{ name: "untyped" }] }), []);
});

test("a paste is refused for other types, too many images, or too many bytes", () => {
  const attached = [{ file: file(new Uint8Array(1024)), number: 1 }];

  assert.equal(imageRefusal(attached, [file(new Uint8Array(1), "image/jpeg")]), null);
  assert.equal(imageRefusal(attached, [file(new Uint8Array(1), "image/webp")]), IMAGE_TYPE_REFUSAL);
  assert.equal(
    imageRefusal(attached, Array.from({ length: MAX_IMAGES }, () => file(new Uint8Array(1)))),
    IMAGE_COUNT_REFUSAL,
  );
  assert.equal(imageRefusal(attached, [file(new Uint8Array(6 * 1024 * 1024))]), IMAGE_SIZE_REFUSAL);
});

test("placeholders number from one past the highest image and renumber by position", () => {
  const images = [
    { file: null, number: 2 },
    { file: null, number: 3 },
  ];

  assert.equal(imagePlaceholder(4), "[Image #4]");
  assert.equal(nextImageNumber([]), 1);
  assert.equal(nextImageNumber(images), 4);
  assert.equal(
    numberedText("[Image #3] after [Image #2]", images),
    "[Image #2] after [Image #1]",
  );
});

test("a removed placeholder takes its following space, and a missing one changes nothing", () => {
  assert.equal(withoutPlaceholder("See [Image #1] here", 1), "See here");
  assert.equal(withoutPlaceholder("See [Image #1]", 1), "See ");
  assert.equal(withoutPlaceholder("See it", 1), "See it");
});

test("images encode as standard base64, also beyond one encoding chunk", async () => {
  const large = new Uint8Array(0x8000 + 3).fill(255);

  const [small, big] = await encodeImages([
    { file: file(Uint8Array.of(0, 1, 2, 3)), number: 1 },
    { file: file(large), number: 2 },
  ]);

  assert.deepEqual(small, { data: "AAECAw==" });
  assert.equal(big.data, Buffer.from(large).toString("base64"));
});

test("sizes show in KB below a megabyte and in MB above", () => {
  assert.equal(imageSize(10), "1 KB");
  assert.equal(imageSize(240 * 1024), "240 KB");
  assert.equal(imageSize(1.5 * 1024 * 1024), "1.5 MB");
});
