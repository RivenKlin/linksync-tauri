#!/usr/bin/env python3
# LinkSync 屏幕共享 TCP 视频中继（WebRTC 打洞失败时的 30fps 兜底通道）
# HTTP: 监听 127.0.0.1:18767，由 nginx 在 8766 端口以 /lsrelay/ 路径反代（轮询模式）。
# WebSocket: 监听 127.0.0.1:18768，由 nginx 以 /lsrelay-ws/ 路径反代（推流模式，低延迟首选）。
# HTTP 协议: POST JSON
#   push: {"user","passhash","action":"push","au":"<base64 H264 AU>","key":bool}
#   pull: {"user","passhash","action":"pull","target":"A","since":<seq>}
#         返回 {"ok":true,"frames":[{"b":<base64>,"k":bool}...],"max":<seq>,"t":<ts>}
# WS 协议（文本 JSON 行）:
#   首条鉴权 push: {"user","passhash","action":"push"} -> {"ok":true,"role":"push"}
#     之后每帧: {"au":"<base64>","key":bool}，服务器广播给观看端 {"b":..,"k":..,"seq":n}
#   首条鉴权 watch: {"user","passhash","action":"watch","target":"A"}
#     -> {"ok":true,"role":"watch","max":seq}，随后先补发最近关键帧起的存量帧，再实时推送
# 每成员一个内存环形缓冲（最近 90 帧 / 8MB），只保留最新，观看端按 seq 增量拉取。
import asyncio
import base64
import json
import time

try:
    import websockets
except Exception:
    websockets = None

ACCOUNTS = "/etc/linksync/accounts.json"
HOST, PORT = "127.0.0.1", 18767
WS_HOST, WS_PORT = "127.0.0.1", 18768
MAXFRAMES = 90          # 约 3 秒 @30fps
MAXBYTES = 8 * 1024 * 1024
MAXAGE = 10             # 超过 10 秒无新帧视为共享端已退出
MAXAU = 2 * 1024 * 1024  # 单帧 AU base64 上限

# member -> {"q": [(seq, b64, key, t)], "seq": int, "bytes": int}
BUFS = {}
# member -> [watcher websocket, ...]
WATCHERS = {}


def load_accounts():
    try:
        with open(ACCOUNTS, "r") as f:
            return json.load(f)
    except Exception:
        return {}


def auth(body):
    """返回成员字母或 None。与 signal.php 相同的 passhash 校验。"""
    u = str(body.get("user", "")).strip().upper()
    h = str(body.get("passhash", "")).strip().lower()
    if not u or len(h) != 64:
        return None
    acc = load_accounts().get(u)
    if not isinstance(acc, dict):
        return None
    if acc.get("member") == "ADMIN" or acc.get("enabled") is False:
        return None
    if acc.get("passhash", "") != h:
        return None
    me = str(acc.get("member", "")).strip().upper()
    return me if len(me) == 1 and "A" <= me <= "Z" else None


def get_buf(letter):
    b = BUFS.get(letter)
    if b is None:
        b = {"q": [], "seq": 0}
        BUFS[letter] = b
    return b


def push_frame(me, au, key):
    """写入环形缓冲，返回 seq；校验失败返回 None。"""
    if not au or len(au) > MAXAU:
        return None
    try:
        raw = base64.b64decode(au, validate=True)
    except Exception:
        return None
    if len(raw) < 4:
        return None
    b = get_buf(me)
    b["seq"] += 1
    b["q"].append((b["seq"], au, key, time.time()))
    # 环形裁剪：先按帧数再按字节
    while len(b["q"]) > MAXFRAMES or sum(len(x[1]) for x in b["q"]) > MAXBYTES:
        b["q"].pop(0)
    return b["seq"]


def catchup_frames(q, since=0):
    """补发帧：从最近关键帧起（since>0 时取 seq>since 的最早关键帧），最多 30 帧。"""
    if not q:
        return []
    idx = None
    for i in range(len(q) - 1, -1, -1):
        if q[i][2] and q[i][0] > since:
            idx = i
            break
    if idx is None:
        return []
    return q[idx:idx + 30]


async def broadcast(me, payload):
    dead = []
    for w in WATCHERS.get(me, []):
        try:
            await w.send(json.dumps(payload, ensure_ascii=False))
        except Exception:
            dead.append(w)
    for w in dead:
        try:
            WATCHERS[me].remove(w)
        except Exception:
            pass


