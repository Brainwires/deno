// About 6 MB of list items in 2,000 chunks of 3,000 bytes each, pulled on
// demand so the rewriter only ever holds a chunk at a time.
const ITEMS_PER_CHUNK = 50;
const CHUNKS = 2000;
const encoder = new TextEncoder();
const item = (i) =>
  `<li class="item" data-i="${String(i).padStart(7, "0")}">x</li>`;

let pulled = 0;
let pulledAtFirstOutput = -1;
const source = new ReadableStream({
  start(controller) {
    controller.enqueue(encoder.encode("<!doctype html><ul>"));
  },
  pull(controller) {
    if (pulled === CHUNKS) {
      controller.enqueue(encoder.encode("</ul>"));
      controller.close();
      return;
    }
    let html = "";
    for (let j = 0; j < ITEMS_PER_CHUNK; j++) {
      html += item(pulled * ITEMS_PER_CHUNK + j);
    }
    pulled++;
    controller.enqueue(encoder.encode(html));
  },
}, { highWaterMark: 1 });

let elements = 0;
let texts = 0;
const transformed = new HTMLRewriter()
  .on("li.item", {
    element(element) {
      elements++;
      element.removeAttribute("class");
      element.setAttribute("data-seen", "1");
    },
    text(text) {
      if (text.lastInTextNode) texts++;
    },
  })
  .transform(new Response(source));

let bytes = 0;
let last = "";
const decoder = new TextDecoder();
for await (const chunk of transformed.body) {
  if (pulledAtFirstOutput === -1) pulledAtFirstOutput = pulled;
  bytes += chunk.byteLength;
  last = (last + decoder.decode(chunk, { stream: true })).slice(-200);
}

const total = CHUNKS * ITEMS_PER_CHUNK;
const expectedBytes = "<!doctype html><ul>".length + "</ul>".length +
  total * `<li data-i="0000000" data-seen="1">x</li>`.length;
console.log("elements", elements === total);
console.log("text nodes", texts === total);
console.log("bytes", bytes === expectedBytes);
console.log("streamed", pulledAtFirstOutput >= 0 && pulledAtFirstOutput < 10);
console.log(
  "tail",
  last.endsWith(
    `<li data-i="${
      String(total - 1).padStart(7, "0")
    }" data-seen="1">x</li></ul>`,
  ),
);
