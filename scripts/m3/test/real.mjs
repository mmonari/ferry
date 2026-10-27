// The other machine, for pair-real.sh: a script that speaks to a real ferryd (a --no-input dev daemon) the way
// a terminal's `ferry pair` or another panel would, and prints what it saw as `NODE ...` lines.
//
//   node real.mjs SOCKET accept  yes|no|none           pair.accept, print the code, answer, wait for the end
//   node real.mjs SOCKET dial ADDR yes|no|none         pair.start, print the code, answer, wait for the end
//   node real.mjs SOCKET answer yes|no TRIGGER_FILE    another connection: answer the code once TRIGGER_FILE exists
//   node real.mjs SOCKET peers                         print the trusted peers' names
//
// `none` means: do not answer, and wait for the pairing to end by itself (or be cancelled by the other side).

import { existsSync } from "node:fs";
import { connect, sleep } from "./fakeclient.mjs";

const [socket, command, ...args] = process.argv.slice(2);
const say = (line) => console.log("NODE " + line);

const c = await connect(socket);
await c.until((e) => e.event === "hello", 5000);
await c.call("subscribe", { topics: ["pairing"] });

const TERMINAL = new Set(["paired", "failed", "closed"]);
const end = async (from, ms = 40000) => {
  const e = await c.until((x) => x.event === "pairing" && TERMINAL.has(x.data.stage), ms, from);
  say(`end stage=${e.data.stage} reason=${e.data.reason || ""} peer=${e.data.peer ? e.data.peer.name : ""}`);
  return e.data;
};

async function code(from, ms = 40000) {
  const e = await c.stage("code", ms, from);
  say(`code=${e.data.code} role=${e.data.role} peer=${e.data.peer ? e.data.peer.name : ""}`);
  return e.data;
}

try {
  if (command === "accept") {
    const from = c.mark();
    const r = await c.call("pair.accept");
    if (r.error) { say("error " + r.error.code + " " + r.error.message); process.exit(2); }
    await c.stage("listening", 3000, from);
    say("ready");
    await code(from);
    const answer = args[0];
    if (answer !== "none") say("answer " + JSON.stringify(await c.call("pair.answer", { match: answer === "yes" })));
    await end(from);
  } else if (command === "dial") {
    const from = c.mark();
    const r = await c.call("pair.start", { addr: args[0] });
    if (r.error) { say("error " + r.error.code + " " + r.error.message); process.exit(2); }
    await code(from);
    const answer = args[1];
    if (answer !== "none") say("answer " + JSON.stringify(await c.call("pair.answer", { match: answer === "yes" })));
    await end(from);
  } else if (command === "answer") {
    const from = c.mark();
    say("ready");
    await code(from);
    for (let i = 0; i < 600 && !existsSync(args[1]); i++) await sleep(50);
    const r = await c.call("pair.answer", { match: args[0] === "yes" });
    say("answer " + JSON.stringify(r));
  } else if (command === "peers") {
    const r = await c.call("peers");
    say("peers " + r.result.peers.map((p) => p.name).join(","));
  }
} catch (e) {
  say("failed " + e.message);
  process.exit(1);
}
c.close();
process.exit(0);
