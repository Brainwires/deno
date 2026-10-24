const html = `<main id="app">
<h1 class="title big">Title</h1>
<ul class="list">
<li>one</li>
<li class="odd">two</li>
<li lang="en-US">three</li>
</ul>
<p><a href="https://deno.com/docs" rel="external noopener">docs</a></p>
<p><a href="/local.html">local</a></p>
<div><span>direct</span><em><span>nested</span></em></div>
</main>`;

const selectors = [
  "*",
  "h1",
  ".big",
  "#app",
  "[lang]",
  "[lang|=en]",
  '[rel~="noopener"]',
  '[href^="https:"]',
  '[href$=".html"]',
  '[href*="deno"]',
  "ul li",
  "div > span",
  "li:first-child",
  "li:nth-child(2)",
  "li:not(.odd)",
  "p:nth-of-type(2) a",
];

for (const selector of selectors) {
  const matched = [];
  new HTMLRewriter()
    .on(selector, {
      element(element) {
        const text = element.getAttribute("class") ??
          element.getAttribute("href") ?? element.getAttribute("id") ?? "";
        matched.push(text ? `${element.tagName}(${text})` : element.tagName);
      },
    })
    .transform(html);
  console.log(`${selector} -> ${matched.join(" ")}`);
}

// Several selectors on one rewriter run in document order.
const order = [];
const out = new HTMLRewriter()
  .on("li", { element: () => void order.push("li") })
  .on("ul", { element: () => void order.push("ul") })
  .on("li.odd", {
    element(e) {
      order.push("li.odd");
      e.setAttribute("class", "even");
    },
  })
  .transform(html);
console.log(order.join(","));
console.log(out.includes('<li class="even">two</li>'));