def handle(body):
    me = auth(body)
    if me is None:
        return {"ok": False, "error": "未登录"}
    action = body.get("action", "")
    if action == "push":
        seq = push_frame(me, str(body.get("au", "")), bool(body.get("key")))
        if seq is None:
            return {"ok": False, "error": "帧无效"}
        # HTTP 推流同样实时广播给 WS 观看端（广播失败不影响推流结果）
        try:
            asyncio.ensure_future(broadcast(me, {"b": body.get("au", ""), "k": bool(body.get("key")), "seq": seq}))
        except Exception:
            pass
        return {"ok": True, "seq": seq}
    if action == "pull":
        target = str(body.get("target", "")).strip().upper()
        if len(target) != 1 or not ("A" <= target <= "Z"):
            return {"ok": False, "error": "目标无效"}
        since = int(body.get("since", 0))
        b = BUFS.get(target)
        now = time.time()
        if not b or not b["q"] or now - b["q"][-1][3] > MAXAGE:
            return {"ok": False, "error": "no_stream", "max": since,
                    "t": b["q"][-1][3] if b and b["q"] else 0}
        frames = [x for x in b["q"] if x[0] > since][:30]
        # 增量起点早于缓冲最老帧（观看端落后太多）：从最早关键帧重同步
        if frames and frames[0][0] != since + 1:
            idx = next((i for i, x in enumerate(b["q"]) if x[2] and x[0] > since), None)
            frames = b["q"][idx:idx + 30] if idx is not None else []
        out = [{"b": x[1], "k": x[2]} for x in frames]
        maxseq = frames[-1][0] if frames else since
        return {"ok": True, "frames": out, "max": maxseq, "t": b["q"][-1][3]}
    return {"ok": False, "error": "bad action"}


async def reader_writer(handler_reader, writer):
    try:
        # 读请求头
        header = b""
        while b"\r\n\r\n" not in header:
            chunk = await asyncio.wait_for(handler_reader.read(4096), timeout=10)
            if not chunk:
                return
            header += chunk
            if len(header) > 64 * 1024:
                return
        head, _, rest = header.partition(b"\r\n\r\n")
        cl = 0
        for line in head.split(b"\r\n")[1:]:
            k, _, v = line.partition(b":")
            if k.strip().lower() == b"content-length":
                cl = int(v.strip() or 0)
        if cl <= 0 or cl > 4 * 1024 * 1024:
            resp = json.dumps({"ok": False, "error": "bad body"})
        else:
            body = rest
            while len(body) < cl:
                chunk = await asyncio.wait_for(handler_reader.read(cl - len(body)), timeout=10)
                if not chunk:
                    return
                body += chunk
            try:
                data = json.loads(body.decode("utf-8"))
            except Exception:
                data = None
            if not isinstance(data, dict):
                resp = json.dumps({"ok": False, "error": "bad json"})
            else:
                resp = json.dumps(handle(data), ensure_ascii=False)
        writer.write(
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json; charset=utf-8\r\n"
            b"Cache-Control: no-store\r\nConnection: close\r\nContent-Length: "
            + str(len(resp.encode("utf-8"))).encode()
            + b"\r\n\r\n"
            + resp.encode("utf-8")
        )
        await writer.drain()
    except Exception:
        pass
    finally:
        try:
            writer.close()
        except Exception:
            pass


async def ws_handler(ws, path=None):
    """WebSocket 端点：push 角色实时推帧并广播；watch 角色订阅指定成员的帧流。
    path 参数兼容 websockets 9.x（旧版回调签名带 path），新版可省略。"""
    me = None
    try:
        first = json.loads(await asyncio.wait_for(ws.recv(), timeout=10))
    except Exception:
        await ws.close()
        return
    if not isinstance(first, dict):
        await ws.close()
        return
    me = auth(first)
    if me is None:
        try:
            await ws.send(json.dumps({"ok": False, "error": "未登录"}))
        except Exception:
            pass
        await ws.close()
        return
    action = first.get("action", "")
    if action == "push":
        await ws.send(json.dumps({"ok": True, "role": "push", "member": me}))
        async for raw in ws:
            try:
                data = json.loads(raw)
            except Exception:
                continue
            if not isinstance(data, dict):
                continue
            au = str(data.get("au", ""))
            key = bool(data.get("key"))
            seq = push_frame(me, au, key)
            if seq is None:
                continue
            await broadcast(me, {"b": au, "k": key, "seq": seq})
    elif action == "watch":
        target = str(first.get("target", "")).strip().upper()
        if len(target) != 1 or not ("A" <= target <= "Z"):
            await ws.send(json.dumps({"ok": False, "error": "目标无效"}))
            await ws.close()
            return
        b = BUFS.get(target)
        await ws.send(json.dumps({"ok": True, "role": "watch",
                                  "max": b["seq"] if b else 0}))
        # 补发存量帧（最近关键帧起），保证观看端能立即解码出画面
        for x in catchup_frames(b["q"] if b else []):
            await ws.send(json.dumps({"b": x[1], "k": x[2], "seq": x[0]},
                                     ensure_ascii=False))
        WATCHERS.setdefault(target, []).append(ws)
        try:
            async for _ in ws:
                pass
        finally:
            try:
                WATCHERS[target].remove(ws)
            except Exception:
                pass
    else:
        await ws.send(json.dumps({"ok": False, "error": "bad action"}))
        await ws.close()


async def main():
    server = await asyncio.start_server(reader_writer, HOST, PORT)
    print("ls-relay listening on {}:{}".format(HOST, PORT), flush=True)
    ws_task = None
    if websockets is not None:
        ws_server = await websockets.serve(ws_handler, WS_HOST, WS_PORT)
        print("ls-relay ws listening on {}:{}".format(WS_HOST, WS_PORT), flush=True)
        ws_task = ws_server
    async with server:
        await server.serve_forever()


if __name__ == "__main__":
    asyncio.run(main())
