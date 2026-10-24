const tick = (ms = 1) => new Promise((resolve) => setTimeout(resolve, ms));
const encoder = new TextEncoder();
const parts = [
  "<!doctype html><html><head><title>t</title></head><body>",
  '<!-- note --><p id="a">first</p><p id="b">sec',
  'ond</p><img src="x.png"></body></html>',
];
const log = [];
const transformed = new HTMLRewriter()
  .on("title", {
    async element(element) {
      // Await something real (a lookup, a fetch) before mutating.
      const title = await Promise.resolve("rewritten");
      await tick();
      element.setInnerContent(title);
      log.push("title");
    },
  })
  .on("p", {
    async element(element) {
      await tick();
      const id = element.getAttribute("id");
      element.setAttribute("data-id", id);
      element.prepend(`[${id}]`);
      element.onEndTag(async (end) => {
        await tick();
        end.before("<i>!</i>", { html: true });
        log.push(`end ${end.name} ${id}`);
      });
      log.push(`p ${id}`);
    },
    async text(text) {
      await tick();
      if (text.lastInTextNode) log.push("text done");
    },
  })
  .on("img", {
    async element(element) {
      await tick();
      element.after("<figcaption>x</figcaption>", { html: true });
    },
  })
  .onDocument({
    async comments(comment) {
      await tick();
      comment.text = " changed ";
    },
    async end(end) {
      await tick(5);
      end.append("<!-- end -->", { html: true });
      log.push("document end");
    },
  })
  .transform(
    new Response(
      new ReadableStream({
        async start(controller) {
          for (const part of parts) {
            await tick();
            controller.enqueue(encoder.encode(part));
          }
          controller.close();
        },
      }),
      { headers: { "content-type": "text/html" } },
    ),
  );
console.log(await transformed.text());
console.log(log.join("\n"));
