import { render } from "@solidjs/testing-library";
import { expect, it } from "vitest";
import { LogList } from "../src/LogList";
import { applySnapshot } from "../src/store";

it("keeps only a window of a long log in the DOM", () => {
  const events = Array.from({ length: 5000 }, (_, i) => ({ seq: i + 1, timestamp: 0, source: "d", event_type: "x", message: "m" + i, level: "info" }));
  applySnapshot({ session: {}, agents: [], tasks: [], messages: [], events });
  const { container } = render(() => <LogList />);
  expect(container.querySelectorAll(".row").length).toBeLessThan(80);
});
