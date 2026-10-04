// Agent text in the page (notes/web-design.md §security, §tests "text.test.ts").
import { expect, test } from "vitest";
import { linkPieces, safeHref, sgrSpans } from "../src/text";

test("only http: and https: become links", () => {
  expect(safeHref("https://example.com/a")).toBe("https://example.com/a");
  expect(safeHref("http://localhost:8080")).toBe("http://localhost:8080/");
  for (const bad of ["javascript:alert(1)", "data:text/html,<b>", "file:///etc/passwd", "vbscript:x", "ftp://x"]) expect(safeHref(bad), bad).toBeNull();
  expect(linkPieces("see javascript:alert(1) and data:text/html,x")).toEqual([{ text: "see javascript:alert(1) and data:text/html,x" }]);
  expect(linkPieces("see https://example.com/x, then stop.")).toEqual([{ text: "see " }, { text: "https://example.com/x", href: "https://example.com/x" }, { text: ", then stop." }]);
});

test("SGR becomes spans with classes; other escape sequences vanish", () => {
  expect(sgrSpans("\x1b[1mcrew\x1b[0m $ ok")).toEqual([{ text: "crew", classes: "b" }, { text: " $ ok", classes: "" }]);
  expect(sgrSpans("\x1b[31;42mred\x1b[39m")).toEqual([{ text: "red", classes: "fg-c1 bg-c2" }]);
  expect(sgrSpans("\x1b[38;5;196mx\x1b[38;2;0;0;255my")).toEqual([{ text: "x", classes: "fg-c1" }, { text: "y", classes: "fg-c4" }]);
  // Title changes, clipboard writes, cursor moves, hyperlinks: gone.
  expect(sgrSpans("a\x1b]0;title\x07b\x1b]52;c;aGk=\x07c\x1b[2Jd\x1b[10;5He\x1b]8;;http://x\x1b\\f\x1b]8;;\x1b\\")).toEqual([{ text: "abcdef", classes: "" }]);
  expect(sgrSpans("x\x07y\x00z")).toEqual([{ text: "xyz", classes: "" }]);
});
