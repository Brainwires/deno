// Copyright 2018-2026 the Deno authors. MIT license.

import { assert, assertEquals, assertThrows } from "./test_util.ts";

function imageBitmapData(bitmap: ImageBitmap) {
  return (bitmap as unknown as Record<symbol, () => Uint8Array>)[
    Symbol.for("Deno_bitmapData")
  ]();
}

// Basic tests for the structured clone algorithm. Mainly tests TypeScript
// typings. Actual functionality is tested in WPT.

Deno.test("self.structuredClone", async () => {
  const arrayOriginal = ["hello world"];
  const channelOriginal = new MessageChannel();
  const [arrayCloned, portTransferred] = self
    .structuredClone(
      [arrayOriginal, channelOriginal.port2] as [string[], MessagePort],
      {
        transfer: [channelOriginal.port2],
      },
    );
  assert(arrayOriginal !== arrayCloned); // not the same identity
  assertEquals(arrayCloned, arrayOriginal); // but same value
  channelOriginal.port1.postMessage("1");
  await new Promise((resolve) => portTransferred.onmessage = () => resolve(1));
  channelOriginal.port1.close();
  portTransferred.close();
});

Deno.test("correct DataCloneError message", () => {
  assertThrows(
    () => {
      const sab = new SharedArrayBuffer(1024);
      structuredClone(sab, {
        // @ts-expect-error cannot assign SharedArrayBuffer because it's not tranferable
        transfer: [sab],
      });
    },
    DOMException,
    "Value not transferable",
  );

  const ab = new ArrayBuffer(1);
  // detach ArrayBuffer
  structuredClone(ab, { transfer: [ab] });
  assertThrows(
    () => {
      structuredClone(ab, { transfer: [ab] });
    },
    DOMException,
    "ArrayBuffer at index 0 is already detached",
  );

  const ab2 = new ArrayBuffer(0);
  assertThrows(
    () => {
      structuredClone([ab2, ab], { transfer: [ab2, ab] });
    },
    DOMException,
    "ArrayBuffer at index 1 is already detached",
  );

  // ab2 should not be detached after above failure
  structuredClone(ab2, { transfer: [ab2] });
});

Deno.test("structuredClone URL throws DataCloneError", () => {
  // URL is not [Serializable] per the WHATWG/HTML spec, so cloning it must
  // throw a DataCloneError rather than silently producing an empty object.
  assertThrows(
    () => structuredClone(new URL("https://example.com/")),
    DOMException,
    "Cannot clone object of unsupported type.",
  );
  assertThrows(
    () => structuredClone(new URLSearchParams("a=1&b=2")),
    DOMException,
    "Cannot clone object of unsupported type.",
  );
});

Deno.test("structuredClone rejects non-serializable platform objects", () => {
  const channel = new MessageChannel();
  const { port1, port2 } = channel;
  const values = [
    channel,
    new Event("test"),
    new EventTarget(),
    AbortSignal.abort(),
    new TextEncoder(),
    new TextDecoder(),
    new TextEncoderStream(),
    new TextDecoderStream(),
    new FormData(),
    new CompressionStream("gzip"),
    new DecompressionStream("gzip"),
    new ByteLengthQueuingStrategy({ highWaterMark: 1 }),
    new CountQueuingStrategy({ highWaterMark: 1 }),
    new URLPattern("https://example.com/:id"),
    new PerformanceMark("test"),
    new PerformanceObserver(() => {}),
    crypto,
    crypto.subtle,
    new Headers(),
    new Request("https://example.com/"),
    new Response(),
    new URL("https://example.com/"),
  ];

  try {
    for (const value of values) {
      assertEquals(
        assertThrows(() => structuredClone(value), DOMException).name,
        "DataCloneError",
      );
      assertEquals(
        assertThrows(() => structuredClone({ value }), DOMException).name,
        "DataCloneError",
      );
      assertEquals(
        assertThrows(() => structuredClone([value]), DOMException).name,
        "DataCloneError",
      );
      assertEquals(
        assertThrows(
          () => structuredClone({ deep: [{ value }] }),
          DOMException,
        ).name,
        "DataCloneError",
      );
      assertEquals(
        assertThrows(
          () => structuredClone(new Map([["value", value]])),
          DOMException,
        ).name,
        "DataCloneError",
      );
      assertEquals(
        assertThrows(() => port2.postMessage({ value }), DOMException).name,
        "DataCloneError",
      );
    }
  } finally {
    port1.close();
    port2.close();
  }
});

