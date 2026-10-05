import { expect, noRequestsAfterStart, test } from "./fixtures.ts";
import { assertNoDialog, dismissQuestions } from "../dev/capture.ts";

test.use({ mockOptions: { scenario: "attention", fixture: "subagents" }, viewport: { width: 1440, height: 960 } });

test("attention capture puts off every question and cannot silently capture a covering dialog", async ({ page, mock, openPage, csp }) => {
  const guard = noRequestsAfterStart(page, mock);
  await openPage();
  const question = page.getByRole("dialog", { name: "question", exact: true });
  await expect(question).toContainText("question 1 of 2");
  await expect(assertNoDialog(page, "team")).rejects.toThrow("Unexpected dialog on surface 'team'");
  await dismissQuestions(page, mock.state.questions.length);
  await assertNoDialog(page, "team");
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect(page.locator(".question-banner")).toBeVisible();
  expect(mock.state.questions).toHaveLength(2);
  expect(mock.calls.filter((c) => c.method === "answer")).toEqual([]);
  await page.keyboard.press("l");
  await assertNoDialog(page, "events");
  await expect(page.locator(".log-list")).toBeVisible();
  expect(guard.requests).toEqual([]);
  expect(csp).toEqual([]);
});
