# ferryd socket API

ferryd serves a JSON-lines API on a Unix socket. The `ferry` command, the bar plugin in `plugin/`
and any other frontend read state and send commands through it. This page is the reference for the
authors of those clients. It describes API version 1 as ferryd 0.1.0 builds it.

The serde types in `crates/ferry-core/src/api.rs` define every shape, and the snapshot tests in
`crates/ferry-core/src/api/snapshots/` and `crates/ferryd/tests/snapshots/` pin their JSON. The
test `crates/ferryd/tests/socket_api_doc.rs` parses each transcript on this page with those types
and checks that the tables list every method, topic, error code, notice kind and config key.
`scripts/m3/test/api-examples.sh` runs the example clients at the end of the page against the fake
daemon.

On this page:

- [Conventions](#conventions)
- [Transport](#transport)
- [The hello event](#the-hello-event)
- [Versioning and feature detection](#versioning-and-feature-detection)
- [Methods](#methods)
- [Topics and events](#topics-and-events)
- [Shapes](#shapes)
- [Errors](#errors)
- [Config keys](#config-keys)
- [Counters](#counters)
- [Example clients](#example-clients)
- [Related documents](#related-documents)

## Conventions

- A peer is a machine that runs ferryd. A frontend is a client program that connects to the socket.
- Names are case-sensitive. Field names are snake_case. The order of fields in an object has no
  meaning.
- A field marked optional is left out when it has no value. ferryd never sends `null` for it. A
  field marked nullable is always present and can be `null`.
- A fingerprint is the SHA-256 hash of a peer's public key, written as 64 lowercase hex digits.
- Times are RFC 3339 in UTC. Durations are integers with the unit in the field name (`_ms`,
  `_us`, `_px`).
- A transcript is a code block of lines that start with `>` or `<`. The client sends the lines that
  start with `>`, and ferryd sends the lines that start with `<`. On the wire every message is one
  line without the marker.
- The examples use two fixture peers. `desk` has the fingerprint `d0` repeated 32 times, and
  `laptop` has the fingerprint `0b` repeated 32 times.

## Transport

### Socket

| Property | Value |
|----------|-------|
| Path | `$XDG_RUNTIME_DIR/ferry/ferryd.sock` |
| Type | Unix stream socket |
| Directory mode | 0700, owned by the user that runs ferryd |
| Socket mode | 0600 |
| Who can connect | Processes of the same user. ferryd reads the uid of each connection with `SO_PEERCRED` and closes a connection from any other uid without sending a line. |
| Stale socket | At start, ferryd replaces a socket file that nothing answers. A second ferryd does not start while one answers. |

A connect that fails with `ENOENT` or `ECONNREFUSED` means that ferryd is not running.

### Lines

- Every message is one JSON object on one line, in UTF-8, ended by `\n`.
- ferryd ignores blank lines.
- A line is at most 1 MiB (1,048,576 bytes). ferryd answers a longer request line with
  `bad_params` and id 0, then closes the connection. The `ferry` command fails on a longer answer
  line.
- ferryd sets no limit on the number of connections. Each connection has its own subscription.

### Message kinds

| Kind | Sent by | Identified by | Shape |
|------|---------|---------------|-------|
| Request | client | has `method` | `{"id":1,"method":"status"}`, or with `"params":{...}` |
| Response | ferryd | has `id`, and `result` or `error` | `{"id":1,"result":{...}}` or `{"id":1,"error":{"code":"...","message":"...","hint":"..."}}` |
| Event | ferryd | has `event` | `{"event":"state","data":{...}}` |

- `id` is an unsigned integer up to 2^64 - 1 that the client picks. ferryd copies it into the
  response.
- `params` is an object. A method that takes no params ignores any `params` it receives.
  `subscribe` and `config.get` read a missing or `null` `params` as `{}`.
- `hint` in an error is optional. `code` is a fixed word and `message` is text for a person.
- `hello` is an event, and it is the first line of every connection.

### Order and limits

- ferryd handles the requests of one connection one at a time, in the order sent, and writes one
  response for each, in the same order. A request that waits delays the later requests of its
  connection. `pair.start` waits until the other peer accepts the pairing connection. Events
  still flow meanwhile.
- ferryd writes every queued response before any queued event. Events can arrive between two
  responses, so a client matches a response to its request by `id`.
- A client that sends requests and does not read the answers is closed once 256 lines wait for it.
  A subscribed client that does not read loses old events instead, as
  [Rules for events](#rules-for-events) describes. Closing a connection cancels the pairing that it
  started.
- A line that is not a valid request gets a `bad_params` response, and the connection stays open.
  The `id` of the response is the request's `id` when ferryd could read it, and 0 otherwise. A
  method that ferryd does not know gets `unknown_method`, with a hint.

```jsonl
> {"id":6,"method":"nonsense"}
< {"id":6,"error":{"code":"unknown_method","message":"unknown method \"nonsense\"","hint":"this ferryd may be older than your client; compare `api` in the hello line"}}
> {"id":5}
< {"id":5,"error":{"code":"bad_params","message":"a request needs a \"method\""}}
> {"method":"status"}
< {"id":0,"error":{"code":"bad_params","message":"a request needs a numeric \"id\""}}
> not json
< {"id":0,"error":{"code":"bad_params","message":"not JSON: expected ident at line 1 column 2"}}
```

## The hello event

ferryd sends `hello` once, right after a connection opens and before it reads a request.

```jsonl
< {"event":"hello","data":{"api":1,"version":"0.1.0","peer":"desk","fingerprint":"d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0","methods":["status","release","go","id","trust","layout.get","layout.place","layout.set","lock","pair.accept","pair.start","pair.answer","pair.cancel","peers","unpair","doctor","subscribe","unsubscribe","config.schema","config.get","config.set","peer.rename"],"topics":["state","peers","layout","pairing","config","notice","ui","edge"]}}
```

| Field | Type | Meaning |
|-------|------|---------|
| `api` | integer | The API version. It is 1. |
| `version` | string | The ferryd package version. |
| `peer` | string | The name of this peer. |
| `fingerprint` | string | The fingerprint of this peer. |
| `methods` | array of strings | Every method that this ferryd answers. |
| `topics` | array of strings | Every topic that `subscribe` accepts, in the order that its answers use. |

An older ferryd sends no `methods` and no `topics`.

## Versioning and feature detection

- `hello.api` and `status.api` carry the API version, an integer. ferryd raises the number when it
  removes a field, renames a field or changes the meaning of one.
- Within one version, ferryd only adds. It adds fields, methods, topics and values of the open sets
  below.
- ferryd ignores unknown fields in params. A client must ignore unknown fields in results and
  events.
- `hello.methods` lists the methods that this ferryd answers. A method that is not in the list gets
  `unknown_method`.
- `hello.topics` lists the topics. `subscribe` with a topic that is not in the list fails as a
  whole with `bad_params`, and subscribes to none of the topics.
- The values of an open set can grow in a later ferryd. The sets are `error.code`,
  `summary.state`, `attention[].code`, `notice.kind`, `notice.urgency`, `ui.open`, the config
  `type` and `applies`, `capture.gate`, `capture.off_reason`, `last_release.reason` and
  `pairing.reason`. The Rust types in `ferry-core` read an unknown value as `Unknown`. A client
  shows the `message` or `title` that comes with an unknown value.
- A client skips an event whose name it does not know.
- `summary`, `attention` and `escape_chord` in `status`, and `role` in a `pairing` event, are
  missing from a ferryd that is older than M3.

## Methods

ferryd 0.1.0 answers 22 methods. A read-only method changes nothing, so a client can repeat it
freely.

| Method | Params | Result | Read-only |
|--------|--------|--------|-----------|
| [`status`](#status) | none | [StatusResult](#statusresult) | yes |
| [`id`](#id) | none | `name`, `fingerprint`, `port`, `addrs` | yes |
| [`release`](#release) | none | `{}` | no |
| [`go`](#go) | `side` | `{}` | no |
| [`lock`](#lock) | `on` (optional) | `locked` | no |
| [`trust`](#trust) | `fingerprint`, `name`, `addr` | [PeerRef](#peerref) | no |
| [`peers`](#peers) | none | `peers` | yes |
| [`unpair`](#unpair) | `peer` | [PeerRef](#peerref) | no |
| [`peer.rename`](#peerrename) | `peer`, `name` | [PeerRef](#peerref) | no |
| [`layout.get`](#layoutget) | none | [LayoutResult](#layoutresult) | yes |
| [`layout.place`](#layoutplace) | `peer`, `rel`, `anchor`, `align` | [LayoutResult](#layoutresult) | no |
| [`layout.set`](#layoutset) | `origins`, `base_rev` | [LayoutResult](#layoutresult) | no |
| [`pair.accept`](#pairaccept) | none | `expires_ms` | no |
| [`pair.start`](#pairstart) | `addr`, `name` | `{}` | no |
| [`pair.answer`](#pairanswer) | `match` | `{}` | no |
| [`pair.cancel`](#paircancel) | none | `{}` | no |
| [`subscribe`](#subscribe) | `topics` (optional) | `topics` | yes |
| [`unsubscribe`](#unsubscribe) | none | `{}` | yes |
| [`config.schema`](#configschema) | none | `keys` | yes |
| [`config.get`](#configget) | `key` (optional) | [ConfigValues](#configvalues) | yes |
| [`config.set`](#configset) | `key`, `value` | `key`, `value`, `applies` | no |
| [`doctor`](#doctor) | none | `checks` | yes |

A peer reference in params is a peer name, or at least 8 hex digits of a fingerprint. The match
ignores the case of the digits. The name of this peer counts as a name. A prefix that matches more
than one peer gets `ambiguous_peer`, and a name or prefix that matches none gets `unknown_peer`.

### `status`

Params: none. Result: [StatusResult](#statusresult), the same value as the `state` event.

```jsonl
> {"id":2,"method":"status"}
< {"id":2,"result":{"api":1,"me":{"name":"desk","fingerprint":"d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0"},"port":4243,"role":"idle","capture":{"backend":"off","gate":"off","barriers":0,"failed_barriers":[]},"screen_locked":null,"locked":false,"peers":[],"counters":{},"summary":{"state":"idle"},"attention":[],"escape_chord":"Super+Ctrl+Escape"}}
```

### `id`

Params: none. Result: the identity of this peer.

| Field | Type | Meaning |
|-------|------|---------|
| `name` | string | The name of this peer. |
| `fingerprint` | string | The fingerprint of this peer. |
| `port` | integer | The UDP port that ferryd listens on. |
| `addrs` | array of strings | The non-loopback addresses of this peer, best effort. |

```jsonl
> {"id":4,"method":"id"}
< {"id":4,"result":{"name":"desk","fingerprint":"d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0","port":4243,"addrs":["192.0.2.111"]}}
```

### `release`

Params: none. Result: `{}`. Forces a release of input capture from any state, on this peer and on
the peer on the other end of a crossing. `release` counts in the `releases` counter.

```jsonl
> {"id":3,"method":"release"}
< {"id":3,"result":{}}
```

Errors: `internal` when a thread of ferryd did not take the release.

### `go`

Params: `side`, one of `left`, `right`, `up` or `down` (`top` and `bottom` are read as `up` and
`down`). Result: `{}`. Moves the keyboard and mouse to the machine on that side of this one
without the pointer, as the Hyprland binds that `ferry setup` writes for `crossing.go_chord` do.

- At home, ferryd picks the link on that side, preferring the monitor under the cursor. It warps
  the cursor next to the middle of the link and moves it across with a short relative motion, so
  the barrier activates and the crossing starts as a pointer crossing would, lock check included.
  The pointer lands in the middle of the link on the other machine.
- While this machine drives another one, `go` toward this machine comes home, to where the cursor
  was before a key crossing (else to the middle of the link). `go` toward a third machine moves
  there. Pressing the `crossing.go_chord` modifiers with an arrow key on the other machine does the
  same.

`go` answers at once. When it does nothing (no machine on that side, capture off, or the barrier did
not activate within 500 ms), ferryd logs why and counts it in `go_refused`. A key crossing that
happened counts in `go_crossings`.

```jsonl
> {"id":5,"method":"go","params":{"side":"down"}}
< {"id":5,"result":{}}
```

Errors: `internal` when the capture thread did not take the request.

### `lock`

Turns lock to this machine on or off. While it is on, no crossing starts from this peer. ferryd
saves the setting, so it survives a restart.

| Param | Type | Meaning |
|-------|------|---------|
| `on` | boolean, optional | `true` locks, `false` unlocks. Without it, the setting toggles. |

Result: `locked`, a boolean, the setting now in force.

```jsonl
> {"id":10,"method":"lock","params":{"on":true}}
< {"id":10,"result":{"locked":true}}
> {"id":11,"method":"lock","params":{}}
< {"id":11,"result":{"locked":false}}
```

Errors: `internal` when ferryd cannot save the setting.

### `trust`

Pins a fingerprint without pairing. `trust` is idempotent. A fingerprint that is already pinned
gets the new name and address.

| Param | Type | Meaning |
|-------|------|---------|
| `fingerprint` | string | 64 hex digits. Colons and spaces between the digits are allowed. |
| `name` | string, optional | The name to pin. It follows the rule of the `peer.name` config key. |
| `addr` | string, optional | `host`, `host:port` or `ip:port`. This peer dials the address. Without it, this peer only accepts the connection of the other peer. |

Result: [PeerRef](#peerref) of the pinned peer.

```jsonl
> {"id":7,"method":"trust","params":{"fingerprint":"0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b","name":"laptop","addr":"192.0.2.11"}}
< {"id":7,"result":{"peer":{"name":"laptop","fingerprint":"0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b"}}}
> {"id":8,"method":"trust","params":{"fingerprint":"xyz"}}
< {"id":8,"error":{"code":"bad_fingerprint","message":"not a fingerprint: \"xyz\"","hint":"a fingerprint is 64 hex digits, as `ferry id` prints it"}}
```

Errors: `bad_fingerprint`, `self_fingerprint`, `bad_params` (a name or address that is not valid,
or a name that belongs to another peer), `unknown_peer` (the pin was removed while `trust` ran),
`internal`.

### `peers`

Params: none. Result: `peers`, an array of the pinned peers.

| Field | Type | Meaning |
|-------|------|---------|
| `name` | string | The name of the peer. |
| `fingerprint` | string | The fingerprint of the peer. |
| `addr` | string, optional | The address that this peer dials. |
| `last_seen` | string, optional | The last time a connection to the peer was up. |
| `paired` | string | `sas` for a peer paired with a code, `manual` for a peer pinned with `trust`. |
| `state` | string | `connected`, `connecting`, `offline` or `refused`. |
| `locked` | boolean | The peer has lock to this machine on. |

```jsonl
> {"id":9,"method":"peers"}
< {"id":9,"result":{"peers":[{"name":"laptop","fingerprint":"0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b","addr":"192.0.2.11:4243","last_seen":"2026-10-18T09:12:03Z","paired":"sas","state":"connected","locked":false}]}}
```

### `unpair`

Removes the pin of a peer and closes its connection with close code 4. A crossing to that peer
ends at once. The other peer learns that it was unpaired and receives the `unpaired` notice.
When the peer is placed, both peers also remove it from the layout in a new
revision, and the `layout` subscribers of each get a `layout` event.

| Param | Type | Meaning |
|-------|------|---------|
| `peer` | string | A peer reference. |

Result: [PeerRef](#peerref) of the peer that was removed.

```jsonl
> {"id":3,"method":"unpair","params":{"peer":"laptop"}}
< {"id":3,"result":{"peer":{"name":"laptop","fingerprint":"0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b"}}}
> {"id":4,"method":"unpair","params":{"peer":"nobody"}}
< {"id":4,"error":{"code":"unknown_peer","message":"no peer is named \"nobody\"","hint":"ferry status lists the peers; a fingerprint prefix needs at least 8 hex digits"}}
```

Errors: `unknown_peer`, `ambiguous_peer`, `bad_params` (the reference names this peer),
`internal` (ferryd could not save `trusted.toml`; the pin stays).

### `peer.rename`

Gives a pinned peer another name.

| Param | Type | Meaning |
|-------|------|---------|
| `peer` | string | A peer reference. |
| `name` | string | The new name. No whitespace, control characters or `:`, at most 64 characters. |

Result: [PeerRef](#peerref) with the new name.

```jsonl
> {"id":7,"method":"peer.rename","params":{"peer":"laptop","name":"netbook"}}
< {"id":7,"result":{"peer":{"name":"netbook","fingerprint":"0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b"}}}
> {"id":8,"method":"peer.rename","params":{"peer":"netbook","name":"has space"}}
< {"id":8,"error":{"code":"bad_params","message":"a peer name cannot contain ' ' (no whitespace, control characters or ':')"}}
```

Errors: `unknown_peer`, `ambiguous_peer`, `bad_params` (the name is not valid, belongs to another
peer, is the name of this peer, or the reference names this peer), `internal`.

### `layout.get`

Params: none. Result: [LayoutResult](#layoutresult).

```jsonl
> {"id":5,"method":"layout.get"}
< {"id":5,"result":{"revision":{"counter":0,"peer":"d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0"},"peers":[{"name":"desk","fingerprint":"d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0","origin":null,"monitors":[{"name":"HDMI-A-1","x":0,"y":0,"w":1920,"h":1080,"scale":1.0,"description":"HDMI-A-1 (test)","transform":0},{"name":"DVI-D-1","x":1920,"y":0,"w":1920,"h":1080,"scale":1.0,"description":"DVI-D-1 (test)","transform":0}],"online":true,"trusted":true,"self":true,"bounds":{"x":0,"y":0,"w":3840,"h":1080}}],"links":[]}}
```

### `layout.place`

Places the group of one peer against an anchor and publishes the new layout to every connected
peer. This peer is the reference of the layout, so the group of this peer is not placed. The first
placement gives this peer the origin (0, 0).

| Param | Type | Meaning |
|-------|------|---------|
| `peer` | string | A peer reference. |
| `rel` | string | `below`, `above`, `left_of` or `right_of`. The group goes on that side of the anchor. |
| `anchor` | string | `<peer>` (the bounding box of the whole group of that peer), `<monitor>` or `<peer>:<monitor>`. A bare monitor name is looked up on this peer first and then on the others. A name that exists on two other peers gets `ambiguous_anchor`. |
| `align` | string or integer, optional | How the group lines up with the anchor along the shared edge. `start` (the default), `center`, `end`, or an integer. An integer puts the start of the group that many pixels after the start of the anchor's edge, and can be negative. |

Result: [LayoutResult](#layoutresult) after the placement.

```jsonl
> {"id":6,"method":"layout.place","params":{"peer":"laptop","rel":"below","anchor":"HDMI-A-1","align":"center"}}
< {"id":6,"result":{"revision":{"counter":1,"peer":"d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0"},"peers":[{"name":"desk","fingerprint":"d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0","origin":[0,0],"monitors":[{"name":"HDMI-A-1","x":0,"y":0,"w":1920,"h":1080,"scale":1.0,"description":"Dell Inc. DELL U2415 7MT0167B1ZUL","transform":0}],"online":true,"trusted":true,"self":true,"bounds":{"x":0,"y":0,"w":1920,"h":1080}},{"name":"laptop","fingerprint":"0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b","origin":[277,1080],"monitors":[{"name":"eDP-1","x":277,"y":1080,"w":1366,"h":768,"scale":1.0,"description":"Chimei Innolux 0x1366","transform":0}],"online":true,"trusted":true,"self":false,"bounds":{"x":277,"y":1080,"w":1366,"h":768}}],"links":[{"from":"desk:HDMI-A-1","side":"bottom","to":"laptop:eDP-1","span":[277,1643],"live":true},{"from":"laptop:eDP-1","side":"top","to":"desk:HDMI-A-1","span":[277,1643],"live":true}]}}
```

Errors: `unknown_peer`, `ambiguous_peer`, `unknown_anchor`, `ambiguous_anchor`,
`peer_monitors_unknown`, `overlap`, `bad_params` (the peer is this peer, the anchor is the peer
itself, the result is too far from the rest of the layout, or the layout would hold more than 4
peers), `internal`.

### `layout.set`

Moves any number of groups in one new revision and publishes it. A peer that is not named keeps its
origin. If every named origin already holds, nothing changes, ferryd makes no revision, and the
result is the current layout.

| Param | Type | Meaning |
|-------|------|---------|
| `origins` | array | At least one entry. Each entry is `{"peer":"<peer reference>","x":<integer>,"y":<integer>}`. A peer appears once. A peer that sits in the layout but is not paired here can be named by 8 or more digits of its fingerprint. |
| `base_rev` | object, optional | A [RevisionView](#revisionview), the one from the `layout.get` result that the caller worked from. |

`x` and `y` are origins as `layout.get` lists them, and their absolute value is at most 2^24.

ferryd checks the request in this order, and the first failure answers. First the params
(`bad_params`, `unknown_peer`, `peer_monitors_unknown`), then `base_rev` (`stale_layout`), then
the geometry (`overlap`). Nothing changes unless every check passes.

Result: [LayoutResult](#layoutresult) after the move.

```jsonl
> {"id":8,"method":"layout.set","params":{"origins":[{"peer":"laptop","x":277,"y":1080}],"base_rev":{"counter":0,"peer":"d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0"}}}
< {"id":8,"result":{"revision":{"counter":1,"peer":"d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0"},"peers":[{"name":"desk","fingerprint":"d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0","origin":[0,0],"monitors":[{"name":"HDMI-A-1","x":0,"y":0,"w":1920,"h":1080,"scale":1.0,"description":"Dell Inc. DELL U2415 7MT0167B1ZUL","transform":0}],"online":true,"trusted":true,"self":true,"bounds":{"x":0,"y":0,"w":1920,"h":1080}},{"name":"laptop","fingerprint":"0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b","origin":[277,1080],"monitors":[{"name":"eDP-1","x":277,"y":1080,"w":1366,"h":768,"scale":1.0,"description":"Chimei Innolux 0x1366","transform":0}],"online":true,"trusted":true,"self":false,"bounds":{"x":277,"y":1080,"w":1366,"h":768}}],"links":[{"from":"desk:HDMI-A-1","side":"bottom","to":"laptop:eDP-1","span":[277,1643],"live":true},{"from":"laptop:eDP-1","side":"top","to":"desk:HDMI-A-1","span":[277,1643],"live":true}]}}
> {"id":10,"method":"layout.set","params":{"origins":[{"peer":"laptop","x":0,"y":0}]}}
< {"id":10,"error":{"code":"overlap","message":"laptop overlaps desk","hint":"ferry layout show lists where each peer's monitors are"}}
> {"id":11,"method":"layout.set","params":{"origins":[{"peer":"laptop","x":0,"y":1080}],"base_rev":{"counter":14,"peer":"d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0"}}}
< {"id":11,"error":{"code":"stale_layout","message":"the layout changed (rev 15 by laptop)","hint":"read the layout again with layout.get, then apply your change on top of it"}}
```

Errors: `unknown_peer`, `ambiguous_peer`, `peer_monitors_unknown`, `stale_layout`, `overlap`,
`bad_params` (empty `origins`, a peer named twice, a coordinate past the limit, a `base_rev.peer`
that is not a fingerprint, a result too far from the rest of the layout, or more than 4 peers),
`internal`.

### `pair.accept`

Opens the pairing window on this peer for 120 seconds. The connection that sends `pair.accept`
owns the pairing. Closing that connection cancels it. Other peers can start a pairing with this
peer while the window is open. After 3 failed attempts the window closes.

Params: none. Result: `expires_ms`, the milliseconds until the window closes.

```jsonl
> {"id":1,"method":"pair.accept"}
< {"id":1,"result":{"expires_ms":120000}}
```

Errors: `pairing_busy`, `internal`. The [`pairing` topic](#the-pairing-topic) shows the events that
follow.

### `pair.start`

Starts a pairing with the peer at `addr`. ferryd answers when that peer accepts the pairing
connection. The connection that sends `pair.start` owns the pairing, and closing it cancels the
pairing.

| Param | Type | Meaning |
|-------|------|---------|
| `addr` | string | `host`, `host:port` or `ip:port`. The default port is 4243. |
| `name` | string, optional | The name to pin the peer under. Without it, ferryd uses the name from the `Hello` of the peer. |

Result: `{}`. `pairing` events can arrive before the answer. When the dial fails, ferryd answers with the
error, and before it every `pairing` subscriber hears a `failed` event whose `reason` says why: `unreachable`,
`not_accepting`, `own_key`, `version` or `connect_failed`. A connection that sent `pair.start` and did
not subscribe has the error alone, with no event after it.

When the pairing succeeds and the new peer is not placed yet, this peer places it right of its own group,
tops aligned, when the peer first connects and its monitors are known. That is a new
layout revision, so `layout` subscribers on both peers get a `layout` event. The peer that accepted the
pairing places nothing. When the place is taken (another group there), nothing is placed and the
`layout_unplaced` attention item shows (on a peer with capture on).

```jsonl
> {"id":1,"method":"pair.start","params":{"addr":"192.0.2.11","name":"laptop"}}
< {"event":"pairing","data":{"stage":"connecting","role":"dialer","expires_ms":120000}}
< {"id":1,"result":{}}
> {"id":2,"method":"pair.start","params":{"addr":"192.0.2.12"}}
< {"event":"pairing","data":{"stage":"connecting","role":"dialer","expires_ms":120000}}
< {"event":"pairing","data":{"stage":"failed","role":"dialer","expires_ms":119000,"reason":"not_accepting","message":"192.0.2.12 is not accepting pairing"}}
< {"id":2,"error":{"code":"pairing_not_accepting","message":"192.0.2.12 is not accepting pairing","hint":"run ferry pair --accept there, or wait: it may be busy with another request"}}
```

The second dial in the example runs on a connection that subscribed to `pairing`.

Errors: `pairing_busy`, `pairing_not_accepting`, `peer_unreachable`, `bad_params` (the address or
name is not valid, or the address is this peer), `internal`.

### `pair.answer`

Gives the answer of the local user to "Same code?". Any connection can send it while a code is
shown. The first answer counts.

| Param | Type | Meaning |
|-------|------|---------|
| `match` | boolean | `true` when both peers show the same code. `false` cancels the pairing. |

Result: `{}`. The pin is written when both peers answer `true`.

```jsonl
> {"id":2,"method":"pair.answer","params":{"match":true}}
< {"id":2,"result":{}}
> {"id":3,"method":"pair.answer","params":{"match":true}}
< {"id":3,"error":{"code":"no_pairing","message":"the code was already answered","hint":"the first answer counts; the pairing events say how it went"}}
```

Errors: `no_pairing` (no pairing runs, the stage is not `code`, or the code has an answer),
`pairing_expired`, `internal`.

### `pair.cancel`

Cancels the pairing that runs. Any connection can send it, as with `pair.answer`. Params: none.
Result: `{}`. The connection that started the pairing and every `pairing` subscriber hear the end
once: `failed` for a dialer, `closed` for an acceptor, with `reason` `cancelled` and `cancelled_by`
`local`.

```jsonl
> {"id":4,"method":"pair.cancel"}
< {"id":4,"result":{}}
< {"event":"pairing","data":{"stage":"closed","role":"acceptor","expires_ms":92000,"reason":"cancelled","message":"pairing window closed: cancelled","cancelled_by":"local"}}
> {"id":5,"method":"pair.cancel"}
< {"id":5,"error":{"code":"no_pairing","message":"no pairing runs on this machine","hint":"start one with ferry pair, or open the Pair panel"}}
```

Errors: `no_pairing` (no pairing runs), `internal`.

### `subscribe`

Starts events on this connection. ferryd answers first. Then it sends one snapshot of each
subscribed snapshot topic in `hello.topics` order, and the current stage of a pairing that runs if
the connection subscribed to `pairing`. After that it sends one event for each change. A second
`subscribe` replaces the set of topics and sends the snapshots again. See
[Topics and events](#topics-and-events) for the rules of the events.

| Param | Type | Meaning |
|-------|------|---------|
| `topics` | array of strings, optional | The topics to subscribe to. Without it, every topic. An empty array ends the subscription. |

Result: `topics`, the topics now subscribed, in `hello.topics` order and without repeats.

```jsonl
> {"id":1,"method":"subscribe","params":{"topics":["layout","peers"]}}
< {"id":1,"result":{"topics":["peers","layout"]}}
< {"event":"peers","data":{"peers":[{"name":"laptop","fingerprint":"0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b","paired":"sas","state":"offline","locked":false}]}}
< {"event":"layout","data":{"revision":{"counter":0,"peer":"d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0"},"peers":[],"links":[]}}
> {"id":2,"method":"subscribe","params":{"topics":["fax"]}}
< {"id":2,"error":{"code":"bad_params","message":"subscribe: unknown variant `fax`, expected one of `state`, `peers`, `layout`, `pairing`, `config`, `notice`, `ui`"}}
```

Errors: `bad_params` (an unknown topic).

### `unsubscribe`

Ends every subscription of this connection. Params: none. Result: `{}`. Events that wait for the
connection are dropped.

```jsonl
> {"id":3,"method":"unsubscribe"}
< {"id":3,"result":{}}
```

### `config.schema`

Params: none. Result: `keys`, an array of [ConfigKey](#configkey), one for each key of
`config.toml`, in the order of the [Config keys](#config-keys) table.

The example lists four keys, the last one not available yet. The full list is the table in [Config keys](#config-keys).

```jsonl
> {"id":3,"method":"config.schema"}
< {"id":3,"result":{"keys":[{"key":"crossing.edge_resistance_px","type":"int","default":0,"min":0,"max":200,"unit":"px","applies":"live","doc":"Push this far into an edge before crossing"},{"key":"crossing.escape_chord","type":"chord","default":"Super+Ctrl+Escape","applies":"setup","doc":"Keys that always give input back"},{"key":"input.capture","type":"enum","default":"auto","choices":["auto","portal","off"],"applies":"restart","doc":"How input is captured"},{"key":"discovery.mdns","type":"bool","default":true,"applies":"restart","doc":"Look for other machines on the network","available":false}]}}
```

### `config.get`

Returns the saved values. A key that has no line in `config.toml` has its default.

| Param | Type | Meaning |
|-------|------|---------|
| `key` | string, optional | One dotted key. Without it, every key. |

Result: [ConfigValues](#configvalues). With `key`, both `values` and `pending` hold only that key.

```jsonl
> {"id":4,"method":"config.get","params":{"key":"crossing.edge_resistance_px"}}
< {"id":4,"result":{"values":{"crossing.edge_resistance_px":40}}}
> {"id":6,"method":"config.get","params":{"key":"peer.port"}}
< {"id":6,"result":{"values":{"peer.port":4290},"pending":["peer.port"]}}
```

Errors: `unknown_key`.

### `config.set`

Checks one value with the rules of the file loader, saves it in `config.toml`, and puts it in force
when the key applies at once. The edit keeps the comments, the order and the spacing of the file.
ferryd saves nothing when any check fails.

| Param | Type | Meaning |
|-------|------|---------|
| `key` | string | A dotted key from `config.schema`. |
| `value` | any | A value of the type of the key. A `chord` is saved in the canonical spelling. |

Result:

| Field | Type | Meaning |
|-------|------|---------|
| `key` | string | The key. |
| `value` | any | The value as saved. |
| `applies` | string | When the value is in force: `live` (now), `restart` (after `systemctl --user restart ferryd`) or `setup` (after `ferry setup`, which also writes the value into the Hyprland config). |

A saved `restart` key is in the `pending` list of [ConfigValues](#configvalues) until ferryd
restarts. A `setup` key is in force in ferryd at once, and the Hyprland bind changes with
`ferry setup`.

```jsonl
> {"id":5,"method":"config.set","params":{"key":"crossing.edge_resistance_px","value":40}}
< {"id":5,"result":{"key":"crossing.edge_resistance_px","value":40,"applies":"live"}}
> {"id":6,"method":"config.set","params":{"key":"peer.port","value":4290}}
< {"id":6,"result":{"key":"peer.port","value":4290,"applies":"restart"}}
> {"id":7,"method":"config.set","params":{"key":"crossing.escape_chord","value":"ctrl+alt+f12"}}
< {"id":7,"result":{"key":"crossing.escape_chord","value":"Ctrl+Alt+F12","applies":"setup"}}
> {"id":8,"method":"config.set","params":{"key":"crossing.edge_resistance_px","value":900}}
< {"id":8,"error":{"code":"bad_value","message":"crossing.edge_resistance_px: 900 is out of range","hint":"0 to 200 px"}}
> {"id":9,"method":"config.set","params":{"key":"crossing.escape_chord","value":"Ctrl+Escape"}}
< {"id":9,"error":{"code":"bad_value","message":"crossing.escape_chord: \"Ctrl+Escape\": the escape chord needs at least 2 modifiers","hint":"at least two modifiers (Super, Ctrl, Alt, Shift) and one key, such as Super+Ctrl+Escape"}}
> {"id":10,"method":"config.set","params":{"key":"edge_resistance_px","value":40}}
< {"id":10,"error":{"code":"unknown_key","message":"there is no config key \"edge_resistance_px\"","hint":"did you mean crossing.edge_resistance_px? `ferry config schema` lists the keys"}}
> {"id":11,"method":"config.set","params":{"key":"crossing.edge_resistance_px"}}
< {"id":11,"error":{"code":"bad_params","message":"config.set: missing field `value`"}}
```

A chord has 2 or 3 modifiers and one key. The modifiers are `Super`, `Ctrl`, `Alt` and `Shift`. The
parser also reads `Control`, `Meta`, `Logo`, `Win`, `Mod4` and `Mod1`, in any case. The keys are
`Escape`, `0` to `9`, `A` to `Z`, `Space`, `Backspace`, `Delete`, `Home`, `End`, `Pause` and `F1`
to `F12`.

Errors: `unknown_key`, `bad_value` (the value has the wrong type, is out of range, is not one of
the choices, or breaks another rule of the loader), `read_only` (`config.toml` or its directory is
not writable), `bad_params`, `internal` (`config.toml` is not valid TOML; ferryd does not
overwrite it).

### `doctor`

Params: none. Result: `checks`, an array of the checks that only ferryd can make. The `ferry doctor`
command adds its own checks to the list.

| Field | Type | Meaning |
|-------|------|---------|
| `name` | string | A short label. |
| `level` | string | `pass`, `warn` or `fail`. |
| `detail` | string | What the check found. |
| `fix` | string, optional | What to run or change. Absent on a pass. |

ferryd 0.1.0 reports the checks `hyprland`, `capture`, `globals`, `barriers`, `escape bind`,
`release_pressed_on_close`, `port`, `firewall`, `lock check`, one `peer <name>` for each pinned peer,
`lock to this machine`, and `unit` when ferryd runs under a systemd unit other than
`ferryd.service`. The set of names can change, so a frontend shows the checks as they are.

The example keeps three checks. A real result lists all of them.

```jsonl
> {"id":12,"method":"doctor"}
< {"id":12,"result":{"checks":[{"name":"hyprland","level":"fail","detail":"no Hyprland IPC: input is off in this ferryd, or Hyprland was not found when it started","fix":"start ferryd inside the Hyprland session (systemctl --user restart ferryd)"},{"name":"port","level":"pass","detail":"UDP 4243 is bound"},{"name":"peer laptop","level":"pass","detail":"connected, round trip 2.4 ms"}]}}
```

## Topics and events

An event is `{"event":"<topic>","data":<payload>}`. A connection receives events only after
`subscribe`, with one exception. The connection that started a pairing receives the `pairing`
events of that pairing without a subscription.

| Topic | Payload | Kind | Sent when |
|-------|---------|------|-----------|
| [`state`](#the-state-topic) | [StatusResult](#statusresult) | snapshot | The status changes outside the noise fields. |
| [`peers`](#the-peers-topic) | the `peers` result | snapshot | The `peers` result changes. |
| [`layout`](#the-layout-topic) | [LayoutResult](#layoutresult) | snapshot | The `layout.get` result changes. |
| [`pairing`](#the-pairing-topic) | [PairingEvent](#pairingevent) | stream | A pairing on this peer changes stage. |
| [`config`](#the-config-topic) | [ConfigValues](#configvalues) | snapshot | A saved value or the `pending` list changes. |
| [`notice`](#the-notice-topic) | [Notice](#notice) | stream | ferryd shows a toast. |
| [`ui`](#the-ui-topic) | [UiEvent](#uievent) | stream | A toast action asks a frontend to open a view. |
| [`edge`](#the-edge-topic) | [EdgeEvent](#edgeevent) | stream | A crossing goes out or comes home over an edge of this peer. |

A snapshot event carries the whole current value, never a difference. A stream event reports
something that happened.

### Rules for events

- **First snapshot.** After the answer to `subscribe`, ferryd sends one snapshot for each
  subscribed snapshot topic, in `hello.topics` order. A running pairing is sent as one `pairing`
  event, with its code when the stage is `code`. No other stream topic sends anything at that
  point.
- **Change detection.** ferryd compares each new snapshot with the one that it last sent on the
  same connection and sends it only when the two differ.
- **Noise fields of `state`.** These fields change while nothing happens: `counters`,
  `last_release.ago_ms`, `last_release.mono_us`, each `rtt_us` of a peer, and
  `pairing.expires_ms`. A difference only in them does not count as a change.
- **Refresh.** While any connection subscribes to `state`, ferryd looks at it every 2 seconds and
  sends the snapshot if anything differs, noise included. Counters and round trips therefore
  reach a frontend at most every 2 seconds.
- **Coalescing.** ferryd looks at one snapshot topic at most once every 50 milliseconds. The first
  change goes out at once. Later changes in the same window go out when the window ends, so the
  last change always arrives.
- **Slow readers.** A connection keeps one unsent snapshot for each snapshot topic, and a newer
  snapshot replaces an older one that is still waiting. A connection keeps up to 32 waiting events
  for each stream topic and drops the oldest first. The counter `events_superseded` counts both
  kinds of drop. Responses are never dropped.
- **Resubscribe.** A second `subscribe` replaces the topics, forgets what ferryd sent before, and
  sends the snapshots again. Closing the connection ends the subscription.

### The `state` topic

The payload is [StatusResult](#statusresult), the `status` result. ferryd sends it when the status
differs from the last one sent, outside the noise fields that [Rules for events](#rules-for-events)
lists. A change of role, active peer, lock, capture gate, screen lock, link state, pairing stage,
`summary` or `attention` is such a difference.

```jsonl
< {"event":"state","data":{"api":1,"me":{"name":"desk","fingerprint":"d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0"},"port":4243,"role":"source","active_peer":{"name":"laptop","fingerprint":"0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b"},"capture":{"backend":"portal","gate":"armed","barriers":1,"failed_barriers":[]},"screen_locked":false,"locked":false,"peers":[{"name":"laptop","fingerprint":"0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b","state":"connected","addr":"192.0.2.11:4243","rtt_us":850,"proto":"1.0","suspect":true,"locked":true},{"name":"old","fingerprint":"abababababababababababababababababababababababababababababababab","state":"refused","proto":"2.0","detail":"version mismatch: 1.0 vs 2.0","suspect":false,"locked":false}],"last_release":{"reason":"home","ago_ms":1200},"counters":{"bounces":2,"silence_releases":0},"summary":{"state":"away","peer":"laptop"},"attention":[],"escape_chord":"Super+Ctrl+Escape"}}
```

### The `peers` topic

The payload is the `peers` result.

```jsonl
< {"event":"peers","data":{"peers":[{"name":"laptop","fingerprint":"0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b","paired":"sas","state":"offline","locked":false}]}}
```

### The `layout` topic

The payload is [LayoutResult](#layoutresult), the `layout.get` result.

```jsonl
< {"event":"layout","data":{"revision":{"counter":0,"peer":"d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0"},"peers":[],"links":[]}}
```

### The `pairing` topic

ferryd sends a `pairing` event at every stage change of the pairing that runs on this peer, to each
connection subscribed to `pairing` and to the connection that started the pairing. A connection
that is both receives each event once. The code appears in the event of stage `code` and in the
title of the `pair_code` notice, and nowhere else. It never appears in `status`.

| Stage | Role | Meaning |
|-------|------|---------|
| `listening` | acceptor | The window is open and no attempt runs. The connection that sent `pair.accept` receives this stage only if it subscribed. |
| `connecting` | dialer | The dial and the exchange before the code. |
| `code` | both | The code is shown and the local user has not answered. |
| `waiting_peer` | both | The local user said yes and the other peer has not. |
| `paired` | both | Both peers said yes and this peer wrote the pin. |
| `failed` | both | The attempt ended without a pin. An acceptor then sends `listening` if the window stays open, or `closed` if it ends. A dialer sends nothing after `failed`. |
| `closed` | acceptor | The window ended without a pin. |

The acceptor sends `listening`, `code`, `waiting_peer` and `paired`. The dialer sends `connecting`,
`code`, `waiting_peer` and `paired`. `waiting_peer` is absent when the other peer answered first.

```jsonl
> {"id":1,"method":"pair.accept"}
< {"id":1,"result":{"expires_ms":120000}}
< {"event":"pairing","data":{"stage":"code","role":"acceptor","peer":{"name":"laptop","fingerprint":"0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b","addr":"192.0.2.11:4243"},"code":"418207","expires_ms":118200}}
> {"id":2,"method":"pair.answer","params":{"match":true}}
< {"id":2,"result":{}}
< {"event":"pairing","data":{"stage":"waiting_peer","role":"acceptor","peer":{"name":"laptop","fingerprint":"0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b","addr":"192.0.2.11:4243"},"expires_ms":117772}}
< {"event":"pairing","data":{"stage":"paired","role":"acceptor","peer":{"name":"laptop","fingerprint":"0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b","addr":"192.0.2.11:4243"},"expires_ms":117763}}
```

Every pairing ends with exactly one last event: `paired` or `failed` for a dialer, `paired` or
`closed` for an acceptor. An acceptor's `failed` ends one attempt, and `listening` (the window stays
open) or `closed` follows it. A failed attempt carries `reason` and `message`.

```jsonl
< {"event":"pairing","data":{"stage":"failed","role":"dialer","peer":{"name":"laptop","fingerprint":"0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b","addr":"192.0.2.11:4243"},"expires_ms":95000,"reason":"rejected","message":"cancelled: codes differ"}}
< {"event":"pairing","data":{"stage":"listening","role":"acceptor","expires_ms":61000}}
```

Do not log the `code` of a `pairing` event.

### The `config` topic

The payload is [ConfigValues](#configvalues). `pending` appears only while a saved `restart` key
waits for a restart.

```jsonl
< {"event":"config","data":{"values":{"crossing.edge_resistance_px":40,"peer.name":"desk"},"pending":["peer.name"]}}
```

### The `notice` topic

ferryd decides which toasts to show, and publishes every toast as a `notice` event. It publishes the
event when no notification daemon answers and when `notify.enabled` is `false`. That key stops only
the delivery to the desktop. A frontend that draws its own toasts subscribes to `notice`.

There is no notice for a crossing. A notice has no time field, and `notice` has no snapshot: a frontend that
connects after a toast was published never sees it. The problems that are still open are in
`status.attention`.

| Kind | Sent when | Urgency | Rate limit |
|------|-----------|---------|------------|
| `pair_code` | A pairing reaches the stage `code` on the acceptor. The dialer started the pairing and sees the code in its own view. | `critical` | None. |
| `paired` | A pairing writes a pin, on both peers. | `normal` | None. |
| `pair_failed` | A pairing ends without a pin. | `normal` | None. |
| `peer_lost` | The link to a pinned peer goes down. | `normal` | One notice for each peer in 30 seconds, for `peer_lost` and `peer_back` together. |
| `peer_back` | The link to a pinned peer comes up again. | `normal` | As `peer_lost`. |
| `unpaired` | The other peer removed this peer from its pins. | `normal` | None. |
| `layout_replaced` | A newer revision of the layout replaced an edit that this peer made. | `normal` | None. |
| `attention` | A new entry appears in `status.attention`. | `critical` | One notice for each code and peer in 5 minutes. |

When the rate limit swallows a change of `peer_lost` and `peer_back`, ferryd announces the state of
the peer when the window ends. The first connection of a peer after ferryd starts or after pairing
is silent.

The `title` and the `body` come from ferryd and are text for a person. ferryd replaces control
characters with spaces, cuts a title at 120 characters and a body at 400 characters, and ends a cut
text with an ellipsis.

| Kind | `title` | `body` | `peer` |
|------|---------|--------|--------|
| `pair_code` | `Pairing code 418 207`. The code is in the title, in two groups of three digits. | `<peer> is asking to pair. Confirm only if it shows the same code.` | The peer that asks. |
| `paired` | `Paired with <peer>` | Empty, or a note such as `Pinned as lap-2: the name lap belongs to another peer` when the pin has another name than the peer sent. | The peer. |
| `pair_failed` | `Pairing cancelled` when the reason is `rejected`, `cancelled`, `timeout` or `other`. Otherwise `Pairing failed`. | The `message` of the `failed` event with a capital first letter, such as `Codes differ`. | Absent. |
| `peer_lost` | `Lost <peer>` | Empty. | The peer. |
| `peer_back` | `<peer> is back` | Empty. | The peer. |
| `unpaired` | `<peer> unpaired this machine` | `Pair again to reconnect` | The peer that removed the pin. |
| `layout_replaced` | `Layout replaced by <winner>` | `Your last change was replaced by a newer one` | The winner. |
| `attention` | The `message` of the entry. | The `hint` of the entry, or empty. | The `peer` of the entry, if any. |

```jsonl
< {"event":"notice","data":{"kind":"pair_code","title":"Pairing code 418 207","body":"laptop is asking to pair. Confirm only if it shows the same code.","peer":"laptop","urgency":"critical"}}
< {"event":"notice","data":{"kind":"paired","title":"Paired with laptop","body":"","peer":"laptop","urgency":"normal"}}
< {"event":"notice","data":{"kind":"pair_failed","title":"Pairing cancelled","body":"Codes differ","urgency":"normal"}}
< {"event":"notice","data":{"kind":"peer_lost","title":"Lost laptop","body":"","peer":"laptop","urgency":"normal"}}
< {"event":"notice","data":{"kind":"peer_back","title":"laptop is back","body":"","peer":"laptop","urgency":"normal"}}
< {"event":"notice","data":{"kind":"unpaired","title":"laptop unpaired this machine","body":"Pair again to reconnect","peer":"laptop","urgency":"normal"}}
< {"event":"notice","data":{"kind":"layout_replaced","title":"Layout replaced by laptop","body":"Your last change was replaced by a newer one","peer":"laptop","urgency":"normal"}}
< {"event":"notice","data":{"kind":"attention","title":"laptop cannot connect: version mismatch: 1.0 vs 2.0","body":"`ferry doctor` has the details. After a key change or if it unpaired this machine, `ferry unpair laptop` and pair again; after a version mismatch, update the older ferry","peer":"laptop","urgency":"critical"}}
```

The title of a `pair_code` notice holds the pairing code. ferryd never logs the `title` or `body` of
a notice, and a client that logs events leaves them out.

### The `ui` topic

A `ui` event asks a frontend to open one of its views. ferryd does not know which shell runs or how
it opens a view, so each frontend decides what to do with the event. Every connection subscribed
to `ui` receives it.

| `open` | View |
|--------|------|
| `pair` | The Pair view. |
| `arrange` | The Arrange view. |
| `popover` | The popover of the bar widget. |
| `settings` | The Settings view. |

ferryd 0.1.0 sends only `{"open":"pair"}`. It sends it when the user presses Open on the toast of
a pairing code. The counter `notification_actions` counts the presses.

```jsonl
< {"event":"ui","data":{"open":"pair"}}
```

### The `edge` topic

An `edge` event says that the pointer crossed a link at an edge of one of this peer's monitors. A
frontend lights that stretch of the edge for a moment: the edge glow. Both peers of a crossing send
one at the same time, so the two machines light the two sides of the same seam.

- The source peer sends it with `role` `source` when its input leaves over the edge. The target
  peer sends it with `role` `target` when that input arrives.
- When the pointer comes back over the same edge, both peers send it again with `home` `true`.
  A crossing that ends any other way (`ferry release`, the escape chord, a lost link) sends no
  event at its end.
- ferryd sends no `edge` event while `crossing.edge_glow` is `false`.

ferryd sends it when the crossing starts, so there is no warning before the pointer reaches the
edge.

```jsonl
< {"event":"edge","data":{"role":"source","home":false,"peer":"laptop","monitor":"HDMI-A-1","side":"bottom","start":285,"end":1635}}
```

## Shapes

### PeerRef

| Field | Type | Meaning |
|-------|------|---------|
| `name` | string | The name of the peer. |
| `fingerprint` | string | The fingerprint of the peer. |

`trust`, `unpair` and `peer.rename` return `{"peer":<PeerRef>}`.

### StatusResult

The `status` result and the payload of the `state` event.

| Field | Type | Meaning |
|-------|------|---------|
| `api` | integer | The API version. |
| `me` | PeerRef | This peer. |
| `port` | integer | The UDP port that ferryd listens on. |
| `role` | string | `idle` (no crossing), `pending` or `arming` (a crossing is starting), `source` (the input of this peer drives another peer) or `target` (the input of another peer drives this peer). |
| `active_peer` | PeerRef, optional | The peer on the other end of a crossing, when there is one. |
| `capture` | object | The state of input capture. See below. |
| `screen_locked` | boolean, nullable | Whether the screen lock is on. `null` when ferryd cannot tell. |
| `locked` | boolean | Lock to this machine is on. |
| `peers` | array | One entry for each pinned peer, and one for each unknown peer whose connection ferryd refused. See below. |
| `last_release` | object, optional | The latest release of input. It is absent before the first one. |
| `counters` | object | A flat map from counter name to a non-negative integer. See [Counters](#counters). |
| `pairing` | object, optional | A pairing that runs on this peer. It has `role`, `stage` and `expires_ms`, and never the code. |
| `summary` | object | The one word for a bar widget. See below. |
| `attention` | array | Problems that the user must act on. It is empty when there are none. |
| `escape_chord` | string, optional | The escape chord in force, such as `Super+Ctrl+Escape`. |

`capture`:

| Field | Type | Meaning |
|-------|------|---------|
| `backend` | string | `portal`, or `off` when capture is off on this peer. |
| `gate` | string | `armed` (barriers set, capture enabled), `idle` (not armed yet), `locked` (screen locked), `suspended` (between sleep and wake), `rezoning` (waiting after a zones change), `barriers_failed` (the last barrier request failed), or `off`. |
| `barriers` | integer | The number of barriers set. |
| `failed_barriers` | array of integers | The barriers that failed. |
| `off_reason` | string, optional | Why capture is off on purpose. `controlled` (another peer drives this one) or `locked_here` (lock to this machine). |
| `notice` | string, optional | A condition that is worth a line in `ferry status`, such as a denied capture permission. |

`peers[]`:

| Field | Type | Meaning |
|-------|------|---------|
| `name` | string | The name of the peer. For an unknown peer, `peer-` and the first 8 hex digits of its fingerprint. |
| `fingerprint` | string | The fingerprint of the peer. |
| `state` | string | `connected`, `connecting`, `offline` or `refused`. |
| `addr` | string, optional | The address of the peer. |
| `rtt_us` | integer, optional | The round trip time in microseconds. |
| `proto` | string, optional | The protocol version from the `Hello` of the peer, such as `1.0`. |
| `detail` | string, optional | Why the state is `refused`, such as `version mismatch: 1.0 vs 2.0`. |
| `last_seen` | string, optional | The last time a connection to the peer was up. |
| `suspect` | boolean | ferryd released input because the peer went silent and has not heard from it since. Links to it are walls until a ping or pong arrives. |
| `locked` | boolean | The peer has lock to this machine on. No crossing starts from it. |

`last_release`:

| Field | Type | Meaning |
|-------|------|---------|
| `reason` | string | `other`, `home`, `hop`, `escape_chord`, `capture_ended`, `silence`, `locked`, `suspend`, `shutdown`, `drag`, `forced`, `layout`, `watchdog`, `preempted`, `refused`, `stale`, `takeover` or `link_lost`. |
| `ago_ms` | integer | Milliseconds since the release. |
| `mono_us` | integer, optional | The time of the release on the `CLOCK_MONOTONIC` clock of the peer, in microseconds. |

`summary`:

| Field | Type | Meaning |
|-------|------|---------|
| `state` | string | The first state in the table below that holds. |
| `peer` | string, optional | The other peer, for `away` and `controlled`. |

| Order | `state` | Holds when |
|-------|---------|------------|
| 1 | `attention` | `attention` is not empty. |
| 2 | `away` | This peer is the source peer of a crossing. `peer` is the target peer. |
| 3 | `controlled` | This peer is the target peer of a crossing. `peer` is the source peer. |
| 4 | `locked` | Lock to this machine is on. |
| 5 | `ready` | At least one pinned peer is connected. |
| 6 | `idle` | None of the above. |

Only a crossing in progress makes `away` or `controlled`. The moments before it (`pending`,
`arming`) read as `ready`. `off`, the state of a widget that cannot reach ferryd, never comes from
ferryd.

`attention[]`:

| Field | Type | Meaning |
|-------|------|---------|
| `code` | string | `peer_refused`, `barriers_failed`, `capture_denied`, `layout_unplaced` or `config_invalid`. |
| `message` | string | What is wrong, in words for a person. |
| `hint` | string, optional | What to do. |
| `peer` | string, optional | The name of the peer that the entry is about. |

### LayoutResult

The `layout.get` result, the result of `layout.place` and `layout.set`, and the payload of the
`layout` event.

| Field | Type | Meaning |
|-------|------|---------|
| `revision` | Revision | The revision of the layout. |
| `peers` | array | One entry for each peer in the layout. |
| `links` | array | The edge links between monitors of different peers. Each shared edge has two entries, one from each monitor. |

#### RevisionView

| Field | Type | Meaning |
|-------|------|---------|
| `counter` | integer | The revision counter. |
| `peer` | string | The fingerprint of the peer that made the edit. |

`layout.set` compares `base_rev` with this value.

`peers[]`:

| Field | Type | Meaning |
|-------|------|---------|
| `name` | string | The name of the peer. |
| `fingerprint` | string | The fingerprint of the peer. |
| `origin` | array of 2 integers, nullable | The revisioned origin `[x, y]` of the group. `null` when the peer is known but not placed. |
| `monitors` | array | The monitors of the peer. |
| `online` | boolean | The peer is connected, or it is this peer. |
| `trusted` | boolean | This peer is paired with that peer directly. A peer that is not has no live edge link to this peer. |
| `self` | boolean | The entry is for the peer that answered. |
| `bounds` | object, nullable | The bounding box `{x, y, w, h}` of the group, on the plane when placed and in local coordinates otherwise. `null` without monitors. |
| `shift` | array of 2 integers, optional | How far the peer moved its group to keep its monitors in place after Hyprland moved them. It is left out when it is zero. |

A monitor sits on the plane at its local position plus `shift` plus `origin`. The `x`, `y` and
`bounds` values of a placed peer already include the shift and the origin.

`monitors[]`:

| Field | Type | Meaning |
|-------|------|---------|
| `name` | string | The connector name, such as `HDMI-A-1`. |
| `x`, `y`, `w`, `h` | integers | The rectangle of the monitor, on the plane when the peer is placed and in local coordinates otherwise. |
| `scale` | number | The scale of the monitor. |
| `description` | string | The Hyprland description (make, model and serial). ferryd recognizes a monitor by it. |
| `transform` | integer | The `wl_output` transform, from 0 to 7. |

`links[]`:

| Field | Type | Meaning |
|-------|------|---------|
| `from` | string | `<peer>:<monitor>`. |
| `side` | string | The side of `from` where the link sits: `left`, `right`, `top` or `bottom`. |
| `to` | string | `<peer>:<monitor>`. |
| `span` | array of 2 integers | The extent of the shared edge on the plane. It is an x range for `top` and `bottom`, and a y range for `left` and `right`. |
| `live` | boolean | Both peers are paired and connected. |

Rectangles are half-open, so two monitors that share an edge do not overlap. A link exists where an
edge of a monitor lies on the opposite edge of a monitor of another peer, over the extent in `span`.
ferryd lists the links in the order of `derive_links` in `crates/ferry-core/src/geom.rs`: the pairs
of monitors in list order, the sides in the order `left`, `right`, `top`, `bottom`, and for each
shared edge the link from the first monitor and then the link from the second.

A frontend that draws the layout or lets the user move peers needs the same rules, so that it shows
the links ferryd will derive and refuses the drop that `layout.set` would refuse with `overlap`.
`plugin/Geometry.js` is a reference implementation of that geometry. It is plain JavaScript with no
QML and no I/O, so it runs under Node. Its `overlaps`, `firstOverlap` and `deriveLinks` follow
`Rect::overlaps`, `first_overlap` and `derive_links` loop for loop. `scripts/m3/test/geometry.test.mjs`
tests them, and `scripts/m3/test/real-layout.sh` compares them with the answers of a real ferryd.

### PairingEvent

The payload of the `pairing` event.

| Field | Type | Meaning |
|-------|------|---------|
| `stage` | string | `listening`, `connecting`, `code`, `waiting_peer`, `paired`, `failed` or `closed`. |
| `role` | string, optional | `dialer` or `acceptor`. |
| `peer` | object, optional | The other peer: `name`, `fingerprint` and `addr` (optional). |
| `code` | string, optional | Six digits with leading zeros, such as `418207`. It appears only at the stage `code`. |
| `expires_ms` | integer | The milliseconds until the window (acceptor) or the attempt (dialer) ends. |
| `reason` | string, optional | At `failed` and `closed`. Values include `rejected`, `cancelled`, `timeout`, `bad_reveal`, `protocol`, `version`, `lost`, `failures`, `save_failed` and `other`, and for a dial that did not reach the other peer `unreachable`, `not_accepting`, `own_key` and `connect_failed`. |
| `cancelled_by` | string, optional | With `reason` `cancelled`: `local` (a `pair.cancel` from any connection, the starting connection closing, or ferryd stopping) or `peer` (the other peer cancelled). |
| `message` | string, optional | Text for the user, such as `cancelled: codes differ`. At `paired` it says why the pin has another name than the peer sent. |

### Notice

The payload of the `notice` event.

| Field | Type | Meaning |
|-------|------|---------|
| `kind` | string | `pair_code`, `paired`, `pair_failed`, `peer_lost`, `peer_back`, `unpaired`, `layout_replaced` or `attention`. |
| `title` | string | The headline of the toast. |
| `body` | string | The text under the headline. It is always present and can be empty. |
| `peer` | string, optional | The name of the peer that the notice is about. |
| `urgency` | string | `low`, `normal` or `critical`. ferryd 0.1.0 sends `normal` and `critical`. |

### UiEvent

The payload of the `ui` event: `{"open":"<view>"}`, with a view from the table in
[The `ui` topic](#the-ui-topic).

### EdgeEvent

The payload of the `edge` event.

| Field | Type | Meaning |
|-------|------|---------|
| `role` | string | `source`: the input of this peer left over the edge. `target`: the input of another peer came in over it. |
| `home` | boolean | The pointer came back over the edge, and the crossing ended. |
| `peer` | string | The name of the peer on the other side of the edge. |
| `monitor` | string | The monitor of this peer that the edge belongs to, by its Hyprland name. |
| `side` | string | `left`, `right`, `top` or `bottom`. |
| `start` | integer | Where the linked stretch starts along the edge, in logical pixels from the left of the monitor (for `top` and `bottom`) or from its top (for `left` and `right`). The dead corners are left out. |
| `end` | integer | Where the stretch ends, exclusive. It is always greater than `start`. |

### ConfigKey

One entry of the `config.schema` result.

| Field | Type | Meaning |
|-------|------|---------|
| `key` | string | The dotted path. `[crossing] edge_resistance_px` in `config.toml` is `crossing.edge_resistance_px`. |
| `type` | string | `bool`, `int`, `string`, `enum`, `chord` or `name`. A `name` is a peer name: no whitespace, control characters or `:`, at most 64 characters. |
| `default` | any | The default value. |
| `min`, `max` | integers, optional | The range of an `int`. |
| `unit` | string, optional | The unit of an `int`, such as `px`, `ms` or `bytes`. |
| `slider_max` | integer, optional | For an `int` whose `max` is far above what a person usually sets: the top of the usual range. A frontend may offer a slider from `min` to here beside a field that still takes up to `max`. Only `crossing.edge_dwell_ms` has one (1000). Absent otherwise, so an older client sees no change. |
| `choices` | array of strings, optional | The values of an `enum`. |
| `applies` | string | `live`, `restart` or `setup`. |
| `doc` | string | One sentence that tells a person what the key does. |
| `available` | boolean, optional | `false` while no code in this version acts on the key: it is saved and read back, and the feature that uses it arrives later. A frontend shows the control disabled, with a note, and not as a switch that does nothing. `config.set` still takes the value. The field is absent when the key is in use, so a client built before it sees no change. |

### ConfigValues

The `config.get` result and the payload of the `config` event.

| Field | Type | Meaning |
|-------|------|---------|
| `values` | object | The saved value of each key, by dotted key. |
| `pending` | array of strings, optional | The keys whose saved value is not the value in force in the running ferryd. They apply after `systemctl --user restart ferryd`. The field is absent when the array would be empty. |

## Errors

An error response is `{"id":<id>,"error":{"code":"<code>","message":"<text>","hint":"<text>"}}`.
`hint` is optional. The `message` and the `hint` are text for a person and can change between
versions. The `code` is fixed.

| Code | Raised by | Meaning |
|------|-----------|---------|
| `unknown_method` | any request | The method is not in `hello.methods`. The hint tells the client to compare `api`. |
| `bad_params` | any request | The line is not a valid request, the params do not fit the method, or a rule on a name, an address or a number failed. |
| `bad_fingerprint` | `trust` | The text is not a fingerprint. |
| `self_fingerprint` | `trust` | The fingerprint is the one of this peer. |
| `unknown_peer` | `trust`, `unpair`, `peer.rename`, `layout.place`, `layout.set` | No peer matches the name or the prefix. |
| `ambiguous_peer` | `unpair`, `peer.rename`, `layout.place`, `layout.set` | A prefix of a fingerprint matches more than one peer. |
| `unknown_anchor` | `layout.place` | The anchor names no peer, no monitor, a monitor that the peer lacks, or a peer that is not placed yet. |
| `ambiguous_anchor` | `layout.place` | A monitor name exists on more than one peer, or a prefix matches more than one peer. |
| `peer_monitors_unknown` | `layout.place`, `layout.set` | ferryd never received the monitors of the peer. |
| `overlap` | `layout.place`, `layout.set` | The result would put two groups on the same area of the plane. |
| `stale_layout` | `layout.set` | `base_rev` is not the current revision. Nothing changed. |
| `internal` | any request | A fault inside ferryd, such as a failed file write. The hint tells the client to read the ferryd log. |
| `pairing_not_accepting` | `pair.start` | The other peer has no pairing window open, is busy, or refuses this address for now. |
| `pairing_busy` | `pair.accept`, `pair.start` | A pairing already runs on this peer. |
| `pairing_expired` | `pair.answer` | The window or the attempt ended before the answer. |
| `no_pairing` | `pair.answer`, `pair.cancel` | No pairing runs, no code waits for an answer, the code has an answer, or another connection owns the pairing. |
| `peer_unreachable` | `pair.start` | The address did not answer. |
| `unknown_key` | `config.get`, `config.set` | `config.toml` has no such key. |
| `bad_value` | `config.set` | The value has the wrong type, is out of range, is not one of the choices, or breaks a rule of the file loader. |
| `read_only` | `config.set` | `config.toml` or its directory is not writable. |

## Config keys

ferryd 0.1.0 has 18 keys. `config.schema` returns the same list, with a one-sentence `doc` for each
key.

| Key | Type | Default | Range or choices | Applies |
|-----|------|---------|------------------|---------|
| `peer.name` | name | `""` | An empty name uses the hostname. | restart |
| `peer.port` | int | `4243` | 0 to 65535. 0 lets the kernel pick a port. | restart |
| `crossing.edge_resistance_px` | int | `0` | 0 to 200 px | live |
| `crossing.edge_dwell_ms` | int | `0` | 0 to 65535 ms | live |
| `crossing.corner_dead_zone_px` | int | `8` | 0 to 200 px | live |
| `crossing.block_while_button_held` | bool | `true` | | live |
| `crossing.escape_chord` | chord | `"Super+Ctrl+Escape"` | 2 or 3 modifiers and one key | setup |
| `crossing.go_chord` | chord | `"Super+Ctrl+Alt"` | 2 or 3 modifiers and no key; an arrow key completes it | setup |
| `crossing.edge_glow` | bool | `true` | | live |
| `input.capture` | enum | `"auto"` | `auto`, `portal`, `direct`, `layer-shell`, `off` | restart |
| `input.emulation` | enum | `"wayland"` | `wayland` | restart |
| `input.scroll_invert` | bool | `false` | | restart |
| `clipboard.enabled` | bool | `true` | | live |
| `clipboard.max_bytes` | int | `33554432` | 0 to 9223372036854775807 bytes | live |
| `discovery.mdns` | bool | `true` | | restart |
| `discovery.advertise` | bool | `true` | | restart |
| `safety.lock_check_budget_ms` | int | `50` | 50 to 100 ms | restart |
| `notify.enabled` | bool | `true` | | live |

`input.scroll_invert` and `discovery.*` are saved and read back, and the features that use them
arrive later. `config.schema` marks these three keys `"available":false`.

## Counters

`status.counters` is a flat map from a counter name to a non-negative integer. A counter counts
from the start of ferryd and restarts at 0 with ferryd. A counter that has not fired can be
missing from the map, and a client reads a missing counter as 0. New counters can appear in any
release, so a frontend shows the map as it is and does not depend on a fixed set.

These counters come from the parts of ferryd that this page describes.

| Counter | Counts |
|---------|--------|
| `events_superseded` | Events that ferryd dropped because a connection read too slowly. A newer snapshot replaced an unsent one, or the oldest waiting event of a stream topic went. |
| `toasts_shown` | Toasts that the notification policy let through. Each one is also a `notice` event. |
| `toasts_undelivered` | Toasts that did not reach the desktop because no notification daemon answered or it refused the call. |
| `toast_queue_full` | Toasts that ferryd dropped because the queue to the notification task was full. |
| `notification_actions` | Presses of the Open button on the toast of a pairing code. |
| `releases` | Calls of `release`. |
| `go_crossings`, `go_refused` | Key crossings (`go` or the go chord) that moved control, and the ones that did nothing. |
| `pair_attempts`, `pair_failures` | Pairing attempts and the attempts that failed. |
| `layout_replaced` | Edits of this peer that a newer revision replaced. |

## Example clients

Each example is a complete program. The first three read the socket path from `XDG_RUNTIME_DIR`
the way the `ferry` command does.

### Shell with socat and jq

`status.sh` prints the `status` result as one line. A client that sends one request and closes its
input still receives the `hello` line first, so the script filters on `id`.

```sh
#!/bin/sh
# Print the status result of ferryd as one JSON line. Needs socat and jq.
SOCK="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/ferry/ferryd.sock"
printf '%s\n' '{"id":1,"method":"status"}' |
	socat - "UNIX-CONNECT:$SOCK" |
	jq -c 'select(.id == 1) | .result'
```

`events.sh` subscribes to the topics that you name, or to every topic, and prints each event.

```sh
#!/bin/sh
# Print the events of the topics you name, or of every topic when you name none, one JSON line each,
# until you press Ctrl+C. Needs socat and jq. Usage: events.sh state layout
SOCK="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/ferry/ferryd.sock"
if [ "$#" -gt 0 ]; then
	TOPICS=$(printf '"%s",' "$@")
	PARAMS="{\"topics\":[${TOPICS%,}]}"
else
	PARAMS='{}'
fi
{
	printf '%s\n' "{\"id\":1,\"method\":\"subscribe\",\"params\":$PARAMS}"
	sleep infinity
} | socat - "UNIX-CONNECT:$SOCK" |
	jq --unbuffered -c 'select(.event != null)'
```

With OpenBSD netcat, `-U` connects to a Unix socket and `-N` closes the connection when the input
ends. The line prints the `hello` line and then the result.

```sh
printf '%s\n' '{"id":1,"method":"id"}' | nc -U -N "$XDG_RUNTIME_DIR/ferry/ferryd.sock"
```

### Python

`client.py` keeps one connection, matches responses to requests by `id`, queues the events that
arrive during a call, and raises `ApiError` for an error response. It runs with the standard
library only.

```python
#!/usr/bin/env python3
"""A minimal ferryd client: one connection, requests with their answers, and events."""
import json
import os
import socket


class ApiError(Exception):
	"""An error answer. code is the machine word, str() is the message, hint may be None."""

	def __init__(self, error):
		super().__init__(error["message"])
		self.code = error["code"]
		self.hint = error.get("hint")


class Ferry:
	def __init__(self, path=None):
		runtime = os.environ.get("XDG_RUNTIME_DIR", f"/run/user/{os.getuid()}")
		self.sock = socket.socket(socket.AF_UNIX)
		self.sock.connect(path or os.path.join(runtime, "ferry", "ferryd.sock"))
		self.lines = self.sock.makefile("r", encoding="utf-8", newline="\n")
		self.next_id = 1
		self.pending = []  # events that arrived while a call waited for its answer
		first = self.read()
		assert first["event"] == "hello", first
		self.hello = first["data"]

	def read(self):
		line = self.lines.readline()
		if not line:
			raise EOFError("ferryd closed the connection")
		return json.loads(line)

	def supports(self, method):
		# an M1 ferryd sends no list of methods
		return method in self.hello.get("methods", [])

	def call(self, method, params=None):
		request = {"id": self.next_id, "method": method}
		if params is not None:
			request["params"] = params
		self.next_id += 1
		self.sock.sendall((json.dumps(request) + "\n").encode())
		while True:
			frame = self.read()
			if "event" in frame:
				self.pending.append(frame)
			elif frame["id"] == request["id"]:
				if "error" in frame:
					raise ApiError(frame["error"])
				return frame["result"]

	def event(self):
		"""The next event, from the ones that arrived during a call or from the socket."""
		while not self.pending:
			frame = self.read()
			if "event" in frame:
				self.pending.append(frame)
		return self.pending.pop(0)


if __name__ == "__main__":
	ferry = Ferry()
	print("hello:", ferry.hello["peer"], "api", ferry.hello["api"])
	if ferry.supports("subscribe"):
		ferry.call("subscribe", {"topics": ["state"]})
		# the snapshot that follows the answer; after it, one event for each change
		event = ferry.event()
		print(event["event"], event["data"]["summary"])
	else:
		print("status:", ferry.call("status")["role"])
	try:
		ferry.call("config.set", {"key": "crossing.edge_resistance_px", "value": 900})
	except ApiError as error:
		print(error.code, "-", error, "-", error.hint)
```

### Quickshell

`Status.qml` connects with the `Socket` type of Quickshell, subscribes to `state` when the `hello`
line arrives, and logs the summary state at each change. Run it with `quickshell -p Status.qml`.
`plugin/FerryClient.qml` is the full client of the bar plugin.

```qml
import QtQuick
import Quickshell
import Quickshell.Io

// Prints the state of ferryd each time it changes. Run it with: quickshell -p Status.qml
ShellRoot {
	Socket {
		id: ferry
		path: Quickshell.env("XDG_RUNTIME_DIR") + "/ferry/ferryd.sock"
		connected: true

		function send(request) {
			write(JSON.stringify(request) + "\n");
			flush();
		}

		parser: SplitParser {
			splitMarker: "\n"
			onRead: function (line) {
				const message = JSON.parse(line);
				if (message.event === "hello")
					ferry.send({ id: 1, method: "subscribe", params: { topics: ["state"] } });
				else if (message.event === "state")
					console.log("ferry state: " + message.data.summary.state);
			}
		}
	}
}
```

## Related documents

- `plugin/README.md` describes the bar plugin, the reference frontend of this API.
- `plugin/Geometry.js` is the reference implementation of the edge-link geometry for frontends that
  draw or edit the layout.
- `ferry events` prints the `hello` line and then every event as ferryd wrote it, one JSON line each, and
  `ferry --json <command>` prints the raw result of a command. Both show the wire form from a terminal.
- `scripts/m3/fake-ferryd.py` is a fake daemon that speaks this API for frontend tests. Its test
  `scripts/m3/test/fake_schema.py` compares its config schema with the one of ferryd.
- `scripts/m3/dev-ferryd.sh` starts one or two real ferryd processes without input, in scratch
  directories, for a frontend to talk to. It never touches the real session.