Deno.test("structuredClone still transfers a nested MessagePort", async () => {
  // MessagePort is an EventTarget, which isn't serializable, but a port in
  // the transfer list must be transferred.
  const a = new MessageChannel();
  const cloned = structuredClone({ port: a.port1 }, { transfer: [a.port1] });
  assert(cloned.port instanceof MessagePort);

  const b = new MessageChannel();
  const c = new MessageChannel();
  const received = new Promise<MessageEvent>((resolve) => {
    c.port2.onmessage = resolve;
  });
  c.port1.postMessage({ port: b.port1 }, [b.port1]);
  const event = await received;
  assert(event.data.port instanceof MessagePort);

  for (
    const port of [cloned.port, a.port2, event.data.port, b.port2, c.port1]
  ) {
    port.close();
  }
  c.port2.close();
});

Deno.test("structuredClone geometry interfaces", () => {
  const values = [
    new DOMPointReadOnly(1, 2, 3, 4),
    new DOMPoint(1, 2, 3, 4),
    new DOMRectReadOnly(1, 2, 3, 4),
    new DOMRect(1, 2, 3, 4),
    new DOMQuad(
      new DOMPoint(1, 2),
      new DOMPoint(3, 4),
      new DOMPoint(5, 6),
      new DOMPoint(7, 8),
    ),
    new DOMMatrixReadOnly([1, 2, 3, 4, 5, 6]),
    new DOMMatrix([1, 2, 3, 4, 5, 6]),
    new DOMMatrixReadOnly([
      0,
      0,
      0,
      0,
      0,
      0,
      0,
      0,
      0,
      0,
      0,
      0,
      0,
      0,
      0,
      0,
    ]),
  ];

  for (const value of values) {
    const clone = structuredClone(value);
    assert(clone !== value);
    assertEquals(Object.getPrototypeOf(clone), Object.getPrototypeOf(value));
    assertEquals(clone.toJSON(), value.toJSON());
    const nested = structuredClone({
      deep: [value],
      map: new Map([["value", value]]),
    });
    assertEquals(
      Object.getPrototypeOf(nested.deep[0]),
      Object.getPrototypeOf(value),
    );
    assertEquals(nested.map.get("value")?.toJSON(), value.toJSON());
    assertThrows(
      () =>
        structuredClone(value, {
          transfer: [value as unknown as Transferable],
        }),
      DOMException,
      "Value not transferable",
    );
  }

  const point = new DOMPoint(1, 2, 3, 4);
  Object.defineProperty(point, "toJSON", {
    value: () => ({ x: 10, y: 20, z: 30, w: 40 }),
  });
  const pointClone = structuredClone(point);
  assertEquals([pointClone.x, pointClone.y, pointClone.z, pointClone.w], [
    1,
    2,
    3,
    4,
  ]);

  const unrestrictedPoint = structuredClone(
    new DOMPoint(Number.NaN, Infinity, -Infinity, Number.NaN),
  );
  assert(Number.isNaN(unrestrictedPoint.x));
  assertEquals(unrestrictedPoint.y, Infinity);
  assertEquals(unrestrictedPoint.z, -Infinity);
  assert(Number.isNaN(unrestrictedPoint.w));
});

