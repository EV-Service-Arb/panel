/**
 * `/api/v1/live` for the dev stub: the panel's WebSocket, enough of RFC 6455 to
 * push text frames and answer a close or a ping — no dependency for a stand-in.
 * The contract is the backend's: `hello` on connect, `changed` after each write,
 * close 4401 without a session.
 *
 *   STUB_LIVE=off    no socket at all (the upgrade answers 404): the status stays `reconnecting`, then `offline`
 *   STUB_LIVE=4401   close every socket at once as "session gone" (4403: "access lost")
 */
import { createHash } from "node:crypto";
import type { IncomingMessage } from "node:http";
import type { Duplex } from "node:stream";

const GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
const MODE = process.env.STUB_LIVE ?? "on";
const open = new Set<Duplex>();

const liveEnabled = MODE !== "off";

/** One unfragmented, unmasked frame: `0x1` text, `0x8` close, `0xA` pong. */
function frame(opcode: number, payload: Buffer): Buffer {
  const len = payload.length;
  const head = len < 126 ? Buffer.from([0x80 | opcode, len]) : len < 65_536 ? Buffer.from([0x80 | opcode, 126, len >> 8, len & 0xff]) : Buffer.alloc(10);
  if (len >= 65_536) {
    head[0] = 0x80 | opcode;
    head[1] = 127;
    head.writeBigUInt64BE(BigInt(len), 2);
  }
  return Buffer.concat([head, payload]);
}

const text = (body: unknown) => frame(0x1, Buffer.from(JSON.stringify(body)));
const closing = (code: number) => frame(0x8, Buffer.from([code >> 8, code & 0xff]));

export function liveUpgrade(req: IncomingMessage, socket: Duplex, session: { signedIn: boolean; userId: string }): void {
  const key = req.headers["sec-websocket-key"];
  if (!liveEnabled || typeof key !== "string") {
    socket.end(`HTTP/1.1 ${liveEnabled ? "400 Bad Request" : "404 Not Found"}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n`);
    return;
  }
  const accept = createHash("sha1").update(key + GUID).digest("base64");
  socket.write(`HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: ${accept}\r\n\r\n`);
  const refuse = !session.signedIn ? 4401 : MODE === "4401" || MODE === "4403" ? Number(MODE) : null;
  if (refuse !== null) {
    socket.end(closing(refuse));
    return;
  }
  open.add(socket);
  socket.write(text({ type: "hello", at: new Date().toISOString(), user_id: session.userId }));
  socket.on("data", (chunk: Buffer) => {
    // The browser only closes and pings; a frame's opcode is the low nibble of its first byte.
    const opcode = (chunk[0] ?? 0) & 0x0f;
    if (opcode === 0x8) {
      open.delete(socket);
      socket.end(closing(1000));
    } else if (opcode === 0x9) socket.write(frame(0xa, Buffer.alloc(0)));
  });
  const forget = () => open.delete(socket);
  socket.on("close", forget);
  socket.on("error", forget);
}

/** Told after the stub's state has changed, as the backend tells after commit. */
export function changed(topic: string, brandId: string | null = null, id: string | null = null): void {
  const msg = text({ type: "changed", topic, brand_id: brandId, id, at: new Date().toISOString() });
  for (const socket of open) socket.write(msg);
}

/** Every `seconds`, or at a random 20–40 s when unset; stops with the process. */
export function every(seconds: number | null, tick: () => void): void {
  const wait = () => (seconds ?? 20 + Math.random() * 20) * 1000;
  const loop = () => {
    tick();
    setTimeout(loop, wait()).unref();
  };
  setTimeout(loop, wait()).unref();
}
