import { afterEach, expect, test, vi } from "vitest";
import { Conn } from "../src/net/conn";
import { Session, STORAGE_KEY } from "../src/net/session";
import { FakeSocket, MemoryStorage, fakeEnv, sha256 } from "./fakes";

afterEach(() => vi.useRealTimers());

test("the reconnect deadline also bounds a socket stuck opening", async () => {
  vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "Date"] });
  FakeSocket.all = [];
  const storage = new MemoryStorage();
  storage.setItem(STORAGE_KEY, JSON.stringify({ token: "token", verifier: sha256("secret") }));
  const conn = new Conn({
    env: fakeEnv(), session: new Session(storage, null), reconnect: true,
    ready() {}, push() {}, state() {},
  });
  conn.start();
  FakeSocket.all[0].drop();
  await vi.advanceTimersByTimeAsync(251);
  expect(FakeSocket.all.length).toBe(2);
  // A listener accepted TCP but never answered the HTTP upgrade.
  await vi.advanceTimersByTimeAsync(60_000);
  expect(conn.stopped).toBe("gone");
});