Deno.test("structuredClone ImageData and ImageBitmap", async () => {
  const data = new ImageData(new Uint8ClampedArray([1, 2, 3, 255]), 1, 1);
  const clonedData = structuredClone(data);
  assert(clonedData instanceof ImageData);
  assertEquals(clonedData.width, 1);
  assertEquals(clonedData.height, 1);
  assertEquals([...clonedData.data], [1, 2, 3, 255]);
  clonedData.data[0] = 9;
  assertEquals(data.data[0], 1);
  assertThrows(
    () =>
      structuredClone(data, { transfer: [data as unknown as Transferable] }),
    DOMException,
    "Value not transferable",
  );

  const floatData = new ImageData(
    new Float16Array([0.5, 0.25, 0.125, 1]),
    1,
    1,
    { colorSpace: "display-p3", pixelFormat: "rgba-float16" },
  );
  const clonedFloatData = structuredClone(floatData);
  assertEquals(clonedFloatData.colorSpace, "display-p3");
  assertEquals(clonedFloatData.pixelFormat, "rgba-float16");
  assertEquals([...clonedFloatData.data], [...floatData.data]);

  const bitmap = await createImageBitmap(data);
  const clonedBitmap = structuredClone(bitmap);
  assert(clonedBitmap instanceof ImageBitmap);
  assertEquals(clonedBitmap.width, bitmap.width);
  assertEquals(clonedBitmap.height, bitmap.height);
  assertEquals([...imageBitmapData(clonedBitmap)], [1, 2, 3, 255]);
  const nestedBitmap = structuredClone({ bitmap, items: [bitmap] });
  assert(nestedBitmap.bitmap !== bitmap);
  assert(nestedBitmap.bitmap === nestedBitmap.items[0]);
  assertEquals([...imageBitmapData(nestedBitmap.bitmap)], [1, 2, 3, 255]);

  assertThrows(
    () => structuredClone(bitmap, { transfer: [bitmap, bitmap] }),
    DOMException,
    "Transfer list contains a duplicate entry",
  );
  assertEquals(bitmap.width, 1);

  const transferredBitmap = structuredClone(bitmap, { transfer: [bitmap] });
  assert(transferredBitmap instanceof ImageBitmap);
  assertEquals(transferredBitmap.width, 1);
  assertEquals([...imageBitmapData(transferredBitmap)], [1, 2, 3, 255]);
  assertEquals(bitmap.width, 0);
  bitmap.close();
  clonedBitmap.close();
  transferredBitmap.close();
});

Deno.test("structuredClone keeps a 16-bit ImageBitmap's depth", async () => {
  const bitmap = await createImageBitmap(
    new Blob([await Deno.readFile("tests/testdata/image/1x1-red16.png")]),
  );
  const data = [...imageBitmapData(bitmap)];
  assertEquals(data.length, 8);
  assertEquals([...imageBitmapData(structuredClone(bitmap))], data);
  const transferred = structuredClone(bitmap, { transfer: [bitmap] });
  assertEquals([...imageBitmapData(transferred)], data);
});

Deno.test("structuredClone transfers OffscreenCanvas", () => {
  const canvas = new OffscreenCanvas(2, 3);
  assertThrows(() => structuredClone(canvas), DOMException);
  assertThrows(
    () => structuredClone(canvas, { transfer: [canvas, canvas] }),
    DOMException,
    "Transfer list contains a duplicate entry",
  );
  assertEquals(canvas.width, 2);
  const transferred = structuredClone(canvas, { transfer: [canvas] });
  assert(transferred instanceof OffscreenCanvas);
  assertEquals(transferred.width, 2);
  assertEquals(transferred.height, 3);
  assertEquals(canvas.width, 0);
  assertEquals(canvas.height, 0);
  assertThrows(
    () => canvas.getContext("bitmaprenderer"),
    DOMException,
    "OffscreenCanvas is detached",
  );
  const error = assertThrows(
    () => structuredClone(canvas, { transfer: [canvas] }),
    DOMException,
  );
  assertEquals(error.name, "DataCloneError");
  assert(transferred.getContext("bitmaprenderer"));

  const emptyCanvas = new OffscreenCanvas(0, 0);
  const transferredEmptyCanvas = structuredClone(emptyCanvas, {
    transfer: [emptyCanvas],
  });
  assertEquals(transferredEmptyCanvas.width, 0);
  assertEquals(transferredEmptyCanvas.height, 0);
});

