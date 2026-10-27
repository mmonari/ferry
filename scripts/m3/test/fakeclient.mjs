// A JSON-lines client for the fake daemon, for the node tests: calls with answers, every event kept,
// and a way to wait for the one a test is about. No dependency but node.

import net from "node:net";
import { spawn } from "node:child_process";
import { mkdtempSync, rmSync, existsSync } from "node:fs";
import { join, dirname } from "node:path";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";

const FAKE = join(dirname(fileURLToPath(import.meta.url)), "..", "fake-ferryd.py");

export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// Starts the fake on a scratch socket. `args` are its command line options.
export async function startFake(args = []) {
  const dir = mkdtempSync(join(tmpdir(), "ferry-fake-test."));
  const socket = join(dir, "run", "ferry", "ferryd.sock");
  const child = spawn("python3", ["-I", FAKE, "--socket", socket, ...args], { stdio: ["ignore", "ignore", "pipe"] });
  let err = "";
  child.stderr.on("data", (d) => { err += d; });
  for (let i = 0; i < 100 && !existsSync(socket); i++) await sleep(50);
  if (!existsSync(socket)) throw new Error("the fake did not start: " + err);
  return {
    socket,
    stop() {
      child.kill("SIGTERM");
      rmSync(dir, { recursive: true, force: true });
    },
    connect: () => connect(socket)
  };
}

export function connect(path) {
  return new Promise((resolve, reject) => {
    const sock = net.createConnection(path);
    let buffer = "";
    let nextId = 1;
    const pending = new Map();
    const waiters = [];
    const client = {
      events: [],
      hello: null,
      // call("pair.start", { addr }) -> { result } or { error }
      call(method, params) {
        const id = nextId++;
        return new Promise((res) => {
          pending.set(id, res);
          sock.write(JSON.stringify({ id, method, ...(params === undefined ? {} : { params }) }) + "\n");
        });
      },
      // the first event after `from` (an index into events) that `match` accepts, within `ms`
      async until(match, ms = 4000, from = 0) {
        const t0 = Date.now();
        for (;;) {
          for (let i = from; i < client.events.length; i++) if (match(client.events[i])) return client.events[i];
          if (Date.now() - t0 > ms) throw new Error("no such event within " + ms + " ms; saw " + JSON.stringify(client.events.slice(from).map(brief)));
          await new Promise((r) => waiters.push(r));
        }
      },
      stage(stage, ms, from) {
        return client.until((e) => e.event === "pairing" && e.data.stage === stage, ms, from);
      },
      // the pairing events so far, as stage names
      stages(from = 0) {
        return client.events.slice(from).filter((e) => e.event === "pairing").map((e) => e.data.stage);
      },
      notices(from = 0) {
        return client.events.slice(from).filter((e) => e.event === "notice").map((e) => e.data);
      },
      mark: () => client.events.length,
      close: () => sock.destroy()
    };
    const wake = () => { while (waiters.length) waiters.shift()(); };
    sock.on("data", (chunk) => {
      buffer += chunk;
      let nl;
      while ((nl = buffer.indexOf("\n")) >= 0) {
        const line = buffer.slice(0, nl);
        buffer = buffer.slice(nl + 1);
        if (!line) continue;
        const msg = JSON.parse(line);
        if (msg.event !== undefined) {
          if (msg.event === "hello") client.hello = msg.data;
          client.events.push(msg);
          wake();
        } else if (pending.has(msg.id)) {
          const res = pending.get(msg.id);
          pending.delete(msg.id);
          res("error" in msg ? { error: msg.error } : { result: msg.result });
        }
      }
    });
    sock.on("connect", () => resolve(client));
    sock.on("error", reject);
  });
}

const brief = (e) => (e.event === "pairing" ? "pairing:" + e.data.stage : e.event);
