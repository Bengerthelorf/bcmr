# Path B Design: SSH Rendezvous + Direct TCP

**Status**: design-only. Not yet implemented. Living on branch
`path-b/direct-tcp`; merges to main only when the threat model is
reviewed and an end-to-end path works.

Path A ([Experiment 19](/ablation/wire-protocol#experiment-19-parallel-ssh-connections-break-the-single-stream-ceiling))
scaled us past scp by opening N parallel SSH sessions. The hard
ceiling that remains is per-connection: **one SSH session is one
cipher stream, walled by one crypto core** (~500 MB/s AES-NI, less
for ChaCha20). On a 10/25/100 GbE LAN that's the binding constraint.

Path B takes the data path off SSH entirely. SSH remains the
authenticated control channel; data moves over a separate TCP
socket with either no encryption (LAN opt-in) or an AEAD that
isn't constrained by OpenSSH's cipher negotiation and can use
multiple CPU cores.

## What we're trying to beat

Rough single-stream throughput ceilings on a modern server
(Zen 4 / Sapphire Rapids, AES-NI, no page-cache misses):

| Transport | Crypto | Ceiling |
|---|---|---|
| `scp` / `rsync -e ssh` | AES-GCM, single core | ~500 MB/s |
| `bcmr serve --parallel 1` | same | ~500 MB/s |
| `bcmr serve --parallel 8` | 8× single-core | ~4 GB/s (then NIC) |
| HPN-SSH (NONE cipher) | plaintext post-auth | ~NIC line rate |
| **Path B (no-crypto LAN)** | plaintext | ~NIC line rate, 1 connection |
| **Path B (AEAD, multi-core)** | AES-GCM chunked across cores | ~2-4 GB/s, 1 connection |
| bbcp / GridFTP | configurable | ~NIC line rate |

Goal: single-TCP-connection throughput approaching NIC line rate
on a 10 GbE LAN (~1.25 GB/s practical). Composable with Path A —
N direct-TCP streams in parallel → still scale with N.

## High-level protocol

```
Client                                      Server
  |                                           |
  | ssh host 'bcmr serve'                     |
  |------------------------------------------>|
  | << normal bcmr serve over SSH (Path A) >> |
  |                                           |
  | (new message) OpenDirectChannel { port_hint?, nonce_c } |
  |------------------------------------------>|
  |                                           | server binds TCP listener
  |                                           | derives session_key from
  |                                           | (ssh_session_id, nonce_c, nonce_s)
  | DirectChannelReady { port, nonce_s }      |
  |<------------------------------------------|
  |                                           |
  | plain TCP connect to server:port          |
  |------------------------------------------>|
  |                                           | server accepts, expects first
  |                                           | frame to contain HMAC(session_key, "hello")
  | AuthHello { hmac }                        |
  |------------------------------------------>|
  |                                           | verify; if mismatch, close
  | AuthOk                                    |
  |<------------------------------------------|
  |                                           |
  | (normal bcmr serve protocol over direct TCP socket) |
  |<==========================================|
  |                                           |
  | SSH control channel stays open as watchdog |
  | if SSH dies mid-batch: server kills listener + tears down data socket |
```

## Key derivation

We need a session-bound key that neither side can forge, and that
ties the data channel to *this* SSH session (prevents replay,
cross-session hijacking).

Option 1 — derive from SSH session-id via `BLAKE3(ssh_session_id || nonce_c || nonce_s)`.
Requires both sides to extract the SSH session ID. **Not
straightforwardly exposed by the OpenSSH client** — unclear if we
can read it from a userspace tool without patching sshd/ssh. TBD.

Option 2 — have the server GENERATE a random session_key, send it
to the client over the (already-authenticated) SSH channel. Client
uses that key to authenticate to the TCP listener. Simpler; the
trust guarantee is "whoever reads the SSH channel can connect to
the TCP listener", which reduces to SSH auth. This is what
**mosh** and **tmate** do.

Lean toward Option 2. Simpler threat model and implementation.

## Wire framing on the direct socket

Same length-prefixed frames as the SSH path. The existing
`protocol.rs` doesn't care about the underlying transport, so
the framing reuses.

AuthHello frame (new type, only valid as the *first* frame on a
direct socket):

```
[4B payload_len][1B TYPE_AUTH_HELLO][32B HMAC-SHA256(session_key, "bcmr-direct-v1")]
```

Server verifies the HMAC before accepting any other frame. Fails →
socket close, no error message (don't give blind probers anything).

Then normal Hello/Welcome handshake runs over the direct socket
(same as SSH path), and Get/Put/etc. follow.

## Data plane crypto: three modes

The direct socket carries what SSH used to. We get to pick the
crypto. Three modes, user-selected:

1. **`direct=plain`** — no crypto on the data socket. For LAN only,
   users explicitly opt in. Wire is a dedicated TCP socket
   between trusted hosts; confidentiality is assumed from network
   segmentation. **Not the default.** Useful when you really want
   NIC line rate and you've read the threat model.
2. **`direct=aead-singlecore`** — AES-GCM-128 with the derived
   session_key, rekeyed every 16 GiB. One CPU core of crypto; same
   ceiling as SSH in practice but without OpenSSH's single-thread
   implementation. Tiny potential win, low complexity.
3. **`direct=aead-multicore`** — AEAD with per-chunk IV so
   different chunks can encrypt/decrypt on different CPU cores.
   Requires a framing tweak (IV counter per Data frame) but scales
   linearly with cores until the memcpy between user and kernel
   bounds us. **The interesting mode.**

Start with `plain` (simplest), add `aead-multicore` once the
rendezvous is proven out.

## Threat model (sketch)

- **Eavesdropper on the LAN wire**: can read everything if
  `direct=plain`. This is the entire point; user opted in. Against
  `direct=aead-*`, protected by AES-GCM confidentiality +
  integrity.
- **Active MITM on the LAN wire**: can inject packets. With
  AuthHello HMAC, can't authenticate to our listener. With `plain`
  mode post-auth, can hijack the session — documented limitation.
- **Attacker with shell on the remote**: can read the session_key
  from bcmr serve's memory, connect to our listener. Same as if
  they'd just run bcmr commands directly; nothing new.
- **Race to the listener**: attacker observes the SSH session,
  sees `DirectChannelReady { port }` (but session_key is
  encrypted inside SSH). Attacker races to `localhost:port` and
  connects first. Server rejects because attacker can't produce
  the HMAC. Legitimate client retries, wins.
- **Port scanner / blind probe**: dials random TCP ports on the
  server. Hits our listener. Sends garbage. We fail AuthHello,
  close. No information leaked beyond "something is listening".

## Firewall considerations

An extra TCP port needs to be reachable. That's a hard "no" on
many locked-down institutional networks that only allow SSH port
22 outbound. For those, Path A is still the answer.

Implementation: `--direct` flag opts INTO path B. Default stays
Path A over SSH. If opted-in and the server reports it can't bind
the port (or client can't reach it), fall back to Path A with a
stderr warning (reusing the fallback-warning machinery from
v0.5.19).

## Port selection

Simple version: server binds `localhost:0` → kernel picks, reports
back. Only works for LAN deployments where the client has direct
TCP reachability. **On a typical SSH-only setup the client can't
reach arbitrary ports on the server** — the server is behind a
bastion / NAT. This is a real deployment gap.

Fancy version: tunnel the direct-TCP connection *through* the SSH
control channel (SSH port forwarding), so the data socket is still
carried over SSH but with our own cipher on top. But now we're
back inside SSH's MTU / crypto path and the benefit is just
"different cipher than OpenSSH picks". Probably not worth the
complexity.

Decision: Path B is a **LAN-mode** feature. WAN users should stay
on Path A (parallel SSH). Document clearly.

## Code organization

```
src/core/transport/
  mod.rs          -- pub trait Transport
  ssh.rs          -- SshTransport: current behavior (always compiled)
  direct_tcp.rs   -- DirectTcpTransport (#[cfg(feature = "direct-transport")])
```

`trait Transport` shape:

```rust
pub trait Transport: Send {
    async fn connect(target: &str, caps: u8) -> Result<Self>;
    async fn send(&mut self, msg: &Message) -> Result<()>;
    async fn recv(&mut self) -> Result<Message>;
    async fn close(self) -> Result<()>;
}
```

`ServeClient` and `ServeClientPool` become generic over T: Transport.
All existing pipelined methods work unchanged — they don't depend
on *which* transport, only that it obeys the Message/Stream
contract.

## Implementation phases

1. **Trait extraction** — pull current `ServeClient` SSH logic
   behind `SshTransport`. No behavior change. All existing tests
   must still pass. **This is the refactor; belongs on a
   `path-b/trait-extraction` sub-branch if it gets messy.**
2. **Direct TCP skeleton** — new subcommand `bcmr serve --listen
   <addr>` that binds, accepts one connection, runs the existing
   dispatch loop on it. No rendezvous yet; user runs it manually.
3. **Rendezvous over SSH** — add OpenDirectChannel /
   DirectChannelReady / AuthHello to the protocol. Auto-binding,
   auto-auth.
4. **Crypto modes** — start with `plain`, add AEAD later if/when
   there's evidence we need it.
5. **Benchmark** — head-to-head on the same loaded-box regime as
   Exp 19, plus an actual 10 GbE LAN if we can borrow one. Target:
   single TCP stream ≥ 2 GB/s on the LAN.

## Non-goals for v1

- WAN support (punted; Path A is the WAN answer).
- Key rotation during a session (16-GiB rekey threshold is a v2
  feature).
- Hardware crypto offload / DPU paths.
- Resume-across-transport (a crash on direct TCP can fall back to
  Path A for the retry — no need to make direct TCP crash-safe on
  its own in v1).

## Open questions that need a real answer before merging

- **Can the client reach server:port?** Deployment reality check —
  on real bcmr user setups (dev machine → workstation, workstation
  → lab server), is the direct TCP port reachable at all?
  Without this, Path B is a dead letter.
- **sshd logging**: binding a TCP listener might generate
  suspicious audit entries on hardened boxes. Check what it looks
  like in syslog / auditd.
- **Metrics**: is the crypto win actually there on modern CPUs,
  or has AES-NI closed the gap enough that per-connection SSH is
  already wire-speed on 10 GbE? **Measure before shipping.** If
  SSH already saturates 10 GbE, Path B is solving a 2020 problem.

**Next step on this branch**: start with phase 1 (trait
extraction). No user-visible change, all existing tests pass,
establishes the seam that the rest of Path B slots into.