Deno.test("structuredClone doesn't transfer an OffscreenCanvas with a context", () => {
  const canvas = new OffscreenCanvas(1, 1);
  assert(canvas.getContext("bitmaprenderer"));
  const error = assertThrows(
    () => structuredClone(canvas, { transfer: [canvas] }),
    DOMException,
  );
  assertEquals(error.name, "InvalidStateError");
  assertEquals(canvas.width, 1);
});

Deno.test("postMessage URL throws DataCloneError", () => {
  // Posting a non-serializable value must take the same path as
  // structuredClone and throw, instead of silently delivering `{}`.
  const { port1, port2 } = new MessageChannel();
  try {
    assertThrows(
      () => port2.postMessage(new URL("https://example.org/")),
      DOMException,
      "Cannot clone object of unsupported type.",
    );
    assertThrows(
      () => port2.postMessage(new URLSearchParams("a=1")),
      DOMException,
      "Cannot clone object of unsupported type.",
    );
  } finally {
    port1.close();
    port2.close();
  }
});

Deno.test("postMessage transfers bitmap and offscreen canvas", async () => {
  const { port1, port2 } = new MessageChannel();
  try {
    const bitmap = await createImageBitmap(
      new ImageData(new Uint8ClampedArray([4, 5, 6, 255]), 1, 1),
    );
    const bitmapMessage = new Promise<ImageBitmap>((resolve) => {
      port2.onmessage = (event) => resolve(event.data);
    });
    port1.postMessage(bitmap, [bitmap]);
    assertEquals(bitmap.width, 0);
    const receivedBitmap = await bitmapMessage;
    assert(receivedBitmap instanceof ImageBitmap);
    assertEquals([...imageBitmapData(receivedBitmap)], [4, 5, 6, 255]);
    receivedBitmap.close();

    const canvas = new OffscreenCanvas(2, 3);
    const canvasMessage = new Promise<OffscreenCanvas>((resolve) => {
      port2.onmessage = (event) => resolve(event.data);
    });
    port1.postMessage(canvas, [canvas]);
    assertEquals(canvas.width, 0);
    const receivedCanvas = await canvasMessage;
    assert(receivedCanvas instanceof OffscreenCanvas);
    assertEquals(receivedCanvas.width, 2);
    assertEquals(receivedCanvas.height, 3);
  } finally {
    port1.close();
    port2.close();
  }
});

Deno.test("structuredClone CryptoKey", async () => {
  // AES key
  const aesKey = await crypto.subtle.generateKey(
    { name: "AES-GCM", length: 256 },
    true,
    ["encrypt", "decrypt"],
  );
  const aesClone = structuredClone(aesKey);
  assert(aesKey !== aesClone);
  assertEquals(aesClone.type, aesKey.type);
  assertEquals(aesClone.extractable, aesKey.extractable);
  assertEquals(aesClone.algorithm, aesKey.algorithm);
  assertEquals([...aesClone.usages], [...aesKey.usages]);

  // Verify the cloned key actually works
  const data = new TextEncoder().encode("hello");
  const iv = crypto.getRandomValues(new Uint8Array(12));
  const encrypted = await crypto.subtle.encrypt(
    { name: "AES-GCM", iv },
    aesClone,
    data,
  );
  const decrypted = await crypto.subtle.decrypt(
    { name: "AES-GCM", iv },
    aesKey,
    encrypted,
  );
  assertEquals(new Uint8Array(decrypted), data);

  // Non-extractable key can be cloned
  const nonExtractable = await crypto.subtle.generateKey(
    { name: "AES-GCM", length: 256 },
    false,
    ["encrypt", "decrypt"],
  );
  const nonExtractableClone = structuredClone(nonExtractable);
  assertEquals(nonExtractableClone.extractable, false);
  assertEquals(nonExtractableClone.algorithm, nonExtractable.algorithm);

  // HMAC key
  const hmacKey = await crypto.subtle.generateKey(
    { name: "HMAC", hash: "SHA-256" },
    true,
    ["sign", "verify"],
  );
  const hmacClone = structuredClone(hmacKey);
  assertEquals(hmacClone.type, hmacKey.type);
  assertEquals(hmacClone.algorithm, hmacKey.algorithm);
  assertEquals([...hmacClone.usages], [...hmacKey.usages]);

  // EC key pair
  const ecKeyPair = await crypto.subtle.generateKey(
    { name: "ECDSA", namedCurve: "P-256" },
    true,
    ["sign", "verify"],
  ) as CryptoKeyPair;
  const ecPrivateClone = structuredClone(ecKeyPair.privateKey);
  const ecPublicClone = structuredClone(ecKeyPair.publicKey);
  assertEquals(ecPrivateClone.type, "private");
  assertEquals(ecPublicClone.type, "public");

  // Ed25519 key pair
  const edKeyPair = await crypto.subtle.generateKey(
    "Ed25519",
    true,
    ["sign", "verify"],
  ) as CryptoKeyPair;
  const edClone = structuredClone(edKeyPair.privateKey);
  assertEquals(edClone.type, "private");
  assertEquals(edClone.algorithm.name, "Ed25519");
});

Deno.test("structuredClone validates the transfer list before serializing", () => {
  let reads = 0;
  const counted = {
    get value() {
      reads++;
      return 1;
    },
  };
  const throwing = {
    get value() {
      throw new Error("marker");
    },
  };

  // An entry that isn't transferable is rejected before any getter runs.
  assertThrows(
    () => structuredClone(counted, { transfer: [{} as ArrayBuffer] }),
    DOMException,
    "Value not transferable",
  );
  assertEquals(reads, 0);
  assertThrows(
    () => structuredClone(throwing, { transfer: [{} as ArrayBuffer] }),
    DOMException,
    "Value not transferable",
  );

  // So is a duplicate entry, and the buffer stays attached.
  const buffer = new ArrayBuffer(4);
  assertThrows(
    () => structuredClone(counted, { transfer: [buffer, buffer] }),
    DOMException,
    "duplicate",
  );
  assertEquals(reads, 0);
  assertEquals(buffer.byteLength, 4);
});

Deno.test("structuredClone doesn't transfer buffers when it throws", () => {
  // A getter that throws.
  let buffer = new ArrayBuffer(4);
  assertThrows(
    () =>
      structuredClone({
        get value() {
          throw new Error("marker");
        },
      }, { transfer: [buffer] }),
    Error,
    "marker",
  );
  assertEquals(buffer.byteLength, 4);

  // A value that can't be cloned.
  buffer = new ArrayBuffer(4);
  assertThrows(
    () => structuredClone({ fn() {} }, { transfer: [buffer] }),
    DOMException,
  );
  assertEquals(buffer.byteLength, 4);

  // An invalid entry after the buffer.
  buffer = new ArrayBuffer(4);
  assertThrows(
    () => structuredClone(1, { transfer: [buffer, {} as ArrayBuffer] }),
    DOMException,
    "Value not transferable",
  );
  assertEquals(buffer.byteLength, 4);

  // The buffer is transferred when cloning succeeds.
  buffer = new ArrayBuffer(4);
  const cloned = structuredClone(buffer, { transfer: [buffer] });
  assertEquals(buffer.byteLength, 0);
  assertEquals(cloned.byteLength, 4);
});

// A fresh worker hasn't loaded the modules of the geometry interfaces,
// ImageData and ImageBitmap, so this also checks that it can deserialize
// them. The worker reports what it received and posts the values back.
async function serializableValues() {
  return [
    new DOMPointReadOnly(1, 2, 3, 4),
    new DOMPoint(1, Number.NaN, Infinity, -0),
    new DOMRectReadOnly(1, 2, 3, 4),
    new DOMRect(-1, -2, 3, 4),
    new DOMQuad(
      new DOMPoint(1, 2),
      new DOMPoint(3, 4),
      new DOMPoint(5, 6),
      new DOMPoint(7, 8),
    ),
    new DOMMatrixReadOnly([1, 2, 3, 4, 5, 6]),
    new DOMMatrix([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]),
    new ImageData(new Uint8ClampedArray([1, 2, 3, 255]), 1, 1),
    await createImageBitmap(
      new ImageData(new Uint8ClampedArray([4, 5, 6, 255]), 1, 1),
    ),
  ];
}

function describeSerializable(value: unknown) {
  const tag = Object.prototype.toString.call(value);
  if (value instanceof ImageData) {
    return [tag, value.width, value.height, [...value.data]];
  }
  if (value instanceof ImageBitmap) {
    return [tag, value.width, value.height, [...imageBitmapData(value)]];
  }
  return [tag, (value as { toJSON(): unknown }).toJSON()];
}

Deno.test("postMessage round-trips serializable platform objects through a worker", async () => {
  const values = await serializableValues();
  const expected = values.map(describeSerializable);
  const worker = new Worker(
    `data:application/javascript,${
      encodeURIComponent(`onmessage = (event) => {
        postMessage({
          tags: event.data.map((value) => Object.prototype.toString.call(value)),
          values: event.data,
        });
      };`)
    }`,
    { type: "module" },
  );
  try {
    const received = new Promise<{ tags: string[]; values: unknown[] }>(
      (resolve, reject) => {
        worker.onmessage = (event) => resolve(event.data);
        worker.onerror = (event) => {
          event.preventDefault();
          reject(new Error(event.message));
        };
      },
    );
    worker.postMessage(values);
    const { tags, values: echoed } = await received;
    assertEquals(tags, expected.map(([tag]) => tag));
    assertEquals(echoed.map(describeSerializable), expected);
    for (let i = 0; i < values.length; i++) {
      assertEquals(
        Object.getPrototypeOf(echoed[i]),
        Object.getPrototypeOf(values[i]),
      );
    }
  } finally {
    worker.terminate();
  }
});

Deno.test("postMessage transfers an ImageBitmap to a worker and detaches it", async () => {
  const bitmap = await createImageBitmap(
    new ImageData(new Uint8ClampedArray([7, 8, 9, 255]), 1, 1),
  );
  const worker = new Worker(
    `data:application/javascript,${
      encodeURIComponent(`onmessage = (event) => {
        const bitmap = event.data;
        postMessage([
          Object.prototype.toString.call(bitmap),
          bitmap.width,
          bitmap.height,
          [...bitmap[Symbol.for("Deno_bitmapData")]()],
        ]);
      };`)
    }`,
    { type: "module" },
  );
  try {
    const received = new Promise((resolve, reject) => {
      worker.onmessage = (event) => resolve(event.data);
      worker.onerror = (event) => {
        event.preventDefault();
        reject(new Error(event.message));
      };
    });
    worker.postMessage(bitmap, [bitmap]);
    assertEquals([bitmap.width, bitmap.height], [0, 0]);
    assertEquals(await received, ["[object ImageBitmap]", 1, 1, [
      7,
      8,
      9,
      255,
    ]]);
    for (const transfer of [[], [bitmap]]) {
      const error = assertThrows(
        () => structuredClone(bitmap, { transfer }),
        DOMException,
      );
      assertEquals(error.name, "DataCloneError");
    }
  } finally {
    worker.terminate();
  }
});

Deno.test("an ImageBitmap from transferToImageBitmap is serializable", async () => {
  // In a fresh worker, so that nothing else has loaded the image module.
  const worker = new Worker(
    `data:application/javascript,${
      encodeURIComponent(`const canvas = new OffscreenCanvas(1, 1);
        canvas.getContext("bitmaprenderer");
        const bitmap = canvas.transferToImageBitmap();
        postMessage(Object.prototype.toString.call(structuredClone(bitmap)));`)
    }`,
    { type: "module" },
  );
  try {
    const received = new Promise((resolve, reject) => {
      worker.onmessage = (event) => resolve(event.data);
      worker.onerror = (event) => {
        event.preventDefault();
        reject(new Error(event.message));
      };
    });
    assertEquals(await received, "[object ImageBitmap]");
  } finally {
    worker.terminate();
  }
});
